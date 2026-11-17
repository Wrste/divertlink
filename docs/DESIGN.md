# DivertLink 设计说明（内部契约）

> 本文是仓库内部的架构与接口契约，面向实现者与协作者。最终用户文档见 `README.md` 与 `docs/` 其余文件。

## 1. 目标

在**没有公网 IP 的家庭宽带**上运行大模型服务（vLLM / Ollama / LM Studio / llama.cpp / new-api 等），通过
`agent`（家庭端）主动向**有固定 IP 的公网服务器**上的 `gateway`（服务端）建立一条加密长连接，
把家庭端服务安全、稳定地映射到公网。网关对外暴露与 OpenAI / Anthropic / vLLM 完全兼容的 HTTP(S) 入口，
可直接作为 new-api / one-api 的上游渠道；网关亦可将某些路径反向代理到 new-api 等平台。

设计原则：

1. **协议透明**：网关不解析大模型业务 JSON，只做 HTTP 语义级转发，因此 OpenAI `/v1/chat/completions`、
   Anthropic `/v1/messages`、vLLM `/health` `/tokenize` `/metrics`、Responses API、embedding、rerank
   等全部天然兼容，SSE 流式响应逐块透传、不缓冲。
2. **安全默认**：隧道强制 TLS 1.3（可降 1.2）+ 双向令牌认证，可选 mTLS、证书 SPKI pinning；
   公网入口 API Key 认证、限流、体积/并发限制、审计日志，管理端点只监听 localhost。
3. **稳定**：应用层心跳 + 传输层心跳可组合，自动指数退避重连（带 jitter），会话热替换，
   流式空闲超时，优雅退出；单条 TCP 连接多路复用，不受 NAT/CGNAT 空闲回收影响。
4. **简单**：一个静态链接二进制，`divertlink server -c server.toml` / `divertlink agent -c agent.toml`；
   `divertlink init` 一键生成双端配置与证书。
5. **高性能**：tokio + hyper、零拷贝 `Bytes`、yamux 多路复用、连接池、TCP_NODELAY、流式双向转发。

## 2. 架构

```
                     公网固定 IP 服务器 (gateway)                         家庭宽带 (agent)
  ┌─────────────────────────────────────────────────────────────┐     ┌─────────────────────┐
  │  :443  TLS (ALPN)                                            │     │                     │
  │   ├─ ALPN "divertlink/1" ── yamux 隧道会话 ◄──────────────────┼─────┼─ 主动 TLS 连接       │
  │   │        │ 控制流(JSON Lines hello/ping/pong)               │     │  yamux client       │
  │   │        └─ 数据流: HTTP/1.1 over yamux (每请求一条流)        │     │   ├─ 控制流          │
  │   └─ ALPN "h2"/"http/1.1" ── 公网 HTTP 入口 (OpenAI/Anthropic)│     │   └─ 数据流 → 本地服务 │
  │                                                              │     │        (vLLM/new-api)│
  │  :80   (可选) 明文入口，前置 nginx/caddy 终止 TLS              │     └─────────────────────┘
  │  :9090 (可选) 管理端点 /healthz /readyz /metrics /status (localhost) │
  └─────────────────────────────────────────────────────────────┘
```

- **单端口复用**：同一个 443 端口按 TLS ALPN 分流：agent 用 `divertlink/1`（yamux 隧道），
  new-api/浏览器用 `h2`/`http/1.1`（HTTP 入口）。防火墙只需放行 443。
- **多路复用**：agent 与 gateway 之间一条 TLS/TCP 连接上跑 yamux，每个公网 HTTP 请求对应一条
  yamux 数据流，流内是标准 HTTP/1.1 报文（`Connection: close`）。天然的端到端背压、双向流式。
- **控制流**：agent 建立连接后打开第一条 yamux 流作为控制流，发送 `hello`（含 tunnel id、token、
  协议能力），服务端回 `hello_ack` 并开始心跳。
- **路由**：网关按 `mount` 前缀 / `host` / `default` 将请求分配给隧道；`static_routes` 走本地反代
  （例如反代到 new-api）。协议透明，所以 `/v1/...`、`/anthropic/...`（Anthropic SDK 的 base_url 自动拼接
  `/v1/messages`）都可用。
- **认证**：入口使用网关 API Key（`Authorization: Bearer` 或 `x-api-key`），按 key 绑定允许的隧道；
  隧道侧使用独立的 agent token。默认把客户端原始认证头透传给家庭端（便于 vLLM `--api-key` /
  new-api 渠道鉴权），也可配置 `upstream_api_key` 替换。
