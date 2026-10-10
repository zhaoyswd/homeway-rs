#!/bin/zsh
# check-wg-removed.sh — **WG 路径退役**的残留扫查门（M5 设计 §7.1；S5 新建）。
#
# 十条断言（fail-closed；任一击红即退出非 0）：
#   ① WG 类型/模块零引用（crates/**/*.rs 剥注释；`\bwgcore::` / `\bwtransport::` 路径边界匹配）
#   ② WG 术语零残留（源码）：`boringtun` / `noise::` / `local_services` / `parse_handshake_anon`
#      / `peer_index`（**收窄**：`TunFdDead`/`PathProbe` 移出——前者是岛活代码、后者全仓仅注释；
#      `RELAY_LEG_MAX` 移出——它是**保留面**腿表的上限常量，不是 WG 语义）
#   ③ 行文面零 WG 冒充（字符串字面量；剥注释后字符串**保留** ⇒ 本条的命中面即字符串）
#   ④ Cargo 依赖面：根 manifest 无 `boringtun` 依赖行、无 `[patch.crates-io]` 节；
#      `Cargo.lock` 无 `ring 0.16`（计数 = 0）且 `ring 0.17` **仍在**（反向断言）
#   ④-bis **tracked 锁文件全扫**（E 棒门整改）：全仓 tracked `Cargo.lock`（含 tools/**、fuzz/**
#      独立 workspace）零 `boringtun` / 零 `ring 0.16` / 零 `quic-ab-wg*` 包条目
#      （S1b 曾漏 `tools/quic-ab/arms/Cargo.lock` ——④ 原文只扫根 lock）
#   ⑤ 禁止复活哨兵：`ring-shim` / `hmw1` / `_wg`（crates/** + tools/**；显式白名单文件）
#   ⑥ **门自身校准（fail-closed）**：把三类样本注入临时文件 ⇒ 对应条确定性红；正常树绿
#   ⑦ 隔离门 ⑪ 的改写仍在位（⑪(e) 的 stackb 消费者收窄锚 + 自校准锚存在）
#   ⑧ 测试面残留：`cargo test --workspace -- --list` 的用例名零 `wgcore`/`wtransport`/
#      `recover::`/`session::` 前缀；`fuzz/**` 源码零 `wgcore`/`wtransport`（独立 workspace）
#   ⑨ tools/** + fuzz/** 作用域：承载三键与 `wgcore::`/`ring-shim` 零命中（非注释面）
#   ⑩ **ID 空间门**：本批新增 ID（`E-q6`/`E25`/`N-e`/`C20`/`C21`/`N-f`/`N-g`）在
#      `docs/INTEROP-CRITERIA.md` 里**各恰一条行**（`^| <ID> |` 行首式；0 = 登记漏；
#      ≥2 = **双占/撞名**——X1 双占就是历史先例）；正向自校准 = 已占 ID（`E-q5`/`C18`/`C19`）
#      同管线必须命中（否则门空跑）。
#      **与设计 §7.1-⑩ 的差异（登记在案）**：设计写「新 ID 全表 grep = 0」——那是**落登记前**
#      的空位核查（"=0" 形式会让本门在登记后恒红）；登记落地后其常驻不变式 = 「行级各恰一条」
#      （正文里的交叉引用不计——这才是 §7.1-⑩ 真正防的「同 ID 两义」）。
#
# 剥注释：**与 `tools/check-quic-isolation.sh` 同一实现**（从其文件内抽取同一 AWK 六态状态机，
# 抽取失败即红——两门口径不得分叉；六态 = 代码/行注释/块注释/字符串/字符/原始串）。
#
# 用法：tools/check-wg-removed.sh [--fast]   （--fast = 跳过 ⑧ 的 cargo test --list）
set -uo pipefail

REPO_ROOT="${0:h:A:h}"
CRATES="$REPO_ROOT/crates"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
FAILED=0
fail() { echo "!! WG 残留门失败：$1" >&2; FAILED=1; }
info() { echo "   · $1" }

