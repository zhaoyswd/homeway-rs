#!/bin/sh
# homeway 安装脚本（交互式）
#
#   sh install.sh
#   curl -fsSL https://raw.githubusercontent.com/zhaoyswd/homeway-rs/main/install.sh | sh
#
# 流程：下载最新 release 并校验 → 安装二进制 → 检测并可选停止旧守护进程 →
#       逐项询问：后端 / 中继 / 添加主机 / macOS 预授权 / 开机自启动。
#
# 说明：
# - 本脚本不使用命令行参数；所有选择都通过交互询问完成。
# - `curl | sh` 时脚本正文来自 stdin，询问从 /dev/tty 读取；没有终端时只安装二进制、跳过询问。
# - 配置写入 `~/.config/homeway/config.toml`；已有配置只改相关键，不覆盖其它内容。
set -eu

REPO="zhaoyswd/homeway-rs"
NAME="homeway"
STATE="${HOME}/.config/homeway"
CONFIG="${STATE}/config.toml"

say()  { printf '%s\n' "$*"; }
die()  { printf '安装失败：%s\n' "$*" >&2; exit 1; }
have() { command -v "$1" >/dev/null 2>&1; }

if [ "$#" -gt 0 ]; then
  say "提示：本脚本为交互式安装，不使用命令行参数；已忽略：$*"
fi

# ---------- 0. 交互面 ----------
# 不能直接用 stdin：`curl | sh` 时 stdin 是脚本正文。
TTY=
if ( : < /dev/tty ) 2>/dev/null; then TTY=/dev/tty; fi
STTY_SAVED=

restore_tty() {
  if [ -n "${STTY_SAVED}" ] && [ -n "${TTY}" ]; then
    stty "${STTY_SAVED}" < "${TTY}" 2>/dev/null || true
    STTY_SAVED=
  fi
}

ask_yes_no() {
  _prompt=$1
  _default=$2
  if [ -z "$TTY" ]; then
    [ "$_default" = y ]
    return $?
  fi
  if [ "$_default" = y ]; then
    _hint="[Y/n]"
  else
    _hint="[y/N]"
  fi
  printf '%s %s ' "$_prompt" "$_hint" > "$TTY"
  _ans=
  IFS= read -r _ans < "$TTY" || _ans=
  case "$_ans" in
    [yY]|[yY][eE][sS]) return 0 ;;
    [nN]|[nN][oO]) return 1 ;;
    "") [ "$_default" = y ]; return $? ;;
    *) [ "$_default" = y ]; return $? ;;
  esac
}

ask_value() {
  _prompt=$1
  _default=$2
  if [ -z "$TTY" ]; then
    printf '%s' "$_default"
    return 0
  fi
  if [ -n "$_default" ]; then
    printf '%s [%s]: ' "$_prompt" "$_default" > "$TTY"
  else
    printf '%s: ' "$_prompt" > "$TTY"
  fi
  _ans=
  IFS= read -r _ans < "$TTY" || _ans=
  if [ -z "$_ans" ]; then
    printf '%s' "$_default"
  else
    printf '%s' "$_ans"
  fi
}

ask_secret() {
  _prompt=$1
  if [ -z "$TTY" ]; then
    printf ''
    return 0
  fi
  printf '%s: ' "$_prompt" > "$TTY"
  _old=
  _old=$(stty -g < "$TTY" 2>/dev/null) || _old=
  if [ -n "$_old" ]; then
    STTY_SAVED=$_old
    stty -echo < "$TTY" 2>/dev/null || true
  fi
  _ans=
  IFS= read -r _ans < "$TTY" || _ans=
  restore_tty
  printf '\n' > "$TTY"
  printf '%s' "$_ans"
}

# ---------- 1. 下载二进制 ----------
os=$(uname -s)
arch=$(uname -m)
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

# 先走 releases/latest 的 302 重定向（无 API 配额限制），拿不到再回落 API。
VERSION=$(curl -fsSLI -o /dev/null -w '%{url_effective}' "https://github.com/${REPO}/releases/latest" 2>/dev/null \
  | sed -n 's#.*/releases/tag/\(v[^/?]*\).*#\1#p') || true
if [ -z "${VERSION}" ]; then
  VERSION=$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" 2>/dev/null \
    | grep -o '"tag_name": *"[^"]*"' | head -1 | sed 's/.*"\(v[^"]*\)"/\1/') || true
fi
[ -n "${VERSION}" ] || die "取不到最新版本号（网络？）"

say "安装 ${NAME} ${VERSION}（${target}）"

tmp=$(mktemp -d 2>/dev/null || mktemp -d -t homeway)
trap 'restore_tty; rm -rf "${tmp}"' EXIT
trap 'restore_tty; exit 130' INT TERM
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

