#!/bin/zsh
# check-vocab.sh — 词表三方门（R5-5e；设计 §6，评审 G-7/G-8/G-9 整改口径）
#
# 真源链：homeway `contracts/ledger.jsonl`（baseline 克隆只读，422 单元）→
# tier `tools/gen/vocab-manifest.json`（16 App 消费单元，只读）→ Rust 侧
# **编译期真源 dump**（tests/vocab_dump.rs——不做源码正则解析）。
#
# 门禁（fail-closed）：
#  ① ledger 文件 sha256 == docs/BASELINE.md 锚定值（三份副本字节同一性断言）；
#  ② 按面过滤后的 ledger active 集 ⊇ Rust 声明集（超集不是恰等——Rust 核只承担
#     cli/cp/direct 面；过滤 = family+unit 匹配 ∧ status ∈ {active, compat-passthrough}
#     ∧ faces 与声明面相交——照抄 tier 生成器三重过滤）；
#  ③ Rust 声明集 ∩ legacy-unreachable = ∅（如 via/tunnel 不得出现）；
#  ④ 带理由允许缺席表（fail-closed 只对未登记缺席生效）；
#  ⑤ files-proto/code 采用 tier 排除表语义（tier check-vocab-sync.sh 本就把它放
#     排除表——族④真源码）：不做 manifest 存在性检查，只对 ledger 值集做 ②③；
#  ⑥ manifest 交叉面报 INFO 不红（App 面值集归 R7 napi 批）。
set -uo pipefail

REPO_ROOT="${0:h:A:h}"
LEDGER="$REPO_ROOT/baseline/homeway/contracts/ledger.jsonl"
# tier manifest（只读）：兄弟仓相对推导 + 环境变量覆盖口（R5 二轮 低-10 整改——原为
# 本机绝对路径硬编码，换机即取不到；缺位仍由下方 [[ -f ]] fail-closed 拦截）
TIER_ROOT="${HOMEWAY_TIER_ROOT:-$REPO_ROOT/../tier}"
MANIFEST="$TIER_ROOT/tools/gen/vocab-manifest.json"
BASELINE_MD="$REPO_ROOT/docs/BASELINE.md"

[[ -f "$LEDGER" ]] || { echo "!! ledger 不在（baseline 克隆缺失）" >&2; exit 1; }
[[ -f "$MANIFEST" ]] || { echo "!! tier manifest 不在（只读路径）" >&2; exit 1; }

# ① ledger sha256 与 BASELINE.md 锚定值一致（fail-closed：锚定值取不到即红——
# 第二道门 中-9 整改，原 [[ -n && != ]] 形态在锚定缺失时静默跳过）
WANT_SHA=$(grep -m1 -oE 'ledger\.jsonl` = `[0-9a-f]{64}' "$BASELINE_MD" | grep -oE '[0-9a-f]{64}')
if [[ -z "$WANT_SHA" ]]; then
  echo "!! BASELINE.md 里取不到 ledger sha256 锚定值（措辞改动破坏了正则？）——fail-closed 拒跑" >&2
  exit 1
fi
GOT_SHA=$(shasum -a 256 "$LEDGER" | cut -d' ' -f1)
if [[ "$WANT_SHA" != "$GOT_SHA" ]]; then
  echo "!! ledger sha256 漂移：锚定 $WANT_SHA ≠ 实际 $GOT_SHA——升级基线未更新 BASELINE.md？" >&2
  exit 1
fi

# Rust 侧编译期真源 dump
DUMP=$(cd "$REPO_ROOT" && cargo test -p homeway-core --test vocab_dump -- --nocapture 2>/dev/null | grep -E '^[a-z-]+/[a-z0-9-]+\t')
if [[ -z "$DUMP" ]]; then
  echo "!! vocab_dump 无输出（编译期真源提取失败）" >&2
  exit 1
fi

python3 - "$LEDGER" "$MANIFEST" <<'PY' "$DUMP"
import json, sys

ledger_path, manifest_path = sys.argv[1], sys.argv[2]
rust_lines = sys.argv[3].strip().splitlines()

# Rust 声明集（unit → {value}），并显式声明每 unit 承担的面
rust = {}
for line in rust_lines:
    unit, value = line.split('\t')
    rust.setdefault(unit, set()).add(value)

# 各 unit 声明面（与 faces 字段相交判定；Rust 核 = cli/cp/direct 面，非 napi-only）
DECLARED_FACES = {
    "speedtest-reason/reason": {"cli", "cp"},
    "portfwd/err": {"cli", "napi"},
    "event-payload/via": {"cli", "cp"},
    "event-payload/state": {"cli", "cp"},
    "files-proto/code": {"direct", "napi"},
}