# ---------- 剥注释（与隔离门同一实现：从该文件抽取 AWK_STRIP 的 heredoc） ----------
ISO="$REPO_ROOT/tools/check-quic-isolation.sh"
AWK_STRIP=$(sed -n "/^AWK_STRIP=\$(cat <<'AWK'\$/,/^AWK\$/p" "$ISO" | sed '1d;$d')
if [[ -z "$AWK_STRIP" ]]; then
  echo "!! 剥注释实现抽取失败（$ISO 的 AWK_STRIP heredoc 形态变了）——门 fail-closed" >&2
  exit 1
fi
strip_rs() { LC_ALL=C awk "$AWK_STRIP" "$1"; }

# 树内扫查：$1 = 根，$2 = 扩展正则，$3 = 输出文件（`相对仓根路径:行号:行`）
scan_tree() {
  local root="$1" pat="$2" out="$3" f rel
  : > "$out"
  find "$root" -name '*.rs' -not -path '*/target/*' 2>/dev/null | while read -r f; do
    rel="${f#$REPO_ROOT/}"
    strip_rs "$f" | grep -nE "$pat" | sed "s:^:$rel\::" >> "$out"
  done
}
# 白名单过滤（in-place；$1 = 文件，$2.. = 前缀白名单）
drop_whitelist() {
  local out="$1"; shift
  local tmp="$out.f"; : > "$tmp"
  local line wl skip
  while IFS= read -r line; do
    skip=0
    for wl in "$@"; do
      case "$line" in "$wl":*) skip=1 ;; esac
    done
    (( skip )) || print -r -- "$line" >> "$tmp"
  done < "$out"
  mv "$tmp" "$out"
}

# ---------- ① WG 类型/模块零引用 ----------
# 白名单 = **显式文件清单**（迁址保留面；当前为空——`legframe.rs`/`stackb.rs`/`reg2.rs` 均无 WG 模块名）
WG_ALLOW_FILES=()
scan_tree "$CRATES" '\b(wgcore|wtransport)::' "$TMP/1.txt"
[[ ${#WG_ALLOW_FILES[@]} -eq 0 ]] || drop_whitelist "$TMP/1.txt" "${WG_ALLOW_FILES[@]}"
[[ ! -s "$TMP/1.txt" ]] || fail "① WG 类型/模块仍有引用：$(cat "$TMP/1.txt")"
info "① \`\\bwgcore::\`/\`\\bwtransport::\`（crates/** 剥注释）= 0"

# ---------- ② WG 术语零残留（源码） ----------
PAT2='boringtun|noise::|local_services|parse_handshake_anon|peer_index'
scan_tree "$CRATES" "$PAT2" "$TMP/2.txt"
[[ ! -s "$TMP/2.txt" ]] || fail "② WG 术语残留：$(cat "$TMP/2.txt")"
info "② WG 术语（$PAT2）= 0"

# ---------- ③ 行文面零 WG 冒充（字符串面） ----------
PAT3='经 WG 拨|回落 WG|尝试 WG 兜底|按承载分档|A/B 开关|HOMEWAY_TRANSPORT|tunConfig\.transport|判据=wg|R1 重握手|R2 换源|R3 重赛跑|serve\.quic[^_]'
# 白名单 = **负向断言**所在文件（测试必须写出该串才能断言「不得出现」——C4 交下）
NEG_ASSERT_FILES=(
  "crates/homeway-core/tests/quic_wg_e2e.rs"
  "crates/homeway-core/tests/quic_island_e2e.rs"
  "crates/homeway-core/tests/quic_pf_e2e.rs"
  "crates/homeway-core/tests/quic_stream_perf.rs"
)
scan_tree "$CRATES" "$PAT3" "$TMP/3.txt"
# 二级：**断言上下文**豁免只作用于**负向断言文件**（M5 代码门 M-3 整改：原实现把
# `assert|contains|不得|banned` 的 `grep -v` 放在**全域**，任何产品行只要含这些词就被豁免
# ——`logf("回落 WG —— 不得惊慌")` 不会被击红 = fail-open）。改法：白名单**之外**的文件
# 不看上下文（直接击红）；白名单**之内**只放行「断言里写出该串以证明它不出现」的行。
ctx_exempt_in_wl() { # ctx_exempt_in_wl <文件> <白名单...>
  local out="$1"; shift
  local tmp="$out.f" line wl is_wl; : > "$tmp"
  while IFS= read -r line; do
    is_wl=0
    for wl in "$@"; do case "$line" in "$wl":*) is_wl=1 ;; esac; done
    # 白名单文件内：放行「断言上下文」行（旧口径）；
    # **其余文件**：只放行**严格负向断言形态**（`assert!`/`assert_eq!`/`assert_ne!`/
    # `!contains(`/`必须已删除`/`banned`）——让**内联单测**里的负向断言也能留（如
    # `tun_exec.rs` 的「A/B 开关行（N-d）必须已删除」、`engine.rs` 的 `assert_eq!(… "不再回落 WG")`），
    # 而产品行 `logf("回落 WG —— 不得惊慌")` 不匹配任何形态 ⇒ 照旧击红（M-3 的反面样本）。
    if (( is_wl )); then
      case "$line" in
        *assert*|*contains*|*必须已删除*|*不得*|*banned*) continue ;;
      esac
    else
      case "$line" in
        *'assert!('*|*'assert_eq!'*|*'assert_ne!'*|*'!contains('*|*必须已删除*|*banned*) continue ;;
      esac
    fi
    print -r -- "$line" >> "$tmp"
  done < "$out"
  mv "$tmp" "$out"
}
ctx_exempt_in_wl "$TMP/3.txt" "${NEG_ASSERT_FILES[@]}"
[[ ! -s "$TMP/3.txt" ]] || fail "③ 行文面 WG 冒充残留：$(cat "$TMP/3.txt")"
info "③ 行文面（含 \`serve.quic\` 键字面）= 0（断言上下文豁免**仅限** ${#NEG_ASSERT_FILES[@]} 个负向断言文件内）"