- **new-api 拓扑**：① new-api 把 `https://gateway/t/<mount>` 或 `https://<tunnel host>` 配为
  OpenAI/Anthropic 渠道；② 网关 `static_routes` 反代到 new-api；③ agent 的 target 指向本机 new-api。
  三种拓扑均支持，文档给出配置。

## 3. 仓库结构（单 crate）

```
Cargo.toml
src/
  main.rs               # CLI 入口（thin）
  lib.rs
  error.rs              # Error/Result (thiserror)
  config.rs             # ServerConfig / AgentConfig、默认值、示例、校验
  tls.rs                # 证书加载、rustls 配置、ALPN、SPKI pin、rcgen 证书生成
  protocol.rs           # 控制流消息、ALPN 常量、session id、协议枚举
  auth.rs               # 网关 API Key 认证、token 摘要/常量时间比较、客户端 IP
  ratelimit.rs          # "600/min" 解析与令牌桶
  metrics.rs            # 原子计数 + Prometheus 文本渲染
  registry.rs           # 隧道注册表：会话安装/替换、路由解析、并发控制、状态快照
  httpfwd.rs            # 逐跳头清洗、URL 拼接、mount 剥离、IdleTimeoutBody
  telemetry.rs          # tracing 初始化（text/json）
  util.rs               # backoff+jitter、字节尺寸解析、随机 token 等
  net.rs                # 跨平台非阻塞 TCP connect + keepalive（EINPROGRESS/WSAEWOULDBLOCK）
  upstream.rs           # 共享池化 HTTP client（http/https、私有 CA）
  setup.rs              # `init` 项目脚手架（CLI 与控制台共用）
  signal.rs             # Ctrl-C / SIGTERM 统一优雅退出
  cli.rs                # clap 子命令 server/agent/console/init/gen-certs/token/check/version
  console/              # 内置图形控制台（单二进制 Web UI / PWA）
    mod.rs              # ConsoleOptions/Handle、监听、PWA/浏览器打开、生命周期
    api.rs              # REST API、SSE 日志流、Host/CSRF/Token 防护
    profile.rs          # profile 存储（JSON, 0600）、配置摘要/角色探测、导入导出
    supervisor.rs       # 子进程启停（SIGTERM→SIGKILL）、日志环形缓冲与广播
    probe.rs            # TLS 连接探测（延迟/TLS 版本/证书/SPKI pin）
    assets.rs           # 编译期嵌入的前端资源
  server/
    mod.rs              # ServerHandle、监听器启动、优雅退出、配置体检
    session.rs          # TLS accept + ALPN 分流 + 隧道会话（yamux server）
    ingress.rs          # 公网 HTTP 入口（axum -> 隧道/静态上游），认证/限流/审计
    admin.rs            # /healthz /readyz /metrics /status
  agent/
    mod.rs              # AgentHandle、supervisor 重连循环
    session.rs          # TLS 连接、yamux client、hello、控制流心跳、数据流 accept
    proxy.rs            # 数据流内 HTTP/1.1 server -> 本地 target 连接池
assets/
  server.example.toml   # `init` 使用的模板（编译期嵌入）
  agent.example.toml
  console/              # index.html / app.js / style.css / manifest / sw.js / icon.svg
tests/
  e2e.rs                # 端到端：TLS/心跳/流式/并发/断线重连/大 body/取消/静态路由/mTLS/pin
  cli.rs                # 真实二进制：init/gen-certs/token/check/lint
  console.rs            # 真实控制台进程：REST/UI/PWA/CSRF/前后端联调/进程托管
  agent/
    mod.rs              # AgentHandle、supervisor 重连循环
    session.rs          # TLS 连接、yamux client、hello、控制流心跳、数据流 accept
    proxy.rs            # 数据流内 HTTP/1.1 server -> 本地 target 连接池
tests/
  e2e.rs                # 端到端：TLS/心跳/流式/并发/断线重连/大 body/取消/静态路由
examples/               # 双端示例配置
deploy/                 # Dockerfile、compose、systemd、nginx/caddy 示例
docs/                   # 用户文档
```

## 4. 线协议

