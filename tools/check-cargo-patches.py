#!/usr/bin/env python3
"""Fail-closed Cargo patch policy for the WG removal gate (Python 3.11+).

Only the reviewed crossterm 0.29 terminal-input fix may replace a registry crate.
Use TOML semantics so aliases, extra registries and alternate table syntax cannot
bypass the policy. Missing tomllib is an error, never a regex fallback.
"""
import argparse
from pathlib import Path
import sys

try:
    import tomllib
except ImportError:
    raise SystemExit("Cargo 补丁门失败：需要 Python 3.11+ 标准库 tomllib；不降级为文本匹配")


ALLOWED = {"crates-io": {"crossterm": {"path": "vendor/crossterm"}}}


def check(text):
    patches = tomllib.loads(text).get("patch", {})
    if patches != {} and patches != ALLOWED:
        raise ValueError("仅允许无 patch 或精确 crossterm = { path = 'vendor/crossterm' }；拒绝其他补丁")


def self_test():
    positive = [
        "[package]\nname='sample'\n",
        '[patch.crates-io]\ncrossterm = { path = "vendor/crossterm" }\n',
        "[patch.'crates-io'.crossterm]\npath = 'vendor/crossterm' # same TOML map\n",
    ]
    negative = [
        '[patch.crates-io]\nring = {path="vendor/retired-ring"}\n',
        '[patch.crates-io]\nboringtun = {path="tools/retired"}\n',
        '[patch.crates-io]\nunknown = {path="vendor/unknown"}\n',
        '[patch.crates-io]\ncrossterm = {path="vendor/other"}\n',
        '[patch.crates-io]\ncrossterm = {path="/tmp/crossterm"}\n',
        '[patch.crates-io]\ncrossterm = {path="vendor/../crossterm"}\n',
        '[patch.crates-io]\ncrossterm = {git="https://example.invalid/crossterm"}\n',
        '[patch.crates-io]\ncrossterm = {path="vendor/crossterm",version="0.29"}\n',
        '[patch.crates-io]\ncrossterm = {path="vendor/crossterm",package="ring"}\n',
        '[patch.crates-io]\nalias = {path="vendor/crossterm",package="crossterm"}\n',
        '[patch."https://example.invalid/index"]\ncrossterm = {path="vendor/crossterm"}\n',
        '[patch.crates-io]\ncrossterm = {path="vendor/crossterm"}\nring = {version="0.17"}\n',
        '[patch.crates-io]\ncrossterm = {path="vendor/crossterm"}\n[patch."other"]\n',
        'patch = false\n',
        '[patch.crates-io]\ncrossterm = {path="vendor/crossterm"}\n[patch.crates-io]\n',
        '[patch.crates-io\n',
    ]
    for text in positive:
        check(text)
    for index, text in enumerate(negative):
        try:
            check(text)
        except (ValueError, tomllib.TOMLDecodeError):
            continue
        raise ValueError(f"补丁门自校准负例 {index + 1} 未被拒绝")
    return len(positive), len(negative)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("manifest", type=Path, nargs="?", default=Path(__file__).resolve().parent.parent / "Cargo.toml")
    args = parser.parse_args()
    try:
        calibration = self_test() if args.self_test else None
        check(args.manifest.read_text())
    except (OSError, ValueError, tomllib.TOMLDecodeError) as error:
        print(f"Cargo 补丁门失败：{error}", file=sys.stderr)
        return 1
    suffix = f"；自校准 {calibration[0]} 正/{calibration[1]} 反" if calibration else ""
    print(f"Cargo 补丁门：PASS（无补丁或唯一受控 crossterm/path{suffix}）")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
