#!/usr/bin/env bash
# =====================================================================
# CloudPhoneKeep CLI 裸机一键安装（无 Docker）
# ---------------------------------------------------------------------
# 默认动作（可反复执行，幂等覆盖升级）：
#   1. 从 GitHub dev Release 下载本机架构的 musl 静态引擎二进制
#   2. 下载 chrome-headless-shell（Chrome for Testing 官方预编译，
#      版本唯一源 cli/chrome-versions.env；googleapis 失败自动换
#      npmmirror 国内镜像）
#   3. 检查运行库缺失并提示（root + apt 环境可自动补装）
#   4. 安装后自检：引擎 CPK_SELFTEST + 浏览器 --version
#
# 用法：
#   curl -fsSL https://raw.githubusercontent.com/xaxka/CloudPhoneKeep/main/cli/install.sh | bash
#   # 或克隆仓库后：bash cli/install.sh
#
#   bash install.sh --prefix /opt/cloudphonekeep   # 自定义安装位置
#   bash install.sh --systemd                      # root：装 systemd 模板单元
#   bash install.sh --chrome-bin /path/to/chrome-headless-shell   # 复用已有浏览器
#   bash install.sh --skip-chrome                  # PATH 里已有 chrome-headless-shell
#   bash install.sh --dns                          # DNS 解析/连通性排障（只诊断不安装）
#   bash install.sh --uninstall                    # 卸载
#
# 安装位置默认：root → /opt/cloudphonekeep，普通用户 → ~/.local/cloudphonekeep
# 命令软链：root → /usr/local/bin，普通用户 → ~/.local/bin
# =====================================================================
set -euo pipefail

DEV_RELEASE="${CPK_RELEASE_BASE:-https://github.com/xaxka/CloudPhoneKeep/releases/download/dev}"
# 浏览器版本唯一源：cli/chrome-versions.env（本地克隆直接读；curl|bash 管道
# 模式从 raw 地址拉取同一份文件——脚本本身即来自 raw，可用性等价）
RAW_VERSIONS="${CPK_VERSIONS_URL:-https://raw.githubusercontent.com/xaxka/CloudPhoneKeep/main/cli/chrome-versions.env}"
SHELL_V_AMD64=""; SHELL_V_ARM64=""
CFT_GOOGLE="https://storage.googleapis.com/chrome-for-testing-public"
CFT_MIRROR="https://registry.npmmirror.com/-/binary/chrome-for-testing"
# 运行库最小集（2026-09 实测 chrome-headless-shell 152：readelf 直连 NEEDED +
# LD_DEBUG dlopen 全程审计）。被裁 7 个：libnspr4/libxcb1/libxrender1/libxi6/
# libdrm2 是纯传递依赖，装直连包时 apt 自动带入；libfontconfig1/libfreetype6
# 全程零加载（文字渲染不经动态 fontconfig）。libudev1 为直连依赖补入
# （此前靠基础镜像默认存在）。libpci/libva/libwayland 为可选 dlopen 探测，
# 缺失无害，故意不装
RUNTIME_DEBS="libasound2 libatk-bridge2.0-0 libatk1.0-0 libatspi2.0-0 libdbus-1-3 \
libexpat1 libgbm1 libglib2.0-0 libnss3 libudev1 libx11-6 libxcomposite1 \
libxdamage1 libxext6 libxfixes3 libxkbcommon0 libxrandr2"

usage() { sed -n '2,26p' "$0" | sed 's/^# \{0,2\}//'; }

log()  { printf '[OK]   %s\n' "$*"; }
warn() { printf '[WARN] %s\n' "$*"; }
fail() { printf '[FAIL] %s\n' "$*" >&2; exit 1; }