# 安装目录：/usr/local/bin 可写就用它，否则 ~/.local/bin。
if [ -w /usr/local/bin ]; then
  BIN_DIR=/usr/local/bin
else
  BIN_DIR="${HOME}/.local/bin"
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

# ---------- 2. 检测并可选停止旧守护进程 ----------
homeway_pids() {
  {
    if have pgrep; then
      pgrep -x homeway 2>/dev/null || true
    fi
    if have lsof && [ -S "${STATE}/control.sock" ]; then
      lsof -nP -t "${STATE}/control.sock" 2>/dev/null || true
    fi
  } | sort -u | tr '\n' ' '
}

stop_homeway() {
  if [ "${os_tag}" = darwin ]; then
    launchctl bootout "gui/$(id -u)/homeway" >/dev/null 2>&1 || true
  else
    systemctl stop homeway >/dev/null 2>&1 || true
    systemctl --user stop homeway >/dev/null 2>&1 || true
  fi
  pids=$(homeway_pids)
  [ -n "${pids}" ] || return 0
  kill ${pids} 2>/dev/null || true
  i=0
  alive=
  while [ "${i}" -lt 50 ]; do
    alive=
    for p in ${pids}; do
      if kill -0 "${p}" 2>/dev/null; then
        alive="${alive} ${p}"
      fi
    done
    [ -z "${alive}" ] && break
    sleep 0.1
    i=$((i + 1))
  done
  if [ -n "${alive}" ]; then
    kill -9 ${alive} 2>/dev/null || true
  fi
}

OLD_RUNNING=no
running=$(homeway_pids)
if [ -n "${running}" ]; then
  say "检测到 homeway 守护进程正在运行（PID:${running}）。"
  if ask_yes_no "是否停止当前进程？" y; then
    stop_homeway
    if [ -n "$(homeway_pids)" ]; then
      OLD_RUNNING=yes
      say "警告：旧进程仍未停止（可能由其它用户或托管服务启动），请手动停止后重试。"
    else
      say "已停止旧守护进程。"
    fi
  else
    OLD_RUNNING=yes
    say "保留当前进程：新配置与自启动要等它重启后才会生效。"
  fi
fi

# ---------- 3. 配置写入 ----------
ensure_config() {
  mkdir -p "${STATE}" "${STATE}/cache" || die "建不了目录 ${STATE}"
  chmod 700 "${STATE}" 2>/dev/null || true
  if [ ! -f "${CONFIG}" ]; then
    cat > "${CONFIG}" <<'CFG'
[serve]
enabled = false

[relay]
enabled = false
CFG
  fi
  chmod 600 "${CONFIG}" 2>/dev/null || true
}

# 只改指定 section 的指定 key，保留文件里的其它键与注释。
set_toml_key() {
  _file=$1
  _section=$2
  _key=$3
  _value=$4
  _tmp="${_file}.tmp.$$"
  # 用 ENVIRON 传 value：awk 的 -v 会解释反斜杠/引号转义，TOML 字符串会被二次解码。
  VALUE="${_value}" awk -v section="${_section}" -v key="${_key}" '
    function is_header(line) { return line ~ /^[[:space:]]*\[/ }
    BEGIN { value=ENVIRON["VALUE"]; in_section=0; found=0; section_found=0 }
    {
      if (is_header($0)) {
        if (in_section && !found) { print key " = " value; found=1 }
        in_section = ($0 ~ ("^[[:space:]]*\\[" section "\\][[:space:]]*(#.*)?$"))
        if (in_section) section_found=1
        print
        next
      }
      if (in_section && $0 ~ ("^[[:space:]]*" key "[[:space:]]*=")) {
        if (!found) { print key " = " value; found=1 }
        next
      }
      print
    }
    END {
      if (in_section && !found) { print key " = " value }
      if (!section_found) {
        print ""
        print "[" section "]"
        print key " = " value
      }
    }
  ' "${_file}" > "${_tmp}" && mv "${_tmp}" "${_file}"
  chmod 600 "${_file}" 2>/dev/null || true
}

toml_string() {
  printf '"%s"' "$(printf '%s' "$1" | sed -e 's/\\/\\\\/g' -e 's/"/\\"/g')"
}

validate_port() {
  _p=$1
  case "${_p}" in
    ''|*[!0-9]*) return 1 ;;
  esac
  [ "${_p}" -ge 1 ] && [ "${_p}" -le 65535 ]
}

# ---------- 4. 逐项询问 ----------
want_serve=no
want_relay=no
want_host=no
want_grant=no
want_autostart=no
serve_port=38241
files_root="${HOME}"
serve_relay=""
relay_listen=":38341"
relay_advertise=""
host_name="home"
host_token=""

