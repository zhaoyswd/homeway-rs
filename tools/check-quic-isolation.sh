#!/bin/zsh
# check-quic-isolation.sh — 「禁止 async 泄漏进同步面 + QUIC 数据面纪律」的源码门
# （M0 设计 §3.4 层 3；M1 S6-2 扩第 ⑥–⑩ 条）。
#
# **九条断言**（fail-closed；任一击红即退出非 0）：
#   ① 异步栈名字只出现在 crates/homeway-quic/Cargo.toml —— homeway-core / homeway-cli /
#      homeway-capi 的 manifest 零命中（防同步面拿到异步依赖 ⇒ 层 0 的 E0433 保证失效）；
#   ② crates/homeway-quic/src/** 里 `tokio::|quinn|rustls|async fn|.await` **只在异步面**
#      （**显式文件清单**，M1 S6-2 由目录前缀收窄而来：整棵子树豁免会把「exit/transport.rs
#      这类本可零异步名的文件」一起免检，且新文件静默落在豁免面内）；
#   ③ 岛内 `std::thread::sleep` 零命中（异步上下文禁阻塞）、`block_on` 只在异步面；
#   ④ `aws-lc` 在 manifest 与 Cargo.lock 零命中（防 rustls 默认 features 回归——
#      默认含 aws_lc_rs ⇒ 拉 aws-lc-sys = BoringSSL 派生 + cmake，OHOS 不可行）；
#   ⑤ `crates/` 内 `dangerous()` / `with_custom_certificate_verifier` 只许出现在 RPK
#      钉定的单一文件（`src/exit/rpk.rs`），且该文件必须**真做签名验证**——
#      `verify_tls13_signature_with_raw_key` 同文件零命中即判红；其余文件仍**零命中**
#      （harness 的跳过验证永不得进产品面）。**harness 侧**（`tools/**`）：文件里的
#      `dangerous()` 必须带 `SECURITY: harness-only` 标记（标记 = 可审计的显式声明）；
#   ⑥ **裸 `send_datagram(` 零命中**（M1 S6-2 #1 / 设计 §1.5/§2.4）：产品面（`crates/**`）
#      的 DATAGRAM 发送**只允许**经两处包装函数体内（`client/dataplane.rs` 与
#      `exit/conn.rs` 的 `send_datagram_checked`）——裸调用在缓冲满时**静默淘汰最旧且恒返
#      Ok**（M1 设计 §0.3 的 P4 实测）。作用于 `crates/**`（`tools/**` 的 harness 不受约束）；
#   ⑦ **`send_datagram_wait` 零命中**（S6-2 #2 / 设计 §6.1）：阻塞式等待会把背压变成岛内
#      await 挂起（热路径禁用）；该 API 只允许出现在 harness；
#   ⑧ **中继面零异步名**（S6-2 #3 / 红线可复核）：`crates/homeway-core/src/relay/**` +
#      `relaywire.rs` 的**整文件**（不止 `use` 行）剥注释后 `quinn|tokio|rustls` 零命中——
#      与「本批 diff 零改动」互补：本门防的是**以后的依赖漂移**；
#   ⑨ **数据面单线程前提**（S6-2 #4 / 设计 §6.4）：三件**可判定**的事实——
#      (a) 岛内 `new_multi_thread` 零命中；(b) 岛 manifest 与 workspace manifest 的 tokio
#      features 不含 `rt-multi-thread`；(c) 两个 `send_datagram_checked` 都**不是 `async fn`**
#      且函数体内零 `.await`（「预检 → 发送」之间在单线程 runtime 上无插入窗口）。
#   ⑩ **命令循环零写等待**（M3 S1 / 设计 §1.4）：三件可判定事实——
#      (a) `driver.rs`（命令循环所在）剥注释后 `write_all|\.feed\(|\.flush\(` 零命中；
#      (b) `client/streams.rs`（写者任务所在）`write_all` 命中 ≥ 1（防 (a) 空过）；
#      (c) `client/streams.rs` 的 `fn write`（命令循环调用的唯一写入口）不是 `async fn`
#      且体内零 `.await`（背压不落命令循环）。
#
# **M1 起对第 ⑤ 条的收窄说明（偏离设计原文，见 commit message 与
# `docs/INTEROP-CRITERIA.md` 判据变更记录）**：RFC 7250 RPK 的客户端钉定在 rustls 公开面里
# **只能**经 `dangerous().with_custom_certificate_verifier(...)` 安装
# （`with_webpki_verifier` 只收 `WebPkiServerVerifier` 具体类型）——故「crates/ 内
# `dangerous()` 零命中」对 RPK 档不可能同时成立。替代形态**更强**：允许面收窄到一个文件，
# 且要求该文件出现原始公钥验签调用（钉定 ≠ 跳过验证）。
#
# **注释剥离口径（M1 S6-2 第 ⑤/⑥/⑧ 条同时升级）**：旧形态 `sed -e 's://.*::'` 会**行内**截断
# ——同一行里字符串含 `//`（如 `let u = "http://x";`）之后的一切都被删掉，等于给「一行内藏
# `quinn::`」留了绕过面；且不认 `/* */`（块注释里的 crate 名会**假红**）。现改为内嵌 awk 状态机
# （代码/行注释/块注释/字符串/字符字面量/raw string 六态；`LC_ALL=C` 字节口径 ⇒ 任意二进制
# 内容不报错）。**字符串内容保留**（与旧口径一致，防新的假红），只保证字符串里的 `//` 不再
# 截断本行。TOML 仍用 `sed 's:#.*::'`（manifest 里 `#` 出现在 TOML 值中的概率与影响都可忽略）。
#
# 调用点：.github/workflows/ci.yml 的 `check-quic-isolation` job + tools/ci-local.sh 步骤 3。
set -uo pipefail