# ---------- ④ Cargo 依赖面 ----------
ROOT_TOML="$REPO_ROOT/Cargo.toml"
LOCK="$REPO_ROOT/Cargo.lock"
sed -e 's:#.*::' "$ROOT_TOML" > "$TMP/root.toml"
grep -qE '^[[:space:]]*boringtun[[:space:]]*=' "$TMP/root.toml" && fail "④ 根 manifest 仍有 boringtun 依赖行"
grep -qE '^\[patch\.crates-io\]' "$TMP/root.toml" && fail "④ 根 manifest 仍有 [patch.crates-io] 节"
ring16=$(awk '/^name = "ring"/{getline; print}' "$LOCK" | grep -c '^version = "0\.16' || true)
ring17=$(awk '/^name = "ring"/{getline; print}' "$LOCK" | grep -c '^version = "0\.17' || true)
[[ "$ring16" == "0" ]] || fail "④ Cargo.lock 仍含 ring 0.16（$ring16 处）"
[[ "$ring17" != "0" ]] || fail "④ 反向断言失败：Cargo.lock 无 ring 0.17（QUIC 用）"
info "④ manifest 无 boringtun/无 [patch]；lock ring 0.16 = 0 / ring 0.17 = $ring17"

# ④-bis **tracked 锁文件全扫**（M5 E 棒门整改：S1b 删 WG 实验臂时漏了
# `tools/quic-ab/arms/Cargo.lock` 的依赖条目——它仍带 `boringtun` / `ring 0.16.20` /
# `quic-ab-wg-ring`，而 ④ 原文只扫根 lock ⇒ 该残留无门可拦。本节把「WG 依赖零残留」
# 扩到**全仓 tracked 的 Cargo.lock**（含 tools/** 与 fuzz/** 独立 workspace），
# 扫的是**文件内容**（非 `git status`）⇒ 未提交的残留同样击红。）
LOCKS=$(cd "$REPO_ROOT" && git ls-files '*Cargo.lock' 2>/dev/null)
[[ -n "$LOCKS" ]] || fail "④-bis 拿不到 tracked Cargo.lock 清单（git ls-files 失败）——门 fail-closed"
while IFS= read -r lk; do
  [[ -n "$lk" ]] || continue
  b=$(grep -c '^name = "boringtun"' "$REPO_ROOT/$lk" || true)
  r16=$(awk '/^name = "ring"/{getline; print}' "$REPO_ROOT/$lk" | grep -c '^version = "0\.16' || true)
  wgring=$(grep -c '^name = "quic-ab-wg' "$REPO_ROOT/$lk" || true)
  [[ "$b" == "0" ]] || fail "④-bis $lk 仍含 boringtun（$b 处）"
  [[ "$r16" == "0" ]] || fail "④-bis $lk 仍含 ring 0.16（$r16 处）"
  [[ "$wgring" == "0" ]] || fail "④-bis $lk 仍含 WG 实验臂包（$wgring 处）"
