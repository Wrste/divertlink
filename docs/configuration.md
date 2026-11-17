# 配置参考

本文是 DivertLink 的完整配置参考。字段、类型与默认值以实现中的
[`src/config.rs`](../src/config.rs) 为唯一事实来源，并与 `assets/server.example.toml`、
`assets/agent.example.toml`（`init` 生成配置所用的模板）保持一致。
`docs/DESIGN.md` 是早期设计契约，个别字段（如 `[server].trust_proxy`、
`[limits].header_read_timeout`、`[[static_routes]].tls_ca` / `headers`、
`[agent].protocols`、agent 的 `[tls].client_cert` / `client_key`）在实现中已经提供，
但设计文档未必同步，请以本文与代码为准。

- [1. 基本约定](#1-基本约定)
- [2. server.toml（网关）](#2-servertoml网关)
  - [2.1 `[server]`](#21-server)
  - [2.2 `[tls]`](#22-tls)
  - [2.3 `[limits]`](#23-limits)
  - [2.4 `[log]`](#24-log)
  - [2.5 `[[api_keys]]`](#25-api_keys)
  - [2.6 `[[tunnels]]`](#26-tunnels)
  - [2.7 `[[static_routes]]`](#27-static_routes)
- [3. agent.toml（家庭端）](#3-agenttoml家庭端)
  - [3.1 `[agent]`](#31-agent)
  - [3.2 `[tls]`](#32-tls)
  - [3.3 `[target]`](#33-target)
  - [3.4 `[log]`](#34-log)
- [4. 环境变量](#4-环境变量)
- [5. 校验规则](#5-校验规则)
- [6. 最小配置](#6-最小配置)
- [7. 完整示例](#7-完整示例)
- [8. 配置校验与生效方式](#8-配置校验与生效方式)

## 1. 基本约定

| 约定 | 说明 |
| --- | --- |
| 文件格式 | TOML；UTF-8；`#` 开头为注释；未列出的字段使用默认值 |
| 未知字段 | 会被拒绝（配置校验报错），用于防止拼写错误被静默忽略 |
| 时长 | 带单位的字符串，如 `"5s"`、`"120s"`、`"10m"`、`"1h"` |
| 字节数 | 带单位的字符串，如 `"64MiB"`、`"8MiB"`、`"1GiB"`；支持 `B`、`KB/KiB`、`MB/MiB`、`GB/GiB`、`TB/TiB`（`KiB` 等为 1024 进制，`KB` 等为十进制） |
| 地址 | `IP:端口` 或 `[IPv6]:端口`，如 `"0.0.0.0:443"`、`"127.0.0.1:9090"` |
| CIDR | 如 `"203.0.113.0/24"`；单个 IPv4 写 `"203.0.113.10/32"`，单个 IPv6 写 `".../128"` |
| 速率 | `"数量/窗口"`，如 `"600/min"`、`"30/s"`；窗口单位支持 `s`/`min`/`h`/`d`（或 `sec`/`hour` 等全称）；**不配置**该字段表示不限，空字符串会被校验拒绝 |
| 路径 | 证书/CA 相对路径相对**配置文件所在目录**解析（不是进程工作目录）；也可写绝对路径 |
| 数组/表 | TOML 原生语法；`[[api_keys]]`、`[[tunnels]]`、`[[static_routes]]` 为可重复表 |

> 可关闭的可选项（例如 `[tls].client_ca`、`[tls].pin`、`[tls].ca`、
> `[static_routes].tls_ca`）应**删除该行**或写 `# 注释掉`，不要写成空字符串 `""`：
> 空字符串会被当作非法路径或非法 pin，在配置校验或启动加载时拒绝。
>
> 例外：`[server]` 下的四个监听地址（`tunnel_listen` / `ingress_listen` /
> `http_listen` / `admin_listen`）可以显式关闭：写 `"off"`（也接受 `"none"`、
> `"disabled"`、空字符串 `""` 或 `false`）。删除字段仍会回落到默认值（保持监听）。

## 2. server.toml（网关）

### 2.1 `[server]`

| 字段 | 类型 | 默认值 | 必填性 | 说明 |
| --- | --- | --- | --- | --- |
| `name` | string | `"divertlink-gateway"` | 可选 | 实例名，用于日志与 `/status` 展示，不参与路由 |
| `tunnel_listen` | string(地址) \| `"off"` | `"0.0.0.0:443"` | 可选 | agent 隧道监听地址（TLS，ALPN `divertlink/1`）；`"off"`/`false` 关闭 |
| `ingress_listen` | string(地址) \| `"off"` | `"0.0.0.0:443"` | 可选 | 公网 HTTP(S) 入口（TLS，ALPN `h2`/`http/1.1`）；与 `tunnel_listen` 相同则共用端口按 ALPN 分流，否则为独立端口；`"off"`/`false` 关闭 |
| `http_listen` | string(地址) \| `"off"` | `"127.0.0.1:8080"` | 可选 | 明文 HTTP 入口，供本机 nginx/Caddy 终止 TLS 后回源。**不要监听公网地址**；`"off"`/`false` 关闭 |
| `admin_listen` | string(地址) \| `"off"` | `"127.0.0.1:9090"` | 可选 | 管理端点 `/healthz`、`/readyz`、`/metrics`、`/status`。只应监听回环地址；`"off"`/`false` 关闭 |
| `trust_proxy` | boolean | `false` | 可选 | 是否采信 `X-Forwarded-For` / `X-Real-IP` 作为客户端 IP。仅当网关前面是可信的本机/内网反向代理时开启；公网直连时开启会被伪造头绕过 `allow_ips` |
| `shutdown_grace` | string(时长) | `"10s"` | 可选 | 优雅退出时等待在途请求/SSE 排空的最长时间；收到 SIGTERM/Ctrl-C 后不再接受新连接 |

> 关闭监听示例：`admin_listen = "off"`、`http_listen = false`。若只想换端口，直接
> 写其他回环地址/端口（例如 `admin_listen = "127.0.0.1:19090"`）。
>
> `[tls].cert` / `[tls].key` 只在启用任一 TLS 监听（`tunnel_listen` 或
> `ingress_listen`）时必填：纯明文拓扑（仅 `http_listen` + `[[static_routes]]`，
> 前面挂 nginx/Caddy 终止 TLS）不需要证书。
>
> 监听地址之间不得冲突（校验阶段拒绝）：
> - `admin_listen` 不能与 `tunnel_listen` / `ingress_listen` / `http_listen` 相同；
> - `http_listen` 不能与 `tunnel_listen` 相同。
>
> 启用 mTLS（`[tls].client_ca`）时 `tunnel_listen` 与 `ingress_listen` 必须是不同端口。
> 至少要配置 `tunnel_listen` / `ingress_listen` / `http_listen` 中的一种监听；缺省时
> tunnel/ingress/http 均按默认值开启，`admin_listen` 也默认开启。

### 2.2 `[tls]`

| 字段 | 类型 | 默认值 | 必填性 | 说明 |
| --- | --- | --- | --- | --- |
| `cert` | string(路径) | `"certs/server.crt"` | 有 TLS 监听时必填（默认路径） | 服务端证书链 PEM；SAN 需覆盖网关域名与所有 `hosts` 路由域名 |
| `key` | string(路径) | `"certs/server.key"` | 有 TLS 监听时必填（默认路径） | 服务端私钥 PEM，权限应为 `0600`/`0640` |
| `client_ca` | string(路径) | 无（不启用） | 可选 | mTLS 客户端 CA；配置后 agent 必须在 TLS 握手中出示由该 CA 签发的客户端证书（agent 侧用 `[tls].client_cert` / `client_key`）。启用时 `tunnel_listen` 与 `ingress_listen` 必须拆分端口。**删除该行表示关闭** |
| `min_version` | string | `"1.3"` | 可选 | 最低 TLS 版本，取值 `"1.2"` 或 `"1.3"`；默认 1.3。该值作用于 TLS 监听（公网入口/旧客户端兼容）；agent 隧道客户端固定使用 TLS 1.3，因此隧道始终为 TLS 1.3 |

> 启用 mTLS 的完整步骤：
> 1. `divertlink gen-certs --dir certs --domain <域名> --client`（或 `init --mtls`）生成
>    `client.crt` / `client.key`；
> 2. `server.toml` 中取消注释 `[tls].client_ca = "certs/ca.crt"`，并把 `tunnel_listen`
>    与 `ingress_listen` 改成不同端口（公网入口的普通客户端不会出示客户端证书）；
> 3. `agent.toml` 的 `[tls]` 同时设置 `client_cert` / `client_key`（两者必须同时出现，
>    缺少任意一个都会在配置校验时报错）。
>
> 注意 `init --mtls` 只生成证书并把 agent 侧路径写入 `agent.toml`，**不会**自动修改
> `server.toml` 的 `client_ca` 或拆分端口；用 `--dir` 指定绝对目录时，写入 agent 的
> 客户端证书路径也是绝对路径，拷贝到家庭端后需按实际目录调整。

### 2.3 `[limits]`

| 字段 | 类型 | 默认值 | 必填性 | 说明 |
| --- | --- | --- | --- | --- |
| `max_request_bytes` | string(字节数) | `"64MiB"` | 可选 | 单请求体上限。请求声明了超过上限的 `Content-Length` 时立即返回 HTTP 413；未声明长度的分块上传在读取超限时中断请求 |
| `request_timeout` | string(时长) | `"10m"` | 可选 | 网关侧单请求超时（等待上游响应头），超时返回 504；可被 `[[tunnels]].request_timeout` 覆盖 |
| `stream_idle_timeout` | string(时长) | `"120s"` | 可选 | **响应体**空闲超时：流式响应连续该时长没有任何数据帧即中断。注意 agent 对家庭端 target 响应体另有固定 300s 空闲上限（当前不可配置），因此该值设得比 300s 更大不会延长实际可空闲时间 |
| `header_read_timeout` | string(时长) | `"10s"` | 可选 | 读取客户端请求头的超时，已接入入口/管理/agent 数据流的 HTTP 读头超时；防止慢速头攻击（Slowloris） |
| `max_streams_per_tunnel` | integer | `256` | 可选 | 每条隧道默认的并发流上限；`[[tunnels]].max_streams` 未设置时继承该值 |
| `max_connections` | integer | `1024` | 可选 | **每个监听器**各自的并发连接上限（TLS 隧道/入口、明文入口、管理端点分别计数），不是全进程总数 |
| `allowed_methods` | array[string] | `["GET","POST","PUT","PATCH","DELETE","HEAD","OPTIONS"]` | 可选 | 入口允许的 HTTP 方法；不在列表中的方法（如 `TRACE`/`CONNECT`）直接返回 405，并带 `Allow` 头。严格场景可收窄到 `["GET","POST"]` |
| `global_rate_limit` | string(速率) | 无（不限） | 可选 | **网关全局**令牌桶限流（如 `"3000/min"`），在认证之前执行，用于整体过载保护；超限返回 429 + `Retry-After`。与 `[[api_keys]].rate_limit` 独立叠加 |

### 2.4 `[log]`

| 字段 | 类型 | 默认值 | 必填性 | 说明 |
| --- | --- | --- | --- | --- |
| `level` | string | `"info"` | 可选 | `tracing` 过滤指令：可写 `trace`/`debug`/`info`/`warn`/`error`，也可写 `divertlink=debug,tower=warn` 这类定向过滤 |
| `format` | string | `"text"` | 可选 | `text` 便于人读；`json` 便于日志采集 |

### 2.5 `[[api_keys]]`

公网入口 API Key。客户端（含 new-api/one-api 渠道）访问网关时携带
`Authorization: Bearer <key>` 或 `x-api-key: <key>`。可配置多个，实现按调用方隔离；
**配置了 `[[tunnels]]` 时至少需要一个 API Key**（公网入口不允许匿名）。

| 字段 | 类型 | 默认值 | 必填性 | 说明 |
| --- | --- | --- | --- | --- |
| `name` | string | 无 | **必填** | Key 名称，用于日志与 `/status` 展示（`debug` 级别请求日志会带上该名称）；同一配置内必须唯一 |
| `key` | string | 无 | **必填** | Key 明文，同一配置内必须唯一且不能含空白字符；建议 `divertlink token --prefix dlk_` 生成 |
| `allow_tunnels` | array[string] | `[]` | 可选 | 该 Key 允许访问的隧道 id 列表；空数组表示全部。引用的隧道 id 必须存在 |
| `allow_ips` | array[string] | `[]` | 可选 | 允许来源 CIDR；空数组表示不限制。经前置代理时配合 `[server].trust_proxy` 才有真实客户端 IP |
| `rate_limit` | string | 无（不限） | 可选 | 令牌桶限流，如 `"600/min"`、`"30/s"`；**不配置该字段表示不限**；写成空字符串会被配置校验拒绝 |
| `max_concurrent` | integer | `0` | 可选 | 该 Key 的并发请求上限；`0` 表示不限 |

> 最小权限建议：一个调用方一个 Key；为 new-api 渠道单独建 Key 并设置
> `allow_tunnels`、`rate_limit`、`max_concurrent`；公网 Key 不要提交到代码仓库。

### 2.6 `[[tunnels]]`

静态注册的隧道，与家庭端 `agent.toml` 一一对应：`id` / `token` 必须双端一致。
每条隧道同一时间只有一个活动会话，新会话会热替换旧会话（用于断线重连）。

| 字段 | 类型 | 默认值 | 必填性 | 说明 |
| --- | --- | --- | --- | --- |
| `id` | string | 无 | **必填** | 隧道 id；同一配置内唯一，不能含空白、`/`、`:`；必须与 agent 的 `[agent].id` 一致 |
| `token` | string | 无 | **必填** | agent 认证令牌，与 agent 的 `[agent].token` 一致；泄露等同隧道被接管 |
| `mount` | string | 无（不使用前缀路由） | 可选 | 公网路径前缀，如 `"/home"`；必须以 `/` 开头，末尾 `/` 会被规范化去掉，不能含 `..` 或空白；命中后转发前剥离；同一 mount 不能同时用于隧道和静态路由 |
| `hosts` | array[string] | `[]` | 可选 | Host 路由列表，如 `["home.llm.example.com"]`；按小写、去端口后匹配，需 DNS 指向网关且证书 SAN 覆盖。**非空时该隧道只接受这些 Host**；命中后路径原样转发（若同时配置 `mount` 则剥离 mount） |
| `default` | boolean | `false` | 可选 | 无匹配请求的兜底隧道，全配置最多一个 `true`；**不能与 `mount` 同时设置** |
| `protocols` | array[string] | `[]`（表示全部） | 可选 | 该隧道声明的协议能力；可用 `openai`、`anthropic`、`vllm`、`custom`（空表示全部），仅用于握手/展示，转发本身协议透明 |
| `upstream_api_key` | string | 无（透传） | 可选 | 发往家庭端时使用的固定认证 Key；配合 `upstream_auth_header = "authorization"` / `"x-api-key"` 生效。留空且 `upstream_auth_header = "passthrough"` 时透传客户端认证头；留空且选择替换模式时不会发送对应认证头 |
| `upstream_auth_header` | string | `"passthrough"` | 可选 | 上游认证头处理方式：`passthrough`（透传客户端 `Authorization` / `x-api-key`）/ `authorization`（用 `upstream_api_key` 作为 `Authorization: Bearer`）/ `x-api-key`（作为 `x-api-key`）/ `none`（不发送认证头）。选择 `authorization` / `x-api-key` 时 `upstream_api_key` 应有值 |
| `public_paths` | array[string] | `[]` | 可选 | 免认证路径前缀，**相对于该隧道的 mount**（如 `["/health"]` 对应公网 `/home/health`）；必须以 `/` 开头。只应放健康检查类只读路径 |
| `max_streams` | integer | 继承 `[limits].max_streams_per_tunnel` | 可选 | 该隧道并发数据流上限，必须 > 0；有效并发还受 agent `max_inflight` 约束 |
| `request_timeout` | string(时长) | 继承 `[limits].request_timeout` | 可选 | 该隧道单请求超时；需覆盖最长生成耗时 |
| `enabled` | boolean | `true` | 可选 | 为 `false` 时保留配置但该隧道不接受流量 |

一条隧道必须至少配置 `mount`、`hosts`、`default = true` 之一，否则校验会报
“unreachable”。实际路由优先级如下（`src/registry.rs` 的 `resolve`）：

1. **Host 匹配优先**：请求 `Host`（去掉端口、转小写）命中某隧道的 `hosts` → 转发到该隧道。
   若该隧道同时配置了 `mount`，路径还需命中该 mount 并剥离前缀；未配置 `mount` 时路径原样保留。
2. **最长 mount 前缀**：在隧道与 `[[static_routes]]` 之间按最长前缀匹配；`mount` 按路径段
   匹配（`/home` 命中 `/home/v1`，不会误匹配 `/homepage`），命中后剥离该前缀。
3. **default 兜底**：以上都未命中时交给 `default = true` 的隧道，路径原样保留。
4. 都没有命中则返回 404。

注意两点：

- **`hosts` 是限制性的**：某隧道一旦配置了非空 `hosts`，只有这些 Host 的请求才能路由到
  该隧道（即使路径命中其 `mount` 也不行）；因此不要给 `default` 隧道配置 `hosts`。
- 配置校验已保证 `mount`（含静态路由）唯一，但嵌套前缀（如 `/home` 与 `/home/v1`）可能
  存在，按“最长前缀优先”处理；为避免歧义，建议不要配置相互嵌套的 mount。

### 2.7 `[[static_routes]]`

把固定前缀直接反向代理到本机（或内网可达）的 HTTP(S) 上游，例如 new-api；不经过
家庭端 agent。

| 字段 | 类型 | 默认值 | 必填性 | 说明 |
| --- | --- | --- | --- | --- |
| `mount` | string | 无 | **必填** | 公网路径前缀，如 `"/newapi"`；规则与隧道 mount 相同，且全局唯一 |
| `upstream` | string(URL) | 无 | **必填** | 上游基础地址，必须是带 scheme/host 的绝对 URL，只支持 `http://` 或 `https://`，如 `"http://127.0.0.1:3000"` |
| `strip_prefix` | boolean | `true` | 可选 | 为 `true` 时转发前剥离 `mount` 前缀；为 `false` 时保留原始路径 |
| `require_auth` | boolean | `true` | 可选 | 是否要求网关入口 API Key。默认 `true`（与隧道一致，做 Key 校验 + 限流 + 并发限制）；仅当上游（如 new-api）自己完成鉴权时才可设为 `false` |
| `tls_ca` | string(路径) | 无（系统根证书） | 可选 | 上游为 HTTPS 且使用私有 CA 时的额外 CA |
| `headers` | map[string]string | `{}` | 可选 | 附加到上游请求的静态头，如 `{ "X-DivertLink-Route" = "newapi" }` |

> **鉴权边界（重要）**：`require_auth = true`（默认）时，静态路由与隧道一样要求网关
> API Key（`Authorization: Bearer` 或 `x-api-key`），并执行 Key 的 `allow_ips`、
> `rate_limit`、`max_concurrent` 检查（`allow_tunnels` 对静态路由不适用）。请求头仍会
> 清洗后透传给上游，因此客户端可以同时携带上游自己的令牌。
>
> 若上游（如 new-api）已经完成鉴权、且希望客户端只带上游令牌，可设置
> `require_auth = false` 让网关只做转发（此时只有请求体上限、超时、逐跳头清洗与请求
> ID 注入生效）——这时 `upstream` 只应指向 `127.0.0.1` / 内网地址，避免网关变成
> 无需认证的开放代理。

## 3. agent.toml（家庭端）

### 3.1 `[agent]`

| 字段 | 类型 | 默认值 | 必填性 | 说明 |
| --- | --- | --- | --- | --- |
| `id` | string | 无 | **必填** | 隧道 id，对应网关 `[[tunnels]].id`；也可用 `DIVERTLINK_ID` 覆盖 |
| `server` | string | 无 | **必填** | 网关地址：`host:port`，也接受 `https://host:port` / `tls://host:port`；`http://` 与 `ws://` 会被拒绝（隧道必须 TLS）；可用 `DIVERTLINK_SERVER` 覆盖 |
| `token` | string | 无 | **必填** | 隧道认证令牌；可用 `DIVERTLINK_TOKEN` 覆盖 |
| `heartbeat` | string(时长) | `"5s"` | 可选 | 应用层心跳间隔（控制流 ping/pong），必须 > 0 |
| `heartbeat_timeout` | string(时长) | `"15s"` | 可选 | 心跳超时；**必须严格大于 `heartbeat`**，建议 ≥ 3 倍 |
| `heartbeat_mode` | string | `"app"` | 可选 | `app`（控制流 ping/pong）/ `transport`（TCP keepalive）/ `both` / `none` |
| `connect_timeout` | string(时长) | `"10s"` | 可选 | TCP 连接 + TLS 握手超时 |
| `max_inflight` | integer | `128` | 可选 | agent 同时处理的上游请求上限，必须 > 0；与网关 `max_streams` 共同决定有效并发 |
| `reconnect_min` | string(时长) | `"1s"` | 可选 | 指数退避重连的最小间隔 |
| `reconnect_max` | string(时长) | `"60s"` | 可选 | 指数退避重连的最大间隔；必须 ≥ `reconnect_min` |
| `reconnect_jitter` | float | `0.3` | 可选 | 重连抖动比例，取值 `0.0`–`1.0`，避免多 agent 同时重连造成惊群 |
| `protocols` | array[string] | `["openai","anthropic","vllm"]` | 可选 | 上报给网关的协议能力（写入控制流 hello）；可用 `openai`/`anthropic`/`vllm`/`custom` |

### 3.2 `[tls]`

| 字段 | 类型 | 默认值 | 必填性 | 说明 |
| --- | --- | --- | --- | --- |
| `ca` | string(路径) | 无（系统根证书） | 可选 | 校验网关证书的 CA 文件 PEM；自签部署必须配置（如 `"certs/ca.crt"`）；使用公网 CA 时**删除该行**（写空字符串会在加载 TLS 时读取失败） |
| `pin` | string | 无（不启用） | 可选 | SPKI SHA-256 pin（base64 或 hex，也接受 `sha256/...` 前缀），`gen-certs` 会打印；启用后即使 CA 被误签发也可发现中间人。**不要写空字符串**（会被校验拒绝）；证书轮换时需同步更新 |
| `servername` | string | 取 `server` 的 host | 可选 | TLS SNI/校验名，需与证书 SAN 一致 |
| `client_cert` | string(路径) | 无（不启用） | 可选 | mTLS 客户端证书 PEM；**仅当网关设置了 `[tls].client_ca` 时需要**，必须与 `client_key` 同时配置。可用 `gen-certs --client` / `init --mtls` 生成 |
| `client_key` | string(路径) | 无（不启用） | 可选 | mTLS 客户端私钥 PEM；必须与 `client_cert` 同时配置，单独配置任意一个都会在配置校验时报错 |
| `insecure` | boolean | `false` | 可选 | 跳过证书校验，**仅限调试**；生产环境严禁开启 |

### 3.3 `[target]`

| 字段 | 类型 | 默认值 | 必填性 | 说明 |
| --- | --- | --- | --- | --- |
| `url` | string(URL) | `"http://127.0.0.1:8000"` | 可选（建议显式配置） | 家庭端服务基础地址，必须是 `http://` 或 `https://` 绝对 URL；vLLM `:8000`、Ollama `:11434`、new-api `:3000` |
| `request_timeout` | string(时长) | `"10m"` | 可选 | 单个上游请求等待响应头的超时；长输出场景应不小于网关侧超时 |
| `connect_timeout` | string(时长) | `"5s"` | 可选 | 连接上游服务的超时，用于快速发现本地服务未启动 |
| `tls_ca` | string(路径) | 无（系统根证书） | 可选 | `url` 为 `https` 时校验上游证书的额外 CA；上游用公网证书时删除该行 |
| `headers` | map[string]string | `{}` | 可选 | 附加到每个上游请求的静态头（如链路追踪头），不要用于存放认证令牌 |

> 响应体流式转发阶段，agent 还有一个**固定 300s** 的 target 响应空闲上限
> （`TARGET_STREAM_IDLE`，当前没有配置字段）。也就是说家庭端服务若超过 300s 没有任何
> 响应数据，数据流仍会被 agent 主动中断；降低该值需要改代码，网关的
> `stream_idle_timeout` 只能收紧、不能放宽这一上限。

### 3.4 `[log]`

| 字段 | 类型 | 默认值 | 必填性 | 说明 |
| --- | --- | --- | --- | --- |
| `level` | string | `"info"` | 可选 | `tracing` 过滤指令（同网关） |
| `format` | string | `"text"` | 可选 | `text` 或 `json` |

## 4. 环境变量

| 变量 | 适用端 | 覆盖的配置项 | 说明 |
| --- | --- | --- | --- |
| `DIVERTLINK_CONFIG` | 双端 | `server` / `agent` 子命令的 `-c` 默认值 | 等价于 `-c <path>`；命令行参数优先。`check` 与 `init` 子命令不读取该变量 |
| `DIVERTLINK_SERVER` | agent | `[agent].server` | 便于容器/临时环境覆盖网关地址；仅 `agent` 子命令（及 `agent --check`）加载时生效 |
| `DIVERTLINK_TOKEN` | agent | `[agent].token` | 便于用密钥管理系统注入，避免令牌写入文件 |
| `DIVERTLINK_ID` | agent | `[agent].id` | 多实例场景下快速切换隧道 id |

> 环境变量只在 agent 进程加载配置（`AgentConfig::load`）时应用；`divertlink check -c`
> 使用纯 TOML 解析（不解析相对路径、不应用环境变量），因此用 env 注入的令牌不会参与
> `check` 校验，也不会出现在其报错中。

## 5. 校验规则

`divertlink check -c <file>` 在通过校验后还会输出**安全/健壮性告警**（弱令牌、占位符密钥、
TLS 1.2、管理端点监听非回环、`require_auth = false` 的静态路由指向非内网地址、
证书即将过期等），告警不影响退出码；加 `--strict` 时告警会视为错误（退出非零），适合
CI/上线门禁。`divertlink server|agent -c <file> --check` 会执行以下
跨字段校验（失败时输出具体字段并退出非零）。`check` 按内容启发式判断类型：文件含
`[agent]` 且不含 `[server]` 时按 agent 配置校验，否则按 server 配置校验。

**server.toml**

- `name` 非空；至少配置一种监听（`tunnel_listen`/`ingress_listen`/`http_listen`）。
- 监听地址冲突检查：`admin_listen` 不得与 tunnel/ingress/http 相同；`http_listen`
  不得与 `tunnel_listen` 相同。
- 启用 `tunnel_listen` 时 `[tls].cert` / `[tls].key` 必须非空（路径是否可读要等启动
  加载 TLS 时才校验）。
- 配置 `[tls].client_ca`（mTLS）时，`tunnel_listen` 必须存在且与 `ingress_listen` 不同。
- `max_request_bytes`、`max_streams_per_tunnel`、各超时不得为 0。
- API Key：`name`/`key` 必填、名称与密钥各自唯一、密钥不含空白；`rate_limit` 语法
  合法（不能是空字符串）；`allow_ips` 均为合法 CIDR；`allow_tunnels` 引用的隧道必须存在。
- 隧道：`id`/`token` 必填；`id` 唯一且不含空白/`/`/`:`；`mount` 规范化后唯一
  （与静态路由共享命名空间）；`mount` 与 `default = true` 不能同时设置；
  `default = true` 最多一个；每条隧道至少 `mount`/`hosts`/`default` 之一；
  `hosts` 小写去端口后唯一；`protocols` 值合法；`public_paths` 以 `/` 开头；
  `max_streams` > 0。
- 配置了隧道时至少一个 `[[api_keys]]`（隧道入口不允许匿名）。
- 至少配置一条隧道或静态路由；启用 `tunnel_listen` 但没有任何隧道会报错。
- 静态路由：`mount` 唯一且与隧道 mount 不冲突；`upstream` 为 `http(s)://` 绝对 URL。

**agent.toml**

- `id`、`token`、`server` 非空；`server` 必须是 `host:port` / `tls://` / `https://`
  （缺端口、端口为 0、`http://`、`ws://` 均报错）。
- `heartbeat` > 0，`heartbeat_timeout` > `heartbeat`。
- `max_inflight` > 0；`reconnect_max` ≥ `reconnect_min`；`reconnect_jitter` 在 `0.0..=1.0`。
- `protocols` 值合法；`target.url` 为 `http/https` 绝对 URL；`[target]` 超时 > 0。
- `[tls].pin` 不能是空字符串（不启用就删除该行）。
- `[tls].client_cert` 与 `[tls].client_key` 必须同时设置或同时不设置。

## 6. 最小配置

以下为能跑通「单隧道 + 单 API Key」的最小集合；未列出的字段取默认值。
`init` 或 `gen-certs` 生成证书后，把两份配置放在同一目录、证书目录 `certs/` 也在
该目录下即可直接运行（路径相对配置文件所在目录解析）。

**server.toml**

```toml
[server]
tunnel_listen = "0.0.0.0:443"
ingress_listen = "0.0.0.0:443"

[tls]
cert = "certs/server.crt"
key = "certs/server.key"

[[api_keys]]
name = "default"
key = "dlk_REPLACE_WITH_RANDOM_KEY"

[[tunnels]]
id = "home"
token = "agt_REPLACE_WITH_RANDOM_TOKEN"
mount = "/home"
```

**agent.toml**

```toml
[agent]
id = "home"
server = "gateway.example.com:443"
token = "agt_REPLACE_WITH_RANDOM_TOKEN"

[tls]
ca = "certs/ca.crt"

[target]
url = "http://127.0.0.1:8000"
```

> 最小配置不含 `[limits]`、`[log]`、`[[static_routes]]` 与第二条隧道，全部取默认值；
> 未列出的监听（`http_listen` / `admin_listen`）也会按默认地址开启，无法通过配置关闭。
> 生产环境请至少设置 `rate_limit` 与 `allow_tunnels`，并补齐
> [上线检查清单](security.md#9-上线检查清单)。

## 7. 完整示例

仓库根目录的 [`examples/server.toml`](../examples/server.toml) 与
[`examples/agent.toml`](../examples/agent.toml) 是可运行的双端完整示例，包含：

- 单端口 443 ALPN 复用；
- `home` 隧道（mount `/home`，可选 hosts 路由）；
- `newapi-local` 隧道（固定上游 Key 替换认证头）；
- `/newapi` 静态路由反代到本机 new-api（默认经过网关 API Key 鉴权；示例用 `require_auth = false` 交给 new-api 自校验）；
- 两个不同权限的 API Key；
- agent 心跳、重连、SPKI pin、mTLS 客户端证书（注释）与 target 配置。

示例中的令牌、域名、路径均为占位值，使用前替换；示例配置与证书的目录布局见
[examples/README.md](../examples/README.md)。

## 8. 配置校验与生效方式

```bash
# 通用配置校验（server 与 agent 均可）
divertlink check -c server.toml
divertlink check -c agent.toml

# 网关启动前自检；只校验不监听
divertlink server -c server.toml --check
```

- 配置在进程**启动时**加载，修改后需重启对应进程生效；当前版本不提供 SIGHUP/热加载，
  请通过 systemd `restart` 或滚动重启完成变更。
- 变更令牌/证书后，先重启网关，再重启 agent；网关会以会话热替换方式接管 agent 重连。
- 相对证书路径以配置文件所在目录为基准，因此从任意工作目录执行
  `divertlink server -c /etc/divertlink/server.toml` 都能正确定位
  `/etc/divertlink/certs/...`。