- ALPN：`divertlink/1`（隧道），`h2`, `http/1.1`（入口）。
- 隧道传输：TCP + rustls(TLS)，其上是 yamux（agent = `Mode::Client`，gateway = `Mode::Server`）。
- 控制流：agent 打开的第一条流，UTF-8 JSON Lines（每行 ≤ 8 KiB），消息类型：
  - C→S `hello`：`{type, tunnel, token, agent_version, protocols[], capabilities[], nonce, timestamp, heartbeat_interval_ms, heartbeat_timeout_ms}`
  - S→C `hello_ack`：`{type, session, tunnel, heartbeat_interval_ms, heartbeat_timeout_ms, max_streams}`；网关采用 agent 请求的心跳值（钳制到 0.5s–5min，超时至少为间隔的 2 倍）
  - `ping` / `pong`：`{type, seq}`，双向，任一侧均可发起。
  - `disconnect`：`{type, reason}` 任一侧在优雅退出时发送。
  - `error`：`{type, code, message}`（如 `invalid_token`、`unknown_tunnel`、`expected_hello`）。
- 关键实现约束：yamux 只有在连接被持续轮询时才会读写帧，因此两端都在**握手之前**启动
  多路复用驱动任务（网关等待首条入站流 + 打开出站流的请求；agent 接受入站数据流），
  握手/心跳在控制流上独立进行。首条入站流之外，网关拒绝 agent 再开新流。
- 数据流：gateway 打开 yamux 流，发送一条 HTTP/1.1 请求（`Connection: close`），agent 以
  HTTP/1.1 server 方式响应；响应体按 chunk 透传（SSE 不缓冲）。流关闭即请求取消。
- 心跳模式（`heartbeat_mode`）：`app`（控制流 ping/pong，默认）、`transport`（TCP keepalive）、
  `both`、`none`。超时 = 连续 `heartbeat_timeout` 未收到任何控制流消息/流数据。

## 5. 配置契约

### 5.1 server.toml

```toml
[server]
name = "divertlink-gateway"
tunnel_listen = "0.0.0.0:443"        # agent 隧道监听（TLS）
ingress_listen = "0.0.0.0:443"       # 公网 HTTP(S) 入口；与上面相同时共用端口按 ALPN 分流
http_listen = "127.0.0.1:8080"       # 可选：明文入口（本地 nginx/new-api 用）；"off"/false = 关闭
admin_listen = "127.0.0.1:9090"      # 管理端点；"off"/false = 关闭
# 四个监听字段均接受 "off"/"none"/"disabled"/""/false 关闭；删除字段回落默认值。

[tls]
cert = "certs/server.crt"
key = "certs/server.key"
client_ca = ""                        # 可选 mTLS 客户端 CA；启用时 tunnel/ingress 必须是不同监听
min_version = "1.3"                   # 1.2 | 1.3

[limits]
max_request_bytes = "64MiB"
request_timeout = "10m"
stream_idle_timeout = "120s"
max_streams_per_tunnel = 256
max_connections = 1024

[log]
level = "info"
format = "text"                       # text | json

[[api_keys]]                          # 公网入口 API Key（可多个）
name = "newapi"
key = "dlk_change_me"
allow_tunnels = []                    # 空 = 全部
allow_ips = []                        # CIDR，空 = 不限制
rate_limit = "600/min"                # 空 = 不限
max_concurrent = 0                    # 0 = 不限

[[tunnels]]                           # 静态注册的隧道（与 agent 配置对应）
id = "home"
token = "agt_change_me"
mount = "/home"                       # 入口前缀；默认剥离后再转发
hosts = []                            # 可选 Host 路由（如 home.llm.example.com）
default = false                       # 无匹配时的兜底隧道（只能一个）
protocols = ["openai", "anthropic", "vllm"]
upstream_api_key = ""                 # 空 = 透传客户端认证头
upstream_auth_header = "passthrough"  # passthrough | authorization | x-api-key | none
public_paths = ["/health"]             # 免认证路径前缀（只读）
max_streams = 128
request_timeout = "10m"
enabled = true

[[static_routes]]                     # 直接反代到固定上游（如 new-api），与隧道互不冲突
mount = "/newapi"
upstream = "http://127.0.0.1:3000"
strip_prefix = true
require_auth = true                   # 默认 true：与隧道一样要求网关 API Key
```

### 5.2 agent.toml

