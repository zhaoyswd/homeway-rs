#!/usr/bin/env python3
"""Independent stdlib oracle for WebServe v1 fixtures; --write explicitly blesses.

Source contract: openspec/changes/independent-webserve/protocol.md. This module
never imports homeway implementation and never reads user state or credentials.
All keys below are public deterministic test material, unsuitable for deployment.
"""
import argparse
import base64
import hashlib
import hmac
import json
from pathlib import Path
import struct

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "fixtures" / "webserve"
PREFIX = b"hww1"
ALPHABET = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_"


def b64(data):
    return base64.urlsafe_b64encode(data).rstrip(b"=").decode("ascii")


def token_body(public_key, rpk, key_id, secret):
    fields = public_key + rpk + key_id + secret
    assert len(fields) == 112
    return fields + hashlib.sha256(PREFIX + fields).digest()[:4]


def frame(op, payload):
    return struct.pack(">BI", op, len(payload)) + payload


def documents():
    public_key = bytes(range(32))
    rpk = bytes(range(32, 64))
    key_id = bytes(range(64, 80))
    secret = bytes(range(80, 112))
    nonce = bytes(range(112, 128))
    exporter = bytes(range(128, 160))
    body = token_body(public_key, rpk, key_id, secret)
    token = "hww1" + b64(body)
    assert len(body) == 116 and len(token) == 159
    canonical_tail = ALPHABET.index(token[-1])
    assert canonical_tail % 4 == 0
    noncanonical = token[:-1] + ALPHABET[canonical_tail + 1]
    assert base64.urlsafe_b64decode(noncanonical[4:] + "=") == body
    corrupt = bytearray(body)
    corrupt[0] ^= 1
    invalid = [
        {"name": "unsupported_version", "input": "hww2" + token[4:], "error": "UnsupportedVersion"},
        {"name": "checksum", "input": "hww1" + b64(corrupt), "error": "Malformed"},
        {"name": "short_body", "input": "hww1" + b64(body[:-1]), "error": "Malformed"},
        {"name": "extra_byte", "input": "hww1" + b64(body + b"x"), "error": "Malformed"},
        {"name": "padding", "input": token + "=", "error": "Malformed"},
        {"name": "internal_newline", "input": token[:30] + "\n" + token[30:], "error": "Malformed"},
        {"name": "internal_space", "input": token[:30] + " " + token[30:], "error": "Malformed"},
        {"name": "non_url_alphabet", "input": token[:30] + "+" + token[31:], "error": "Malformed"},
        {"name": "noncanonical_tail_bits", "input": noncanonical, "error": "Malformed"},
    ]
    token_doc = {
        "schema": "homeway-web-token-v1", "source": "protocol.md §1; independent Python stdlib oracle",
        "public_test_material": True,
        "valid": [{"name": "incrementing_fields", "public_key_hex": public_key.hex(), "rpk_hex": rpk.hex(),
                   "key_id_hex": key_id.hex(), "secret_hex": secret.hex(), "body_hex": body.hex(),
                   "token": token, "disk_key_hex": (b"HWWK\x01" + key_id + secret).hex(),
                   "relay_label_hex": hashlib.sha256(public_key).digest()[:8].hex(),
                   "trimmed_input": "\u2003" + token + "\r\n"}], "invalid": invalid,
    }
    message = b"homeway-web-auth-v1" + b"\x01" + key_id + nonce + exporter
    mac = hmac.digest(secret, message, "sha256")
    handshake = {
        "schema": "homeway-web-auth-v1", "source": "protocol.md §2; independent Python stdlib oracle",
        "public_test_material": True, "version": 1, "key_id_hex": key_id.hex(), "secret_hex": secret.hex(),
        "nonce_hex": nonce.hex(), "exporter_hex": exporter.hex(),
        "exporter_label": "homeway-web-exporter-v1", "exporter_context_hex": "", "mac_input_hex": message.hex(),
        "mac_hex": mac.hex(),
        "hello_hex": (b"W1\x01" + key_id).hex(), "challenge_hex": (b"WC\x01" + nonce).hex(),
        "proof_hex": (b"WP\x01" + key_id + mac).hex(), "accept_hex": b"WA\x01".hex(),
        "negative_mac": [
            {"name": "wrong_secret", "secret_hex": bytes([0xA5] * 32).hex(), "exporter_hex": exporter.hex(),
             "mac_hex": hmac.digest(bytes([0xA5] * 32), message, "sha256").hex()},
            {"name": "other_tls_connection", "secret_hex": secret.hex(), "exporter_hex": bytes([0x5A] * 32).hex(),
             "mac_hex": hmac.digest(secret, message[:-32] + bytes([0x5A] * 32), "sha256").hex()},
        ],
        "invalid_frames": [
            {"name": "hello_wrong_version", "wire_hex": (b"W1\x02" + key_id).hex(), "close_code": 0x71},
            {"name": "proof_wrong_magic", "wire_hex": (b"XX\x01" + key_id + mac).hex(), "close_code": 0x71},
            {"name": "proof_wrong_key_id", "wire_hex": (b"WP\x01" + bytes(16) + mac).hex(), "close_code": 0x70},
        ],
    }
    valid_frames = []
    for name, op, payload, direction in [
        ("open_100x30", 1, struct.pack(">BHH", 1, 100, 30), "client"),
        ("input_utf8", 2, "printf '回家🙂'\r".encode(), "client"),
        ("resize_max", 3, struct.pack(">HH", 1000, 500), "client"),
        ("output_ansi_utf8", 4, "\x1b[32m回家🙂\x1b[0m\r\n".encode(), "server"),
        ("close", 5, b"", "both"),
        ("error", 6, bytes([5]) + "本机控制面不可用".encode(), "server"),
        ("exit_unknown", 7, struct.pack(">i", -1), "server"),
    ]:
        valid_frames.append({"name": name, "op": op, "direction": direction, "payload_hex": payload.hex(),
                             "wire_hex": frame(op, payload).hex()})
    invalid_frames = [
        {"name": "oversize_declared_length", "wire_hex": struct.pack(">BI", 2, 65537).hex(), "reason": "length_before_allocation"},
        {"name": "truncated_header", "wire_hex": "020000", "reason": "unexpected_eof"},
        {"name": "truncated_payload", "wire_hex": (struct.pack(">BI", 2, 2) + b"x").hex(), "reason": "unexpected_eof"},
        {"name": "unknown_opcode", "wire_hex": frame(255, b"").hex(), "reason": "unknown_opcode"},
        {"name": "open_zero_columns", "wire_hex": frame(1, struct.pack(">BHH", 1, 0, 30)).hex(), "reason": "dimensions"},
        {"name": "open_wrong_version", "wire_hex": frame(1, struct.pack(">BHH", 2, 100, 30)).hex(), "reason": "version"},
        {"name": "resize_too_tall", "wire_hex": frame(3, struct.pack(">HH", 100, 501)).hex(), "reason": "dimensions"},
        {"name": "empty_input", "wire_hex": frame(2, b"").hex(), "reason": "empty_input"},
        {"name": "close_with_body", "wire_hex": frame(5, b"x").hex(), "reason": "payload_length"},
        {"name": "exit_short", "wire_hex": frame(7, b"x").hex(), "reason": "payload_length"},
    ]
    terminal = {"schema": "homeway-web-terminal-v1", "source": "protocol.md §3; independent Python stdlib oracle",
                "valid": valid_frames, "invalid": invalid_frames,
                "invalid_states": [{"name": "input_before_open", "sequence": ["input_utf8"]},
                                   {"name": "duplicate_open", "sequence": ["open_100x30", "open_100x30"]},
                                   {"name": "client_output", "direction": "client", "sequence": ["output_ansi_utf8"]}]}
    return {"token_hww1.json": token_doc, "management_auth.json": handshake, "terminal_frames.json": terminal}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--write", action="store_true", help="explicitly regenerate checked-in deterministic vectors")
    mode.add_argument("--check", action="store_true", help="verify byte-for-byte, without writing (default)")
    args = parser.parse_args()
    for name, data in documents().items():
        encoded = (json.dumps(data, ensure_ascii=False, indent=2) + "\n").encode()
        path = OUT / name
        if args.write:
            OUT.mkdir(parents=True, exist_ok=True)
            path.write_bytes(encoded)
        elif not path.is_file() or path.read_bytes() != encoded:
            raise SystemExit("FAIL: deterministic WebServe fixture differs: " + str(path))
    print("PASS: independent WebServe v1 vectors " + ("written" if args.write else "verified"))


if __name__ == "__main__":
    main()