# ---------- 参数 ----------
PREFIX="" ENGINE="" CHROME="" SKIP_CHROME=0 SKIP_ENGINE=0
WANT_SYSTEMD=0 NO_DEPS=0 PREFER_MIRROR=0 UNINSTALL=0 DNS_ONLY=0
while [ $# -gt 0 ]; do
  case "$1" in
    --prefix)     PREFIX="${2:?}"; shift 2;;
    --engine)     ENGINE="${2:?}"; shift 2;;
    --chrome-bin) CHROME="${2:?}"; shift 2;;
    --skip-chrome) SKIP_CHROME=1; shift;;
    --skip-engine) SKIP_ENGINE=1; shift;;
    --systemd)    WANT_SYSTEMD=1; shift;;
    --no-deps)    NO_DEPS=1; shift;;
    --mirror)     PREFER_MIRROR=1; shift;;
    --dns)        DNS_ONLY=1; shift;;
    --uninstall)  UNINSTALL=1; shift;;
    -h|--help)    usage; exit 0;;
    *) printf '未知参数: %s\n\n' "$1" >&2; usage >&2; exit 1;;
  esac
done

# ---------- 架构 ----------
ARCH="$(uname -m)"
case "$ARCH" in
  x86_64)        TARGET=amd64; CFT_PLATFORM=linux64;;
  aarch64|arm64) TARGET=arm64; CFT_PLATFORM=linux-arm64;;
  *) fail "不支持的架构 $ARCH (当前发布 linux-amd64 / linux-arm64)";;
esac

# ---------- 位置 ----------
ROOT=0; [ "$(id -u)" -eq 0 ] && ROOT=1
[ -z "$PREFIX" ] && { [ "$ROOT" -eq 1 ] && PREFIX=/opt/cloudphonekeep || PREFIX="$HOME/.local/cloudphonekeep"; }
[ "$ROOT" -eq 1 ] && BIN_DIR=/usr/local/bin || BIN_DIR="$HOME/.local/bin"
CHROME_DIR="$PREFIX/chrome-headless-shell"
CHROME_BIN_DEFAULT="$CHROME_DIR/chrome-headless-shell"

# ---------- 卸载 ----------
if [ "$UNINSTALL" -eq 1 ]; then
  [ -e "$PREFIX" ] || fail "未发现 $PREFIX (本脚本只卸载自己装的)"
  for l in "$BIN_DIR/cloudphonekeep" "$BIN_DIR/chrome-headless-shell"; do
    if [ -L "$l" ] && [ "$(readlink "$l")" = "$PREFIX"/cloudphonekeep -o "$(readlink "$l")" = "$CHROME_BIN_DEFAULT" ]; then
      rm -f "$l" && log "已删命令软链 $l"
    fi
  done
  rm -rf "$PREFIX" && log "已删 $PREFIX"
  if [ "$ROOT" -eq 1 ] && [ -f /etc/systemd/system/cpk@.service ]; then
    if grep -q "ExecStart=$PREFIX/cloudphonekeep" /etc/systemd/system/cpk@.service 2>/dev/null; then
      rm -f /etc/systemd/system/cpk@.service && systemctl daemon-reload \
        && log "已删 systemd 模板单元并 daemon-reload"
    fi
  fi
  echo "数据目录 (登录态/日志) 默认在 ~/.local/share/cloudphonekeep，如确认不要可手动删除"
  exit 0
fi

# ---------- 工具 ----------
fetch() { # fetch <url> <dest>
  if command -v curl >/dev/null 2>&1; then
    curl -fL --retry 3 --connect-timeout 20 -o "$2" "$1"
  elif command -v wget >/dev/null 2>&1; then
    wget -q --tries=3 -O "$2" "$1"
  else
    fail "需要 curl 或 wget 下载"
  fi
}
do_unzip() { # do_unzip <zip> <destdir>
  if command -v unzip >/dev/null 2>&1; then unzip -q -o "$1" -d "$2"
  elif command -v python3 >/dev/null 2>&1; then python3 -m zipfile -e "$1" "$2"
  else fail "需要 unzip 或 python3 解压"
  fi
}