REPO_ROOT="${0:h:A:h}"
CRATES="$REPO_ROOT/crates"
ISLAND="$CRATES/homeway-quic"
SRC="$ISLAND/src"
LOCK="$REPO_ROOT/Cargo.lock"

# ---------- 异步面 = **显式文件清单**（相对 $SRC；新增即改门） ----------
# 纪律（有意为之）：每一处新增的异步面都要显式过一次门——目录前缀豁免会连带免检
# 同目录下的纯 std 文件（如 `exit/transport.rs`、`client/migration.rs`），故不用。
ASYNC_FILES=(
  "driver.rs"
  "client/mod.rs" "client/race.rs" "client/register.rs"
  "client/dataplane.rs" "client/relay_sock.rs" "client/tests.rs"
  "client/streams.rs"
  "exit/mod.rs" "exit/conn.rs" "exit/socket.rs" "exit/bridge.rs"
  "exit/rpk.rs" "exit/transport.rs" "exit/serve.rs" "exit/tests.rs"
  # M3 S2：服务流的 socketpair 适配器 + 异步泵（**本清单双向 fail-closed**——
  # `exit/intake.rs` 是纯 std（两面共用）⇒ **不入清单**，受 ② 条真扫描）
  "exit/pump.rs"
  # M3 S4：快探阶梯（§3.1/§3.2——预算语义走 `tokio::time`，`start_paused` 用例要它）
  "client/ladder.rs"
)
# 异步名的判定式（②条、豁免自证、扫描器自校准**共用同一串**——三处不同步 = 门自相矛盾）。
# **代码门 r15 G1 整改**：`client/migration.rs` 原在清单里但它**零异步名**（纯逻辑小件，
# 与 `exit/transport.rs` 不同）⇒ 移出清单，改由 ② 条真扫描（移出后 ② 仍绿 ⇒ 证明它确实纯 std）。
ASYNC_NAME_PATTERN='tokio::|quinn|rustls|async fn|\.await'
# 第 ⑤/⑥ 条：RPK 钉定的唯一合法文件（相对 $REPO_ROOT）、其「真做签名验证」证据，
# 以及两个 `send_datagram_checked` 包装（相对 $REPO_ROOT）。
RPK_VERIFIER_FILE="crates/homeway-quic/src/exit/rpk.rs"
RPK_PROOF_PATTERN='verify_tls13_signature_with_raw_key'
DATAGRAM_WRAPPERS=(
  "crates/homeway-quic/src/client/dataplane.rs"
  "crates/homeway-quic/src/exit/conn.rs"
)
# 第 ⑧ 条：中继零异步名的文件面（目录 + 单文件）。
RELAY_FACE_DIR="crates/homeway-core/src/relay"
RELAY_FACE_FILES=( "crates/homeway-core/src/relaywire.rs" )