done <<< "$LOCKS"
info "④-bis tracked Cargo.lock（$(printf '%s\n' "$LOCKS" | wc -l | tr -d ' ') 份）零 boringtun / 零 ring 0.16 / 零 WG 实验臂"

# ---------- ⑤ 禁止复活哨兵 ----------
PAT5='ring-shim|hmw1|_wg'
# 白名单（显式文件清单；每条注明理由——新增必须改门）：
#  · token_vectors.rs：负例**必须**含存量 `hmw1…` 串（「旧串⇒版本拒」的判据本体）
#  · rltoken.rs：测试里 `"hmw1xxxx"` = 「非 rl1 前缀拒」的输入样本
#  · token.rs：**冻结** v1 布局的文档注释（剥注释后本不命中，留档）
#  · quic_wg_e2e.rs：`quic_wg_e2e` 是**文件名/用例名**（`_wg` 子串），非 WG 面引用
#  · vecgen_vectors_test.go：已退役的 Go oracle 函数体（不接线；`hmw1` 是其历史输入）
SB_ALLOW_FILES=(
  "crates/homeway-core/tests/token_vectors.rs"
  "crates/homeway-core/src/relay/rltoken.rs"
  "crates/homeway-core/src/token.rs"
  "crates/homeway-core/tests/quic_wg_e2e.rs"
  #  · quic-wg-e2e.sh：同因（它跑的就是 `--test quic_wg_e2e` 这个**目标名**）
  "tools/quic-wg-e2e.sh"
  "tools/vector-gen/vecgen_vectors_test.go"
  #  · 本门自身：哨兵词表与白名单清单**必须**写出这些串（自指面）
  "tools/check-wg-removed.sh"
)
scan_tree "$CRATES" "$PAT5" "$TMP/5c.txt"
: > "$TMP/5.txt"
cat "$TMP/5c.txt" >> "$TMP/5.txt"
find "$REPO_ROOT/tools" \( -name '*.rs' -o -name '*.sh' \) -not -path '*/target/*' 2>/dev/null | while read -r f; do
  rel="${f#$REPO_ROOT/}"
  case "$f" in
    *.rs) strip_rs "$f" | grep -nE "$PAT5" | sed "s:^:$rel\::" >> "$TMP/5.txt" ;;
    # 脚本面：无六态状态机 ⇒ 按行首 `#` 剥行注释（口径见门头；命中即为真残留）
    *)    grep -nE "$PAT5" "$f" 2>/dev/null | grep -vE '(^|:)[0-9]+:[[:space:]]*#' | sed "s:^:$rel\::" >> "$TMP/5.txt" ;;
  esac
done
drop_whitelist "$TMP/5.txt" "${SB_ALLOW_FILES[@]}"
[[ ! -s "$TMP/5.txt" ]] || fail "⑤ 复活哨兵命中：$(cat "$TMP/5.txt")"
info "⑤ ring-shim / hmw1 / _wg = 0（白名单 ${#SB_ALLOW_FILES[@]} 文件豁免——均为用例/目标名或负例样本）"