# ---------- DNS 排障 ----------
# 双源下载失败的短提示（详细诊断用 --dns 子命令）
dns_short_hint() {
  echo "  · 国内网络对 storage.googleapis.com 常见 DNS 污染：临时换公共 DNS (119.29.29.29 / 223.5.5.5)"
  echo "  · 加 --mirror 反转双源顺序 (npmmirror 国内源优先)"
  echo "  · 逐项解析/连通诊断：bash install.sh --dns"
}
dns_full_diag() {
  local h ip
  echo "DNS 解析与连通性诊断 (引擎/浏览器全部下载源)："
  for h in storage.googleapis.com registry.npmmirror.com github.com raw.githubusercontent.com; do
    ip=""
    if command -v getent >/dev/null 2>&1; then
      ip="$(getent hosts "$h" 2>/dev/null | awk '{print $1}' | head -3 | tr '\n' ' ')"
    elif command -v python3 >/dev/null 2>&1; then
      ip="$(python3 -c "import socket
ips=sorted({x[4][0] for x in socket.getaddrinfo('$h',443)})
print(' '.join(ips[:3]))" 2>/dev/null || true)"
    fi
    if [ -n "$ip" ]; then
      echo "  [OK]   $h -> $ip"
    else
      echo "  [FAIL] $h 无法解析 (DNS 不通或被污染)"
    fi
  done
  cat <<'DNSTIP'

判读与处置：
  * 全部 FAIL           → DNS 服务不可用：检查 /etc/resolv.conf，
                          或临时改公共 DNS：echo "nameserver 223.5.5.5" > /etc/resolv.conf
  * 仅 googleapis FAIL  → 国内常见 DNS 污染：安装加 --mirror 走 npmmirror，
                          或换 DNS (119.29.29.29 / 223.5.5.5)
  * 解析 OK 仍下载失败  → 出口被防火墙拦截：--mirror 换源，
                          或 --chrome-bin / --engine 复用本机已有文件
  * github.com FAIL     → 引擎二进制同样会失败：换网络，或本机拷贝后
                          bash install.sh --engine <文件路径>
DNSTIP
}

mkdir -p "$PREFIX" "$BIN_DIR"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

# ---------- --dns：只诊断不安装 ----------
if [ "$DNS_ONLY" -eq 1 ]; then
  dns_full_diag
  exit 0
fi

# ---------- 浏览器版本唯一源 ----------
load_shell_versions() {
  [ -n "$SHELL_V_AMD64" ] && return 0
  local env_local
  env_local="$(dirname "$0")/chrome-versions.env"
  if [ -f "$env_local" ] && grep -q '^SHELL_V_AMD64=' "$env_local"; then
    # 本地克隆模式：仓库内的版本清单
    # shellcheck disable=SC1090
    . "$env_local"
  else
    # 管道模式 ($0=bash)：从 raw 拉取同一份清单
    echo "获取浏览器版本清单 (chrome-versions.env)..."
    fetch "$RAW_VERSIONS" "$TMP/chrome-versions.env" \
      || fail "版本清单下载失败：$RAW_VERSIONS
  可先克隆仓库后 bash cli/install.sh (本地直接读清单)，或网络恢复后重试"
    # shellcheck disable=SC1091
    . "$TMP/chrome-versions.env"
  fi
  [ -n "$SHELL_V_AMD64" ] && [ -n "$SHELL_V_ARM64" ] \
    || fail "chrome-versions.env 内容不完整 (缺 SHELL_V_AMD64 / SHELL_V_ARM64)"
}

# ---------- 1. 引擎 ----------
if [ "$SKIP_ENGINE" -eq 1 ]; then
  [ -x "$PREFIX/cloudphonekeep" ] || fail "--skip-engine 但 $PREFIX/cloudphonekeep 不存在"
  log "跳过引擎下载 (沿用 $PREFIX/cloudphonekeep)"
elif [ -n "$ENGINE" ]; then
  [ -f "$ENGINE" ] || fail "引擎文件不存在: $ENGINE"
  install -m 0755 "$ENGINE" "$PREFIX/cloudphonekeep"
  log "已安装本地引擎 $ENGINE"