fail() { echo "!! QUIC 隔离门失败：$1" >&2; exit 1; }

# ---------- 剥注释（六态状态机；见文件头「注释剥离口径」） ----------
AWK_STRIP=$(cat <<'AWK'
BEGIN { state = "code"; raw_h = 0 }
{
  line = $0
  out = ""
  i = 1
  n = length(line)
  while (i <= n) {
    if (state == "block") {
      p = index(substr(line, i), "*/")
      if (p == 0) { i = n + 1; break }
      i = i + p + 1
      state = "code"
      continue
    }
    if (state == "str") {
      c = substr(line, i, 1)
      out = out c
      if (c == "\\") { out = out substr(line, i + 1, 1); i += 2; continue }
      if (c == "\"") { state = "code" }
      i++
      continue
    }
    if (state == "raw") {
      c = substr(line, i, 1)
      if (c == "\"") {
        ok = 1
        for (k = 1; k <= raw_h; k++) { if (substr(line, i + k, 1) != "#") ok = 0 }
        if (ok) { out = out "\"" substr(line, i + 1, raw_h); i = i + 1 + raw_h; state = "code"; continue }
      }
      out = out c
      i++
      continue
    }
    if (substr(line, i, 2) == "//") { break }
    if (substr(line, i, 2) == "/*") { state = "block"; i += 2; continue }
    c = substr(line, i, 1)
    if (c == "\"") { state = "str"; out = out c; i++; continue }
    if (c == "'") {
      if (substr(line, i, 3) ~ /^'\\.'/) { out = out substr(line, i, 3); i += 3; continue }
      if (substr(line, i, 3) ~ /^'[^'\\]'/) { out = out substr(line, i, 3); i += 3; continue }
      out = out c; i++; continue
    }
    if (c == "r" || c == "b") {
      j = i
      if (substr(line, j, 1) == "b") j++
      if (substr(line, j, 1) == "r") {
        j++
        h = 0
        while (substr(line, j, 1) == "#") { h++; j++ }
        if (substr(line, j, 1) == "\"") {
          out = out substr(line, i, j - i + 1)
          raw_h = h
          state = "raw"
          i = j + 1
          continue
        }
      }
    }
    out = out c
    i++
  }
  print out
}
AWK
)

# Rust 源：剥注释后输出（`LC_ALL=C` = 字节口径，任意内容不报错）。
strip_rs() { LC_ALL=C awk "$AWK_STRIP" "$1"; }
# 命中行（可读证据）：$1 = 文件，$2 = 扩展正则；无命中输出空。
hits_rs() { strip_rs "$1" | grep -nE "$2" || true; }
# TOML/锁文件（剥 `#` 注释；口径见文件头）。
strip_toml() { sed -e 's:#.*::' "$1"; }
hits_toml() { strip_toml "$1" | grep -nE "$2" || true; }
# 命中行号（去重）。
hit_lines() { strip_rs "$1" | grep -nE "$2" | cut -d: -f1 || true; }
# 函数体行号区间（`fn <名>` 起，到下一个行首 `}` 止——rustfmt 形态足够）。
fn_body_lines() {
  LC_ALL=C awk -v pat="$2" 'index($0, pat) > 0 { inb = 1 } inb { print NR } inb && /^\}/ { exit }' "$1" || true
}
# 递归列 crates/ 下的 .rs（排除 target）。
all_crates_rs() { find "$CRATES" -name '*.rs' -not -path '*/target/*' | sort; }
# 空格分隔串的成员判定（zsh 默认不做词分割 ⇒ 显式 `${=…}`）。
in_list() { local x; for x in ${=2}; do [[ "$x" == "$1" ]] && return 0; done; return 1; }