```toml
[agent]
id = "home"
server = "gateway.example.com:443"    # 也接受 https:// / tls:// 前缀
token = "agt_change_me"
heartbeat = "5s"
heartbeat_timeout = "15s"
heartbeat_mode = "app"                # app | transport | both | none
connect_timeout = "10s"
max_inflight = 128
reconnect_min = "1s"
reconnect_max = "60s"
reconnect_jitter = "0.3"              # 0.0-1.0
protocols = ["openai", "anthropic", "vllm"]  # 向网关声明能力（校验/展示）

[tls]
ca = "certs/ca.crt"                   # 自签 CA；空 = 系统根证书
pin = ""                              # 可选 SPKI SHA-256（base64/base64url/hex，可带 sha256/ 前缀）
servername = ""                       # 默认取 server 的 host
client_cert = ""                      # 可选：mTLS 客户端证书（网关开启 tls.client_ca 时必填）
client_key = ""                       # 可选：mTLS 客户端私钥（与 client_cert 同时设置）
insecure = false                      # 仅调试，强烈不建议

[target]
url = "http://127.0.0.1:8000"         # vLLM/Ollama/LM Studio/new-api
request_timeout = "10m"
connect_timeout = "5s"
tls_ca = ""                           # target 为 https 时的额外 CA
headers = {}                          # 附加到上游请求的静态头

[log]
level = "info"
format = "text"
```

环境变量覆盖：`DIVERTLINK_CONFIG`、agent 侧 `DIVERTLINK_SERVER` / `DIVERTLINK_TOKEN` / `DIVERTLINK_ID`。

## 6. CLI

```
divertlink server   [-c server.toml] [--check]
divertlink agent    [-c agent.toml]  [--check] [--once]
divertlink init     [--role server|agent|both] [--dir .] [--domain example.com] [--mtls] [--force]
divertlink gen-certs --dir certs [--domain ...] [--self-signed] [--client] [--force]
divertlink token    [--prefix dlk_] [--count 1] [--bytes 24]
divertlink check    -c config.toml [--strict]
divertlink console  [--listen 127.0.0.1:17653] [--open] [--token TOKEN] [--store PATH]
divertlink version
```

环境变量：`DIVERTLINK_CONFIG`（`-c` 默认值）、agent 侧 `DIVERTLINK_SERVER` / `DIVERTLINK_TOKEN` / `DIVERTLINK_ID`。

`init` 生成 `server.toml`、`agent.toml` 与 `certs/`（`--mtls` 时含 client 证书），把随机的
`dlk_` API key 与 `agt_` token 写进双端配置，agent 自动获得 `ca`、`pin`（`--mtls` 时还有 client
证书路径），最后打印 next steps；单独 `--role agent` 不生成证书。`gen-certs` 生成 CA/server/client
证书并输出 SPKI pin。

## 7. 关键接口（库内）

```rust
// config
ServerConfig::load(path) -> Result<ServerConfig>; ServerConfig::validate(&self) -> Result<()>;
AgentConfig::load(path) -> Result<AgentConfig>;  AgentConfig::validate(&self) -> Result<()>;
ServerConfig::EXAMPLE / AgentConfig::EXAMPLE: &'static str

// tls
install_default_provider();
build_server_config(&TlsServerSettings) -> Result<Arc<rustls::ServerConfig>>;      // ALPN: divertlink/1,h2,http/1.1
build_server_config_with_alpn(&TlsServerSettings, Vec<Vec<u8>>) -> Result<...>;    // 专用入口监听只用 h2/http1.1
build_agent_client_config(&AgentTlsSettings) -> Result<Arc<rustls::ClientConfig>>; // 支持 CA/pin/mTLS
generate_certs(&GenCertsOptions) -> Result<CertReport>;
verify_peer_pin(peer_certs, pin) -> Result<()>;

// registry
Registry::new(&ServerConfig, Arc<Metrics>) -> Arc<Registry>;
TunnelEntry::open_stream(&self) -> Result<(yamux::Stream, OwnedSemaphorePermit)>;
Registry::install_session(&self, tunnel_id, Arc<ActiveSession>) -> Result<()>;
Registry::remove_session(&self, tunnel_id, session_id);
Registry::resolve(&self, req: &http::Request<()>, client_ip) -> Result<ResolvedRoute>;
Registry::status_json(&self) -> serde_json::Value;

// httpfwd
sanitize_request_headers(headers, &ForwardCtx) -> HeaderMap;
sanitize_response_headers(headers);
strip_mount(path, mount) -> Option<String>;
join_upstream_uri(base, path, query) -> Result<http::Uri>;
IdleTimeoutBody<B> / idle_timeout_stream(...);

// server / agent
server::run(ServerConfig, Option<CancellationToken>) -> Result<ServerHandle>;
//   ServerHandle::addrs() -> ServerAddrs { tunnel, ingress, http, admin }，测试用
//   ServerHandle::state() / cancel() / shutdown()
agent::run(AgentConfig, Option<CancellationToken>) -> Result<AgentHandle>;
agent::run_once(AgentConfig) -> Result<()>;          // --once
agent::build_state(AgentConfig) -> Result<Arc<AgentState>>;
```

