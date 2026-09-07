# CLI 版构建与发布

构建对象为 `cli/` + `shared/`（根 Cargo workspace，纯 Rust 无 C 依赖）；
Windows 版构建见 [../../src-tauri/README.md](../../src-tauri/README.md)「构建」。

## 源码构建

```bash
# 仓库根目录执行（根 workspace：cli + shared）
cargo build --release --manifest-path cli/Cargo.toml
# 产物：target/release/cloudphonekeep
```

## musl 静态二进制（裸机直跑推荐）

```bash
rustup target add x86_64-unknown-linux-musl   # arm64 换 aarch64-unknown-linux-musl
RUSTFLAGS="-C linker=rust-lld" cargo build --release \
  --target x86_64-unknown-linux-musl --manifest-path cli/Cargo.toml
# 产物：target/x86_64-unknown-linux-musl/release/cloudphonekeep
```

纯 Rust + musl 自包含 libc（rust-lld 链接，无需目标架构 gcc），glibc 机器也能跑；
机器上只需 chrome-headless-shell 及其运行库（安装方法见
[../README.md](../README.md)「裸机直跑」）。

## Docker 镜像本地构建

```bash
# 构建上下文在仓库根（Dockerfile 引用 cli/ 与 shared/），
# 多阶段：rust:1-alpine 容器内交叉编译 → debian 运行层 + CfT chrome-headless-shell，
# 本地无需 Rust 工具链
cd cli && docker compose up -d --build

# 或单独构建镜像：
docker build -t cpk:local -f cli/Dockerfile .
```

## CI 自动发布

推送 `main` 后 GitHub Actions（`.github/workflows/ci.yml`）自动：

- 交叉编译 musl 静态二进制（amd64 + arm64）发布到 `dev` 预发布版：
  `cloudphonekeep-linux-amd64` / `cloudphonekeep-linux-arm64`
  （与镜像内引擎同源同构，裸机直跑用）
- 构建多架构 Docker 镜像（amd64 + arm64）推送
  `ghcr.io/xaxka/cloudphonekeep`（`:latest` 与 commit SHA 双标签，
  `docker pull` 自动选架构）

CI 流程细节与 GHCR 可见性说明见 [deploy.md](deploy.md)「CI（GitHub Actions）」；
Windows exe 同批发布，见 [../../src-tauri/README.md](../../src-tauri/README.md)。