if ask_yes_no "是否把这台机器配置为后端（出口）？" n; then
  want_serve=yes
  while :; do
    serve_port=$(ask_value "出口端口（listen 与 quic_listen 同端口）" "38241")
    if validate_port "${serve_port}"; then
      break
    fi
    say "端口必须是 1-65535 的数字，请重试。"
  done
  files_root=$(ask_value "文件服务根目录" "${HOME}")
  case "${files_root}" in
    ""|"~") files_root="${HOME}" ;;
    "~/"*) files_root="${HOME}/${files_root#??}" ;;
  esac
  serve_relay=$(ask_value "后端使用的中继凭据（rl1… 或 ip:port；留空不修改，输入 - 清空）" "")
  serve_relay=$(printf '%s' "${serve_relay}" | tr -d '\r')
fi

if ask_yes_no "是否把这台机器配置为中继？" n; then
  want_relay=yes
  while :; do
    relay_listen=$(ask_value "中继监听地址（:端口 或 IP:端口）" ":38341")
    case "${relay_listen}" in
      :[0-9]*) _rp=${relay_listen#:} ;;
      \[*\]*:[0-9]*) _rp=${relay_listen##*:} ;;
      [0-9]*.[0-9]*.[0-9]*.[0-9]*:[0-9]*) _rp=${relay_listen##*:} ;;
      *) _rp= ;;
    esac
    if validate_port "${_rp}"; then
      break
    fi
    say "格式应为 :端口 或 IP:端口（例如 :38341），请重试。"
  done
  relay_advertise=$(ask_value "对外公告地址（IP:port；留空自动探测）" "")
fi

if ask_yes_no "是否添加一台主机（客户端）？" n; then
  want_host=yes
  host_name=$(ask_value "主机名称" "home")
  if [ -z "${host_name}" ]; then
    host_name="home"
  fi
  while :; do
    host_token=$(ask_secret "粘贴主机 token（hmw2…）")
    host_token=$(printf '%s' "${host_token}" | tr -d '\r')
    if [ -n "${host_token}" ]; then
      break
    fi
    if ! ask_yes_no "token 为空，是否重试？" y; then
      want_host=no
      break
    fi
  done
  if [ "${want_host}" = yes ]; then
    case "${host_token}" in
      hmw2*) ;;
      *) say "提示：token 前缀不是 hmw2，CLI 可能会拒绝。" ;;
    esac
  fi
fi

if [ "${os_tag}" = darwin ]; then
  if ask_yes_no "是否配置 macOS 文件夹预授权（文稿/桌面/下载）？" n; then
    want_grant=yes
  fi
fi

if ask_yes_no "是否配置开机自启动？" n; then
  want_autostart=yes
fi

# ---------- 5. 写配置 ----------
if [ "${want_serve}" = yes ] || [ "${want_relay}" = yes ]; then
  ensure_config
fi

if [ "${want_serve}" = yes ]; then
  set_toml_key "${CONFIG}" serve enabled true
  set_toml_key "${CONFIG}" serve listen "${serve_port}"
  set_toml_key "${CONFIG}" serve quic_listen "${serve_port}"
  set_toml_key "${CONFIG}" serve files_root "$(toml_string "${files_root}")"
  if [ "${serve_relay}" = "-" ]; then
    set_toml_key "${CONFIG}" serve relay '""'
  elif [ -n "${serve_relay}" ]; then
    set_toml_key "${CONFIG}" serve relay "$(toml_string "${serve_relay}")"
  fi
  say "后端配置已写入：端口 ${serve_port}，文件根目录 ${files_root}"
fi

if [ "${want_relay}" = yes ]; then
  set_toml_key "${CONFIG}" relay enabled true
  set_toml_key "${CONFIG}" relay listen "$(toml_string "${relay_listen}")"
  set_toml_key "${CONFIG}" relay advertise "$(toml_string "${relay_advertise}")"
  say "中继配置已写入：监听 ${relay_listen}，公告 ${relay_advertise:-自动探测}"
fi

# ---------- 6. 必要时重启 / 配置自启动 ----------
need_daemon=no
if [ "${want_serve}" = yes ] || [ "${want_relay}" = yes ] || [ "${want_host}" = yes ]; then
  need_daemon=yes
fi

if [ "${OLD_RUNNING}" = yes ]; then
  if [ "${need_daemon}" = yes ] || [ "${want_autostart}" = yes ]; then
    if ask_yes_no "配置需要重启守护进程才能生效，是否现在停止旧进程并重启？" y; then
      stop_homeway
      OLD_RUNNING=no
    else
      say "旧进程继续运行：配置已写入，但需要你稍后手动重启 homeway 才会生效。"
      need_daemon=no
      want_autostart=no
    fi
  fi
fi