else
  echo "下载引擎 ($TARGET, musl 静态二进制, dev Release)..."
  if ! fetch "$DEV_RELEASE/cloudphonekeep-linux-$TARGET" "$PREFIX/cloudphonekeep"; then
    warn "引擎下载失败——DNS/网络排查提示："
    dns_short_hint
    fail "引擎下载失败：$DEV_RELEASE/cloudphonekeep-linux-$TARGET
  (GitHub 不可达时可换网络重试，或本机拷贝二进制后 --engine 本地安装)"
  fi
  chmod +x "$PREFIX/cloudphonekeep"
  log "引擎已装：$PREFIX/cloudphonekeep"
fi

# ---------- 2. chrome-headless-shell ----------
if [ -n "$CHROME" ]; then
  case "$CHROME" in /*) ;; *) CHROME="$(command -v "$CHROME" || fail "找不到 $CHROME")";; esac
  [ -x "$CHROME" ] || fail "--chrome-bin 指定的不是可执行文件: $CHROME"
  CHROME_USE="$CHROME"
  log "复用已有浏览器：$CHROME"
elif [ "$SKIP_CHROME" -eq 1 ]; then
  CHROME_USE="$(command -v chrome-headless-shell || true)"
  [ -n "$CHROME_USE" ] || warn "--skip-chrome：PATH 里暂无 chrome-headless-shell，引擎待机可用，选平台前需 CPK_CHROME_BIN 指路"
  log "跳过浏览器下载"
else
  load_shell_versions
  case "$TARGET" in
    arm64) SHELL_V="$SHELL_V_ARM64";;
    *)     SHELL_V="$SHELL_V_AMD64";;
  esac
  ZIP_NAME="chrome-headless-shell-$CFT_PLATFORM.zip"
  URL_GOOGLE="$CFT_GOOGLE/$SHELL_V/$CFT_PLATFORM/$ZIP_NAME"
  URL_MIRROR="$CFT_MIRROR/$SHELL_V/$CFT_PLATFORM/$ZIP_NAME"
  echo "下载浏览器 (chrome-headless-shell $SHELL_V, $CFT_PLATFORM)..."
  if [ "$PREFER_MIRROR" -eq 1 ]; then SET1="$URL_MIRROR"; SET2="$URL_GOOGLE"; else SET1="$URL_GOOGLE"; SET2="$URL_MIRROR"; fi
  if ! fetch "$SET1" "$TMP/shell.zip"; then
    warn "首选源失败，换镜像重试 ..."
    if ! fetch "$SET2" "$TMP/shell.zip"; then
      warn "双源均不可达——DNS 排障提示："
      dns_short_hint
      fail "浏览器下载失败 (googleapis 与 npmmirror 均不可达)，可用 --chrome-bin 复用已有浏览器后重试"
    fi
  fi
  do_unzip "$TMP/shell.zip" "$PREFIX"
  rm -rf "$CHROME_DIR"
  mv "$PREFIX/chrome-headless-shell-$CFT_PLATFORM" "$CHROME_DIR"
  chmod +x "$CHROME_BIN_DEFAULT"
  # 与容器镜像同样的资源瘦身 (失败不影响功能)
  rm -rf "$CHROME_DIR/hyphen-data" 2>/dev/null || true
  find "$CHROME_DIR/locales" -name '*.pak' ! -name 'en-US*.pak' ! -name 'zh-CN*.pak' -delete 2>/dev/null || true
  CHROME_USE="$CHROME_BIN_DEFAULT"
  log "浏览器已装：$CHROME_DIR ($SHELL_V)"
fi

# ---------- 3. 命令软链 ----------
ln -sf "$PREFIX/cloudphonekeep" "$BIN_DIR/cloudphonekeep"
[ -n "${CHROME_USE:-}" ] && [ "${CHROME_USE:-}" = "$CHROME_BIN_DEFAULT" ] \
  && ln -sf "$CHROME_BIN_DEFAULT" "$BIN_DIR/chrome-headless-shell"
case ":$PATH:" in *":$BIN_DIR:"*) ;; *) warn "$BIN_DIR 不在 PATH 里，请加入 shell 配置 (export PATH=\"\$PATH:$BIN_DIR\")";; esac

# ---------- 4. 运行库检查 ----------
if [ "$NO_DEPS" -eq 0 ] && [ -n "${CHROME_USE:-}" ]; then
  MISS="$(ldd "$CHROME_USE" 2>/dev/null | grep 'not found' || true)"
  if [ -n "$MISS" ]; then
    echo "$MISS"
    if [ "$ROOT" -eq 1 ] && command -v apt-get >/dev/null 2>&1; then
      echo "自动补装运行库 (apt)..."
      apt-get update -qq && apt-get install -y --no-install-recommends $RUNTIME_DEBS
      log "运行库已补装"
    else
      warn "缺少上述运行库。Debian/Ubuntu 手动安装："
      echo "  apt install $RUNTIME_DEBS"
      warn "中文字体可选 (截图可读性)：apt install fonts-wqy-microhei"
    fi
  else
    log "运行库完整 (ldd 无缺失)"
  fi
fi

# ---------- 5. 自检 ----------
CPK_SELFTEST=1 timeout 60 "$PREFIX/cloudphonekeep" >/dev/null \
  && log "引擎自检通过 (CPK_SELFTEST)" \
  || fail "引擎自检失败：$PREFIX/cloudphonekeep 不可用"
if [ -n "${CHROME_USE:-}" ]; then
  if OUT="$("$CHROME_USE" --version 2>/dev/null)"; then log "浏览器自检通过：$OUT"
  else warn "浏览器 --version 启动失败 (多半缺运行库，见上方缺失清单)"; fi
fi

# ---------- 6. 环境文件 (systemd / 脚本常驻用) ----------
ENV_FILE="$PREFIX/cpk.env"
if [ -n "${CHROME_USE:-}" ]; then
  printf 'CPK_CHROME_BIN=%s\nTZ=Asia/Shanghai\n# 公网可达时建议启用鉴权 (去掉注释并改密码)：\n# CPK_AUTH_USER=admin\n# CPK_AUTH_PASS=change-me\n' "$CHROME_USE" > "$ENV_FILE"
  log "环境文件已写：$ENV_FILE"
fi

# ---------- 7. systemd 模板单元 (可选, root) ----------
if [ "$WANT_SYSTEMD" -eq 1 ]; then
  if [ "$ROOT" -ne 1 ]; then
    warn "--systemd 需要 root；普通用户请直接前台/tmux 运行 (见下方说明)"
  else
    cat > /etc/systemd/system/cpk@.service <<UNIT
# CloudPhoneKeep systemd 模板单元 —— cpk@<账号> 启动 (本文件由 install.sh 生成)
# 多实例端口：systemctl edit cpk@<账号>，追加：
#   [Service]
#   Environment=CPK_REPORT_PORT=8089
[Unit]
Description=CloudPhoneKeep %i
After=network-online.target
Wants=network-online.target

[Service]
EnvironmentFile=$PREFIX/cpk.env
Environment=CPK_ACCOUNT=%i
Environment=CPK_DATA_DIR=/var/lib/cloudphonekeep
ExecStart=$PREFIX/cloudphonekeep
Restart=always

[Install]
WantedBy=multi-user.target
UNIT
    systemctl daemon-reload
    log "systemd 模板单元已装 (cpk@<账号> 启动，数据目录 /var/lib/cloudphonekeep)"
  fi
fi

# ---------- 完成 ----------
cat <<SUMMARY

安装完成。运行：

  cloudphonekeep                # 单账号 (CPK_ACCOUNT 默认 1)
  # 浏览器打开 http://127.0.0.1:8088/ →「设置→平台」选移动/联通 → 首次登录
  # 登录态持久化，之后自动保活；健康检查 curl http://127.0.0.1:8088/healthz

多账号 (各实例独立端口与数据目录)：

  CPK_ACCOUNT=138xxxx1234 CPK_REPORT_PORT=8089 cloudphonekeep

数据目录：交互运行默认 ~/.local/share/cloudphonekeep (CPK_DATA_DIR 可改)。
SUMMARY
[ "$WANT_SYSTEMD" -eq 1 ] && [ "$ROOT" -eq 1 ] && echo 'systemd 常驻：systemctl enable --now cpk@138xxxx1234 (账号随意，建议手机号)'
exit 0
