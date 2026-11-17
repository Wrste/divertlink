# 常见问题（FAQ）

- [1. DivertLink 和 frp / Cloudflare Tunnel 有什么区别？](#1-divertlink-和-frp--cloudflare-tunnel-有什么区别)
- [2. 和 nginx 反向代理有什么区别？](#2-和-nginx-反向代理有什么区别)
- [3. 家庭宽带没有公网 IP 能用吗？](#3-家庭宽带没有公网-ip-能用吗)
- [4. 必须要域名吗？只用 IP 行不行？](#4-必须要域名吗只用-ip-行不行)
- [5. 支持哪些模型和推理服务？](#5-支持哪些模型和推理服务)
- [6. 能同时暴露多个模型 / 多个服务吗？](#6-能同时暴露多个模型--多个服务吗)
- [7. 支持多用户、多租户吗？](#7-支持多用户多租户吗)
- [8. 能接 new-api / one-api 吗？](#8-能接-new-api--one-api-吗)
- [9. SSE 流式会被缓冲或截断吗？](#9-sse-流式会被缓冲或截断吗)
- [10. 支持 HTTP/3、WebSocket、gRPC 吗？](#10-支持-http3websocketgrpc-吗)
- [11. Windows 能部署吗？](#11-windows-能部署吗)
- [12. 安全吗？家庭端会不会被暴露？](#12-安全吗家庭端会不会被暴露)
- [13. 证书必须买吗？支持自动签发吗？](#13-证书必须买吗支持自动签发吗)
- [14. 能多个 agent 负载均衡同一条隧道吗？](#14-能多个-agent-负载均衡同一条隧道吗)
- [15. 改了配置需要重启吗？有热加载吗？](#15-改了配置需要重启吗有热加载吗)
- [16. 有计费、额度、限流吗？](#16-有计费额度限流吗)
- [17. 会拖慢推理速度吗？](#17-会拖慢推理速度吗)
- [18. 443 端口被 nginx 占用了怎么办？](#18-443-端口被-nginx-占用了怎么办)

## 1. DivertLink 和 frp / Cloudflare Tunnel 有什么区别？

| 维度 | DivertLink | frp | Cloudflare Tunnel |
| --- | --- | --- | --- |
| 定位 | 面向 LLM 服务的反向隧道网关 | 通用内网穿透/反向代理 | 第三方托管的隧道服务 |
| 部署依赖 | 自有公网服务器 + 单二进制 | 自有公网服务器 + frps/frpc | Cloudflare 账号与托管网络 |
| 公网入口 | 内置 HTTP(S) 入口，支持 ALPN 单端口复用 | 以 TCP/HTTP 转发为主，需自行配合入口 | 由 Cloudflare 提供域名入口 |
| 入口鉴权 | 网关 API Key（按 Key 绑定隧道/IP/限流） | frp 有基础认证，业务鉴权需额外层 | Cloudflare Access 等 |
| LLM 特性 | 协议透明、SSE 零缓冲、可直接接 new-api/one-api | 通用转发，SSE 可用但需自行调优 | SSE 可用，但流量经第三方 |
| 数据主权 | 全链路自控（自有机房/云主机） | 自控 | 流量经过 Cloudflare |

如果你的需求只是“把某个端口映射到公网”，frp 也能做到；DivertLink 的增量价值在于
**单端口 ALPN 复用、LLM 透明转发、按 Key 的入口鉴权/限流、以及与 new-api 的原生组合
方式**。Cloudflare Tunnel 省运维，但数据与可用性依赖第三方。

## 2. 和 nginx 反向代理有什么区别？

nginx 反向代理解决的是“已知上游地址的七层转发”；DivertLink 解决的是“家庭端**没有公网
IP**、需要**主动外连**才能被公网访问”的问题：

- 家庭端 agent 主动向网关建立 TLS 长连接，网关不需要能路由到家庭内网；
- 一条 TCP 连接上 yamux 多路复用，所有请求共享隧道，不受 NAT 空闲回收影响；
- 网关内置入口认证、限流、体积/并发限制与 Prometheus 指标；
- nginx 更多用于 TLS 终止、静态/通用反代。两者可以叠加：nginx 终止公网 TLS 后
  反代到 DivertLink 的 `http_listen`，或按 ALPN 做 TLS 直通到 `tunnel_listen`。

## 3. 家庭宽带没有公网 IP 能用吗？

可以。连接始终由家庭端 agent **主动发起**，因此：

- 不需要公网 IP，CGNAT、动态 IP、双 NAT 都能用；
- 不需要路由器端口映射；
- 只需要家庭网络允许**出站 TCP 443** 到网关；
- 网关重启或网络抖动后 agent 会按指数退避（带 jitter）自动重连。

## 4. 必须要域名吗？只用 IP 行不行？

功能上可以直连 IP（`[agent].server` 写 `IP:443`），但生产环境强烈建议用域名：

- TLS 证书的名称校验按域名（SAN）进行；用 IP 访问需要证书 SAN 包含该 IP，
  自签/公网 CA 都要相应签发，运维更麻烦；
- 使用 `[[tunnels]].hosts` 的 Host 路由必须有域名；
- Let's Encrypt 等公网 CA 不为裸 IP 签发常规证书。

如果暂时没有域名，可在内网/实验环境用自签证书 + `--insecure` 或 SPKI pin 验证通路，
但不要把它当作生产方案。

## 5. 支持哪些模型和推理服务？

网关**不解析模型与业务 JSON**，所以只要家庭端服务提供 HTTP 接口就能透传，包括：

- vLLM（OpenAI 兼容接口 + `/health`、`/tokenize`、`/metrics` 等原生端点）；
- Ollama（OpenAI 兼容 `/v1` 与原生 `/api/...` 均可透传）；
- LM Studio、llama.cpp server；
- new-api、one-api、LiteLLM 等聚合网关；
- 任何 OpenAI / Anthropic 兼容服务（Responses API、embeddings、rerank 等）。

具体模型名、路径以家庭端服务为准；网关不改写 `model` 字段。

## 6. 能同时暴露多个模型 / 多个服务吗？

- **多个模型**：只要同一个家庭端服务（如 vLLM 多模型实例或 new-api）能提供这些模型，
  一个隧道即可暴露全部，模型名原样透传。
- **多个服务/多台机器**：一个 agent 进程对应一个隧道 id 与一个 `[target].url`。
  需要暴露多个独立服务时，运行多个 agent 实例（不同 id/token/target，各自配置），
  或在家庭端用 new-api 聚合成一个统一入口再只暴露 new-api。
- 网关侧为每个隧道配置独立的 `[[tunnels]]`、mount/hosts 与并发上限。

## 7. 支持多用户、多租户吗？

支持到“按调用方隔离”的粒度：

- 多个 `[[api_keys]]`，各自可绑定 `allow_tunnels`、`allow_ips`、`rate_limit`、
  `max_concurrent`；
- 不同 Key 可访问不同隧道，适合“每个团队一条隧道”；
- `debug` 级别的转发日志会带上 Key 名称，可按名称区分调用方（默认 `info` 级别不记录
  逐请求日志）。

更细的用户体系、额度、计费、模型级权限需要 new-api / one-api 等上层实现；DivertLink
本身不解析业务内容，不做按模型/按 token 的授权。

## 8. 能接 new-api / one-api 吗？

可以，而且有三种拓扑（详见 [协议与调用 §5](protocols.md#5-与-new-api--one-api-集成)）：

1. **new-api 把网关配成上游渠道**：OpenAI 类型渠道 Base URL 填
   `https://gateway.example.com/home`（通常不带 `/v1`，由渠道拼接
   `/v1/chat/completions`；「OpenAI 兼容」类型则填 `.../home/v1`，以渠道测试为准），
   密钥填网关 API Key；适合 new-api 做额度与渠道管理。
2. **网关 `[[static_routes]]` 反代到本机 new-api**：只暴露一个 443 入口，
   客户端访问 `https://gateway.example.com/newapi/v1/...`。静态路由默认**经过网关
   API Key 鉴权与限流**；若设置 `require_auth = false` 则由 new-api 自行鉴权，此时
   客户端携带的是 new-api 令牌，且 `upstream` 只应指向回环/内网地址。
3. **agent 指向家庭端 new-api**：把家庭端 new-api（及其管理的多个本地模型）
   整体映射到公网，外部只看到网关地址。

one-api 同理：OpenAI 类型渠道通常填不带 `/v1` 的根地址（「OpenAI 兼容」类型填带
`/v1` 的地址）；Anthropic 渠道的 Base URL 不带 `/v1`（SDK/渠道会自动追加
`/v1/messages`）。渠道测试 404 时在不带/带 `/v1` 之间切换复测。

## 9. SSE 流式会被缓冲或截断吗？

设计上**不会缓冲**：网关按 HTTP chunk 逐块透传，SSE 数据到达即转发，并支持端到端取消
（客户端断开 → 上游连接关闭）。但以下情况会造成“看起来被缓冲/被截断”：

- 前置 nginx 的 `proxy_buffering` 未关闭、CDN/压缩层缓冲；
- 客户端 `curl` 未加 `-N`、SDK/管道缓冲；
- 上游长时间无输出命中 `stream_idle_timeout`（默认 120s）；
- 超过约 300s 无输出时被 agent 的 target 空闲上限中断（当前固定值，不可配置）；
- 生成时间超过 `request_timeout`（默认 10m）被截断。

验证与调优方法见 [协议与调用 §6](protocols.md#6-sse-流式验证) 与
[故障排查 §3](troubleshooting.md#3-sse-流式被缓冲)。

## 10. 支持 HTTP/3、WebSocket、gRPC 吗？

当前 MVP 不支持：

- 隧道传输是 TCP + TLS + yamux，数据流内是 **HTTP/1.1**；
- WebSocket、gRPC（依赖 HTTP/2 双向流）无法透传；
- QUIC/HTTP/3 属于后续版本的非目标（见 [DESIGN 第 9 节](DESIGN.md#9-非目标mvp-不做)）。

普通 HTTP/1.1 与 HTTP/2 的请求/响应（含 SSE）不受影响：公网入口支持 `h2` 与
`http/1.1`，只是隧道内部统一为 HTTP/1.1 语义。

## 10.5 有图形界面 / 桌面客户端吗？

有。`divertlink console --open` 启动内置控制台（本地 Web UI，编进同一个二进制，无需
Node/Electron/Tauri）：多套配置管理、一键启停、实时日志、流量看板、TLS/证书探测、
在线编辑配置；浏览器可「安装」为 PWA 桌面应用，`scripts/desktop/` 另提供
Windows/macOS/Linux 双击启动脚本。UI 参考了 cc-switch / cc-switch-web /
frpc-desktop 等开源项目。详见 [图形化客户端](console.md)。

> 控制台默认只监听 127.0.0.1，是本地管理工具，不替代 systemd/Docker 的常驻托管。

## 11. Windows 能部署吗？

可以。DivertLink 是单二进制，CI 产物包含 Windows x86_64 版本：

- 网关与 agent 都能在 Windows 上运行；
- 家庭端长期运行可用任务计划程序配置“开机启动”，或用 NSSM/WinSW 等第三方工具
  注册为 Windows 服务（这些工具不是 DivertLink 内置功能）；
- PowerShell 下用 `curl.exe` 验证，注意 PowerShell 的 `curl` 别名指向
  `Invoke-WebRequest`，建议显式写 `curl.exe`；
- 配置文件路径、证书路径使用 Windows 风格（如 `C:\divertlink\certs\server.crt`），
  `[target].url` 写 `http://127.0.0.1:8000`。

生产环境的网关仍建议部署在 Linux 上（systemd + 非 root + 能力最小化更成熟）。

## 12. 安全吗？家庭端会不会被暴露？

- 家庭端**不开放任何入站端口**，只主动外连；公网只能通过网关入口访问；
- 隧道固定 TLS 1.3 + token 认证，可选 mTLS 与 SPKI pinning；入口有 API Key、限流、
  体积/并发限制与 Prometheus 指标；
- 家庭内服务（vLLM/new-api）应只监听 `127.0.0.1`，不因部署 DivertLink 而暴露到局域网；
- **注意**：网关是 TLS 终止点，网关运营者（也就是你）在协议上可以看到请求元数据
  与内容；协议透明指“不解析业务语义”，不等于端到端加密。对网关主机本身要做加固，
  并妥善保管私钥与令牌。完整威胁模型见 [安全指南](security.md)。

## 13. 证书必须买吗？支持自动签发吗？

- 不必须买：可以用 `gen-certs` 生成自签 CA 与服务端证书，agent 配置 `[tls].ca` 校验；
  但外部客户端（浏览器/其他 SDK）需要信任该 CA，或使用 pin/insecure（后者禁止生产）。
- 推荐用免费的公网 CA（Let's Encrypt 等）以获得客户端天然信任：
  - 用 certbot/Caddy 等外部工具签发与续期，证书路径填入 `[tls].cert`/`[tls].key`；
  - 网关占用 443 时，建议用 **HTTP-01（80 端口）或 DNS-01** 验证；TLS-ALPN-01 需要
    在 443 上响应专用 ALPN，与运行中的网关冲突，通常要停网关才能签发。
- DivertLink 本身**不内置 ACME 自动签发**（不在当前版本范围内）；续期后需重启网关生效。

## 14. 能多个 agent 负载均衡同一条隧道吗？

当前版本**不支持**“同一隧道 id 挂多个 agent 做负载均衡”：

- 一个隧道 id 同时只有一个活动会话，新会话会**热替换**旧会话（用于断线重连）；
- 多个 agent 使用同一 id/token 会互相抢占，表现为隧道频繁上下线；
- 需要横向扩展时，可以为每台家庭机器配置不同隧道（不同 id/token/mount/hosts），
  在客户端或 new-api 层做多渠道路由/负载均衡。

## 15. 改了配置需要重启吗？有热加载吗？

需要重启，当前版本不提供 SIGHUP/热加载能力：

- 改 `server.toml` → 重启网关；
- 改 `agent.toml` → 重启 agent；
- 改令牌/证书 → 先重启网关，再重启 agent（agent 会以新凭据重连）；
- 用 `divertlink check -c ...` 或 `divertlink server -c ... --check` 在重启前校验配置。

会话热替换只保证 agent 重连时流量平滑接管，不改变“配置启动时加载”的事实。

## 16. 有计费、额度、限流吗？

- **限流/并发**：有。`[[api_keys]].rate_limit`、`max_concurrent`，以及网关级
  `max_request_bytes`、`max_connections`、`max_streams` 等保护。
- **可观测/审计**：有限。`/metrics` 提供请求量、认证失败、限流、隧道在线等指标；
  逐请求转发详情（`request_id`、Key 名称、隧道、状态码、耗时）需要 `[log].level =
  "debug"` 才输出，默认 `info` 只记录生命周期事件；认证失败等拒绝只计数不写日志。
  需要完整访问审计时，可在网关前加一层带访问日志的代理，或使用 new-api 的调用日志。
- **计费/额度/按 token 计量**：没有。网关不解析业务 JSON，无法统计 token 数；
  这类能力请用 new-api / one-api 等上层做，DivertLink 只负责安全地把请求送到家庭端。

## 17. 会拖慢推理速度吗？

设计目标是尽量轻：网关只做认证、限流、路由与字节转发，不解析 JSON、不压缩；
tokio/hyper + `Bytes` 零拷贝、yamux 多路复用、`TCP_NODELAY`、端到端背压。
实际延迟主要由两段公网 RTT + GPU 推理决定，网关处理通常在亚毫秒到数毫秒量级。

真实链路的 TTFB、吞吐与并发拐点请用自己的场景压测，方法与调参见
[性能与容量](performance.md)。

## 18. 443 端口被 nginx 占用了怎么办？

三种可行方式（详见 [部署文档](deployment.md)）：

1. **nginx 终止公网 TLS**：nginx 监听 443，反代到 DivertLink 的 `http_listen`
   （如 `127.0.0.1:8080`），agent 隧道改用独立端口，并在 nginx 配置中关闭 SSE 缓冲；
2. **nginx 做 TLS 直通**：用 `stream` 模块 + `ssl_preread` 按 ALPN/SNI 判断，
   把 `divertlink/1` 的流量原样转发到 `tunnel_listen`，其余交给 ingress 或 nginx 自己的
   HTTP 服务；
3. **拆监听**：`tunnel_listen` 与 `ingress_listen` 使用不同端口，公网按需放行；
   入口仍可直接由 DivertLink 终止 TLS。

不建议把 agent 隧道接到会终止 TLS 的七层代理后面——那样 `divertlink/1` ALPN 无法协商。
