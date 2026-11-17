# 协议与调用方式

DivertLink 网关不解析大模型业务 JSON，只做 HTTP 语义级转发，因此 OpenAI、Anthropic、
vLLM 以及任何 OpenAI/Anthropic 兼容服务（new-api、one-api、LiteLLM 等）的接口都可以
原样透传。本文说明 base_url 与路径如何拼接、认证头如何处理、如何验证 SSE 流式，
以及如何把网关接入 new-api / one-api。

- [1. 请求如何到达家庭端](#1-请求如何到达家庭端)
- [2. base_url 与路径拼接](#2-base_url-与路径拼接)
- [3. 认证头与上游鉴权](#3-认证头与上游鉴权)
- [4. 各协议调用示例](#4-各协议调用示例)
  - [4.1 OpenAI 兼容 API](#41-openai-兼容-api)
  - [4.2 Anthropic Messages API](#42-anthropic-messages-api)
  - [4.3 vLLM 原生端点](#43-vllm-原生端点)
- [5. 与 new-api / one-api 集成](#5-与-new-api--one-api-集成)
- [6. SSE 流式验证](#6-sse-流式验证)
- [7. 透传边界与响应语义](#7-透传边界与响应语义)

## 1. 请求如何到达家庭端

```text
客户端/ new-api ──TLS──► 公网网关 :443 ──yamux 数据流──► 家庭端 agent ──HTTP──► vLLM / new-api
                 h2/http1.1            HTTP/1.1 报文            target.url
```

网关按以下规则选择隧道（详见 [配置参考 §2.6](configuration.md#26-tunnels)）：

| 路由方式 | 客户端访问示例 | 转发到家庭端的路径 |
| --- | --- | --- |
| `mount` 前缀（最常用） | `https://gateway.example.com/home/v1/chat/completions` | 剥离 `/home` → `/v1/chat/completions` |
| `hosts` 域名 | `https://home.llm.example.com/v1/chat/completions` | 原路径 → `/v1/chat/completions`（若该隧道同时配了 mount，则还会剥离 mount） |
| `default` 兜底 | `https://gateway.example.com/v1/chat/completions` | 原路径 → `/v1/chat/completions` |

要点：

- 一条隧道只有一个 `mount`；OpenAI 与 Anthropic 接口由**同一个家庭端 target** 提供时
  （例如 target 是 new-api），共用同一个 `mount` 即可，只是客户端 base_url 写法不同。
- **`hosts` 是限制性的**：隧道配置了非空 `hosts` 后，只有这些 Host 的请求能进入该隧道
  （即使 URL 路径命中了它的 mount）；Host 匹配优先于纯 mount 匹配。
- 网关会保留查询字符串（`?stream=true`、`?api-version=...` 等）。
- 客户端断开 yamux 数据流即取消请求，agent 会关闭到上游的连接，避免上游继续无谓生成。

## 2. base_url 与路径拼接

不同 SDK 的默认路径不同，base_url 是否包含 `/v1` 也不同，这是接入时最常见的错误来源。
下表中的网关域名、隧道前缀、模型名均为占位值，请替换为你的实际值。

### 2.1 OpenAI 兼容客户端

OpenAI SDK 会在 `base_url` 后**自动追加** `/chat/completions`、`/embeddings`、`/responses`
等路径，因此 base_url 必须包含 `/v1`：

| 项目 | 值 |
| --- | --- |
| 隧道 mount | `/home` |
| 客户端 base_url | `https://gateway.example.com/home/v1` |
| SDK 请求路径 | `/chat/completions` |
| 网关收到路径 | `/home/v1/chat/completions` |
| 家庭端收到路径 | `/v1/chat/completions`（已剥离 mount） |

```python
from openai import OpenAI

client = OpenAI(
    base_url="https://gateway.example.com/home/v1",
    api_key="dlk_REPLACE_WITH_GATEWAY_KEY",
)

resp = client.chat.completions.create(
    model="Qwen2.5-7B-Instruct",  # 必须与家庭端服务实际提供的模型名一致
    messages=[{"role": "user", "content": "用三句话解释什么是反向隧道"}],
    stream=True,
)
for chunk in resp:
    print(chunk.choices[0].delta.content or "", end="", flush=True)
```

### 2.2 Anthropic 兼容客户端

Anthropic SDK 会在 `base_url` 后**自动追加** `/v1/messages`（以及
`/v1/messages/count_tokens`），因此 base_url **不要**再写 `/v1`：

| 项目 | 值 |
| --- | --- |
| 隧道 mount | `/home` |
| 客户端 base_url | `https://gateway.example.com/home` |
| SDK 请求路径 | `/v1/messages` |
| 网关收到路径 | `/home/v1/messages` |
| 家庭端收到路径 | `/v1/messages` |

```python
from anthropic import Anthropic

client = Anthropic(
    base_url="https://gateway.example.com/home",
    api_key="dlk_REPLACE_WITH_GATEWAY_KEY",  # SDK 以 x-api-key 头发送
)

with client.messages.stream(
    model="claude-compatible-model",
    max_tokens=1024,
    messages=[{"role": "user", "content": "写一段 100 字的产品介绍"}],
) as stream:
    for text in stream.text_stream:
        print(text, end="", flush=True)
```

### 2.3 vLLM 原生端点

vLLM 的非 OpenAI 路径直接拼接在 mount 之后：

| 用途 | 网关 URL |
| --- | --- |
| 健康检查 | `https://gateway.example.com/home/health` |
| 模型列表 | `https://gateway.example.com/home/v1/models` |
| 文本补全 | `https://gateway.example.com/home/v1/completions` |
| 分词 | `https://gateway.example.com/home/tokenize` |
| 指标 | `https://gateway.example.com/home/metrics` |

> 具体端点以家庭端服务实际暴露的路径为准；网关不做路径改写（mount 剥离后原样转发）。

### 2.4 Host 路由与 default 路由

- 给隧道配置 `hosts = ["home.llm.example.com"]` 后，可把该域名解析到网关，客户端
  base_url 直接写 `https://home.llm.example.com/v1`（OpenAI）或
  `https://home.llm.example.com`（Anthropic）。要求：
  1. DNS 记录指向网关公网 IP；
  2. 网关 `[tls].cert` 的 SAN 覆盖该域名；
  3. 防火墙放行 443。
- 给某个隧道配置 `default = true`（全配置唯一）后，未命中任何 mount/host 的请求都会
  转发给它，此时 base_url 不带隧道前缀，例如 `https://gateway.example.com/v1`。

## 3. 认证头与上游鉴权

### 3.1 入口认证

访问**隧道**（`[[tunnels]]`）的客户端必须携带网关 API Key（`[[api_keys]].key`），两种头
都支持；`[[tunnels]].public_paths` 中的路径（如 `/health`）免认证：

| 客户端类型 | 实际发送的认证头 |
| --- | --- |
| OpenAI SDK / curl | `Authorization: Bearer dlk_xxx` |
| Anthropic SDK / curl | `x-api-key: dlk_xxx` |
| new-api / one-api 渠道 | 由渠道类型决定，OpenAI 类型为 `Authorization: Bearer`，Anthropic 类型为 `x-api-key` |

入口认证失败返回 `401`（JSON 错误体 `missing_api_key` / `invalid_api_key`）；Key 无权访问
该隧道（`allow_tunnels`）或来源 IP 不在 `allow_ips` 内返回 `403`。

> **静态路由**：`[[static_routes]]`（如 `/newapi`）默认 `require_auth = true`，与隧道
> 一样要求网关 API Key（并执行 `allow_ips` / `rate_limit` / `max_concurrent` 检查），
> 网关随后把认证头透传给上游；仅当显式设置 `require_auth = false` 时才跳过网关鉴权、
> 完全由上游（如 new-api）校验令牌。详见
> [§5.2](#52-拓扑-b网关静态路由反代到-new-api) 与
> [配置参考 §2.7](configuration.md#27-static_routes)。

> 多模型/多上游场景（一个网关多个模型、一个 agent 多本地服务、多 agent 冗余）的 base_url
> 与路由规则见 [多模型与多对一部署](multi-model.md)。

### 3.2 上游认证头的四种处理方式

每个隧道用 `upstream_auth_header` 决定发到家庭端的认证头（见
[配置参考 §2.6](configuration.md#26-tunnels)）：

| 取值 | 行为 | 适用场景 |
| --- | --- | --- |
| `passthrough`（默认） | 原样透传客户端认证头 | 客户端持有与家庭端一致的 Key（vLLM `--api-key` 与网关 Key 相同，或 new-api 令牌与网关 Key 相同） |
| `authorization` | 用 `upstream_api_key` 作为 `Authorization: Bearer <key>` 发往家庭端 | 推荐：客户端只持网关 Key，家庭端 Key 由网关注入（vLLM、new-api） |
| `x-api-key` | 用 `upstream_api_key` 作为 `x-api-key` 发往家庭端 | 家庭端是 Anthropic 兼容服务且只认 `x-api-key` |
| `none` | 不向家庭端发送认证头 | 家庭端 vLLM 未设置 `--api-key` 等无鉴权场景 |

推荐的安全组合（客户端只持有网关 Key，真实上游 Key 不出网关）：

| 家庭端服务 | `upstream_auth_header` | `upstream_api_key` |
| --- | --- | --- |
| vLLM（启动时带 `--api-key`） | `"authorization"` | vLLM 的 API Key |
| new-api / one-api | `"authorization"` | new-api 令牌（如 `sk-...`） |
| 无鉴权 vLLM / Ollama | `"none"` | `""`（留空） |
| 透传模式 | `"passthrough"` | `""`（留空） |

> `passthrough` 模式下 `upstream_api_key` 会被忽略；`none` 模式则会剥离认证头。启用替换
> 模式后，客户端 Key 不会出现在家庭端请求中，便于 key 分层与审计。

## 4. 各协议调用示例

以下 `gateway.example.com`、`dlk_REPLACE_ME`、模型名均为占位值，请替换。
非流式请求用于验证连通性，流式请求用于验证 SSE 透传。

### 4.1 OpenAI 兼容 API

**curl（非流式）**

```bash
curl -sS https://gateway.example.com/home/v1/chat/completions \
  -H "Authorization: Bearer dlk_REPLACE_ME" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "Qwen2.5-7B-Instruct",
    "messages": [{"role": "user", "content": "你好，请用一句话自我介绍"}],
    "temperature": 0.7,
    "max_tokens": 64,
    "stream": false
  }'
```

**curl（流式，SSE）**

```bash
curl -N -sS https://gateway.example.com/home/v1/chat/completions \
  -H "Authorization: Bearer dlk_REPLACE_ME" \
  -H "Content-Type: application/json" \
  -H "Accept: text/event-stream" \
  -d '{
    "model": "Qwen2.5-7B-Instruct",
    "messages": [{"role": "user", "content": "从 1 数到 20，每行一个数字"}],
    "stream": true
  }'
```

期望：逐行出现 `data: {...}`，以 `data: [DONE]` 结束，而不是等全部生成完才一次性输出。
判定方法见 [§6](#6-sse-流式验证)。

**其他 OpenAI 兼容端点**

| 端点 | 网关 URL |
| --- | --- |
| 模型列表 | `GET https://gateway.example.com/home/v1/models` |
| Embeddings | `POST https://gateway.example.com/home/v1/embeddings` |
| Responses API | `POST https://gateway.example.com/home/v1/responses` |
| 图片/音频等 | 按家庭端服务实际路径拼接，如 `POST .../home/v1/audio/transcriptions` |

### 4.2 Anthropic Messages API

**curl**

```bash
curl -sS https://gateway.example.com/home/v1/messages \
  -H "x-api-key: dlk_REPLACE_ME" \
  -H "anthropic-version: 2023-06-01" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "claude-compatible-model",
    "max_tokens": 256,
    "messages": [{"role": "user", "content": "用两句话说明什么是 SSE"}]
  }'
```

**curl（流式）**

```bash
curl -N -sS https://gateway.example.com/home/v1/messages \
  -H "x-api-key: dlk_REPLACE_ME" \
  -H "anthropic-version: 2023-06-01" \
  -H "Content-Type: application/json" \
  -H "Accept: text/event-stream" \
  -d '{
    "model": "claude-compatible-model",
    "max_tokens": 512,
    "stream": true,
    "messages": [{"role": "user", "content": "写一首四行诗"}]
  }'
```

**token 计数**

```bash
curl -sS https://gateway.example.com/home/v1/messages/count_tokens \
  -H "x-api-key: dlk_REPLACE_ME" \
  -H "anthropic-version: 2023-06-01" \
  -H "Content-Type: application/json" \
  -d '{"model": "claude-compatible-model", "messages": [{"role": "user", "content": "hello"}]}'
```

> Anthropic SDK 使用 `base_url = "https://gateway.example.com/home"`（不带 `/v1`），
> 且 `anthropic-version` 等头会被透传；若家庭端是 Anthropic 兼容服务，请按该服务要求
> 携带对应版本头。

### 4.3 vLLM 原生端点

```bash
# 健康检查（若该路径加入 [[tunnels]].public_paths，则可不带认证）
curl -sS https://gateway.example.com/home/health

# 分词
curl -sS https://gateway.example.com/home/tokenize \
  -H "Authorization: Bearer dlk_REPLACE_ME" \
  -H "Content-Type: application/json" \
  -d '{"model":"Qwen2.5-7B-Instruct","prompt":"hello"}'

# Prometheus 指标（建议只对可信 Key 开放）
curl -sS https://gateway.example.com/home/metrics \
  -H "Authorization: Bearer dlk_REPLACE_ME"
```

### 4.4 Ollama / LM Studio / llama.cpp

这些服务都提供 OpenAI 兼容接口，按 4.1 的方式调用即可，关键是 `[target].url`：

| 家庭端服务 | `[target].url` 示例 | 说明 |
| --- | --- | --- |
| Ollama | `http://127.0.0.1:11434` | OpenAI 兼容路径为 `/v1`；原生 `/api/...` 也可透传 |
| LM Studio | `http://127.0.0.1:1234` | OpenAI 兼容路径为 `/v1` |
| llama.cpp server | `http://127.0.0.1:8080` | OpenAI 兼容路径为 `/v1` |
| vLLM | `http://127.0.0.1:8000` | 默认端口 8000 |
| new-api | `http://127.0.0.1:3000` | 默认端口 3000 |

## 5. 与 new-api / one-api 集成

new-api / one-api 与 DivertLink 有三种组合拓扑，可单独使用，也可叠加使用。

### 5.1 拓扑 A：new-api 把网关配成上游渠道（推荐）

```text
客户端 ──► new-api ──HTTP(S)──► 网关 /home/v1 ──隧道──► 家庭端 vLLM
```

适合需要 new-api 做用户、额度、渠道管理的场景。

**new-api 渠道配置（OpenAI 兼容）**

| 配置项 | 填写值 |
| --- | --- |
| 渠道类型 | OpenAI（new-api/one-api 最常见的 OpenAI 类型） |
| Base URL | `https://gateway.example.com/home`（**通常不带 `/v1`**：该类型渠道会自动拼接 `/v1/chat/completions`） |
| 密钥 | 网关 `[[api_keys]].key`，如 `dlk_REPLACE_ME` |
| 模型 | 填写家庭端实际模型名，如 `Qwen2.5-7B-Instruct`；多个用逗号/换行分隔 |
| 模型重定向 | 如把 `gpt-4o-mini` 映射为 `Qwen2.5-7B-Instruct`，使客户端不用关心真实模型名 |
| 代理 | 留空（new-api 直连网关） |

> **Base URL 的 `/v1` 规则与渠道类型有关**：标准 OpenAI 类型渠道通常填**根地址**
> （`.../home`），由渠道自行拼接 `/v1/chat/completions`；one-api 的
> 「OpenAI 兼容」类型则会先剥掉路径里的 `/v1` 再拼 `/chat/completions`，此时应填
> `https://gateway.example.com/home/v1`。不同 new-api/one-api 版本行为略有差异：
> 若渠道测试返回 404，先在不带 `/v1` 与带 `/v1` 两种写法之间切换并重新测试，
> 以渠道测试的实际结果为准。

推荐同时在对应隧道配置 `upstream_auth_header = "authorization"` 与
`upstream_api_key = "<家庭端真实 Key>"`，这样 new-api 渠道密钥只需要是网关 Key，
家庭端真实 Keys 不会离开网关。

**new-api 渠道配置（Anthropic 类型）**

| 配置项 | 填写值 |
| --- | --- |
| 渠道类型 | Anthropic |
| Base URL | `https://gateway.example.com/home`（**不要包含 `/v1`**：Anthropic 协议会自动追加 `/v1/messages`） |
| 密钥 | 网关 `[[api_keys]].key`（网关同时接受 `x-api-key` 头） |
| 模型 | 家庭端 Anthropic 兼容模型名 |

> new-api 不同版本的渠道表单字段命名可能略有差异，以上为语义对应关系。Anthropic
> 渠道与客户端 SDK 一致：base URL 到 mount 为止，不带 `/v1`；OpenAI 渠道则按上文
> 按类型选择带或不带 `/v1`，两者不要混用。

### 5.2 拓扑 B：网关静态路由反代到 new-api

```text
客户端 ──► 网关 /newapi/v1/... ──静态反代──► 本机 new-api :3000 ──► 上游渠道
```

适合 new-api 与网关同机部署、希望只暴露一个 443 入口的场景。
`server.toml` 配置：

```toml
[[static_routes]]
mount = "/newapi"
upstream = "http://127.0.0.1:3000"
strip_prefix = true
```

客户端调用（`Authorization` 头会被网关原样透传给 new-api，由 new-api 自己校验）：

```bash
curl -sS https://gateway.example.com/newapi/v1/chat/completions \
  -H "Authorization: Bearer <new-api 令牌>" \
  -H "Content-Type: application/json" \
  -d '{"model":"Qwen2.5-7B-Instruct","messages":[{"role":"user","content":"hi"}]}'
```

**鉴权说明（重要）**：静态路由默认 `require_auth = true`，客户端像访问隧道一样携带
网关 API Key（`Authorization: Bearer dlk_...`；也可用 `x-api-key`），网关完成鉴权后
把认证头（清洗后）透传给 new-api。因此：

1. new-api 侧默认仍会校验自己的令牌——若希望客户端只带 new-api 令牌，请把该静态路由
   设为 `require_auth = false`；
2. `require_auth = false` 时网关不做入口鉴权，new-api **必须**开启令牌校验并做额度/
   渠道管理，否则静态路由就是无需认证的代理；
3. 无论哪种模式，`upstream` 只能写 `127.0.0.1` / 内网地址，否则网关会变成开放代理；
4. 需要网关级 API Key 鉴权与限流时，保持 `require_auth = true`（默认）。

若 new-api 返回 `401`，先确认它实际收到的 `Authorization` 头是有效 new-api 令牌
（可在 new-api 日志中确认），再检查是否配置了 `require_auth` 而客户端没带网关 Key。

### 5.3 拓扑 C：家庭端 agent 指向 new-api

```text
客户端 ──► 网关 /home/v1 ──隧道──► 家庭端 new-api :3000 ──► 本地 vLLM/Ollama
```

适合家庭端已用 new-api 统一管理多个本地模型，希望把整个 new-api 暴露到公网。
agent 配置：

```toml
[target]
url = "http://127.0.0.1:3000"
```

网关隧道配置推荐注入 new-api 令牌：

```toml
[[tunnels]]
id = "home"
token = "agt_REPLACE_ME"
mount = "/home"
upstream_auth_header = "authorization"
upstream_api_key = "sk-REPLACE_WITH_NEWAPI_TOKEN"
```

此时外部客户端只持网关 Key（`dlk_...`），new-api 看到的是自己的令牌；
new-api 内部的渠道仍指向家庭端 vLLM。

### 5.4 模型名与路径映射要点

- **模型名透传**：网关不改写请求 JSON 中的 `model` 字段，最终由家庭端服务决定是否接受。
  因此客户端/new-api 渠道填写的模型名必须与家庭端服务实际提供的名称一致。
- **模型映射放 new-api**：需要把 `gpt-4o-mini` 这类名称映射到本地模型时，用 new-api
  的「模型重定向」或渠道模型列表实现，而不是在 DivertLink 配置。
- **Base URL 后缀**是接入失败的第一大原因，且“客户端 SDK”与“渠道”的规则不同：
  - 客户端 SDK（OpenAI Python/JS 等）：base_url 必须带 `/v1`（SDK 只追加 `/chat/completions`）；
  - 客户端 SDK（Anthropic）：base_url 到 mount 为止，不带 `/v1`（SDK 追加 `/v1/messages`）；
  - new-api/one-api 渠道：OpenAI 类型通常填根地址（不带 `/v1`），「OpenAI 兼容」类型填带 `/v1` 的地址；Anthropic 渠道不带 `/v1`。渠道测试 404 时切换写法并复测。
- **one-api**：使用「OpenAI」类型渠道时 Base URL 填 `https://gateway.example.com/home`
  （若使用其「OpenAI 兼容」类型则填 `https://gateway.example.com/home/v1`），
  密钥填网关 API Key；Anthropic 支持取决于 one-api 版本，若有 Anthropic 渠道类型，
  Base URL 填 `https://gateway.example.com/home`。one-api 的渠道测试若失败，
  先确认服务器能否直接访问网关域名（DNS、防火墙、TLS 证书链）。

## 6. SSE 流式验证

DivertLink 的设计目标之一是**零缓冲、逐块透传** SSE。以下方法可以证明流式是否真的
逐块到达，而不是在网关或中间代理处被攒齐后一次性返回。

### 6.1 方法一：给每行数据打时间戳

```bash
curl -N -sS https://gateway.example.com/home/v1/chat/completions \
  -H "Authorization: Bearer dlk_REPLACE_ME" \
  -H "Content-Type: application/json" \
  -H "Accept: text/event-stream" \
  -d '{"model":"Qwen2.5-7B-Instruct","stream":true,
       "messages":[{"role":"user","content":"从 1 数到 30，每行一个数字"}]}' \
| python3 -u -c '
import sys, time
t0 = time.monotonic()
while True:
    line = sys.stdin.readline()
    if not line:
        break
    line = line.rstrip("\n")
    if line.startswith("data:"):
        print(f"{time.monotonic() - t0:7.3f}s  {line[:120]}")
'
```

解读：

- 时间戳应**逐步递增、间隔与生成速度相当**（通常几十到几百毫秒一行）；
- 若所有行的时间戳几乎相同、且集中在最后（例如 `12.000s` 后瞬间出现几十行），
  说明响应被某处缓冲了（常见于前置 nginx 未关闭 `proxy_buffering`、CDN 或客户端
  未使用 `-N`）；
- 首行时间戳即首 token 延迟（TTFB），可用于对比直连家庭端与经网关的额外延迟。

### 6.2 方法二：对比 TTFB 与总耗时

```bash
curl -N -sS -o /dev/null \
  -w 'http=%{http_code} ttfb=%{time_starttransfer}s total=%{time_total}s\n' \
  https://gateway.example.com/home/v1/chat/completions \
  -H "Authorization: Bearer dlk_REPLACE_ME" \
  -H "Content-Type: application/json" \
  -H "Accept: text/event-stream" \
  -d '{"model":"Qwen2.5-7B-Instruct","stream":true,
       "messages":[{"role":"user","content":"写一篇 800 字的短文"}]}'
```

流式正常时 `ttfb` 远小于 `total`（例如 `ttfb≈0.3s`、`total≈20s`）。
若 `ttfb≈total` 且数值很大，说明数据在最后才集中返回。

### 6.3 方法三：检查响应头

```bash
curl -sS -D /tmp/divertlink-headers.txt -o /dev/null \
  https://gateway.example.com/home/v1/chat/completions \
  -H "Authorization: Bearer dlk_REPLACE_ME" \
  -H "Content-Type: application/json" \
  -H "Accept: text/event-stream" \
  -d '{"model":"Qwen2.5-7B-Instruct","stream":true,
       "messages":[{"role":"user","content":"hi"}]}'
cat /tmp/divertlink-headers.txt
```

期望看到 `content-type: text/event-stream`。注意：

- 入口走 HTTP/2 时不会出现 `Transfer-Encoding: chunked`，这是 HTTP/2 的正常行为，
  不能作为“是否缓冲”的判断依据；
- `curl` 必须加 `-N`（`--no-buffer`），否则是 curl 自身在缓冲；
- 若前面还有 nginx/CDN，请同时检查其 `proxy_buffering`、`proxy_read_timeout` 与压缩设置
  （见 [部署文档](deployment.md)）。

### 6.4 方法四：Anthropic 流式事件

Anthropic 流式响应以 `event:` 事件名区分（如 `message_start`、`content_block_delta`、
`message_stop`），同样应逐事件到达：

```bash
curl -N -sS https://gateway.example.com/home/v1/messages \
  -H "x-api-key: dlk_REPLACE_ME" \
  -H "anthropic-version: 2023-06-01" \
  -H "Content-Type: application/json" \
  -H "Accept: text/event-stream" \
  -d '{"model":"claude-compatible-model","max_tokens":512,"stream":true,
       "messages":[{"role":"user","content":"写 10 行短句"}]}'
```

### 6.5 常见流式问题定位

| 现象 | 可能原因 | 处理 |
| --- | --- | --- |
| 全部内容一次性出现 | 前置代理缓冲（nginx `proxy_buffering on`、CDN） | 关闭代理缓冲；直连网关 443 对比验证 |
| `curl` 没有逐行输出 | 未加 `-N` / 管道被缓冲 | 使用 `-N` 与 `python3 -u` |
| 30 秒左右中断 | 前置代理 `proxy_read_timeout` 过短 | 调大代理读超时；网关侧超时另见 [排障](troubleshooting.md) |
| 120 秒无数据后断开 | 命中 `stream_idle_timeout` | 确认模型是否长时间无输出；必要时调大该值 |
| 10 分钟被截断 | 命中 `request_timeout` | 调大 `[limits].request_timeout` 或 `[[tunnels]].request_timeout` |

## 7. 透传边界与响应语义

- **协议透明**：请求行、查询参数、请求体字节、响应状态码、响应头、响应体均按 HTTP
  语义转发；网关不解析 JSON，不感知模型、token 数或计费字段。
- **逐跳头清洗**：`Connection`、`Keep-Alive`、`Transfer-Encoding` 等逐跳头不会透传；
  上游请求使用 `Connection: close` 语义（每个 yamux 数据流一条 HTTP/1.1 请求）。
- **网关注入的请求头**：转发到家庭端的请求会被添加 `X-Forwarded-For`、
  `X-Forwarded-Proto`、`X-Forwarded-Host`、`X-Request-Id`（及审计用
  `X-DivertLink-Request-Id` / `X-DivertLink-Tunnel`）；客户端伪造的这些头会被清洗，
  上游服务如需真实客户端 IP 可读取 `X-Forwarded-For`。注意这是网关视角的 IP：
  若网关前还有代理，需要 `[server].trust_proxy = true` 才能得到代理前的真实来源 IP。
- **请求体上限**：受 `[limits].max_request_bytes`（默认 64 MiB）约束；声明了超过上限的
  `Content-Length` 时直接返回 413，未声明长度的分块上传在读取超限时中断请求。
- **超时**：网关侧受 `request_timeout` / `stream_idle_timeout` 约束，家庭端另受
  `[target].request_timeout` 约束；超时返回 504 或被中断。注意 agent 对 target 响应体
  另有固定 300s 空闲上限（不可配置），长时间无输出的模型会被该上限截断。
- **静态路由**：`[[static_routes]]` 做反向代理；默认 `require_auth = true`（走网关
  API Key、限流与并发限制）。`require_auth = false` 时仅透传认证头，安全性依赖上游
  自身鉴权（见 [配置参考 §2.7](configuration.md#27-static_routes)）。
- **取消**：客户端断开连接后，网关关闭对应 yamux 流，agent 关闭上游连接，实现端到端取消。
- **不支持**：MVP 不支持 WebSocket、QUIC/HTTP/3 传输（见 DESIGN 第 9 节非目标）；
  WebSocket 类业务无法通过本网关透传。
- **大文件/音频上传**：只要单请求不超过 `max_request_bytes`，按普通 HTTP 请求透传；
  建议按业务需要调大上限并配合 `request_timeout`。
