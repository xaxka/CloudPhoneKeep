# syntax=docker/dockerfile:1
# =====================================================================
# CloudPhoneKeep Linux — 一个容器 = 一个账号（Alpine Linux，多架构）
# Rust 引擎（musl 静态编译，常驻内存 ~10MB）+
# Alpine 发行版 Chromium（原生 musl 构建，--headless=new 运行）。
#
# 多架构（linux/amd64 + linux/arm64）：
#   构建阶段固定在 BUILDPLATFORM 原生运行（CI 上 amd64 全速编译），
#   按目标架构选择 target 三元组，用 rustc 自带的 rust-lld 交叉链接——
#   纯 Rust + musl 自包含 libc，无需目标架构的 gcc，也不用 QEMU 跑 rustc。
#   本地构建：docker build . （在仓库根目录，注意 build context 是整个仓库）
# =====================================================================

# —— 构建阶段：BUILDPLATFORM 原生交叉编译（产物为目标架构静态二进制）——
FROM --platform=$BUILDPLATFORM rust:1-alpine AS builder
ARG TARGETARCH
WORKDIR /build
COPY linux/Cargo.toml linux/Cargo.lock ./
COPY linux/src ./src
# include_str!("../../shared/…") 相对 /build/src 解析到 /shared（见构建器源码注释）
COPY shared/keepalive.inject.js /shared/keepalive.inject.js
# rust-lld 链接 + test -s 产物断言（二进制缺失时构建立刻失败，防静默空镜像）
# 双架构构建并行共享 cargo 缓存挂载：cargo 自带 target 目录文件锁，天然串行安全
RUN --mount=type=cache,target=/build/target \
    --mount=type=cache,target=/usr/local/cargo/registry \
    set -eux; \
    case "${TARGETARCH}" in \
      arm64) triple=aarch64-unknown-linux-musl ;; \
      *)     triple=x86_64-unknown-linux-musl ;; \
    esac; \
    rustup target add "${triple}"; \
    RUSTFLAGS="-C linker=rust-lld" cargo build --release --target "${triple}"; \
    cp "target/${triple}/release/cloudphonekeep" /usr/local/bin/cloudphonekeep; \
    test -s /usr/local/bin/cloudphonekeep

# —— 运行阶段：Alpine + 发行版 Chromium（apk 原生支持 x86_64 / aarch64）——
FROM alpine:3.21

# chromium：Alpine community 仓库的官方包（原生 musl，与 APK 依赖一体管理）
# nss/freetype/harfbuzz：Chromium 运行时硬依赖
# font-wqy-zenhei：中文字体（截图/渲染中文必需，~10MB，比 noto-cjk 小一个数量级）
# ttf-freefont：西文兜底字体；tzdata：让 TZ=Asia/Shanghai 生效（日志走本地时区）
RUN apk add --no-cache \
      chromium nss freetype harfbuzz ca-certificates \
      font-wqy-zenhei ttf-freefont tzdata

ENV TZ=Asia/Shanghai \
    CPK_DATA_DIR=/data \
    CPK_REPORT_PORT=8088 \
    CPK_BIND=0.0.0.0 \
    CPK_CHROME_BIN=chromium-browser \
    CPK_HEADLESS=1

VOLUME ["/data"]
EXPOSE 8088

# 健康检查：浏览器存活 + 心跳不超龄 + 未退出云机（任一不满足 → 503 → unhealthy）
HEALTHCHECK --interval=60s --timeout=10s --start-period=120s --retries=3 \
  CMD wget -q -O /dev/null "http://127.0.0.1:${CPK_REPORT_PORT:-8088}/healthz" || exit 1

# Rust 引擎（musl 静态二进制，无运行时库依赖）
COPY --from=builder /usr/local/bin/cloudphonekeep /usr/local/bin/cloudphonekeep

ENTRYPOINT ["/usr/local/bin/cloudphonekeep"]