[[ -d "$ISLAND" ]] || fail "岛 crate 不在：$ISLAND"
[[ -f "$LOCK" ]] || fail "Cargo.lock 不在：$LOCK"

# ---------- ① 异步栈名字只许出现在岛的 manifest ----------
BAD_MANIFEST=""
for m in "$CRATES/homeway-core/Cargo.toml" "$CRATES/homeway-cli/Cargo.toml" "$CRATES/homeway-capi/Cargo.toml"; do
  [[ -f "$m" ]] || fail "manifest 缺失：$m"
  h="$(hits_toml "$m" '(^|[^A-Za-z0-9_-])(quinn|rustls|tokio)([^A-Za-z0-9_-]|$)')"
  [[ -n "$h" ]] && BAD_MANIFEST+="${m}:"$'\n'"${h}"$'\n'
done
[[ -z "$BAD_MANIFEST" ]] || fail $'同步面 manifest 出现异步栈依赖（层 0 保证失效）：\n'"$BAD_MANIFEST"
ISLAND_MANIFEST_HITS="$(hits_toml "$ISLAND/Cargo.toml" '(^|[^A-Za-z0-9_-])(quinn|rustls|tokio)([^A-Za-z0-9_-]|$)' | wc -l | tr -d ' ')"
(( ISLAND_MANIFEST_HITS >= 3 )) || fail "岛的 manifest 未声明全三条异步依赖（仅 ${ISLAND_MANIFEST_HITS} 条命中）"
echo "  ① 通过：异步栈依赖只在 crates/homeway-quic/Cargo.toml（其余三个 manifest 零命中）"

# ---------- ② 异步名字只在异步面（**显式文件清单**） ----------
is_async_face() {
  local rel="${1#$SRC/}"
  in_list "$rel" "${ASYNC_FILES[*]}"
}
BAD_SRC=""
SCANNED=0
ASYNC_SCANNED=0
NOT_LISTED=""
for f in "${(@f)$(find "$SRC" -name '*.rs' | sort)}"; do
  if is_async_face "$f"; then
    ASYNC_SCANNED=$(( ASYNC_SCANNED + 1 ))
    continue
  fi
  SCANNED=$(( SCANNED + 1 ))
  h="$(hits_rs "$f" "$ASYNC_NAME_PATTERN")"
  [[ -n "$h" ]] && BAD_SRC+="${f}:"$'\n'"${h}"$'\n'
done
[[ -z "$BAD_SRC" ]] || fail $'岛内公面/协议/小件文件出现异步栈名字（只许清单内的异步面文件）：\n'"$BAD_SRC"
# 清单自校准（fail-closed）：清单里的每个文件必须真在盘上（改名/删除即红——否则清单会退化）
for rel in "${ASYNC_FILES[@]}"; do
  [[ -f "$SRC/$rel" ]] || NOT_LISTED+="  $rel"$'\n'
done
[[ -z "$NOT_LISTED" ]] || fail $'异步面清单指向不存在的文件（改名/删除后必须同批改门）：\n'"$NOT_LISTED"
# **豁免自证**（fail-closed；代码门 r15 G1 整改，= 设计 S2-4「双向负例自检」的第①向）：
# 清单里的每个文件必须**真含**异步栈名字——否则「往白名单塞一个纯 std 文件」= 静默放宽
# （原门只查存在性与计数下限，往清单里加 `exit/admit.rs` 实测**仍全绿**）。纯 std 文件
# 必须移出清单、由 ② 条真扫描。
BAD_EXEMPT=""
for rel in "${ASYNC_FILES[@]}"; do
  n="$(hits_rs "$SRC/$rel" "$ASYNC_NAME_PATTERN" | wc -l | tr -d ' ')"
  (( n >= 1 )) || BAD_EXEMPT+="  $rel"$'\n'
