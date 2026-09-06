# syntax=docker/dockerfile:1
# =====================================================================
# CloudPhoneKeep Linux — 一个容器 = 一个账号（多架构，chrome-headless-shell）
#
# 浏览器：Google Chrome for Testing 官方预编译 chrome-headless-shell
#   （无头渲染内核，无 Chrome UI/标签页/扩展，单页面场景比完整 Chrome
#    省 100MB+；CfT 二进制为 glibc 构建 → 运行层用 debian:bookworm-slim）
#   版本策略：amd64 用 stable 152.0.7977.82（本地端到端冒烟验证过的版本）；
#             arm64 用 154.0.8037.0（CfT 自 153.0.8001.0 才提供 linux-arm64，
#             stable 尚无 arm64，取 arm64 可用的 Beta 渠道最新）
#
# 多架构（linux/amd64 + linux/arm64）：
#   构建阶段固定在 BUILDPLATFORM 原生运行（CI 上 amd64 全速编译），
#   按目标架构选择 musl target 三元组，用 rustc 自带的 rust-lld 交叉链接——
#   纯 Rust + musl 静态自包含 libc，无需目标架构 gcc，也不用 QEMU 跑 rustc。
#   本地构建（在仓库根目录）：docker build -f linux/Dockerfile .
# =====================================================================

# —— 构建阶段：BUILDPLATFORM 原生交叉编译（产物为目标架构静态二进制）——
FROM --platform=$BUILDPLATFORM rust:1-alpine AS builder
ARG TARGETARCH
WORKDIR /build
COPY linux/Cargo.toml linux/Cargo.lock ./
COPY linux/src ./src
# include_str!("../../shared/…") 相对 /build/src 解析到 /shared
COPY shared/keepalive.inject.js /shared/keepalive.inject.js
# rust-lld 链接 + test -s 产物断言（二进制缺失时构建立刻失败，防静默空镜像）
# 双架构构建并行共享 cargo 缓存挂载：cargo 自带 target 目录文件锁，天然串行安全
RUN --mount=type=cache,target=/build/target \
    --mount=type=cache,target=/usr/local/cargo/registry \
    TARGET=$(case "$TARGETARCH" in arm64) echo aarch64-unknown-linux-musl;; *) echo x86_64-unknown-linux-musl;; esac) \
    && rustup target add "$TARGET" \
    && RUSTFLAGS="-C linker=rust-lld" cargo build --release --target "$TARGET" \
    && cp "target/$TARGET/release/cloudphonekeep" /usr/local/bin/cloudphonekeep \
    && test -s /usr/local/bin/cloudphonekeep

# —— 运行阶段：debian + CfT chrome-headless-shell ——
FROM debian:bookworm-slim
ARG TARGETARCH
ARG SHELL_AMD64=152.0.7977.82
ARG SHELL_ARM64=154.0.8037.0

# 运行时依赖（ldd 实测最小集）+ 中文字体 + tzdata；curl/unzip 仅构建期用完即删
RUN apt-get update && apt-get install -y --no-install-recommends \
      ca-certificates tzdata wget fonts-wqy-zenhei fonts-liberation \
      libnss3 libnspr4 libglib2.0-0 libexpat1 \
      libx11-6 libxcb1 libxext6 libxrender1 libxi6 \
      libxcomposite1 libxdamage1 libxfixes3 libxrandr2 \
      libatk1.0-0 libatk-bridge2.0-0 libatspi2.0-0 libdbus-1-3 \
      libasound2 libdrm2 libgbm1 libxkbcommon0 \
      libfontconfig1 libfreetype6 \
      curl unzip \
    && case "$TARGETARCH" in \
         arm64) V="$SHELL_ARM64"; P="linux-arm64";; \
         *)     V="$SHELL_AMD64"; P="linux64";; \
       esac \
    && curl -fsSL -o /tmp/shell.zip \
       "https://storage.googleapis.com/chrome-for-testing-public/${V}/${P}/chrome-headless-shell-${P}.zip" \
    && unzip -q /tmp/shell.zip -d /opt \
    && mv "/opt/chrome-headless-shell-${P}" /opt/chrome-headless-shell \
    && rm -f /tmp/shell.zip \
    && chmod +x /opt/chrome-headless-shell/chrome-headless-shell \
    && apt-get purge -y curl unzip \
    && apt-get autoremove -y \
    && rm -rf /var/lib/apt/lists/* \
    && test -x /opt/chrome-headless-shell/chrome-headless-shell

ENV TZ=Asia/Shanghai \
    CPK_DATA_DIR=/data \
    CPK_REPORT_PORT=8088 \
    CPK_BIND=0.0.0.0 \
    CPK_CHROME_BIN=/opt/chrome-headless-shell/chrome-headless-shell \
    CPK_HEADLESS=0

VOLUME ["/data"]
EXPOSE 8088

# 健康检查：浏览器存活 + 心跳不超龄 + 未退出云机（任一不满足 → 503 → unhealthy）
HEALTHCHECK --interval=60s --timeout=10s --start-period=120s --retries=3 \
  CMD wget -q -O /dev/null "http://127.0.0.1:${CPK_REPORT_PORT:-8088}/healthz" || exit 1

# Rust 引擎（musl 静态二进制，与 glibc 运行层无依赖耦合）
COPY --from=builder /usr/local/bin/cloudphonekeep /usr/local/bin/cloudphonekeep

ENTRYPOINT ["/usr/local/bin/cloudphonekeep"]
