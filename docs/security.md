# 安全指南

**中文** · [English](en/security.md) · [文档目录](README.md)

DivertLink 把一个没有公网 IP 的家庭 LLM 服务暴露到公网，安全边界需要明确：
**网关服务器是信任锚点，家庭端出站连接是唯一入口**。本文给出威胁模型、可用的防护措施、
密钥管理与运维加固清单。

- [1. 安全模型与信任边界](#1-安全模型与信任边界)
- [2. 威胁模型](#2-威胁模型)
- [3. 传输安全（TLS / mTLS / pinning）](#3-传输安全tls--mtls--pinning)
- [4. 认证与密钥管理](#4-认证与密钥管理)
- [5. 入口防护（限流 / 体积 / 并发）](#5-入口防护限流--体积--并发)
- [6. 审计与可观测](#6-审计与可观测)
- [7. 网络暴露面](#7-网络暴露面)
- [8. 主机与进程加固](#8-主机与进程加固)
- [9. 上线检查清单](#9-上线检查清单)
- [10. 密钥泄露应急响应](#10-密钥泄露应急响应)
- [11. 已知边界](#11-已知边界)

## 1. 安全模型与信任边界

```text
[不可信] 公网客户端 ──TLS──► [信任锚点] 网关服务器 ──加密隧道──► [半可信] 家庭端 agent ──HTTP──► 家庭内服务
                              │ 承担：入口认证/限流/审计/TLS 终止
                              │ 家庭端只主动外连，不开放任何入站端口
```

- **网关服务器**：公网唯一入口，持有服务端私钥与 `server.toml`（含所有令牌）。
  必须由你控制、加固、监控。
- **家庭端**：只发起出站 TLS 连接，不需要公网 IP 或端口映射；token 泄露意味着他人可
  以该隧道 id 加入会话池并接收请求，因此 token 等同密码。
- **客户端**：持有网关 API Key；通过 `upstream_api_key` 替换模式可以做到真实上游 Key
  只存在于网关配置中。
- **家庭内服务**：应只监听 `127.0.0.1`，不因 DivertLink 的引入而额外暴露到局域网；
  网关不会替你校验家庭端服务的入站来源。

## 2. 威胁模型

| # | 威胁 | 可能后果 | 主要缓解措施 |
| --- | --- | --- | --- |
| T1 | 公网扫描、暴力猜解 API Key | 盗用算力/额度 | 高熵 Key、`rate_limit`、`allow_ips`、`max_concurrent`、日志告警 |
| T2 | API Key 泄露 | 未授权调用、费用损失 | 一调用方一 Key、`allow_tunnels` 最小权限、轮换、吊销后重启生效 |
| T3 | agent token 泄露 | 攻击者以同名隧道加入会话池并接收流量 | token 视同密码、定期轮换、mTLS/SPKI pin、监控 `/status` 会话变化 |
| T4 | 中间人攻击（TLS 降级/伪造证书） | 窃听或篡改 LLM 流量 | TLS 1.3 默认、`min_version="1.3"`、agent `[tls].ca` 校验、可选 SPKI pin、禁止 `insecure` |
| T5 | 客户端或 new-api 被攻破 | 以合法 Key 发起滥用 | 细粒度 Key、限流、并发上限、按需 `allow_tunnels`，new-api 侧额度控制 |
| T6 | 请求体/并发洪水导致 DoS | 网关内存、连接、上游算力被打满 | `max_request_bytes`、`max_connections`、`max_streams`、`request_timeout`、`stream_idle_timeout` |
| T7 | 上游服务被直接暴露 | 家庭网络被扫描 | target 只监听回环；家庭路由器不做端口映射；agent 仅出站 |
| T8 | 网关主机被入侵 | 服务端私钥、全部令牌、内网 new-api 暴露 | 主机加固、最小服务、补丁、只开 443、管理端点仅回环 |
| T9 | 审计缺失 | 无法定位滥用或泄露 | JSON 日志集中采集、保留期、`/metrics` 告警 |
| T10 | 供应链/二进制被篡改 | 后门或崩溃 | 校验 release 的 SHA-256、固定版本、从源码 `--locked` 构建 |

> DivertLink 不是 WAF，也不解析 LLM 业务 JSON（协议透明）。按模型、按 token 数、按内容
> 做授权与过滤应在 new-api / 业务层完成，网关只提供网络与凭据层面的控制。

## 3. 传输安全（TLS / mTLS / pinning）

### 3.1 网关入口与隧道

- 网关在 443 上终止 TLS：同一端口按 ALPN 分流，`divertlink/1` 走 agent 隧道，
  `h2`/`http/1.1` 走公网 HTTP 入口。因此**入口与隧道共用同一张服务端证书**。
- 隧道连接固定使用 TLS 1.3：agent 的 TLS 客户端只协商 1.3，与服务端 `min_version`
  取 1.2 还是 1.3 无关。
- 公网入口默认最低版本 TLS 1.3（`[tls].min_version = "1.3"`），由 rustls 提供现代
  密码套件与前向保密。仅在确有老客户端（旧版本 new-api、旧 SDK）兼容需求时改为
  `"1.2"`——该值作用于监听器，意味着公网入口也接受 TLS 1.2 客户端。
- 正式环境使用公网 CA 证书（Let's Encrypt 等）；自签 CA 仅建议内网/演练或配合 pin 使用。
- 证书要求：
  - SAN 覆盖网关域名与所有 `[[tunnels]].hosts` 路由域名；
  - 私钥文件权限 `0600`，属主为运行进程的用户；
  - 续期后重启网关进程生效（当前版本不提供证书热加载）。

### 3.2 可选 mTLS

`server.toml` 的 `[tls].client_ca` 非空时，网关在 TLS 握手阶段要求客户端出示由该 CA
签发的证书；agent 侧用 `[tls].client_cert` / `[tls].client_key` 加载对应证书与私钥。
`divertlink gen-certs --client` 或 `divertlink init --mtls` 可生成客户端证书。要点：

- 启用 mTLS 时 `tunnel_listen` 与 `ingress_listen` **必须拆分为不同端口**，
  因为公网入口的普通客户端不会出示客户端证书（同一端口会同时要求浏览器证书）。
- agent 的 `client_cert` 与 `client_key` 必须**同时设置**；只设其中一个会在配置校验时
  直接报错。
- `init --mtls` 只负责生成证书并把路径写进 `agent.toml`；`server.toml` 的
  `client_ca` 仍需手动取消注释、端口仍需手动拆分；用绝对 `--dir` 时写入的是绝对路径，
  拷贝到家庭端后要调整。
- mTLS 保护的是 TLS 层的对端身份，不能替代 agent token 与 API Key，二者应同时启用。
- 客户端证书私钥只保存在对应家庭端，不要集中分发；`ca.key` 只留在网关/离线保存。

### 3.3 SPKI pinning

agent 可配置 `[tls].pin`（SPKI SHA-256，base64 或 hex），`gen-certs` 会直接打印该值。
作用：在正常证书链、主机名与有效期校验之后，额外匹配证书公钥。pin 不会代替 CA 校验，
自签证书仍需配置 `ca`；未配置 `ca` 时 agent 使用内置公共根证书。

- 适用于自签/内部 CA 环境，或需要对公网 CA 再加一层绑定的场景。
- **轮换成本**：pin 是单值字段，换用新密钥后 pin 会变化（同公钥续签不改变 SPKI），必须同步更新所有 agent
  的 `pin` 并重启 agent，建议安排在维护窗口。
- 生产环境若使用公网 CA 且不希望通过 pin 增加运维复杂度，可只依赖 `[tls].ca` 校验。

### 3.4 禁止项

- 生产环境禁止 `insecure = true`（跳过证书校验）。
- 禁止把相同的服务端私钥复制到多台机器。
- 禁止把自签 CA 的私钥（`ca.key` 一类文件）分发到家庭端；家庭端只需要公开的 `ca.crt`。

## 4. 认证与密钥管理

### 4.1 两层独立凭据

| 凭据 | 配置位置 | 作用域 | 生成方式 |
| --- | --- | --- | --- |
| 网关 API Key | `server.toml` 的 `[[api_keys]].key` | 公网入口认证，可绑定隧道、IP、限流 | `divertlink token --prefix dlk_` |
| agent 隧道 token | 双端 `[[tunnels]].token` / `[agent].token` | 隧道注册认证 | `divertlink token --prefix agt_` |
| 上游真实 Key | 隧道 `upstream_api_key`（推荐替换模式） | 家庭端服务鉴权 | 由家庭端服务（vLLM/new-api）提供 |

两套凭据用途不同、必须独立生成与轮换。`--prefix` 只是便于识别，不提供额外安全性。

### 4.2 存储与分发

- `server.toml`、`agent.toml` 权限收紧（`0600`，目录 `0700`）；不要提交到 Git。
- agent 令牌优先通过环境变量注入（`DIVERTLINK_TOKEN`），配合权限 0600 的
  systemd `EnvironmentFile`，避免明文写入配置文件。
- 使用密钥管理系统（Vault、云 KMS、Kubernetes Secret）时，通过环境变量或启动模板注入，
  日志与报错中禁止打印凭据。
- 家庭端只保存自己的一条隧道 token，不要分发其他隧道的 token 或网关 API Key。

### 4.3 最小权限

- **每个调用方一个 Key**：new-api 渠道、运维脚本、监控探针分别建 Key。
- 用 `allow_tunnels` 限定 Key 只能访问必要隧道；用 `allow_ips` 限定来源网段
  （new-api 服务器固定出口 IP 时尤其有效）。
- `rate_limit` 与 `max_concurrent` 按调用方配额设置；`max_concurrent = 0` 表示不限，
  `rate_limit` **不配置**（删除该行）表示不限。公网 Key 不应两者都不限。
- 只用 `upstream_api_key` 替换模式暴露服务时，家庭端真实 Key 不会离开网关；客户端
  只持有网关 Key，即便泄露也不等于拿到上游 Key。

### 4.4 轮换流程

当前版本配置在启动时加载，**变更后需重启进程生效**（无 SIGHUP 热加载）。

**API Key 轮换（可灰度，不中断）**

1. 生成新 Key：`divertlink token --prefix dlk_`；
2. 在 `[[api_keys]]` 中**新增**一个条目（保留旧条目），重启网关；
3. 让客户端/new-api 渠道切换到新 Key，观察 `/metrics` 与错误率；
4. 删除旧条目并再次重启网关，回收旧 Key。

**隧道 token 轮换（需要协调双端）**

1. 生成新 token：`divertlink token --prefix agt_`；
2. 在维护窗口同时更新 `server.toml` 的 `[[tunnels]].token` 与家庭端 `agent.toml`；
3. 先重启网关，再重启 agent（agent 会自动重连并注册成功）；
4. 检查 `/readyz` 与 `/status` 确认隧道在线。

**上游 Key 轮换**：更新家庭端服务（vLLM `--api-key` / new-api 令牌）与网关的
`upstream_api_key`，重启对应进程；两端必须同时保持可用的窗口内完成。

**证书轮换**：更新 `[tls].cert` / `[tls].key`（公网 CA 续期或重新 `gen-certs`），
重启网关；如启用 pin，同步更新所有 agent 的 `pin` 并重启 agent。

## 5. 入口防护（限流 / 体积 / 并发）

| 控制项 | 配置 | 作用 |
| --- | --- | --- |
| 单 Key 速率 | `[[api_keys]].rate_limit`，如 `"600/min"` | 令牌桶限流，防暴力刷接口；429 带 `Retry-After` |
| 单 Key 并发 | `[[api_keys]].max_concurrent` | 限制单调用方同时进行的请求数；超限返回 429（不带 `Retry-After`） |
| 请求体上限 | `[limits].max_request_bytes`（默认 64 MiB） | 声明超限的 `Content-Length` 立即 413；未声明长度的分块上传在读取超限时中断 |
| 每请求超时 | `[limits].request_timeout` / `[[tunnels]].request_timeout` | 防慢请求长期占用 |
| 流空闲超时 | `[limits].stream_idle_timeout`（默认 120s） | 防 SSE 半开连接与僵死上游；agent 侧对 target 响应另有 300s 固定上限 |
| 隧道并发 | `[limits].max_streams_per_tunnel` / `[[tunnels]].max_streams` | 限制单隧道同时在途请求；打满返回 429 `tunnel_busy` |
| 网关连接数 | `[limits].max_connections` | **每个监听器**的并发连接上限（隧道/入口、明文入口、管理端点各一份） |
| 免认证路径 | `[[tunnels]].public_paths` | 仅放行只读健康检查类前缀，不要把业务路径放进去 |

建议公网 Key 一律配置 `rate_limit` 与 `max_concurrent`；家庭端 vLLM 的计算能力是最终
瓶颈，网关限流应与 GPU 容量匹配，避免排队导致的超时雪崩。

> **静态路由的鉴权**：`[[static_routes]]` 默认 `require_auth = true`，与隧道一样受
> API Key、`allow_ips`、`rate_limit` 与 `max_concurrent` 保护（`allow_tunnels` 只对
> 隧道生效）。只有显式设置 `require_auth = false` 时才退化为“仅透传”，此时只受请求
> 体上限与超时约束。因此 `require_auth = false` 的静态路由只应指向 `127.0.0.1` /
> 内网上游，并确保上游（如 new-api）自己校验客户端令牌。

## 5.5 图形控制台的安全边界

内置控制台（`divertlink console`，见 [console.md](console.md)）是**本地管理面**，默认
行为与加固点：

- 默认只监听 `127.0.0.1:17653`；绑定非回环地址必须显式提供 `--token`，否则拒绝启动；
- 校验 `Host` 头（防 DNS rebinding）；所有写操作要求自定义头 `x-divertlink-console: 1`
  （浏览器跨域无法在不触发 CORS 预检的情况下发送，而服务端从不返回 CORS 允许头）；
- 支持 `--token` / `DIVERTLINK_CONSOLE_TOKEN`，比较为常量时间；令牌不写入日志；
- profile 存储为 JSON、权限 `0600`，含配置路径而不含证书/令牌明文（令牌仍在配置文件中，
  由文件权限保护）；
- 控制台只启动 `divertlink server|agent -c <已登记配置>`，不接受任意命令；
- 远程管理建议 `ssh -L 17653:127.0.0.1:17653`，不要把控制台端口直接暴露公网。

## 6. 审计与可观测

- 网关默认（`[log].level = "info"`）记录生命周期事件：监听地址、agent 连上/断开、会话
  替换、心跳超时、关闭等；**不记录每个请求**。
- 每请求转发详情在 **`debug` 级别**输出：`tunnel request forwarded` 含 `request_id`、
  隧道 id、状态码、耗时与 Key 名称（`public` 表示免认证路径）；静态路由对应
  `static route forwarded`（mount、状态码、耗时、request_id）。需要网关侧逐请求审计时
  须显式把 `[log].level` 调到 `debug`（流量大时 IO 成本明显），并用
  `[log].format = "json"` 交给 Loki/ELK/云日志采集。
- **认证失败、403、429 等拒绝只累加 `/metrics` 计数，不写日志**：可用
  `divertlink_auth_failures_total`、`divertlink_rate_limited_total`、
  `divertlink_tunnel_unavailable_total` 等做告警；需要逐次拒绝详情时在隧道入口前加
  一层带访问日志的反向代理，或提高日志级别。
- 网关日志**不包含**客户端 IP、HTTP 方法与完整请求路径（`X-Forwarded-For` 会转发给
  上游，但不落盘）；需要这些字段时请依赖前置 nginx/Caddy 的访问日志、new-api 日志或
  在 `debug` 日志基础上自行补充采集规则。
- 不要把完整认证头、body 或令牌写入日志；`debug` 级别仅用于短时排障。
- 管理端点 `/metrics` 提供 Prometheus 指标，用于请求量、错误率、隧道在线状态告警；
  `/status` 提供隧道/会话快照。两者默认只监听 `127.0.0.1`，通过 SSH 隧道或本机
  Prometheus 抓取，不要暴露到公网。
- 建议告警项：401/403/429 比例突增、502/503 比例突增（隧道掉线）、p99 延迟上升、
  `/readyz` 失败、网关进程重启、磁盘/证书到期时间。常用指标名：
  `divertlink_requests_total`、`divertlink_tunnel_connections_active`、
  `divertlink_tunnel_requests_total{tunnel=...}`、`divertlink_stream_errors_total`、
  `divertlink_heartbeat_timeouts_total`。

## 7. 网络暴露面

- **公网入站只开 443/tcp**（或你自定义的 tunnel/ingress 端口组合）；`http_listen` 与
  `admin_listen` 只绑回环地址，不开公网。
- 家庭端不需要任何入站端口；只需允许出站 TCP 443 到网关。CGNAT、动态 IP、双 NAT 均不影响。
- 如 443 被 nginx/Caddy 占用：
  - 让 nginx/Caddy 终止公网 TLS，反代到 `http_listen`，agent 隧道使用独立端口；或
  - 用 nginx `stream` 按 ALPN `divertlink/1` 做 TLS 直通（不要终止 TLS）。
  两种模式均见 [部署文档](deployment.md)。
- 网关服务器上的 new-api 等上游只监听 `127.0.0.1`；`[[static_routes]].upstream`
  不要写成可被公网访问的地址，避免网关变成开放代理。注意静态路由**不做网关入口
  鉴权**，其安全性完全取决于上游自身鉴权与"只反代内网地址"这一前提。
- 可选：用云安全组限制 443 的来源网段；如客户端来源固定，配合 `allow_ips` 双重限制。
- **前置代理与真实 IP**：只有网关前是可信的本机/内网 nginx/Caddy 时才设置
  `[server].trust_proxy = true`，让 `allow_ips` 使用 `X-Forwarded-For`/`X-Real-IP`。
  公网直连时保持 `false`，否则客户端可伪造该头绕过 IP 白名单；经代理部署时也应在
  代理层再做一层 IP 访问控制。
- SSH 管理端口限制来源 IP，禁用密码登录，保持系统与二进制更新。

## 8. 主机与进程加固

- 使用独立系统用户运行，不要以 root 常驻；443 权限用 `AmbientCapabilities` 或
  `setcap` 授予。
- 证书与配置目录 `0700`，文件 `0600`；日志走 journald/集中采集，本地不落敏感内容。
- systemd 推荐加固项（与 [部署文档](deployment.md) 中的单元文件保持一致）：

```ini
[Service]
User=divertlink
Group=divertlink
AmbientCapabilities=CAP_NET_BIND_SERVICE
CapabilityBoundingSet=CAP_NET_BIND_SERVICE
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
PrivateTmp=true
PrivateDevices=true
ProtectKernelTunables=true
ProtectKernelModules=true
ProtectControlGroups=true
RestrictAddressFamilies=AF_INET AF_INET6
RestrictNamespaces=true
LockPersonality=true
UMask=0077
LimitNOFILE=65535
LimitCORE=0
Restart=on-failure
RestartSec=2
```

- Docker 部署时使用非 root 用户、只读根文件系统、`cap_drop: [ALL]` +
  `cap_add: [NET_BIND_SERVICE]`、`security_opt: [no-new-privileges:true]`，
  配置与证书以只读卷挂载。
- 定期更新操作系统与 DivertLink；升级前用 `divertlink check -c ...` 校验配置兼容性。
- 校验发行包 SHA-256，或从源码以 `cargo build --release --locked` 固定依赖构建。

## 9. 上线检查清单

- [ ] 所有示例令牌（`dlk_change_me`、`agt_change_me` 等）已替换为高熵随机值
- [ ] `[[api_keys]]` 按调用方拆分，配置了 `allow_tunnels`、`rate_limit`、`max_concurrent`
- [ ] 公网 Key 按需配置 `allow_ips`；运维/监控 Key 与业务 Key 分离
- [ ] agent token 仅存在于对应家庭端与网关配置，未进入 Git、聊天记录或公开镜像
- [ ] 配置文件与私钥权限为 `0600`，属主为运行用户；`ca.key` 未分发到家庭端
- [ ] TLS 使用公网 CA 或受控自签 CA；SAN 覆盖所有访问域名；`min_version = "1.3"`
- [ ] `insecure = false`；如启用 pin，已在所有 agent 上同步并演练过轮换
- [ ] mTLS 如启用：tunnel/ingress 已分端口，`[tls].client_ca` 与 agent 的
      `[tls].client_cert` / `client_key` 均已配置并通过 `divertlink check`
- [ ] `max_request_bytes`、`request_timeout`、`stream_idle_timeout` 已按业务校准
- [ ] `max_streams` / `max_inflight` / `max_connections` 与 GPU 及带宽容量匹配
- [ ] 公网仅放行 443；`http_listen`、`admin_listen` 只监听回环（注意这两个监听无法用
      配置关闭，只能改到其他回环端口）
- [ ] 仅在前置可信代理时开启 `trust_proxy`，公网直连保持 `false`，避免伪造 `X-Forwarded-For` 绕过 `allow_ips`
- [ ] `/healthz`、`/readyz`、`/metrics`、`/status` 未暴露到公网，且有监控告警
- [ ] 已明确审计来源（网关 `debug` 日志 / 前置代理访问日志 / new-api 日志）并集中采集；
      确认日志不含认证头与 body
- [ ] `[[static_routes]]` 默认 `require_auth = true`；设为 `false` 的静态路由只指向
      回环/内网地址，且上游（new-api 等）已自行校验客户端令牌
- [ ] 已配置 401/403/429/502/503 突增、隧道掉线、证书到期的告警
- [ ] systemd/Docker 加固项生效：非 root、`NoNewPrivileges`、能力最小化、UMask
- [ ] 已演练：网关重启后 agent 自动重连恢复；证书/令牌轮换流程可用
- [ ] 上游（vLLM/new-api）只监听回环，家庭路由器未做额外端口映射
- [ ] 备份了 `server.toml`、证书与私钥（离线加密保存），并记录恢复步骤

## 10. 密钥泄露应急响应

1. **立即隔离**：如怀疑 API Key 泄露，先删除对应 `[[api_keys]]` 条目并重启网关
   （可临时收紧 `rate_limit` 作为止损）；如为隧道 token 泄露，先更换 token、重启网关，
   再更新家庭端并重启 agent。
2. **确认影响面**：检索网关 `debug` 日志/前置代理访问日志中该 Key 的调用时间、来源 IP、
   路径与状态码；检查 `/status` 是否有异常的会话替换记录，并查看 `/metrics` 中
   `divertlink_auth_failures_total`、`divertlink_requests_total{tunnel=...}` 的变化。
3. **轮换关联凭据**：同一环境中的上游 Key（vLLM/new-api）、new-api 渠道 Key、
   监控 Key 一并评估是否需要轮换。
4. **修复入口**：若是客户端泄露，检查客户端日志/配置是否打印了 Key；若是服务器泄露，
   检查配置权限、备份、镜像层与 shell history。
5. **复盘**：补充告警（异常来源 IP、请求量突增）、收紧 `allow_ips` 与限流，并更新本清单。

## 11. 已知边界

- **网关是 TLS 终止点**：网关（及其运营者）在协议上可以观察请求路径、头与 body；
  协议透明只表示不解析业务语义，不等于端到端加密。对网关运营者不可信的场景，应自行
  评估或在网关前另加可信 TLS 层。
- **无细粒度业务授权**：除 `allow_tunnels`、`allow_ips` 与 `public_paths` 外，网关不按
  模型/用户/内容做授权，因为这需要解析业务 JSON，与协议透明设计冲突；这类控制请放在
  new-api 等上层。
- **静态路由鉴权可关闭**：`require_auth = false` 时 `[[static_routes]]` 绕过 API Key、IP/隧道白名单、限流与并发
  限制，只透传客户端认证头（见 [配置参考 §2.7](configuration.md#27-static_routes)）。
  必须由上游自行鉴权，并只反代到回环/内网地址。
- **监听可显式关闭**：`tunnel_listen` / `ingress_listen` / `http_listen` /
  `admin_listen` 写 `"off"`/`false` 即关闭；省略字段则回落默认地址。
- **固定值**：agent 对 target 响应体的空闲上限固定为 300s，不可配置；
  `[limits].header_read_timeout` 已接入入口/管理/agent 数据流的 HTTP 读头超时。
- **网关级防护**：`[limits].global_rate_limit` 在认证前限流；
  `[limits].allowed_methods` 拒绝 TRACE/CONNECT 等非预期方法；
  SSE 响应自动加 `X-Accel-Buffering: no` / `Cache-Control: no-transform`；
  优雅退出由 `[server].shutdown_grace` 控制（SIGTERM 与 Ctrl-C 行为一致）。
- **配置体检**：`divertlink check -c <file>` 输出安全告警，`--strict` 可把告警当错误，
  适合 CI 门禁。
- **无内置 ACME/证书热加载**：证书需外部签发/续期并重启进程。
- **多会话池**：同一隧道允许多个 agent 并存并轮询分流；token 泄露会让未授权 agent 加入
  会话池并接收流量，因此需同步轮换网关及全部 agent 的 token。
- **MVP 非目标**：动态隧道注册、面向公网的管理 Web UI、QUIC/WebSocket
  传输、跨网关联邦不在当前版本内；本地控制台及同隧道多 agent 已实现（见 [DESIGN 第 9 节](DESIGN.md#9-非目标mvp-不做)）。