done
[[ -z "$BAD_EXEMPT" ]] || fail $'异步面清单里的文件零异步名（豁免只许给真异步文件；纯 std 文件须移出清单受 ② 条扫描）：\n'"$BAD_EXEMPT"
# 扫描器自校准（fail-closed）：同一 strip+regex 管线必须「判得出真异步名、放过纯 std、
# 不认注释里的名字」——否则 ② 条可能整体空过（如剥注释状态机坏了 ⇒ 全员零命中）。
SELFTEST_RS="$(mktemp -t hw-iso-selftest.XXXXXX)" || fail "mktemp 失败（自校准无法进行）"
printf 'fn pure_std() { let _ = std::collections::HashMap::new(); }\n' > "$SELFTEST_RS"
h="$(hits_rs "$SELFTEST_RS" "$ASYNC_NAME_PATTERN")"
[[ -z "$h" ]] || fail $'扫描器自校准失败：纯 std 样本被误判：\n'"$h"
printf 'fn uses_async() { let _ = tokio::runtime::Runtime::new(); }\n' > "$SELFTEST_RS"
h="$(hits_rs "$SELFTEST_RS" "$ASYNC_NAME_PATTERN")"
[[ -n "$h" ]] || fail "扫描器自校准失败：含 tokio:: 的样本未被判出（②条可能空过）"
printf '// 注释里的 tokio:: 不算名字\nfn doc_only() {}\n' > "$SELFTEST_RS"
h="$(hits_rs "$SELFTEST_RS" "$ASYNC_NAME_PATTERN")"
[[ -z "$h" ]] || fail $'扫描器自校准失败：注释中的异步名被误判（剥注释状态机坏了）：\n'"$h"
rm -f "$SELFTEST_RS"
# 扫描面下限自校准（防空过；M1 S6-2 由 4/2 上调到实测量级）
(( SCANNED >= 8 )) || fail "扫描到的公面源文件数异常（${SCANNED} < 8）——检查 find/排除逻辑，门可能空过"
(( ASYNC_SCANNED >= 12 )) || fail "异步面文件数异常（${ASYNC_SCANNED} < 12）——清单可能被改窄成空过"
DRIVER_HITS="$(hits_rs "$SRC/driver.rs" 'tokio::' | wc -l | tr -d ' ')"
(( DRIVER_HITS >= 1 )) || fail "driver.rs 零 tokio:: 命中——门自身失准（宿主必须用 runtime）"
echo "  ② 通过：tokio/quinn/rustls/async/.await 只出现在异步面（异步面清单 ${ASYNC_SCANNED} 个文件；公面递归扫过 ${SCANNED} 个文件，零命中；豁免自证 ${#ASYNC_FILES[@]}/${#ASYNC_FILES[@]}、扫描器自校准 3/3）"

# ---------- ③ 阻塞面：零 sleep；block_on 只在异步面 ----------
BAD_SLEEP=""
for f in "${(@f)$(find "$SRC" -name '*.rs' | sort)}"; do
  h="$(hits_rs "$f" 'std::thread::sleep|thread::sleep\(')"
  [[ -n "$h" ]] && BAD_SLEEP+="${f}:"$'\n'"${h}"$'\n'
done
[[ -z "$BAD_SLEEP" ]] || fail $'岛内出现阻塞 sleep（异步上下文禁阻塞）：\n'"$BAD_SLEEP"
BAD_BLOCK=""
for f in "${(@f)$(find "$SRC" -name '*.rs' | sort)}"; do
  is_async_face "$f" && continue
  h="$(hits_rs "$f" 'block_on')"
  [[ -n "$h" ]] && BAD_BLOCK+="${f}:"$'\n'"${h}"$'\n'
done
[[ -z "$BAD_BLOCK" ]] || fail $'block_on 只许出现在异步面（清单内文件）：\n'"$BAD_BLOCK"
echo "  ③ 通过：岛内零阻塞 sleep；block_on 只在异步面（显式清单；递归扫描）"

# ---------- ④ aws-lc 零命中 ----------
for m in "$REPO_ROOT/Cargo.toml" "$ISLAND/Cargo.toml" "$CRATES/homeway-core/Cargo.toml"; do
  h="$(hits_toml "$m" 'aws-lc|aws_lc')"
  [[ -z "$h" ]] || fail $'manifest 出现 aws-lc（rustls 默认 features 回归？）：'"$m"$'\n'"$h"
