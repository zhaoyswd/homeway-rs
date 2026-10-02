#!/bin/zsh
# gen-fuzz-seeds.sh — fuzz corpus 种子运行期展开（R5-5b；评审 ②-5：以 fixtures 为
# 单源，不重复入库字节；CI 校验展开 hash）。
#
# 从 fixtures/vectors/*.json 提取字节字段（token/relay/stun_sped/reg/files_frames），
# 展开到 fuzz/corpus.seeds/<目标>/（**纯种子区**——cargo-fuzz 的 corpus/ 是运行期增长
# 目录会污染，种子 hash 只对本目录算）。跑 fuzz 前把种子拷进 corpus/：
#   cp fuzz/corpus.seeds/<t>/* fuzz/corpus/<t>/ 2>/dev/null || true
# 展开汇总 hash 打印到 stdout 供 CI 比对（种子文件不变时应稳定）。
set -euo pipefail

REPO_ROOT="${0:h:A:h}"
OUT="$REPO_ROOT/fuzz/corpus.seeds"

python3 - "$REPO_ROOT" "$OUT" <<'PY'
import json, os, sys, hashlib

root, out = sys.argv[1], sys.argv[2]
vec = os.path.join(root, "fixtures/vectors")

def unhex(s):
    return bytes.fromhex(s) if s else b""

def write(target, name, data: bytes):
    d = os.path.join(out, target)
    os.makedirs(d, exist_ok=True)
    with open(os.path.join(d, name), "wb") as f:
        f.write(data)

n = 0
# token：正样本串 + 错误样本 body（若含 hex）
t = json.load(open(os.path.join(vec, "token.json")))
for i, c in enumerate(t.get("cases", [])):
    if "token" in c:
        write("fuzz_token", f"tok{i}", c["token"].encode())
        n += 1
for i, c in enumerate(t.get("errors", [])):
    body = c.get("body_b64") or ""
    if not body:
        continue
    import base64
    try:
        write("fuzz_token", f"tokerr{i}", base64.b64decode(body))
        n += 1
    except Exception:
        pass

# relay：控制帧 case 载荷 + legup 样本
r = json.load(open(os.path.join(vec, "relay.json")))
for i, c in enumerate(r.get("cases", [])):
    wire = unhex(c.get("wire", ""))
    if wire:
        # 壳形态（出口/中继腿上收到的形态）
        write("fuzz_relay_ctl", f"ctl{i}", bytes([0xBB, 3]) + len(wire).to_bytes(2, "big") + wire)
        write("fuzz_relay_ctl", f"ctlraw{i}", wire)
        n += 2
for key in ("dh", "nonce", "cookie"):
    v = r.get(key)
    if isinstance(v, str):
        write("fuzz_relay_ctl", key, unhex(v))
        n += 1

# stun_sped：请求/应答/帧
ss = json.load(open(os.path.join(vec, "stun_sped.json")))
for i, c in enumerate(ss["stun"]["requests"]):
    write("fuzz_probe", f"stunreq{i}", unhex(c["wire"]))
    n += 1
for i, c in enumerate(ss["stun"]["responses"]):
    write("fuzz_probe", f"stunresp{i}", unhex(c["wire"]))
    n += 1
for i, c in enumerate(ss["sped"]["controls"]):
    write("fuzz_speedtest", f"ctl{i}", unhex(c["wire"]))
    n += 1
for i, c in enumerate(ss["sped"]["datas"]):
    # 65535 样本太大（默认 max_len 4096）——只收 1B/1400B
    if c["len"] <= 4096:
        write("fuzz_speedtest", f"data{i}", unhex(c["wire"]))
        n += 1

# reg：报文字节（腿帧族）
g = json.load(open(os.path.join(vec, "reg.json")))
for i, c in enumerate(g.get("cases", [])):
    wire = unhex(c.get("wire", ""))
    if wire:
        write("fuzz_leg_frame", f"reg{i}", bytes([0xBB, 2]) + len(wire).to_bytes(2, "big") + wire)
        n += 1

# files_frames：4B 前缀帧
ff = json.load(open(os.path.join(vec, "files_frames.json")))
for i, c in enumerate(ff.get("cases", []) if isinstance(ff.get("cases"), list) else ff.get("frames", [])):
    wire = c.get("wire_hex") or c.get("wire") or ""
    if isinstance(wire, str) and wire:
        b = unhex(wire)
        if len(b) <= 4096:
            write("fuzz_files", f"frame{i}", b)
            n += 1

print(f"seeds={n}", file=sys.stderr)

# 汇总 hash（种子文件内容级——文件名参与排序后稳定）
h = hashlib.sha256()
for target in sorted(os.listdir(out)):
    d = os.path.join(out, target)
    if not os.path.isdir(d):
        continue
    for f in sorted(os.listdir(d)):
        h.update(target.encode())
        h.update(f.encode())
        h.update(open(os.path.join(d, f), "rb").read())
print(h.hexdigest())
PY
