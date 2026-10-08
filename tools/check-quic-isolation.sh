#!/bin/zsh
# check-quic-isolation.sh — 「禁止 async 泄漏进同步面」的源码门（M0 设计 §3.4 层 3）。
#
# 五条断言（fail-closed；任一击红即退出非 0）：
#   ① 异步栈名字只出现在 crates/homeway-quic/Cargo.toml —— homeway-core / homeway-cli /
#      homeway-capi 的 manifest 零命中（防同步面拿到异步依赖 ⇒ 层 0 的 E0433 保证失效）；
#   ② crates/homeway-quic/src/** 里 `tokio::|quinn|rustls|async fn|.await` **只在
#      driver.rs** —— 公面/协议/小件文件零命中（公面夹带 = 泄漏面本体）；
#   ③ 岛内 `std::thread::sleep` 零命中（异步上下文禁阻塞）、`block_on` 只在 driver.rs；
#   ④ `aws-lc` 在 manifest 与 Cargo.lock 零命中（防 rustls 默认 features 回归——
#      默认含 aws_lc_rs ⇒ 拉 aws-lc-sys = BoringSSL 派生 + cmake，OHOS 不可行）；
#   ⑤ `crates/` 内 `dangerous()` / `with_custom_certificate_verifier` 零命中
#      （harness 的跳过验证永不得进产品面；唯一合法位置 = tools/quic-ab/，且带
#       `// SECURITY: harness-only` 标记——设计 §3.4 层 3 第 5 条 / §8.2 R-L）。
#
# 判据取的是**代码**：`//` 行注释先剥离（文档允许点名这些 crate，代码不许命名）。
# 注释剥离的副作用：字符串里的 `http://` 会被截断——本门只做名字/模式匹配，无碍。
#
# 调用点：.github/workflows/ci.yml 的 `check-quic-isolation` job + tools/ci-local.sh 步骤 3。
set -uo pipefail

REPO_ROOT="${0:h:A:h}"
CRATES="$REPO_ROOT/crates"
ISLAND="$CRATES/homeway-quic"
SRC="$ISLAND/src"
LOCK="$REPO_ROOT/Cargo.lock"

fail() { echo "!! QUIC 隔离门失败：$1" >&2; exit 1; }

# 只保留代码（剥注释）—— stdin→stdout。Rust 用 `//`，TOML/锁文件用 `#`。
# 判据取的是**代码**：文档/注释允许点名这些 crate，依赖声明与源码不许命名。
strip_rs() { sed -e 's://.*::' ; }
strip_toml() { sed -e 's:#.*::' ; }

# 命中行（可读证据）：$1 = 文件，$2 = 扩展正则，$3 = 注释剥离器；无命中输出空。
hits_in() { "$3" < "$1" | grep -nE "$2" || true; }
# Rust 源文件的简写（剥 `//` 注释；副作用：字符串里的 `http://` 会被截断——本门只做
# 名字/模式匹配，无碍）。
hits_rs() { hits_in "$1" "$2" strip_rs; }
# TOML/锁文件（剥 `#` 注释）。
hits_toml() { hits_in "$1" "$2" strip_toml; }

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

# ---------- ② 异步名字只在 driver.rs（**递归**：M1 起 src/ 会长出子目录） ----------
BAD_SRC=""
SCANNED=0
for f in "${(@f)$(find "$SRC" -name '*.rs' | sort)}"; do
  [[ "${f:t}" == "driver.rs" ]] && continue
  SCANNED=$(( SCANNED + 1 ))
  h="$(hits_rs "$f" 'tokio::|quinn|rustls|async fn|\.await')"
  [[ -n "$h" ]] && BAD_SRC+="${f}:"$'\n'"${h}"$'\n'
done
[[ -z "$BAD_SRC" ]] || fail $'岛内公面/协议/小件文件出现异步栈名字（只许 driver.rs）：\n'"$BAD_SRC"
# 自校准（fail-closed）：扫描面不得为空/被截断（新增文件漏扫时本门必须红）
(( SCANNED >= 4 )) || fail "扫描到的源文件数异常（${SCANNED} < 4）——检查 find/排除逻辑，门可能空过"
DRIVER_HITS="$(hits_rs "$SRC/driver.rs" 'tokio::' | wc -l | tr -d ' ')"
(( DRIVER_HITS >= 1 )) || fail "driver.rs 零 tokio:: 命中——门自身失准（宿主必须用 runtime）"
echo "  ② 通过：tokio/quinn/rustls/async/.await 只出现在 src/driver.rs（递归扫过 ${SCANNED} 个文件，零命中）"

# ---------- ③ 阻塞面：零 sleep；block_on 只在 driver.rs ----------
BAD_SLEEP=""
for f in "${(@f)$(find "$SRC" -name '*.rs' | sort)}"; do
  h="$(hits_rs "$f" 'std::thread::sleep|thread::sleep\(')"
  [[ -n "$h" ]] && BAD_SLEEP+="${f}:"$'\n'"${h}"$'\n'
done
[[ -z "$BAD_SLEEP" ]] || fail $'岛内出现阻塞 sleep（异步上下文禁阻塞）：\n'"$BAD_SLEEP"
BAD_BLOCK=""
for f in "${(@f)$(find "$SRC" -name '*.rs' | sort)}"; do
  [[ "${f:t}" == "driver.rs" ]] && continue
  h="$(hits_rs "$f" 'block_on')"
  [[ -n "$h" ]] && BAD_BLOCK+="${f}:"$'\n'"${h}"$'\n'
done
[[ -z "$BAD_BLOCK" ]] || fail $'block_on 只许出现在 driver.rs：\n'"$BAD_BLOCK"
echo "  ③ 通过：岛内零阻塞 sleep；block_on 只在 src/driver.rs（递归扫描）"

# ---------- ④ aws-lc 零命中 ----------
for m in "$REPO_ROOT/Cargo.toml" "$ISLAND/Cargo.toml" "$CRATES/homeway-core/Cargo.toml"; do
  h="$(hits_toml "$m" 'aws-lc|aws_lc')"
  [[ -z "$h" ]] || fail $'manifest 出现 aws-lc（rustls 默认 features 回归？）：'"$m"$'\n'"$h"
done
N_AWSLC="$(grep -c 'aws-lc' "$LOCK" || true)"
[[ "$N_AWSLC" == "0" ]] || fail "Cargo.lock 出现 aws-lc（计数 ${N_AWSLC}）——rustls 必须 default-features=false"
echo "  ④ 通过：aws-lc 在 manifest 与 Cargo.lock 零命中（grep -c = 0）"

# ---------- ⑤ 产品面零跳过验证 ----------
BAD_DANGER=""
while IFS= read -r f; do
  h="$(hits_rs "$f" 'dangerous\(\)|with_custom_certificate_verifier')"
  [[ -n "$h" ]] && BAD_DANGER+="${f}:"$'\n'"${h}"$'\n'
done < <(find "$CRATES" -name '*.rs' -not -path '*/target/*' | sort)
[[ -z "$BAD_DANGER" ]] || fail $'产品面出现跳过证书验证（唯一合法位置 = tools/quic-ab/，须带 SECURITY 标记）：\n'"$BAD_DANGER"
echo "  ⑤ 通过：crates/ 内 dangerous()/with_custom_certificate_verifier 零命中"

echo "QUIC 隔离门全绿（五条断言：异步边界 / 阻塞面 / aws-lc / 跳过验证）"
