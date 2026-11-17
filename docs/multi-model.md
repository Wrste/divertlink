# 多模型与多对一部署

DivertLink 的核心是「多条加密隧道 → 一个固定 IP 的公网网关」。实际使用中最常见的需求是
**多对一**：把多个大模型（或多台家庭机器、或一台机器上的多个服务）统一收敛到一个公网
入口。本文给出三种推荐拓扑、多机冗余（HA）方案、鉴权隔离、观测与调优，并附可直接运行的
示例配置（[examples/multi-model](../examples/multi-model/README.md)）。

## 目录
- [拓扑总览](#拓扑总览)
- [方案 A：多个 agent → 一个网关（每个模型一个隧道）](#方案-a多个-agent--一个网关每个模型一个隧道)
- [方案 B：一个 agent → 多个本地模型（路径前缀路由）](#方案-b一个-agent--多个本地模型路径前缀路由)
- [方案 C：家庭端统一入口（new-api / LiteLLM）](#方案-c家庭端统一入口new-api--litellm)
- [多机冗余：多个 agent 服务同一条隧道](#多机冗余多个-agent-服务同一条隧道)
- [鉴权、配额与隔离](#鉴权配额与隔离)
- [客户端怎么填 base_url](#客户端怎么填-base_url)
- [观测与排障](#观测与排障)
- [容量与调优](#容量与调优)
- [验证步骤](#验证步骤)
- [常见问题](#常见问题)

## 拓扑总览

```text
方案 A（多个 agent，每模型一个隧道）
  家庭机器 A (vLLM :8000, Qwen) ──┐
  家庭机器 B (vLLM :8001, Llama) ─┼─ TLS 隧道 ──► 网关 :443 ──► /qwen/...  /llama/...  /mixed/...
  家庭机器 C (多服务)            ──┘

方案 B（一个 agent，多个本地上游，路径前缀路由）
  家庭机器 (agent) ──► :8000 默认上游
                    ├──► :8001  /qwen/*
                    └──► :8002  /llama/*

方案 C（家庭端统一入口）
  家庭机器 (new-api/LiteLLM) ──► 多个本地模型；agent 只代理这个入口，按模型名路由
```

选择建议：

| 场景 | 推荐方案 |
| --- | --- |
| 每个模型独占一台机器/一张 GPU，希望独立扩缩容 | A |
| 一台机器跑多个 vLLM/Ollama 实例，不想开多个 agent 进程 | B |
| 已经用 new-api / LiteLLM 管理模型、额度与路由 | C |
| 同一模型需要多机冗余/更高并发 | A/B + 多 agent 同隧道（HA） |

## 方案 A：多个 agent → 一个网关（每个模型一个隧道）

每个模型（或每台机器）跑一个 agent，网关为每个 agent 配置一条 `[[tunnels]]`，用不同的
`mount` 作为命名空间。

网关 `server.toml`（节选）：

```toml
[[tunnels]]
id = "qwen"
token = "agt_qwen_..."
mount = "/qwen"          # 公网路径前缀，转发前剥离
protocols = ["openai", "vllm"]
public_paths = ["/health"]
max_streams = 128

[[tunnels]]
id = "llama"
token = "agt_llama_..."
mount = "/llama"
protocols = ["openai", "vllm"]
public_paths = ["/health"]
max_streams = 64
```

家庭机器 A 的 `agent.toml`：

```toml
[agent]
id = "qwen"                    # 必须与 [[tunnels]].id 一致
server = "gateway.example.com:443"
token = "agt_qwen_..."
[target]
url = "http://127.0.0.1:8000"  # 本机 vLLM/Qwen
```

家庭机器 B 同理，`id = "llama"`、`url = "http://127.0.0.1:8001"`。

公网访问路径：

| 模型 | 网关 URL | 客户端 base_url |
| --- | --- | --- |
| Qwen | `https://gw.example.com/qwen/v1/...` | `https://gw.example.com/qwen/v1` |
| Llama | `https://gw.example.com/llama/v1/...` | `https://gw.example.com/llama/v1` |

要点：

- 每个隧道可独立设置 `max_streams`、`request_timeout`、`hosts`（按域名区分）、
  `default`；
- 建议每个模型单独分配 API Key，并用 `allow_tunnels` 绑定（见[鉴权隔离](#鉴权配额与隔离)）；
- agent 只需能访问网关 443，无需公网 IP/端口映射。

## 方案 B：一个 agent → 多个本地模型（路径前缀路由）

一台机器上跑了多个模型服务时，不必启动多个 agent 进程：在 `agent.toml` 里用
`[[targets]]` 声明多个本地上游，agent 按**路径前缀**选择上游并剥离前缀。

```toml
[agent]
id = "mixed"
server = "gateway.example.com:443"
token = "agt_mixed_..."

# 默认上游（没有匹配到任何前缀时使用）
[target]
url = "http://127.0.0.1:8000"

# 额外上游：prefix 最长优先匹配，匹配后剥离 prefix 再转发
[[targets]]
name = "qwen"
prefix = "/qwen"
url = "http://127.0.0.1:8001"
request_timeout = "10m"

[[targets]]
name = "llama"
prefix = "/llama"
url = "http://127.0.0.1:8002"
headers = { "x-api-key" = "sk-internal" }   # 仅发往该上游
```

网关只需要一条隧道：

```toml
[[tunnels]]
id = "mixed"
token = "agt_mixed_..."
mount = "/mixed"
```

路由规则（重要）：

1. 匹配 **最长前缀**，且必须命中完整路径段：`/qwen` 匹配 `/qwen`、`/qwen/v1/...`，
   **不匹配** `/qwenlite`；
2. 命中后**剥离前缀**再拼接上游：`/mixed/qwen/v1/chat/completions` →
   agent 收到 `/qwen/v1/chat/completions` → 上游 `:8001/v1/chat/completions`；
3. 没有命中任何 `[[targets]].prefix` 的路径走 `[target].url`（默认上游）；
4. 每个 target 可以单独设置 `request_timeout` / `connect_timeout` / `tls_ca` /
   `headers` / `enabled`；`prefix` 不能是 `/`（默认上游用 `[target]`）；
5. `[[targets]]` 最多 64 个；更复杂的模型名路由请用方案 C。

公网访问：

```text
POST https://gw.example.com/mixed/qwen/v1/chat/completions   # -> 本机 :8001
POST https://gw.example.com/mixed/llama/v1/chat/completions  # -> 本机 :8002
POST https://gw.example.com/mixed/v1/chat/completions        # -> 本机 :8000（默认）
```

响应头 `x-divertlink-target: qwen` 会标注实际命中的上游，便于排障。

> 说明：DivertLink 坚持「协议透明」，**不解析请求体里的 `model` 字段**做路由；按模型名
> 路由请使用方案 C（new-api/LiteLLM）或在客户端使用不同 base_url（方案 A/B）。

## 方案 C：家庭端统一入口（new-api / LiteLLM）

如果家里已经用 [new-api](https://github.com/Calcium-Ion/new-api) / LiteLLM / one-api 把多个
模型聚合在一个端口（如 `:3000`），agent 只需要代理这个统一入口：

```toml
[agent]
id = "home"
server = "gateway.example.com:443"
token = "agt_home_..."

[target]
url = "http://127.0.0.1:3000"
```

网关一条隧道即可（例如 mount `/home`）。模型名、额度、渠道、失败重试全部由 new-api 处理，
DivertLink 只负责安全隧道与公网入口。这也是「按模型名路由」最省事的方式。

## 多机冗余：多个 agent 服务同一条隧道

同一模型希望在多台机器上冗余/分担并发时，让**多个 agent 使用相同的 `id` 和 `token`**
接入同一条隧道即可（复制同一个 `agent.toml` 到多台机器，或启动多个进程）：

- 网关对每个数据流做 **round-robin**，请求自动分散到多个 agent；
- 某个 agent 失联时，流打开失败会自动**切换到下一个会话**，并淘汰失效会话，
  不会把请求发给已断开的机器；
- `/status` 中每条隧道显示 `agents`（会话数）与 `sessions`（明细：agent 版本、peer、
  连接时长）；控制台「实时状态」也会显示 agent 数；
- `[[tunnels]].max_streams` 是**隧道级**上限（所有 agent 共享），
  `agent.max_inflight` 是**单机**上限；
- agent 重连（同机网络抖动）会新增会话并淘汰旧会话，最多在心跳超时窗口内短暂重叠，
  请求仍可用。

示例：`agent-qwen.toml` 复制到 3 台机器 → `/qwen` 自动 3 路负载 + 故障转移。

> 多 agent 必须提供**相同模型集合**才能互相替代；如果分别提供不同模型，请用不同隧道
> （方案 A）或方案 B 的前缀路由。

## 鉴权、配额与隔离

多模型场景建议「一个调用方一个 Key，一个模型一条隧道」：

```toml
[[api_keys]]
name = "team-qwen"
key = "dlk_qwen_..."
allow_tunnels = ["qwen"]     # 只能访问 /qwen
rate_limit = "600/min"
max_concurrent = 64

[[api_keys]]
name = "team-all"
key = "dlk_all_..."
allow_tunnels = []           # 空 = 所有隧道
rate_limit = "1200/min"
max_concurrent = 128

[limits]
global_rate_limit = "6000/min"   # 网关总限流（认证前）
```

- `allow_ips` 可再做来源限制；
- 每个 Key 的 `rate_limit` / `max_concurrent` 独立生效，适合按团队/模型分配配额；
- 需要按 token 计费/额度/模型级权限时，让 new-api 作为上游（方案 C）或把 new-api 放在
  网关 `static_routes` 后面；
- 静态路由默认也要网关 API Key（`require_auth = true`），只有显式关闭时才由上游自校验。

## 客户端怎么填 base_url

### OpenAI 兼容（Python / Node / curl）

```python
from openai import OpenAI

qwen = OpenAI(base_url="https://gw.example.com/qwen/v1", api_key="dlk_qwen_...")
llama = OpenAI(base_url="https://gw.example.com/llama/v1", api_key="dlk_llama_...")

qwen.chat.completions.create(model="qwen2.5-72b-instruct", messages=[...], stream=True)
```

方案 B（一个 agent 多上游）时，base_url 带上前缀即可：

```python
mixed = OpenAI(base_url="https://gw.example.com/mixed/qwen/v1", api_key="dlk_all_...")
```

### Anthropic 兼容

Anthropic SDK 会自动追加 `/v1/messages`，因此 base_url 填到前缀为止：

```python
import anthropic
client = anthropic.Anthropic(
    base_url="https://gw.example.com/claude",   # -> /claude/v1/messages
    api_key="dlk_all_...",
)
```

### new-api / one-api 渠道

- 把每个模型对应的网关地址（`https://gw.example.com/qwen`）配成一个「OpenAI 兼容」渠道；
  类型选「OpenAI」时 base_url 通常不带 `/v1`，选「OpenAI 兼容」时带 `/v1`，以渠道测试为准；
- 或在 new-api 里给渠道加模型重定向，把 `qwen2.5` 映射到家庭端实际模型名；
- Anthropic 渠道的 Base URL 不带 `/v1`（SDK/渠道自动追加 `/v1/messages`）。

## 观测与排障

```bash
# 每个隧道挂了多少 agent、是否在线、会话明细
curl -fsS http://127.0.0.1:9090/status | jq '.tunnels[] | {id, mount, agents, connected}'

# 指标：请求量/流量/失败数按隧道
curl -fsS http://127.0.0.1:9090/metrics | grep divertlink_tunnel

# 响应头里能看到请求 id 与实际命中的本地上游（方案 B）
curl -i https://gw.example.com/mixed/qwen/v1/models -H "Authorization: Bearer dlk_..."
# x-divertlink-request-id: dl_...
# x-divertlink-target: qwen
```

常见问题：

| 现象 | 排查 |
| --- | --- |
| 某模型 404 | 检查网关 mount 与客户端 base_url 是否一致；方案 B 检查 `prefix` 与上游路径 |
| 多 agent 只有一台在服务 | `/status` 看 `agents` 数；同隧道要求 id/token **完全一致**，agent 需能连到网关 443 |
| 某台 agent 故障后请求偶发 502 | 属于流级失败后的自动切换；确认心跳/重连参数，查 `/metrics` 的 `session_replacements`、`stream_errors` |
| 前缀路由没生效 | `prefix` 必须以 `/` 开头且命中完整路径段；查看响应头 `x-divertlink-target` |
| 模型名/额度路由需求 | 用方案 C（new-api/LiteLLM），DivertLink 不解析请求体 |

## 容量与调优

- 并发：隧道级 `max_streams` × 隧道数 ≤ 后端总并发；每个 agent 还有 `max_inflight`；
- 多 agent 同隧道：把 `max_streams` 设为所有 agent 能力之和，`agent.max_inflight` 按单机
  GPU 能力设置；
- 超时：长输出模型建议 `request_timeout = "10m"`、保持 `stream_idle_timeout ≥ 120s`；
- 流式：SSE 逐块透传，网关/agent 不会攒包；前置 nginx 需 `proxy_buffering off`
  （仓库示例已配置）；
- 请求体上限按业务调整（图片/音频 base64 可能较大）；
- 网关总限流 `[limits].global_rate_limit` 用于过载保护，避免一个客户端打满所有模型出口。

## 验证步骤

以仓库示例配置为例（需要三台家庭机器或本机模拟）：

```bash
# 1) 在网关机器生成证书，拷贝 server.toml / certs 与各 agent.toml / ca.crt
divertlink init --role server --dir /etc/divertlink --domain gw.example.com
divertlink check -c examples/multi-model/server.toml

# 2) 各家庭机器启动对应 agent
divertlink agent -c agent-qwen.toml
divertlink agent -c agent-llama.toml

# 3) 网关侧确认在线
curl -fsS http://127.0.0.1:9090/readyz
curl -fsS http://127.0.0.1:9090/status | jq '.tunnels[] | {id, agents}'

# 4) 逐个模型验证（OpenAI 兼容）
curl -fsS https://gw.example.com/qwen/v1/models -H "Authorization: Bearer dlk_qwen_..."
curl -fsS https://gw.example.com/llama/v1/models -H "Authorization: Bearer dlk_llama_..."
```

> 仓库根目录的 `scripts/smoke.sh` 会在临时环境自动验证「单隧道 + 单模型」全链路；
> 多模型/多 agent 场景的自动化测试见 `tests/e2e.rs` 中的
> `agent_multi_targets_route_by_prefix` 与 `multiple_agents_share_one_tunnel_with_failover`。

## 常见问题

**Q：一个 agent 能同时代理多个模型吗？**
能。用 `[[targets]]` 前缀路由（方案 B），或让 agent 指向本地 new-api/LiteLLM（方案 C）。
若各模型需要独立并发/重启，建议拆成多个 agent（方案 A）。

**Q：多个 agent 能共用一条隧道吗？**
能，且会自动负载均衡 + 故障转移；要求 id/token 相同、提供的模型能力一致。

**Q：网关会解析请求体做模型路由吗？**
不会。DivertLink 保持协议透明；按 `model` 字段路由请交给 new-api/LiteLLM（方案 C），
或按 base_url 前缀选择隧道/本地上游（方案 A/B）。

**Q：多模型会不会互相抢带宽/显存？**
网关只做流式转发，不缓存响应；瓶颈在家庭端 GPU 与上行带宽。用 `max_streams`、
`agent.max_inflight`、Key 级 `rate_limit` 做隔离与保护。

**Q：一个网关最多接多少模型/agent？**
没有硬编码上限（受内存、连接数、`max_connections` 与文件描述符限制）；每个隧道一条长连接，
每个请求一条 yamux 流。几百个隧道/agent 属于可控范围，注意 systemd/Docker 的
`LimitNOFILE` 与网关 `max_connections`。