# ---------- ⑦ 隔离门 ⑪ 的改写仍在位 ----------
ISO_OK=1
grep -q '消费者面收窄' "$ISO" || ISO_OK=0
grep -q 'server/intercept' "$ISO" || ISO_OK=0
grep -q 'tag_for_port' "$ISO" || ISO_OK=0
[[ "$ISO_OK" == "1" ]] || fail "⑦ 隔离门 ⑪ 的改写面不在（⑪(e) 消费者收窄锚/`tag_for_port` 自校准锚缺）"
info "⑦ 隔离门 ⑪(e)（stackb 消费者收窄）+ 自校准锚在位"

# ---------- ⑧ 测试面残留 ----------
if [[ "${1:-}" == "--fast" ]]; then
  info "⑧ 跳过（--fast）"
else
  (cd "$REPO_ROOT" && cargo test --workspace -- --list 2>/dev/null) | grep -E ': test$' > "$TMP/list.txt" || true
  if [[ ! -s "$TMP/list.txt" ]]; then
    fail "⑧ 用例清单为空（cargo test --list 失败？）——门 fail-closed"
  else
    grep -E '^(wgcore|wtransport|recover|session)::' "$TMP/list.txt" > "$TMP/list-bad.txt" || true
    [[ ! -s "$TMP/list-bad.txt" ]] || fail "⑧ 测试面残留 WG 模块前缀：$(cat "$TMP/list-bad.txt")"
    info "⑧ 用例清单 $(wc -l < "$TMP/list.txt" | tr -d ' ') 条，零 wgcore/wtransport/recover/session 前缀"
  fi
  find "$REPO_ROOT/fuzz" -name '*.rs' 2>/dev/null | while read -r f; do
    grep -nE '\b(wgcore|wtransport)::' "$f" | sed "s:^:${f#$REPO_ROOT/}\::"
  done > "$TMP/fuzz.txt"
  [[ ! -s "$TMP/fuzz.txt" ]] || fail "⑧ fuzz/** 残留 WG 模块引用：$(cat "$TMP/fuzz.txt")"
  info "⑧ fuzz/**（独立 workspace）零 WG 模块引用"
fi

# ---------- ⑨ tools/** + fuzz/** 作用域 ----------
# 排除面：① `*/target/**`（构建产物）；② 注释行（^#，脚本）；③ 显式白名单（退役说明/自校准夹具）
# **M5 代码门 M-4 整改**：补设计 §7.1-⑨ 点名的第五键 `serve.quic`（`[^_]` 边界——
# `serve.quic_listen`/`serve.quic_admit` 是**保留键**，不得误伤）。
PAT9='HOMEWAY_TRANSPORT|tunConfig\.transport|serve\.quic[^_]|wgcore::|ring-shim'
grep -rnE "$PAT9" "$REPO_ROOT/tools" "$REPO_ROOT/fuzz" 2>/dev/null \
  | grep -v '/target/' \
  | grep -vE '(^|:)[0-9]+:[[:space:]]*#' \
  | sed "s:^$REPO_ROOT/::" > "$TMP/9.txt" || true
drop_whitelist "$TMP/9.txt" \
  "tools/check-quic-isolation.sh" \
  "tools/check-wg-removed.sh" \
  "tools/m1-ab-e2e.sh" \
  "tools/m2-s5-e2e.sh" \
  "tools/quic-wg-e2e.sh" \
  "tools/quic-ab/README.md" \
  "tools/vector-gen/README.md" \
  "tools/vector-gen/vecgen_vectors_test.go" \
  "tools/m1-ab/src/main.rs" \
  "tools/quic-island-e2e.sh"
[[ ! -s "$TMP/9.txt" ]] || fail "⑨ tools/**+fuzz/** 残留（非注释）：$(cat "$TMP/9.txt")"
info "⑨ tools/** + fuzz/** 承载三键/模块引用 = 0"

