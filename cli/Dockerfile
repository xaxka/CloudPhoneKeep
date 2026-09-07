# syntax=docker/dockerfile:1
# =====================================================================
# CloudPhoneKeep CLI — 一个容器 = 一个账号（多架构，chrome-headless-shell）
#
# 浏览器：Google Chrome for Testing 官方预编译 chrome-headless-shell
#   （无头渲染内核，无 Chrome UI/标签页/扩展，单页面场景比完整 Chrome
#    省 100MB+；CfT 二进制为 glibc 构建 → 运行层用 debian:bookworm-slim）
#   版本唯一源：cli/chrome-versions.env（amd64 用 stable 152，本地端到端
#    验证过；arm64 取 154 Beta——CfT 自 153 才提供 linux-arm64，stable 尚无
#    arm64 渠道。升级浏览器版本只改那一个文件，与 install.sh / CI 冒烟同源）
#
# 多架构（linux/amd64 + linux/arm64）：
#   构建阶段固定在 BUILDPLATFORM 原生运行（CI 上 amd64 全速编译），
#   按目标架构选择 musl target 三元组，用 rustc 自带的 rust-lld 交叉链接——
#   纯 Rust + musl 静态自包含 libc，无需目标架构 gcc，也不用 QEMU 跑 rustc。
#   本地构建（在仓库根目录）：docker build -f cli/Dockerfile .
#
# 产物：musl 静态二进制 /usr/local/bin/cloudphonekeep——与 CI 发布的
#   裸机直跑二进制（cloudphonekeep-linux-amd64/arm64）同源同构，
#   装了 chrome-headless-shell 的机器上可直接运行（见 cli/README.md）。
# =====================================================================

# —— 构建阶段：BUILDPLATFORM 原生交叉编译（产物为目标架构静态二进制）——
FROM --platform=$BUILDPLATFORM rust:1-alpine AS builder
ARG TARGETARCH
WORKDIR /build
# 根 workspace（cli + shared；src-tauri 被 exclude，无需进入构建上下文）
# + 仓库锁文件保证依赖版本可复现
COPY Cargo.toml Cargo.lock ./
COPY shared ./shared
COPY cli/Cargo.toml ./cli/Cargo.toml
COPY cli/src ./cli/src
# 控制页模板（cli/src/report_server.rs include_str!("../control_page.html")）
COPY cli/control_page.html ./cli/control_page.html
# rust-lld 链接 + test -s 产物断言（二进制缺失时构建立刻失败，防静默空镜像）
# 双架构构建并行共享 cargo 缓存挂载：cargo 自带 target 目录文件锁，天然串行安全
RUN --mount=type=cache,target=/build/target \
    --mount=type=cache,target=/usr/local/cargo/registry \
    TARGET=$(case "$TARGETARCH" in arm64) echo aarch64-unknown-linux-musl;; *) echo x86_64-unknown-linux-musl;; esac) \
    && rustup target add "$TARGET" \
    && RUSTFLAGS="-C linker=rust-lld" cargo build --release --target "$TARGET" --manifest-path cli/Cargo.toml \
    && cp "target/$TARGET/release/cloudphonekeep" /usr/local/bin/cloudphonekeep \
    && test -s /usr/local/bin/cloudphonekeep

# —— 运行阶段：debian + CfT chrome-headless-shell ——
FROM debian:bookworm-slim
ARG TARGETARCH
# 浏览器版本唯一源（与 install.sh / CI 冒烟同一份文件）
COPY cli/chrome-versions.env /tmp/chrome-versions.env

# 运行时依赖（readelf 直连 NEEDED + LD_DEBUG dlopen 实测最小集；被裁的
# libnspr4/libxcb1/libxrender1/libxi6/libdrm2 为传递依赖由 apt 自动带入，
# libfontconfig1/libfreetype6 全程零加载）+ 中文字体 + tzdata；
# curl/unzip 仅构建期用完即删。
# 镜像瘦身五连（amd64 579MB → 约 490MB，双架构同享）：
#   1) dpkg path-exclude：doc/man/翻译文件自始不落盘（copyright 保留），比装完再删层更小
#   2) 中文字体 wqy-zenhei(16MB) → wqy-microhei(5MB)：CJK 覆盖同源，截图可读
#   3) CfT locale 资源包仅留 en-US + zh-CN：引擎默认 --lang=zh-CN；locale .pak 只是
#      Chrome 自身 UI 文案，页面内容渲染走系统字体，删了不影响自动化（双架构目录同构）
#   4) hyphen-data（CSS 连字符断词数据）删除：保活页面用不到
#   5) apt 元数据清理补上 /var/cache/apt/*（仅清 lists 会把 pkgcache.bin ~25MB 留在层里）
RUN printf 'path-exclude=/usr/share/doc/*\npath-include=/usr/share/doc/*/copyright\npath-exclude=/usr/share/man/*\npath-exclude=/usr/share/locale/*\n' \
      > /etc/dpkg/dpkg.cfg.d/01-slim \
    && apt-get update && apt-get install -y --no-install-recommends \
      ca-certificates tzdata wget fonts-wqy-microhei fonts-liberation \
      libasound2 libatk-bridge2.0-0 libatk1.0-0 libatspi2.0-0 libdbus-1-3 \
      libexpat1 libgbm1 libglib2.0-0 libnss3 libudev1 \
      libx11-6 libxcomposite1 libxdamage1 libxext6 libxfixes3 \
      libxkbcommon0 libxrandr2 \
      curl unzip \
    && . /tmp/chrome-versions.env \
    && case "$TARGETARCH" in \
         arm64) V="$SHELL_V_ARM64"; P="linux-arm64";; \
         *)     V="$SHELL_V_AMD64"; P="linux64";; \
       esac \
    && curl -fsSL -o /tmp/shell.zip \
       "https://storage.googleapis.com/chrome-for-testing-public/${V}/${P}/chrome-headless-shell-${P}.zip" \
    && unzip -q /tmp/shell.zip -d /opt \
    && mv "/opt/chrome-headless-shell-${P}" /opt/chrome-headless-shell \
    && find /opt/chrome-headless-shell/locales -type f -name '*.pak' \
         ! -name 'en-US*.pak' ! -name 'zh-CN*.pak' -delete \
    && rm -rf /opt/chrome-headless-shell/hyphen-data \
    && rm -f /tmp/shell.zip /tmp/chrome-versions.env \
    && chmod +x /opt/chrome-headless-shell/chrome-headless-shell \
    && apt-get purge -y curl unzip \
    && apt-get autoremove -y \
    && rm -rf /var/lib/apt/lists/* /var/cache/apt/* \
    && test -x /opt/chrome-headless-shell/chrome-headless-shell

ENV TZ=Asia/Shanghai \
    CPK_DATA_DIR=/data \
    CPK_REPORT_PORT=8088 \
    CPK_BIND=0.0.0.0 \
    CPK_CHROME_BIN=/opt/chrome-headless-shell/chrome-headless-shell

VOLUME ["/data"]
EXPOSE 8088

# 健康检查：浏览器存活 + 心跳不超龄 + 未退出云机（任一不满足 → 503 → unhealthy）
HEALTHCHECK --interval=60s --timeout=10s --start-period=120s --retries=3 \
  CMD wget -q -O /dev/null "http://127.0.0.1:${CPK_REPORT_PORT:-8088}/healthz" || exit 1

# Rust 引擎（musl 静态二进制，与 glibc 运行层无依赖耦合）
COPY --from=builder /usr/local/bin/cloudphonekeep /usr/local/bin/cloudphonekeep

ENTRYPOINT ["/usr/local/bin/cloudphonekeep"]
