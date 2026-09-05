# syntax=docker/dockerfile:1
# =====================================================================
# CloudPhoneKeep Linux — 一个容器 = 一个账号（Alpine Linux）
# Rust 引擎（musl 静态编译，常驻内存 ~10MB）+
# Alpine 发行版 Chromium（原生 musl 构建，--headless=new 运行）。
# 构建即编译：多阶段构建在容器内完成 Rust 编译，无需本地 Rust 工具链。
# 构建：docker build -t cloudphonekeep ./linux
# =====================================================================

# —— 构建阶段：Rust musl 静态编译（产物不依赖任何运行时库）——
FROM rust:1-alpine AS builder
WORKDIR /build
COPY Cargo.toml ./
COPY src ./src
# 依赖仅 serde_json；cargo 缓存挂载加速增量编译。
# test -s：产物断言——二进制缺失时构建立刻失败（防静默空镜像）
RUN --mount=type=cache,target=/build/target \
    --mount=type=cache,target=/usr/local/cargo/registry \
    cargo build --release \
    && cp target/release/cloudphonekeep /usr/local/bin/cloudphonekeep \
    && test -s /usr/local/bin/cloudphonekeep

# —— 运行阶段：Alpine + 发行版 Chromium ——
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
    CPK_REPORT_PORT=8080 \
    CPK_BIND=0.0.0.0 \
    CPK_CHROME_BIN=chromium-browser \
    CPK_HEADLESS=1

VOLUME ["/data"]
EXPOSE 8080

# 健康检查：浏览器存活 + 心跳不超龄 + 未退出云机（任一不满足 → 503 → unhealthy）
HEALTHCHECK --interval=60s --timeout=10s --start-period=120s --retries=3 \
  CMD wget -q -O /dev/null "http://127.0.0.1:${CPK_REPORT_PORT:-8080}/healthz" || exit 1

# Rust 引擎（musl 静态二进制，无运行时库依赖）
COPY --from=builder /usr/local/bin/cloudphonekeep /usr/local/bin/cloudphonekeep

ENTRYPOINT ["/usr/local/bin/cloudphonekeep"]