## 8. 测试与验收

- 单元：配置解析/校验/示例可解析、key/token 常量时间比较、令牌桶、头清洗、mount/URL、pin、证书生成、backoff。
- 端到端（`tests/e2e.rs`，进程内启动 gateway+agent+mock 上游）：
  1. 明文/ TLS 入口 OpenAI 请求透传；2. SSE 逐块实时到达（带时延断言）；
  3. Anthropic `/v1/messages` + `x-api-key` 透传/替换；4. 错误 key 401、错误 token 拒绝注册；
  5. 同一隧道多请求并发不串流；6. 大 body（≥8MiB）完整性；7. 客户端断开 → 上游收到取消；
  8. 网关重启 → agent 自动重连恢复；9. static_routes 反代；10. public_paths 免认证；
  11. 管理端点 /healthz /metrics。
- 发布物：`cargo fmt --check`、`cargo clippy --all-targets -- -D warnings`、`cargo test`、release 构建；
  CI 多平台（linux musl x86_64/aarch64、macOS、Windows）；Docker 多阶段静态镜像；systemd 单元。
- 当前实现状态：127 个测试全部通过（93 单元 + 10 CLI 集成 + 3 控制台集成 + 21 端到端），clippy `-D warnings` 干净，
  rustdoc `-D warnings` 干净；端到端覆盖还包括 mTLS、SPKI pin 匹配/不匹配、错误 token 拒绝、
  请求体超限、SSE 增量性断言、HTTP/2 入口、静态路由鉴权开关、纯明文部署。
- 冒烟脚本 `scripts/smoke.sh`（15 项断言，含内置控制台 UI/API 自检）与 `make check`（fmt/clippy/test/doc/部署资产自检）均已验证通过。

## 8.1 安全边界（实现细节）

- mTLS 只作用于 **tunnel 监听**：入口监听使用独立的 rustls 配置（无 client CA、ALPN 仅 h2/http1.1），
  因此公网客户端永远不会被要求出示证书；`tls.client_ca` 在配置校验阶段强制要求
  `tunnel_listen != ingress_listen`。
- SPKI pin 在标准 WebPKI 校验**之后**再做常量时间比对（pin 不替代证书校验，属纵深防御）。
- 全部密钥/token 比较使用 SHA-256 + 常量时间比较；API key 仅以摘要驻留内存。
- 请求头清洗：剥离 hop-by-hop、`Host`、`Content-Length`、`Authorization`/`x-api-key`（按策略重写）、
  `X-Forwarded-*`/`X-Request-Id` 等网关管辖头，杜绝请求走私与头伪造。
- 请求体上限在网关入口强制执行（超限 413），SSE 响应体有逐块空闲超时；客户端断开时
  通过 yamux 流关闭向上游传播取消。

## 8.2 图形控制台

`divertlink console` 在同一二进制内提供本地 Web 管理面（无 Node/Electron/Tauri 依赖）：

- 监听默认 `127.0.0.1:17653`；非回环必须 `--token`；校验 Host、写操作要求
  `x-divertlink-console: 1`、永不返回 CORS 头（防 DNS rebinding / CSRF）。
- profile 存储 `~/.config/divertlink/console/profiles.json`（macOS Application Support、
  Windows `%APPDATA%`，可用 `DIVERTLINK_HOME` 覆盖；权限 0600）。
- 子进程托管：`current_exe()` + `server|agent -c <config>`，SIGTERM 后 8s 升级 SIGKILL；
  stdout/stderr 行流 → 环形缓冲（2000 行）+ broadcast → SSE 推送。
- REST：`/api/state|profiles|profiles/:id/{start,stop,restart,status,config,logs}`、
  `/api/init`（脚手架）、`/api/token`、`/api/probe`（TLS 探测）、`/api/export|import`、
  `/api/shutdown`；静态资源经 `include_str!` 嵌入。
- 前端为无构建单页应用 + PWA（manifest + service worker，API 不缓存），可安装为桌面应用。
- UI 参考：cc-switch / cc-switch-web / pi-switch / frpc-desktop / frpmgr（见
  `docs/console.md`，未复用其代码）。

## 9. 非目标（MVP 不做）

动态隧道注册/管理 Web UI、ACM E 自动签证书、多 agent 负载均衡同一隧道、QUIC/WebSocket 传输
（后续版本以 feature 形式加入）、跨网关联邦。文档中列明扩展点。