done
N_AWSLC="$(grep -c 'aws-lc' "$LOCK" || true)"
[[ "$N_AWSLC" == "0" ]] || fail "Cargo.lock 出现 aws-lc（计数 ${N_AWSLC}）——rustls 必须 default-features=false"
echo "  ④ 通过：aws-lc 在 manifest 与 Cargo.lock 零命中（grep -c = 0）"

# ---------- ⑤ 产品面零跳过验证（RPK 钉定唯一豁免，且须自证真验签）+ harness 标记 ----------
# 豁免面自证（fail-closed，顺序在前）：文件在、且真做原始公钥验签
[[ -f "$REPO_ROOT/$RPK_VERIFIER_FILE" ]] || fail "RPK 豁免文件不在：$RPK_VERIFIER_FILE（豁免面不得指向空气）"
RPK_PROOF="$(hits_rs "$REPO_ROOT/$RPK_VERIFIER_FILE" "$RPK_PROOF_PATTERN" | wc -l | tr -d ' ')"
(( RPK_PROOF >= 1 )) || fail "RPK 豁免文件未出现原始公钥验签（$RPK_PROOF_PATTERN）——豁免不得退化成「跳过验证」"
BAD_DANGER=""
while IFS= read -r f; do
  rel="${f#$REPO_ROOT/}"
  [[ "$rel" == "$RPK_VERIFIER_FILE" ]] && continue
  h="$(hits_rs "$f" 'dangerous\(\)|with_custom_certificate_verifier')"
  [[ -n "$h" ]] && BAD_DANGER+="${f}:"$'\n'"${h}"$'\n'
done < <(all_crates_rs)
[[ -z "$BAD_DANGER" ]] || fail $'产品面出现跳过证书验证（唯一合法位置 = RPK 钉定文件 '"$RPK_VERIFIER_FILE"'）：\n'"$BAD_DANGER"
# harness 侧（tools/**）：跳过验证必须带 `SECURITY: harness-only` 显式标记（可审计；不入口 = 目录隔离）
BAD_HARNESS=""
while IFS= read -r f; do
  hits_rs "$f" 'dangerous\(\)|with_custom_certificate_verifier' >/dev/null 2>&1 || true
  h="$(hits_rs "$f" 'dangerous\(\)|with_custom_certificate_verifier')"
  [[ -z "$h" ]] && continue
  grep -q 'SECURITY: harness-only' "$f" || BAD_HARNESS+="  ${f#$REPO_ROOT/}"$'\n'
done < <(find "$REPO_ROOT/tools" -name '*.rs' -not -path '*/target/*' | sort)
[[ -z "$BAD_HARNESS" ]] || fail $'harness 侧跳过验证缺 `SECURITY: harness-only` 标记：\n'"$BAD_HARNESS"
echo "  ⑤ 通过：dangerous()/with_custom_certificate_verifier 只在 RPK 钉定文件（含 $RPK_PROOF_PATTERN）；其余 crates/ 零命中；tools/ 命中处均带 SECURITY 标记"

# ---------- ⑥ 裸 send_datagram( 零命中（产品面只许经 send_datagram_checked 包装体） ----------
BAD_DGRAM=""
WRAP_OK=0
for rel in "${DATAGRAM_WRAPPERS[@]}"; do
  f="$REPO_ROOT/$rel"
  [[ -f "$f" ]] || fail "DATAGRAM 包装面缺失：$rel"
  body="$(fn_body_lines "$f" 'fn send_datagram_checked')"
  [[ -n "$body" ]] || fail "$rel 内找不到 `send_datagram_checked` 函数体——包装面判据失准（不得空过）"
  WRAP_OK=$(( WRAP_OK + 1 ))
  allowed+=( "${rel}::$(echo "$body" | tr '\n' ',')" )