setup_launchd() {
  plist="${HOME}/Library/LaunchAgents/homeway.plist"
  mkdir -p "${HOME}/Library/LaunchAgents" "${STATE}/cache"
  cat > "${plist}" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>homeway</string>
  <key>ProgramArguments</key><array><string>${dest}</string></array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>WorkingDirectory</key><string>${STATE}</string>
  <key>StandardOutPath</key><string>${STATE}/cache/launchd.log</string>
  <key>StandardErrorPath</key><string>${STATE}/cache/launchd.log</string>
</dict></plist>
PLIST
  launchctl bootout "gui/$(id -u)/homeway" >/dev/null 2>&1 || true
  launchctl bootstrap "gui/$(id -u)" "${plist}" || die "launchctl 加载失败（看 ${plist}）"
  launchctl kickstart -k "gui/$(id -u)/homeway" >/dev/null 2>&1 || true
  say "已配置自启动：${plist}（launchctl kickstart -k gui/$(id -u)/homeway 可重启）"
}

setup_systemd() {
  have systemctl || { say "没有 systemd，请自行配置开机启动：${dest}"; return 1; }
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
    systemctl daemon-reload && systemctl enable homeway && systemctl restart homeway || die "systemctl 启用失败"
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
    systemctl --user daemon-reload && systemctl --user enable homeway && systemctl --user restart homeway || die "systemctl --user 启用失败"
    say "已配置自启动：${unit}（systemctl --user restart homeway 可重启）"
    say "提示：要让它在你未登录时也跑，执行一次：sudo loginctl enable-linger $(id -un)"
  fi
}

wait_control() {
  i=0
  while [ ! -S "${STATE}/control.sock" ] && [ "${i}" -lt 40 ]; do
    sleep 0.25
    i=$((i + 1))
  done
  if [ ! -S "${STATE}/control.sock" ]; then
    say "提示：守护进程尚未就绪（control.sock 未出现），稍后可用 homeway status 检查。"
    return 1
  fi
  return 0
}

start_background() {
  mkdir -p "${STATE}/cache"
  nohup "${dest}" < /dev/null >>"${STATE}/cache/install-start.log" 2>&1 &
  started_pid=$!
  say "已在后台启动守护进程（PID ${started_pid}；日志 ${STATE}/cache/install-start.log）"
}

autostart_done=no
if [ "${want_autostart}" = yes ]; then
  stop_homeway
  case "${os_tag}" in
    darwin) setup_launchd && autostart_done=yes ;;
    linux)  setup_systemd && autostart_done=yes ;;
  esac
  wait_control || true
elif [ "${need_daemon}" = yes ]; then
  start_background
  wait_control || true
fi

# ---------- 7. 添加主机 / 预授权 / 输出凭证 ----------
if [ "${want_host}" = yes ]; then
  if [ -n "${host_token}" ]; then
    if "${dest}" host add --name "${host_name}" "${host_token}"; then
      say "已添加主机：${host_name}"
    else
      say "添加主机失败。若出口暂时不可达，可稍后执行（token 用刚才粘贴的那串）："
      say "  ${dest} host add --name ${host_name} --force <token>"
    fi
  fi
fi

if [ "${want_grant}" = yes ]; then
  if [ "${want_serve}" != yes ]; then
    say "未配置后端，跳过文件夹预授权。"
  elif [ "${os_tag}" != darwin ]; then
    say "非 macOS，跳过文件夹预授权。"
  else
    if [ "${autostart_done}" != yes ]; then
      say "提示：未配置自启动时，预授权可能记到当前终端名下；建议配置自启动后重跑："
      say "  ${dest} serve grant"
    fi
    "${dest}" serve grant || say "预授权未完成——稍后可手动执行：${dest} serve grant"
  fi
fi

if [ "${want_serve}" = yes ]; then
  say ""
  say "后端 token（给客户端/手机 App 添加主机用）："
  "${dest}" serve token 2>/dev/null || say "（守护进程未运行，稍后执行：${dest} serve token）"
fi

if [ "${want_relay}" = yes ]; then
  say ""
  say "中继 token（给后端配置 serve.relay 用）："
  "${dest}" relay token 2>/dev/null || say "（守护进程未运行，稍后执行：${dest} relay token）"
fi

# ---------- 8. 总结 ----------
say ""
say "装好了。"
if [ "${want_serve}" = yes ]; then
  say "  后端：${dest} serve status / ${dest} serve token"
fi
if [ "${want_relay}" = yes ]; then
  say "  中继：${dest} relay status / ${dest} relay token"
fi
if [ "${want_host}" = yes ]; then
  say "  主机：${dest} host list / ${dest} status"
fi
if [ "${want_autostart}" = yes ] && [ "${autostart_done}" = yes ]; then
  say "  自启动：已配置（修改 config.toml 后重启服务生效）"
else
  say "  自启动：未配置（需要时重新运行本脚本，或手动配置 launchd/systemd）"
fi
say "  配置文件：${CONFIG}"
say "  常用：${dest} term new work / ${dest} files list /"
