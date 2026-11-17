# syntax=docker/dockerfile:1

# DivertLink 静态 musl 镜像。
#
# 多架构构建（buildx 会为每个目标平台选用对应架构的 rust:*-alpine 构建器；
# 跨架构构建时该构建器经 QEMU 模拟运行，因此耗时明显高于同架构构建）：
#   docker buildx build --platform linux/amd64,linux/arm64 \
#     -t ghcr.io/wrste/divertlink:dev .
# 单架构本地构建：
#   docker build -t divertlink:dev .
#
# 运行镜像为 distroless static：不含 shell/包管理器，自带 CA 根证书与非 root 用户
# （nonroot, uid/gid 65532）。监听 443 需要容器的 NET_BIND_SERVICE 或内核 sysctl
# net.ipv4.ip_unprivileged_port_start=0，deploy/docker-compose.yml 已给出配置。
#
# Rust 版本与 Cargo.toml 的 rust-version = "1.88" 对齐；如依赖需要更新的编译器，
# 可用 `--build-arg RUST_VERSION=<版本>` 覆盖。

ARG RUST_VERSION=1.88

# ------------------------------------------------------------------------------
# 阶段 1：编译（alpine/musl，构建器的架构由 buildx 按 --platform 选定）
# ------------------------------------------------------------------------------
FROM rust:${RUST_VERSION}-alpine AS builder

# ring 等依赖含 C 代码，需要完整的 C 工具链（gcc/musl-dev/make），不能只装 musl-dev。
# 版本随 rust:alpine 基础镜像的 Alpine 版本走，固定具体版本号反而会破坏基础镜像兼容性。
# hadolint ignore=DL3018
RUN apk add --no-cache build-base

WORKDIR /src
ENV CARGO_TERM_COLOR=always \
    CARGO_INCREMENTAL=0

# 先用占位 main.rs 编译依赖，依赖层可被缓存；Cargo.lock + --locked 保证可复现。
# 注意：Cargo.toml 里没有 build script，占位构建不依赖 assets/ 或 LICENSE 文件。
COPY Cargo.toml Cargo.lock ./
RUN mkdir -p src \
    && printf 'fn main() {}\n' > src/main.rs \
    && cargo build --release --locked \
    && rm -rf src

# src/config.rs 通过 include_str!("../assets/server.example.toml") 把示例配置编进
# 二进制，缺失 assets/ 会直接编译失败；LICENSE 文件一并带入运行镜像以满足许可证要求。
COPY assets ./assets
COPY LICENSE-MIT LICENSE-APACHE ./
COPY src ./src
RUN cargo build --release --locked

# ------------------------------------------------------------------------------
# 阶段 2：healthcheck 工具（distroless 不含 wget/curl，用静态 busybox）
# ------------------------------------------------------------------------------
FROM busybox:1.36.1-musl AS tools

# Docker 官方 busybox 的 -musl 变体为静态链接，可直接放进 distroless。

# ------------------------------------------------------------------------------
# 阶段 3：运行镜像（distroless static, nonroot）
# ------------------------------------------------------------------------------
FROM gcr.io/distroless/static-debian12:nonroot AS runtime

COPY --from=tools /bin/busybox /bin/busybox

COPY --from=builder --chown=65532:65532 \
    /src/target/release/divertlink /usr/local/bin/divertlink
COPY --from=builder /src/LICENSE-MIT /src/LICENSE-APACHE /licenses/

# 443  = agent 隧道 + 公网 TLS 入口（tunnel_listen/ingress_listen 共用，按 ALPN 分流）
# 8080 = http_listen（可选明文入口，供本机 nginx/caddy 终止 TLS 后回源）
# 9090 = admin_listen（/healthz /readyz /metrics /status，务必只绑定本机）
EXPOSE 443/tcp 8080/tcp 9090/tcp

# distroless 的 nonroot 用户：uid=65532, gid=65532（用数字 uid，避免依赖镜像内 /etc/passwd）
USER 65532:65532
# 以配置目录为工作目录：server.toml 中的相对证书路径（certs/server.crt）可直接生效
# （实现上相对路径也会按配置文件所在目录解析，这里保持一致更直观）。
WORKDIR /etc/divertlink

# 健康检查走管理端点；配置里 admin_listen 建议保持 127.0.0.1:9090。
# 若把 admin_listen 关掉，请同时删除本 HEALTHCHECK 与 compose 中的 healthcheck。
HEALTHCHECK --interval=30s --timeout=5s --start-period=15s --retries=3 \
    CMD ["/bin/busybox", "wget", "-q", "-O", "-", "http://127.0.0.1:9090/healthz"]

ENTRYPOINT ["/usr/local/bin/divertlink"]
# 默认作为 gateway 运行；agent 用法：
#   docker run ... divertlink:dev agent -c /etc/divertlink/agent.toml
CMD ["server", "-c", "/etc/divertlink/server.toml"]
