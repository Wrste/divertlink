# 故障排查

**中文** · [English](en/troubleshooting.md) · [文档目录](README.md)

本文按**症状**组织：先做通用检查，再按 HTTP 状态码、流式、心跳、TLS、ALPN、new-api
与家庭网络逐项排查。文中的日志关键词为定位线索，具体措辞以你的版本实际日志为准，
可先用 `grep -Ei` 粗筛再放大。

- [1. 通用排查流程](#1-通用排查流程)
- [2. 按 HTTP 状态码排查](#2-按-http-状态码排查)
- [3. SSE 流式被缓冲](#3-sse-流式被缓冲)
- [4. 心跳超时与反复重连](#4-心跳超时与反复重连)
- [5. TLS 证书问题](#5-tls-证书问题)
- [6. ALPN 未命中](#6-alpn-未命中)
- [7. new-api / one-api 连接失败](#7-new-api--one-api-连接失败)
- [8. 家庭端网络与 NAT](#8-家庭端网络与-nat)
- [9. 日志关键词与收集信息](#9-日志关键词与收集信息)
- [10. 配置错误速查](#10-配置错误速查)

## 1. 通用排查流程

按“从近到远”的顺序定位，每步都有可执行命令：

```bash
# ① 配置是否正确（语法、必填项、证书路径）
divertlink check -c server.toml
divertlink check -c agent.toml

# ② 网关进程与监听端口（网关本机）
curl -fsS http://127.0.0.1:9090/healthz
curl -fsS http://127.0.0.1:9090/readyz
curl -sS  http://127.0.0.1:9090/status
sudo ss -ltnp 'sport = :443'

# ③ agent 是否连上、隧道是否在线（网关本机 /status 中查隧道状态）
curl -sS http://127.0.0.1:9090/status | head -c 2000

# ④ 入口是否可达、证书与 ALPN 是否正常（客户端执行）
curl -v https://gateway.example.com/home/health
openssl s_client -connect gateway.example.com:443 -servername gateway.example.com \
  -alpn divertlink/1 </dev/null 2>/dev/null | grep -E 'ALPN|Verify return code|verify error'

# ⑤ 家庭端 target 是否正常（家庭端执行）
curl -v http://127.0.0.1:8000/v1/models

# ⑥ 看日志（网关与家庭端）
#    systemd: journalctl -u divertlink-server -f
#    前台运行: 直接观察输出
```

> **日志级别提醒**：网关默认 `info` 只记录启动/监听、agent 连上/断开、会话替换、心跳
> 超时等生命周期事件；401/403/429 等拒绝**不写日志**，只累加 `/metrics` 计数。逐请求
> 转发详情（`request_id`、隧道、状态码、耗时、Key 名称）需要把 `[log].level` 调到
> `debug`（`tunnel request forwarded` / `static route forwarded`）。客户端 IP、方法与
> 完整路径不在网关日志中，需要时看前置代理或上游日志。

常见结论：

| 检查结果 | 说明 |
| --- | --- |
| `/readyz` 返回 503 | 已配置隧道但当前没有任何隧道连接；仅静态路由的网关始终 ready（返回 JSON 中的 `connected`/`tunnels` 可看明细） |
| `/status` 中隧道离线 | agent 未连上或 token 不匹配，见第 4/8 节 |
| `openssl` 握手失败 | 证书、SNI、ALPN 或防火墙问题，见第 5/6 节 |
| target curl 失败 | 家庭端服务本身问题，见第 8 节 |
| 以上都正常但业务失败 | 看第 2 节的状态码与上游响应 |

## 2. 按 HTTP 状态码排查

### 2.1 401 Unauthorized

**含义**：网关入口认证失败（Key 缺失、错误或头格式不对）。

| 可能原因 | 排查 | 修复 |
| --- | --- | --- |
| 未携带认证头 | `curl -v` 看请求头 | 加上 `Authorization: Bearer <key>` 或 `x-api-key: <key>` |
| 头名用错 | OpenAI 风格用 `Authorization: Bearer`；Anthropic 风格用 `x-api-key` | 两种网关都接受，按客户端类型选 |
| Key 复制错误/含空格换行 | 对比 `server.toml` 的 `[[api_keys]].key` | 重新复制；避免 shell 变量尾部空格 |
| 用错了凭据 | agent token（`agt_...`）不是入口 API Key（`dlk_...`） | 客户端用 `[[api_keys]].key` |
| new-api 渠道密钥填成 new-api 令牌 | 渠道密钥必须是网关 API Key（除非另配了放行方式） | 改成网关 Key，见 [协议 §5](protocols.md#5-与-new-api--one-api-集成) |
| 前置代理吞掉认证头 | 在网关日志/抓包确认是否收到 `Authorization`/`x-api-key` | 在代理中显式转发认证头（默认通常已转发） |

日志/指标：网关默认不逐请求写日志；401 只体现在 `/metrics` 的
`divertlink_auth_failures_total`，排障用 `curl -v` 对比实际请求头，或临时把
`[log].level = "debug"` 观察鉴权前后的转发日志。

### 2.2 403 Forbidden

**含义**：Key 有效，但不允许访问该资源（隧道授权或来源 IP 限制）。

| 可能原因 | 排查 | 修复 |
| --- | --- | --- |
| `allow_tunnels` 未包含目标隧道 id | 看该 Key 的 `allow_tunnels` 与 URL 的 mount 对应隧道 | 加入隧道 id，或换用有权限的 Key |
| 来源 IP 不在 `allow_ips` | 看网关日志客户端 IP；与 `allow_ips` CIDR 对比 | 补 CIDR；单个 IP 写 `/32` |
| 经过 nginx 等前置代理 | 默认 `trust_proxy = false` 时网关看到的来源 IP 是代理地址（如 `127.0.0.1`） | 仅当代理可信且在本机/内网时设置 `[server].trust_proxy = true`；否则先用直连 443 验证，或取消 `allow_ips` |
| 用 Anthropic 渠道访问未授权的隧道 | 渠道 Key 的 `allow_tunnels` 限制 | 调整授权或渠道 |

日志/指标：同 401，403 不写日志，只在 `/metrics` 的 `divertlink_auth_failures_total`
计数；确认来源 IP 可用 `curl -v` 配合网关前代理日志，或临时开启 `debug` 日志。

### 2.3 404 Not Found

**含义**：路由未命中。网关找不到匹配的 mount/host/default 隧道，或静态路由未命中；
也可能是家庭端上游自己返回 404。

| 可能原因 | 排查 | 修复 |
| --- | --- | --- |
| URL 少了 mount 前缀 | 例如漏写 `/home`：`https://gateway.example.com/v1/...` | 改为 `/home/v1/...`，或给隧道设 `default = true` |
| mount 不匹配 | 对比 URL 前缀与 `[[tunnels]].mount`；注意结尾斜杠差异 | 统一写法（建议 mount 不带结尾 `/`，URL 中 `/home/v1`） |
| Host 路由未命中 | 用 `curl -H 'Host: home.llm.example.com'` 测试；确认 `hosts` 配置与 DNS | 补 `hosts`、DNS 与证书 SAN |
| 静态路由未命中 | 对比 `[[static_routes]].mount` 与 URL | 修正 mount 或 `strip_prefix` |
| 上游返回 404（如模型不存在） | `curl -v` 看响应体是否来自家庭端服务；在 target 本机复现同一路径 | 检查模型名、路径、new-api 路由 |
| 隧道 `enabled = false` | `/status` 或配置 | 改回 `true` |

日志关键词：`no tunnel or static route` 错误体（`no_route`）；网关不记录 404 请求日志。
若是上游返回的 404，agent 的 `debug` 日志 `local target responded` 会带状态码与路径。

### 2.4 413 Payload Too Large

**含义**：请求体超过 `[limits].max_request_bytes`（默认 64 MiB）。声明了超限的
`Content-Length` 时网关立即返回 413；未声明长度（分块上传）的请求会在读取超过上限时
中断，客户端可能看到连接中断而不是 413 响应体。

| 排查 | 修复 |
| --- | --- |
| 看 `Content-Length` 或实际 body 大小 | 调大 `max_request_bytes`（如 `"128MiB"`），同时评估内存与上游能力 |
| 是否误传大文件/批量 embedding | 拆分请求或限制客户端上传 |

### 2.5 429 Too Many Requests

**含义**：命中 `[[api_keys]].rate_limit`（令牌桶，错误体 `rate_limited`，带
`Retry-After`）、`max_concurrent`（`too_many_concurrent_requests`，不带 `Retry-After`），
或单隧道并发流打满（`tunnel_busy`，带 `Retry-After: 1`）。

| 排查 | 修复 |
| --- | --- |
| 确认是哪个 Key、哪条限制 | 压测 Key 与生产 Key 共用；调大对应 `rate_limit` / `max_concurrent` |
| 多个客户端共用一个 Key | 按调用方拆 Key，分别配额 |
| 短时突发 | 提高窗口容量或增加重试/退避；确认上游是否真的过载 |
| 压测时误伤 | 用专用 `bench` Key 且不配置 `rate_limit`（`max_concurrent = 0`），压完删除 |

日志/指标：`divertlink_rate_limited_total` 与错误体 `code`/`x-divertlink-error` 头；
429 本身不写日志。

> 429 是网关主动保护，通常说明上游已接近容量；盲目调大限流会转为 504/超时。

### 2.6 502 Bad Gateway

**含义**：隧道本身在线，但数据流或家庭端 target 失败（错误体 code 为
`tunnel_stream_error` / `tunnel_request_failed`，或 agent 返回的 `target_unreachable` /
`bad_target_uri`）。**agent 未连接返回的是 503，不是 502**。

| 可能原因 | 排查 | 修复 |
| --- | --- | --- |
| 家庭端 target 未启动 | 家庭端 `curl http://127.0.0.1:8000/v1/models` | 启动 vLLM/new-api；确认端口 |
| target 地址写错 | 检查 `[target].url`；尤其 `localhost` 解析到 `::1` 而服务只听 `127.0.0.1` | 统一写 `http://127.0.0.1:8000` |
| target 是 https 且证书校验失败 | agent 日志 TLS/certificate 错误 | 配置 `[target].tls_ca` 或改用内网 http |
| target 拒绝连接/提前断开 | agent 日志 `local target request failed` | 检查上游进程、并发与超时 |
| 隧道数据流建立失败 | 网关日志 `cannot establish tunnel stream` / `stream_errors` 指标 | 看心跳/重连日志与 agent 负载，见第 4 节 |
| 隧道被新会话替换 | `/status` 会话 id 变化或日志有替换记录 | 确认没有第二个 agent 用同一 id；泄露 token 时立即轮换 |

日志/指标：`divertlink_stream_errors_total`；agent 侧 `debug` 日志
`local target request failed` / `local target responded`。

### 2.7 503 Service Unavailable

**含义**：请求已经路由到某条隧道，但该隧道当前没有可用会话（错误体
`tunnel_unavailable`），或隧道并发流打满（`tunnel_busy` 为 429，见 2.5）。

| 可能原因 | 排查 | 修复 |
| --- | --- | --- |
| agent 未启动 / 已退出 | 家庭端 `ps`/`systemctl status divertlink-agent`；网关 `/status` 中隧道 `connected=false` | 启动 agent；看其日志 |
| agent token 错误 | 网关日志出现 `invalid token for tunnel`；agent 日志认证失败 | 统一双端 token，重启两端 |
| id 与隧道不匹配 | 对比 `[agent].id` 与 `[[tunnels]].id` | 改为一致 |
| 隧道 `enabled = false` | 网关启动日志/`/status` 没有该隧道 | 改回 `true` 并重启网关 |
| agent 正在重连（网络抖动） | agent 日志 `reconnecting after backoff`；`/status` 频繁上下线 | 见第 4 节；确认链路与心跳配置 |
| 隧道未知（网关无此 id） | 网关日志 `unknown tunnel` | 在网关补 `[[tunnels]]` 或修正 agent id |

日志/指标：网关 `agent disconnected` / `invalid token` / `unknown tunnel`；
`divertlink_tunnel_unavailable_total`；`/readyz` 在无任何隧道连接时返回 503。

### 2.8 504 Gateway Timeout

**含义**：请求超时（`request_timeout`）或流空闲超时（`stream_idle_timeout`），
也可能是 agent 侧 target 超时（错误体 `target_timeout`）。

| 可能原因 | 排查 | 修复 |
| --- | --- | --- |
| 生成时间超过 `request_timeout`（默认 10m） | 看 `debug` 日志耗时与 `[[tunnels]].request_timeout` | 调大网关/隧道超时 |
| 上游长时间无输出超过 `stream_idle_timeout`（默认 120s） | 长思考模型、工具调用、排队 | 调大该值，或让上游增加 SSE 心跳 |
| agent 侧固定 300s 空闲上限 | 无输出超过约 5 分钟后断开，且调大 `stream_idle_timeout` 无效 | 让上游定期输出 SSE 心跳/进度，或等待该上限可配置 |
| 上游排队/GPU 过载 | 家庭端服务指标 | 降并发、扩容；调整 `max_streams`/`max_inflight` |
| 家庭端 `[target].request_timeout` 更短 | 对比两端超时配置 | 让家庭端超时 ≥ 网关超时 |
| 网络 RTT 高或丢包 | `mtr` 到网关/家庭端 | 链路优化；调整超时 |

日志/指标：网关 `debug` 转发日志的耗时；流空闲超时会有 `upstream response body idle timeout`
warn；504 响应本身不写日志。

## 3. SSE 流式被缓冲

**症状**：`stream=true` 却在最后一次性收到全部内容；或首 token 延迟等于总耗时。

| 层 | 可能原因 | 排查/修复 |
| --- | --- | --- |
| 客户端 | `curl` 未加 `-N`；SDK/管道缓冲 | 加 `-N`；Python 用 `-u`；见 [协议 §6](protocols.md#6-sse-流式验证) |
| 前置 nginx | `proxy_buffering on`（默认开启） | 关闭 `proxy_buffering`，设置 `proxy_read_timeout` 与 `X-Accel-Buffering: no` |
| Caddy/CDN | 中间层压缩或缓冲 | 对该路径关闭压缩/缓冲；必要时绕过 CDN |
| 前置代理超时 | 30s 左右集中断开 | 调大 `proxy_read_timeout` 等 |
| 网关超时 | 120s 无输出被 `stream_idle_timeout` 断开 | 调大 `stream_idle_timeout` |
| agent 固定上限 | 约 300s 无输出被 agent 的 target 空闲上限断开（不可配置） | 让上游定期输出 SSE 心跳/进度 |
| 上游本身 | 家庭端服务未按 SSE 发送（一次生成完才 flush） | 直接在家庭端 `curl -N` 验证；该问题在网关之外 |

验证顺序：**家庭端直连 → 经网关直连 443 → 加一层前置代理**，逐层对比 TTFB 与逐行时间戳，
即可定位缓冲发生在哪一层。

## 4. 心跳超时与反复重连

**症状**：agent 日志出现心跳超时/连接关闭；网关 `/status` 隧道频繁上下线。

| 可能原因 | 排查 | 修复 |
| --- | --- | --- |
| 家庭网络空闲回收/路由器 NAT 超时 | 家宽 NAT 通常允许长连接，但部分路由器会回收 | 保持 `heartbeat_mode = "app"`（默认），`heartbeat = "5s"` |
| 心跳超时过短 | `heartbeat_timeout` 小于链路抖动 | 设为 ≥ 3×`heartbeat`，如 `heartbeat=5s`、`heartbeat_timeout=15s` 或更宽 |
| 网关过载 | 网关 CPU/连接数打满 | 扩容、限制 `max_connections`、调低日志级别 |
| 中间设备拦截 | 公司网关/防火墙对长连接限时 | 更换出口网络；确认允许到网关 443 的长期出站 |
| 系统休眠/断网 | 笔记本合盖、Wi-Fi 切换 | 唤醒后 agent 自动重连；必要时用 `reconnect_min/max` 调整退避 |
| 时钟偏差 | 证书有效期校验失败（TLS 层） | 启用 NTP；`timedatectl` 检查 |
| token/id 错误 | 网关日志认证失败 | 统一双端配置并重启 |
| 第二个 agent 无法加入同一隧道 | `/status` 的 agents/connected、双方日志与 token | 确认 ID/token 相同且服务能力等价；多 agent 会话会进入池并轮询，泄露 token 时轮换全部 agent |

**重连调试**：家庭端用前台模式观察完整日志；`divertlink agent -c agent.toml --once`
只建立一次连接（不进入长期重连循环），适合快速判断握手与认证是否正常。

日志关键词：`heartbeat`、`ping`、`timeout`、`session closed`、`reconnect`、`backoff`、
`handshake`、`tls`。

## 5. TLS 证书问题

**典型错误**（客户端/agent 侧）：`InvalidCertificate`、`UnknownIssuer`、`NotValidForName`、
`CertificateExpired`、`certificate verify failed`。

| 可能原因 | 排查 | 修复 |
| --- | --- | --- |
| 证书过期 | `openssl x509 -in certs/server.crt -noout -dates` | 续期并重启网关 |
| 域名不在 SAN 中 | `openssl x509 -in certs/server.crt -noout -text \| grep -A1 'Subject Alternative Name'` | 重新签发（`gen-certs --domain` 或公网 CA），SAN 覆盖实际访问域名/`hosts` |
| agent 用错 CA / 读不到证书 | 检查 `[tls].ca` 路径：相对路径以**配置文件所在目录**为基准，确认 `certs/ca.crt` 与 `agent.toml` 的相对位置 | 调整目录布局或用绝对路径；确认运行用户对 CA/私钥有读权限 |
| 用了公网 CA 但 agent 仍配自签 CA | `[tls].ca` 指向了旧 CA | 公网 CA 部署时**删除** `agent.toml` 的 `[tls].ca` 行（使用系统根证书）；不要写空字符串 |
| 证书链不完整 | 公网 CA 证书只放了叶子证书 | 用 fullchain（含中间 CA）配置 `[tls].cert` |
| 私钥权限/属主不对 | 网关日志无法读取 key | `chown`/`chmod 600`，属主为运行用户 |
| 通过 IP 访问但证书是域名证书 | `curl https://<IP>` 报名称不匹配 | 用域名访问；或实验环境用 `--insecure`（禁止用于生产） |
| agent 时间不对 | 系统时间超出证书有效期 | 启用 NTP |

自建 CA 场景务必区分：**私钥 `ca.key` 只留在网关（或离线保存），家庭端只放 `ca.crt`**。

## 6. ALPN 未命中

**症状**：agent 连不上或握手异常；用 `openssl` 看不到 `divertlink/1`；网关日志出现
ALPN/协议协商相关错误；前面加了 nginx/LB 后突然全部失败。

**验证**：

```bash
# 隧道 ALPN
openssl s_client -connect gateway.example.com:443 -servername gateway.example.com \
  -alpn divertlink/1 </dev/null 2>/dev/null | grep -E 'ALPN|Verify return code'

# 入口 ALPN（浏览器/new-api 走 h2 或 http/1.1）
openssl s_client -connect gateway.example.com:443 -servername gateway.example.com \
  -alpn h2 </dev/null 2>/dev/null | grep -E 'ALPN|Verify return code'
```

期望分别看到 `ALPN protocol: divertlink/1` 与 `ALPN protocol: h2`（或 `http/1.1`）。

| 可能原因 | 排查 | 修复 |
| --- | --- | --- |
| agent 连到了错误的端口 | 独立监听模式下 `tunnel_listen` 与 `ingress_listen` 不同端口 | agent 的 `server` 指向 `tunnel_listen` 端口 |
| 中间层终止了 TLS | 前置 nginx/Caddy/LB 自己处理 TLS，不识别 `divertlink/1` | 改为 TLS 直通（nginx `stream` + `ssl_preread` 按 SNI/ALPN 转发到 `tunnel_listen`），或让 agent 直连 |
| 直接对隧道端口发 HTTP | 用浏览器访问 tunnel 端口 | 公网入口走 ingress 端口 |
| 服务器前面有云 LB | LB 默认只支持 HTTP/HTTPS，不支持自定义 ALPN | 用 TCP/四层转发或直连 |

日志关键词：`alpn`、`no application protocol`、`handshake`、`protocol version`。

## 7. new-api / one-api 连接失败

先确认属于哪种拓扑（见 [协议 §5](protocols.md#5-与-new-api--one-api-集成)），再按层排查：

1. **new-api 作为客户端接网关（拓扑 A）**
   - 渠道 Base URL 是否以 `/v1` 结尾（OpenAI 类型）或 mount 结尾（Anthropic 类型）；
   - 渠道密钥是否为网关 `[[api_keys]].key`；
   - new-api 服务器能否访问网关域名：`curl -v https://gateway.example.com/home/v1/models -H "Authorization: Bearer ..."`；
   - DNS/TLS/防火墙：new-api 所在机器的出口是否放行 443；
   - 渠道测试报“无可用渠道/模型不存在” → 检查渠道模型列表与家庭端实际模型名。
2. **网关静态路由反代 new-api（拓扑 B）**
   - `upstream` 是否写对（本机 new-api 监听 `127.0.0.1:3000`）；
   - 网关本机 `curl http://127.0.0.1:3000/api/status` 是否正常；
   - **鉴权**：静态路由默认 `require_auth = true`（走网关 API Key）；设为 `false` 时网关只透传请求头，客户端必须携带
     new-api 能识别的令牌（不是网关 `[[api_keys]].key`），且 new-api 侧开启令牌校验；
   - new-api 返回 401 时，在 new-api 日志里确认实际收到的 `Authorization` 头。
3. **agent 指向家庭端 new-api（拓扑 C）**
   - `[target].url = "http://127.0.0.1:3000"` 是否可 curl 通；
   - 是否配置了 `upstream_auth_header = "authorization"` 与 `upstream_api_key = "<new-api 令牌>"`；
   - new-api 内部渠道是否指向本地 vLLM 且可用；
   - 家庭端 new-api 日志能看到请求，但返回 401 → 上游 auth 替换配置错误。

new-api 常见报错与含义：

| 现象 | 可能含义 |
| --- | --- |
| 渠道测试 401 | 渠道密钥不是网关 API Key，或网关 Key 已删除/轮换 |
| 渠道测试 404 | Base URL 少了 `/v1` 或多了 `/v1`（Anthropic 类型），或网关 mount 不匹配 |
| “无可用渠道” | 渠道模型列表与实际请求模型不匹配 |
| 502/504 | agent 离线、家庭端服务不可达、超时太短 |
| 流式一次性返回 | 前置代理缓冲，或 new-api 到网关之间还有缓冲层 |

## 8. 家庭端网络与 NAT

| 问题 | 说明与处理 |
| --- | --- |
| 家庭没有公网 IP 能否用 | **可以**。agent 主动外连，CGNAT、动态 IP、双 NAT 均不影响；不需要端口映射 |
| 需要路由器设置吗 | 不需要入站映射；只需允许内网设备出站 TCP 443 到网关 |
| 网关地址解析失败 | 家庭端 `dig +short gateway.example.com`；必要时在 agent 中直接用公网 IP（证书 SAN 需覆盖访问名） |
| 出站 443 被拦截 | 家庭端 `nc -vz gateway.example.com 443` 或 `curl -v https://gateway.example.com`；公司网络可能限制长连接 |
| 必须经企业 HTTP 代理 | 当前版本不提供 HTTP 代理配置项；请让 agent 走可直连的网络出口（或在前置网络层做透明代理） |
| IPv6 误解析 | 网关默认只监听 IPv4（`0.0.0.0`）；若 DNS 同时返回 AAAA 而家庭端优先走 IPv6，可让 server 显式监听 IPv6 地址（如 `[::]:443`，注意与 `ingress_listen` 保持一致），或在家庭端优先使用 IPv4 |
| 大 body 上传慢/中断 | 家庭宽带上行有限；检查 `max_request_bytes`、`request_timeout`，并测上行带宽 |
| MTU/分片异常 | 大请求偶发卡死：尝试在网关/家庭端降低隧道 MTU 或换网络验证（家宽 PPPoE 常见） |
| 笔记本休眠/网络切换 | agent 会自动重连；长期运行建议用 mini PC/服务器并配置 systemd 自启 |

日志关键词：`connect timeout`、`dns`、`connection refused`、`network unreachable`、
`broken pipe`、`reset by peer`。

## 9. 日志关键词与收集信息

```bash
# 网关：只看错误与关键事件
journalctl -u divertlink-server --since '15 min ago' -o cat \
  | grep -Ei 'error|warn|tunnel|heartbeat|upstream|alpn|cert|invalid|unknown'

# 家庭端：连接、重连、target 相关
journalctl -u divertlink-agent --since '15 min ago' -o cat \
  | grep -Ei 'connect|reconnect|heartbeat|timeout|tls|certificate|handshake|token|target|error'
```

> 默认 `info` 级别下网关不会记录每个请求，也**不会记录 401/403/429 等拒绝**；需要逐
> 请求线索时把 `[log].level` 调到 `debug` 复现一次（含 `tunnel request forwarded` /
> `static route forwarded`），排障后恢复。状态码计数看 `/metrics`
> （如 `divertlink_auth_failures_total`、`divertlink_rate_limited_total`、
> `divertlink_tunnel_unavailable_total`）。

建议在提 issue/求助时附上：

1. `divertlink version`、操作系统与架构；
2. 脱敏后的 `server.toml` / `agent.toml`（去掉 token/key 与域名敏感信息）；
3. `divertlink check` 输出；
4. 网关与 agent 最近 5–10 分钟日志（`[log].format = "json"` 时直接贴 JSON 行）；
5. `curl -v` 完整输出（隐藏认证头）；
6. 拓扑描述：是否经过 nginx/CDN/云 LB，客户端类型（SDK/curl/new-api）；
7. 复现时间点与请求路径（保留 mount 前缀）。

## 10. 配置错误速查

| 症状 | 优先检查的配置 |
| --- | --- |
| agent 一直重连、隧道离线 | 双端 `id`/`token` 是否一致；`server` 地址与端口；TLS `ca` 路径 |
| 入口 401 | `[[api_keys]].key`；客户端认证头类型 |
| 入口 403 | `allow_tunnels`、`allow_ips` |
| 入口 404 | `[[tunnels]].mount` 与 URL 前缀；`hosts`；`default`；`enabled` |
| 413 | `[limits].max_request_bytes` |
| 429 | `[[api_keys]].rate_limit` / `max_concurrent`；单隧道并发打满也会 429（`tunnel_busy`） |
| 502 | 隧道在线但数据流/target 失败：`[target].url`、家庭端服务、上游证书 |
| 503 | 隧道无可用会话：agent 是否运行、双端 `id`/`token`、隧道 `enabled` |
| 504 | `request_timeout`、`stream_idle_timeout`、agent 固定 300s 空闲上限、家庭端超时 |
| 流式不流 | 前置代理缓冲；`curl -N`；`stream_idle_timeout` |
| 证书/握手错误 | `[tls].cert`/`key`、SAN、agent `[tls].ca`/`servername`/`pin`、mTLS 的 `client_cert`/`client_key`、`min_version` |
| 静态路由 401 | 默认 `require_auth = true`：客户端需携带网关 API Key；如果希望只由上游鉴权，设置 `require_auth = false`（见 §7） |
| 网关启动即退出 | `divertlink check -c server.toml`；端口占用；证书文件权限；mTLS 分端口要求；"unknown field" 拼写错误 |
| 证书路径找不到 | 相对路径以配置文件所在目录为基准；确认 `server.toml` 与 `certs/` 的相对布局，或改用绝对路径 |
| 家庭端找不到 target | `[target].url` 写 `127.0.0.1` 而非 `localhost`；服务端口；服务是否监听回环 |
