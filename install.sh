#!/bin/sh
# homeway 安装脚本
#
#   curl -fsSL https://raw.githubusercontent.com/zhaoyswd/homeway-rs/main/install.sh | sh
#
# 做的事：识别系统 → 下载最新 release 并校验 → 放进可直接执行的 bin 目录（macOS 顺手清扩展属性）
# → 询问是否配置自启动 → 打印最常用的几条命令。
#
# 可选参数（`sh install.sh --autostart yes` 这类形态；`curl | sh` 时无交互，自启动默认不配）：
#   --version vX.Y.Z   装指定版本（缺省 = 最新 release）
#   --bin-dir DIR      装到哪（缺省：/usr/local/bin 可写就用它，否则 ~/.local/bin）
#   --autostart yes|no 是否配置自启动（缺省：交互式询问；非交互 = no）
set -eu

REPO="zhaoyswd/homeway-rs"
NAME="homeway"
VERSION=""
BIN_DIR=""
AUTOSTART=""

say()  { printf '%s\n' "$*"; }
die()  { printf '安装失败：%s\n' "$*" >&2; exit 1; }
have() { command -v "$1" >/dev/null 2>&1; }

while [ $# -gt 0 ]; do
  case "$1" in
    --version)   VERSION="$2"; shift 2 ;;
    --bin-dir)   BIN_DIR="$2"; shift 2 ;;
    --autostart) AUTOSTART="$2"; shift 2 ;;
    -h|--help)   sed -n '2,13p' "$0"; exit 0 ;;
    *) die "不认识的参数：$1（--help 看用法）" ;;
  esac
done

# ---------- 1. 识别系统 ----------
os=$(uname -s); arch=$(uname -m)
case "${os}" in
  Darwin) os_tag=darwin ;;
  Linux)  os_tag=linux ;;
  *) die "暂不支持 ${os}（Windows 请到 https://github.com/${REPO}/releases 下载 zip）" ;;
esac
case "${arch}" in
  arm64|aarch64) arch_tag=arm64 ;;
  x86_64|amd64)  arch_tag=amd64 ;;
  *) die "暂不支持架构 ${arch}" ;;
esac
target="${os_tag}-${arch_tag}"
have curl || die "需要 curl"
have tar  || die "需要 tar"

# ---------- 2. 版本 ----------
if [ -z "${VERSION}" ]; then
  # 先走 releases/latest 的 302 重定向（无 API 配额限制），拿不到再回落 API
  VERSION=$(curl -fsSLI -o /dev/null -w '%{url_effective}' "https://github.com/${REPO}/releases/latest" 2>/dev/null \
    | sed -n 's#.*/releases/tag/\(v[^/?]*\).*#\1#p') || true
  if [ -z "${VERSION}" ]; then
    VERSION=$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" 2>/dev/null \
      | grep -o '"tag_name": *"[^"]*"' | head -1 | sed 's/.*"\(v[^"]*\)"/\1/') || true
  fi
  [ -n "${VERSION}" ] || die "取不到最新版本号（网络？）可用 --version vX.Y.Z 指定"
fi
say "安装 ${NAME} ${VERSION}（${target}）"

# ---------- 3. 下载 + 校验 ----------
tmp=$(mktemp -d 2>/dev/null || mktemp -d -t homeway)
trap 'rm -rf "${tmp}"' EXIT
base="https://github.com/${REPO}/releases/download/${VERSION}"
asset="${NAME}-${VERSION}-${target}.tar.gz"
curl -fsSL -o "${tmp}/${asset}" "${base}/${asset}" || die "下载失败：${base}/${asset}"
curl -fsSL -o "${tmp}/SHA256SUMS" "${base}/SHA256SUMS" || die "下载校验文件失败"
if have sha256sum; then
  sum=$(sha256sum "${tmp}/${asset}" | cut -d' ' -f1)
elif have shasum; then
  sum=$(shasum -a 256 "${tmp}/${asset}" | cut -d' ' -f1)
else
  die "需要 sha256sum 或 shasum 做校验"
fi
grep -q "^${sum}  *${asset}\$" "${tmp}/SHA256SUMS" || die "校验不通过（文件可能损坏，请重试）"
tar -xzf "${tmp}/${asset}" -C "${tmp}" || die "解包失败"
[ -f "${tmp}/${NAME}" ] || die "包里没有 ${NAME}"

# ---------- 4. 放进 bin 目录 ----------
if [ -z "${BIN_DIR}" ]; then
  if [ -w /usr/local/bin ]; then
    BIN_DIR=/usr/local/bin
  else
    BIN_DIR="${HOME}/.local/bin"
  fi