# ---------- ⑩ ID 空间门 ----------
DOC="$REPO_ROOT/docs/INTEROP-CRITERIA.md"
NEW_IDS=(E-q6 E25 N-e C20 C21 N-f N-g)
OCCUPIED_IDS=(E-q5 C18 C19)
# 自校准（**两条管线各自校准**——M5 代码门 M-2 整改：原实现用「全表词边界」校
# `E-q5`/`C18`/`C19`，而受测管线是「行级」，两者不同管线 ⇒ 校准对被测面零覆盖，
# 门自身日志却打「行级全命中」= 假陈述）：
#  · 词边界存在性管线（散文/登记行里的 ID 也能命中）——校已占 ID `E-q5`/`C18`/`C19`；
#  · **行级管线**（与受测面同管线）——校已占且**行内可判**的 ID `E7`/`E22`/`C13`（主表行式）。
row_count() { grep -cE "^\| *(~~)?$1(~~)? *\|" "$DOC" || true; }
for id in "${OCCUPIED_IDS[@]}"; do
  n=$(grep -cE "(^|[^A-Za-z0-9-])${id}([^A-Za-z0-9-]|$)" "$DOC" || true)
  [[ "$n" != "0" ]] || fail "⑩ 自校准失败（词边界管线）：已占 ID ${id} 零命中（门空跑 = 失准）"
done
ROW_CALIB_IDS=(E7 E22 C13)
for id in "${ROW_CALIB_IDS[@]}"; do
  n=$(row_count "$id")
  [[ "$n" != "0" ]] || fail "⑩ 自校准失败（**行级**管线）：已占 ID ${id} 零行命中（受测管线失准）"
done
# 新 ID：**行级**各恰一条（`| ID |` 或 `| ~~ID~~ |`）——双占 = 撞名（X1 双占是历史先例）
for id in "${NEW_IDS[@]}"; do
  n=$(row_count "$id")
  [[ "$n" != "0" ]] || fail "⑩ 新 ID ${id} 零行命中（登记漏）"
  [[ "$n" -le 1 ]] || fail "⑩ 新 ID ${id} 占 ${n} 行（双占/撞名——X1 双占是历史先例）"
done
info "⑩ ID 空间：新 ID 行级各恰 1；自校准 = 词边界（${OCCUPIED_IDS[*]}）+ 行级（${ROW_CALIB_IDS[*]}）双管线命中"

# ---------- ⑥ 门自身校准（注入负例 ⇒ 确定性红） ----------
calib() { # $1 = 文件名（临时），$2 = 内容，$3 = 正则
  print -r -- "$2" > "$TMP/$1.rs"
  LC_ALL=C awk "$AWK_STRIP" "$TMP/$1.rs" | grep -qE "$3"
}
calib i1 'fn f() { let _ = wgcore::Client::start; }' '\b(wgcore|wtransport)::' \
  || fail "⑥ 注入负例①（wgcore::）未命中——① 条失准"
calib i2 'fn f() { let s = "本世代回落 WG 承载"; }' '回落 WG' \
  || fail "⑥ 注入负例②（回落 WG 字符串）未命中——③ 条失准"
calib i3 'serve.quic = true' 'serve\.quic[^_]' \
  || fail "⑥ 注入负例③（serve.quic 键）未命中——③ 条失准"
calib i4 'let p = "tools/ring-shim";' 'ring-shim' \
  || fail "⑥ 注入负例④（ring-shim）未命中——⑤ 条失准"
if calib i5 '// wgcore:: 已退役' '\b(wgcore|wtransport)::'; then
  fail "⑥ 反校准失败：注释里的 wgcore:: 被判命中（剥注释没在剥）"
fi
if calib i6 'let s = "quic://x"; // wgcore::' '\b(wgcore|wtransport)::'; then
  fail "⑥ 反校准失败：字符串后的行注释未剥（旧 sed 口径的绕过面）"
fi
info "⑥ 门自校准 4 正 2 反，全过"

echo ""
if (( FAILED )); then
  echo "!! WG 残留门：**有断言击红**（见上）" >&2
  exit 1
fi
echo "WG 残留门：**十条全绿**"