done
(( WRAP_OK == 2 )) || fail "DATAGRAM 包装面数量异常（${WRAP_OK} != 2）"
while IFS= read -r f; do
  rel="${f#$REPO_ROOT/}"
  case "$rel" in
    */tests.rs|*/tests/*) continue ;;   # 测试面：合法（与产品面分离，且只在 crates/ 内判）
  esac
  for ln in ${(f)$(hit_lines "$f" '(^|[^A-Za-z0-9_])send_datagram[[:space:]]*\(')}; do
    [[ -z "$ln" ]] && continue
    hit=0
    for a in "${allowed[@]}"; do
      [[ "${a%%::*}" == "$rel" ]] || continue
      [[ ",${a#*::}," == *",$ln,"* ]] && hit=1
    done
    (( hit )) || BAD_DGRAM+="  ${rel}:${ln}"$'\n'
  done
done < <(all_crates_rs)
[[ -z "$BAD_DGRAM" ]] || fail $'产品面出现裸 `send_datagram(` 调用（只许在两处包装函数体内；裸调用缓冲满时静默淘汰最旧）：\n'"$BAD_DGRAM"
# 计数自校准：包装体内必须真有裸调用（否则「零命中」是空过的假绿）
WRAP_CALLS=0
for a in "${allowed[@]}"; do
  n="$(echo "${a#*::}" | tr ',' '\n' | wc -l | tr -d ' ')"
  (( n >= 1 )) && WRAP_CALLS=$(( WRAP_CALLS + 1 ))
done
(( WRAP_CALLS == 2 )) || fail "两处包装函数体内未同时抽出行号（${WRAP_CALLS}）——判据可能空过"
echo "  ⑥ 通过：crates/ 内裸 send_datagram( 只出现在两处 send_datagram_checked 包装体内（测试文件除外）；tools/ harness 不在本门作用域"

# ---------- ⑦ send_datagram_wait 零命中（热路径禁阻塞式等待） ----------
BAD_WAIT=""
while IFS= read -r f; do
  h="$(hits_rs "$f" 'send_datagram_wait')"
  [[ -n "$h" ]] && BAD_WAIT+="${f}:"$'\n'"${h}"$'\n'
done < <(all_crates_rs)
[[ -z "$BAD_WAIT" ]] || fail $'产品面出现 send_datagram_wait（热路径禁阻塞式等待；该 API 只许在 harness）：\n'"$BAD_WAIT"
echo "  ⑦ 通过：crates/ 内 send_datagram_wait 零命中（tools/ harness 不受约束）"

# ---------- ⑧ 中继面零异步名（整文件，不止 use 行） ----------
BAD_RELAY=""
RELAY_SCANNED=0
while IFS= read -r f; do
  RELAY_SCANNED=$(( RELAY_SCANNED + 1 ))
  h="$(hits_rs "$f" '(quinn|tokio|rustls)')"
  [[ -n "$h" ]] && BAD_RELAY+="${f}:"$'\n'"${h}"$'\n'
done < <(find "$REPO_ROOT/$RELAY_FACE_DIR" -name '*.rs' | sort; printf '%s\n' "${RELAY_FACE_FILES[@]/#/$REPO_ROOT/}")
[[ -z "$BAD_RELAY" ]] || fail $'中继面（relay/** + relaywire.rs）出现异步栈名字（红线：中继零改动、不得依赖 QUIC 面）：\n'"$BAD_RELAY"
(( RELAY_SCANNED >= 3 )) || fail "中继面扫描文件数异常（${RELAY_SCANNED} < 3）——门可能空过"
echo "  ⑧ 通过：relay/** + relaywire.rs 整文件（剥注释后）quinn|tokio|rustls 零命中（扫过 ${RELAY_SCANNED} 个文件）"

# ---------- ⑨ 数据面单线程前提（三条可判定事实） ----------
BAD_ST=""
while IFS= read -r f; do
  h="$(hits_rs "$f" 'new_multi_thread')"
  [[ -n "$h" ]] && BAD_ST+="${f}:"$'\n'"${h}"$'\n'
done < <(find "$SRC" -name '*.rs' | sort)
[[ -z "$BAD_ST" ]] || fail $'岛内出现 new_multi_thread（单线程是结构不变量）：\n'"$BAD_ST"
for m in "$REPO_ROOT/Cargo.toml" "$ISLAND/Cargo.toml"; do
  h="$(hits_toml "$m" 'rt-multi-thread')"
  [[ -z "$h" ]] || fail $'tokio features 出现 rt-multi-thread（单线程运行时被打开）：'"$m"$'\n'"$h"
done
BAD_WRAP_ASYNC=""
for rel in "${DATAGRAM_WRAPPERS[@]}"; do
  f="$REPO_ROOT/$rel"
  LC_ALL=C awk -v pat='fn send_datagram_checked' '
    index($0, pat) > 0 { inb = 1 }
    inb { print NR": "$0 }
    inb && /^\}/ { exit }' "$f" > /tmp/.hw-wrap.$$ || true
  grep -qE 'async fn send_datagram_checked' /tmp/.hw-wrap.$$ && BAD_WRAP_ASYNC+="  ${rel}: 包装是 async fn（预检与发送之间出现 await 点）"$'\n'
  grep -qE '\.await' /tmp/.hw-wrap.$$ && BAD_WRAP_ASYNC+="  ${rel}: 包装体内出现 .await"$'\n'
  rm -f /tmp/.hw-wrap.$$
done
[[ -z "$BAD_WRAP_ASYNC" ]] || fail $'send_datagram_checked 必须是同步 fn 且体内零 .await（单线程前提的承载形态）：\n'"$BAD_WRAP_ASYNC"
echo "  ⑨ 通过：岛内零 new_multi_thread；tokio 未开 rt-multi-thread；两处 DATAGRAM 包装均为同步 fn 且体内零 .await"

# ---------- ⑩ 命令循环零写等待（M3 S1；三件可判定事实） ----------
STREAM_WRITER="crates/homeway-quic/src/client/streams.rs"
[[ -f "$REPO_ROOT/$STREAM_WRITER" ]] || fail "流写者面缺失：$STREAM_WRITER（M3 S1 的写路径真源）"
BAD_LOOP_WRITE=""
h="$(hits_rs "$SRC/driver.rs" 'write_all|\.feed\(|\.flush\(')"
[[ -z "$h" ]] || BAD_LOOP_WRITE+="  driver.rs（命令循环所在）："$'\n'"${h}"$'\n'
[[ -z "$BAD_LOOP_WRITE" ]] || fail $'命令循环出现流写等待（背压必须落在写者任务里，设计 §1.4）：\n'"$BAD_LOOP_WRITE"
WRITER_HITS="$(hits_rs "$REPO_ROOT/$STREAM_WRITER" 'write_all' | wc -l | tr -d ' ')"
(( WRITER_HITS >= 1 )) || fail "流写者任务缺 write_all（${WRITER_HITS} 命中）——⑩(a) 的零命中可能整体空过"
LC_ALL=C awk -v pat='fn write' '
  index($0, pat) > 0 { inb = 1 }
  inb { print NR": "$0 }
  inb && /^    \}/ { exit }' "$REPO_ROOT/$STREAM_WRITER" > /tmp/.hw-write.$$ || true
grep -qE 'async fn write' /tmp/.hw-write.$$ && fail "站内写入口是 async fn（背压会落回命令循环）：$STREAM_WRITER"
grep -qE '\.await' /tmp/.hw-write.$$ && fail "站内写入口体内出现 .await（命令循环会被写满挂住）：$STREAM_WRITER"
WCALL="$(wc -l < /tmp/.hw-write.$$ | tr -d ' ')"
rm -f /tmp/.hw-write.$$
(( WCALL >= 3 )) || fail "站内写入口抽出的行数异常（${WCALL} < 3）——⑩(c) 的判据可能空过"
echo "  ⑩ 通过：driver.rs 零 write_all/feed/flush；client/streams.rs 写者任务有 write_all（${WRITER_HITS} 处）；流写入口为同步 fn 且体内零 .await"

echo "QUIC 隔离门全绿（十条断言：异步边界 / 阻塞面 / aws-lc / 跳过验证 / DATAGRAM 纪律 / 中继零异步名 / 单线程前提 / 命令循环零写等待）"