# 允许缺席表（带理由——fail-closed 只对未登记缺席生效；G-7 整改）
ALLOWED_ABSENT = {
    ("speedtest-reason/reason", "bridge_down"): "App bridge 面值（pkg/speedtest engine 桥层归一码）——Rust 核无 bridge，R7 napi 批再产",
    ("speedtest-reason/reason", "bridge_auth"): "同上（App bridge 面）",
    ("portfwd/err", "dial_failed"): "登记保留值（台账 note 标 reserve）——本核形态不产出",
    ("portfwd/err", "invalid_target"): "登记保留值——本核形态不产出",
}

# tier 侧排除表语义（check-vocab-sync.sh 同款）：files-proto 不做 manifest 存在性检查
TIER_EXCLUDED = {"files-proto/code"}

recs = [json.loads(l) for l in open(ledger_path) if l.strip()]
errors = []
info = []

# ledger 值集（三重过滤）
def ledger_values(unit, faces):
    fam, _, uname = unit.partition('/')
    out = {}
    for r in recs:
        if r.get('family') != fam or r.get('unit') != uname:
            continue
        if r.get('status') not in ('active', 'compat-passthrough'):
            continue
        rfaces = set(r.get('faces', []))
        if rfaces & faces:
            v = r.get('value')
            if v is not None:
                out[v] = r.get('status')
    return out

for unit, values in sorted(rust.items()):
    if unit not in DECLARED_FACES:
        errors.append(f"Rust 声明了未登记的单元 {unit}（vocab_dump 与 check-vocab 的映射表不同步？）")
        continue
    faces = DECLARED_FACES[unit]
    lv = ledger_values(unit, faces)
    # ② 超集判定 + ④ 允许缺席
    for v in sorted(values):
        if v in lv:
            continue
        # 反向不豁免（R5 二轮 低-9 整改：原 if/else 两臂同一 append 是死分支）——
        # 缺席表只服务「ledger 有 Rust 无」的方向；Rust 产出 ledger 不认识的值恒红
        errors.append(f"{unit}: Rust 值 {v!r} 不在 ledger 过滤后值集（ledger 全态：{sorted(lv)}）")
    # ③ legacy-unreachable 相交 = 空（按全 ledger 查——不看 faces 过滤，防面掩蔽）
    fam, _, uname = unit.partition('/')
    legacy = {r.get('value') for r in recs
              if r.get('family') == fam and r.get('unit') == uname
              and r.get('status') == 'legacy-unreachable'}
    bad = values & legacy
    if bad:
        errors.append(f"{unit}: Rust 声明了 legacy-unreachable 值 {sorted(bad)}")
    # ④ 反向缺席：ledger active（按面）有、Rust 没有 → 查缺席表
    for v, st in sorted(lv.items()):
        if v not in values and (unit, v) not in ALLOWED_ABSENT:
            errors.append(f"{unit}: ledger {st} 值 {v!r} 在 Rust 声明集中缺席且不在允许缺席表")
        elif (unit, v) in ALLOWED_ABSENT:
            info.append(f"缺席（登记）：{unit}/{v}——{ALLOWED_ABSENT[(unit, v)]}")

# ⑥ manifest 交叉（INFO 不红）
try:
    manifest = json.load(open(manifest_path))
    munits = {(u['family'], u['unit']) for u in manifest.get('units', [])}
    for unit in rust:
        if unit in TIER_EXCLUDED:
            info.append(f"manifest 交叉：{unit} 在 tier 排除表（族④真源码）——不做存在性检查")
            continue
        fam, _, uname = unit.partition('/')
        if (fam, uname) not in munits:
            info.append(f"manifest 交叉：{unit} 不在 tier manifest 16 单元（App 生成面未消费——值集仍对 ledger 对账）")
except Exception as e:
    info.append(f"manifest 读取失败（不红）：{e}")

for i in info:
    print(f"  INFO {i}")
if errors:
    for e in errors:
        print(f"  RED  {e}")
    print(f"词表门：FAIL（{len(errors)} 处漂移）")
    sys.exit(1)
n = sum(len(v) for v in rust.values())
print(f"词表门：PASS（Rust 声明 {len(rust)} 单元 / {n} 值；ledger sha256 一致；缺席表 {len([1 for k in ALLOWED_ABSENT])} 项在册）")
PY