fi
mkdir -p "${BIN_DIR}" || die "建不了目录 ${BIN_DIR}"
dest="${BIN_DIR}/${NAME}"
if [ -x "${dest}" ]; then
  old=$("${dest}" --version 2>/dev/null | awk '{print $2}') || old=old
  cp "${dest}" "${dest}.bak-${old}" 2>/dev/null && say "旧版已备份：${dest}.bak-${old}"
  rm -f "${dest}"   # 换新 inode（macOS 覆盖写旧 inode 的二进制会被拒绝执行）
fi
cp "${tmp}/${NAME}" "${dest}" || die "写入 ${dest} 失败"
chmod +x "${dest}"
if [ "${os_tag}" = darwin ] && have xattr; then
  xattr -c "${dest}" 2>/dev/null || true
fi
"${dest}" --version >/dev/null 2>&1 || die "${dest} 无法执行"
say "已安装：${dest}（$("${dest}" --version)）"

case ":${PATH}:" in
  *":${BIN_DIR}:"*) ;;
  *) say "提示：${BIN_DIR} 不在 PATH 里，把这行加进 shell 配置后重开终端："
     say "  export PATH=\"${BIN_DIR}:\${PATH}\"" ;;
esac

# ---------- 5. 自启动 ----------
ask_autostart() {
  if [ -n "${AUTOSTART}" ]; then
    [ "${AUTOSTART}" = yes ]; return
  fi
  if [ -t 0 ]; then
    printf '要配置开机自启动吗？（出口机 / 中继机需要；只当客户端用可以不配）[y/N] '
    read -r ans || ans=
    case "${ans}" in y|Y|yes|YES) return 0 ;; *) return 1 ;; esac
  fi
  say "（非交互运行，未配置自启动；需要的话：sh install.sh --autostart yes）"
  return 1
}

setup_launchd() {
  plist="${HOME}/Library/LaunchAgents/homeway.plist"
  state="${HOME}/.config/homeway"
  mkdir -p "${HOME}/Library/LaunchAgents" "${state}/cache"
  cat > "${plist}" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>homeway</string>
  <key>ProgramArguments</key><array><string>${dest}</string></array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>WorkingDirectory</key><string>${state}</string>
  <key>StandardOutPath</key><string>${state}/cache/launchd.log</string>
  <key>StandardErrorPath</key><string>${state}/cache/launchd.log</string>
</dict></plist>
PLIST
  launchctl bootout "gui/$(id -u)/homeway" >/dev/null 2>&1 || true
  launchctl bootstrap "gui/$(id -u)" "${plist}" || die "launchctl 加载失败（看 ${plist}）"
  say "已配置自启动：${plist}（launchctl kickstart -k gui/$(id -u)/homeway 可重启）"
}

setup_systemd() {
  have systemctl || { say "没有 systemd，请自行配置开机启动：${dest}"; return; }
  if [ "$(id -u)" = 0 ]; then
    unit=/etc/systemd/system/homeway.service
    cat > "${unit}" <<UNIT
[Unit]
Description=homeway
After=network-online.target
Wants=network-online.target

[Service]
ExecStart=${dest}
Restart=always
RestartSec=3

[Install]
WantedBy=multi-user.target
UNIT
    systemctl daemon-reload && systemctl enable --now homeway || die "systemctl 启用失败"
    say "已配置自启动：${unit}（systemctl restart homeway 可重启）"
  else
    mkdir -p "${HOME}/.config/systemd/user"
    unit="${HOME}/.config/systemd/user/homeway.service"
    cat > "${unit}" <<UNIT
[Unit]
Description=homeway

[Service]
ExecStart=${dest}
Restart=always
RestartSec=3

[Install]
WantedBy=default.target
UNIT
    systemctl --user daemon-reload && systemctl --user enable --now homeway || die "systemctl --user 启用失败"
    say "已配置自启动：${unit}（systemctl --user restart homeway 可重启）"
    say "提示：要让它在你未登录时也跑，执行一次：sudo loginctl enable-linger $(id -un)"
  fi
}

if ask_autostart; then
  case "${os_tag}" in
    darwin) setup_launchd ;;
    linux)  setup_systemd ;;
  esac
fi

# ---------- 6. 常用命令 ----------
say ""
say "装好了。最常用的几条："
say "  出口机（家里那台）："
say "    homeway serve start          # 开出口（第一次需要；之后自启动会带起）"
say "    homeway serve token          # 取凭证，给客户端加主机用"
say "  客户端（电脑 / 手机 App）："
say "    homeway host add --name home <凭证>   # 加主机（手机 App 里是「添加主机」粘贴同一串）"
say "    homeway term new work        # 在出口机上开一个终端（Ctrl-b d 分离，attach 回来）"
say "    homeway files list /         # 浏览出口机上的文件（get / put 传文件）"
say "    homeway status               # 看连接与流量"
