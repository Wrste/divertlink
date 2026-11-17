# DivertLink 示例配置

本目录给出可直接运行的双端示例配置与三端拓扑说明。示例中的域名
`gateway.example.com`、`home.llm.example.com`、令牌与端口均为占位值，运行前请按实际
情况替换（见各文件内注释）。

> **路径规则**：配置中证书/CA 的相对路径以**配置文件所在目录**为基准解析，不是进程
> 工作目录。因此推荐把本目录的 `server.toml` / `agent.toml` 复制到一个工作目录，并把
> `certs/` 放在同级，如下文所示；若直接以 `-c examples/server.toml` 运行，则路径会相对
> `examples/` 解析，需要相应调整配置或在 `examples/` 下放置证书。

## 1. 三端拓扑

```
┌──────────────────────┐        TLS 443 (ALPN)        ┌────────────────────────┐
│ 公网固定 IP 服务器    │   ◄──── 主动长连接 ─────    │ 家庭宽带（无公网 IP）   │
│ divertlink server     │                            │ divertlink agent        │
│                       │                            │                         │
│ :443  ├─ divertlink/1 │  yamux 隧道 + 控制流       │ └─ target: 127.0.0.1:8000│
│       │               │                            │    vLLM / Ollama /      │
│       └─ h2 / http1.1 │ ── 公网入口 ──► 客户端    │    LM Studio / new-api  │
│ :8080 明文入口(本地)  │ ── 本机 nginx/new-api      │                         │
│ :9090 管理端点(本地)  │ ── /healthz /metrics       │                         │
│ :3000 本机 new-api    │ ── /newapi 静态反代 ──    │                         │
└──────────────────────┘                            └────────────────────────┘
```

三条典型请求路径：

1. **客户端 → 隧道 → 家庭端服务**（主路径）
   `https://gateway.example.com/home/v1/chat/completions`
   → 网关按 mount `/home` 命中隧道 `home` → 剥离前缀 → 经 yamux 数据流转发到
   家庭端 `agent.target.url` 的 `/v1/chat/completions`。

2. **客户端 → 网关静态路由 → 本机 new-api**
   `https://gateway.example.com/newapi/v1/chat/completions`
   → 网关按 `[[static_routes]]` 直接反代到 `http://127.0.0.1:3000/v1/chat/completions`。

3. **new-api → 网关隧道 → 家庭端服务**
   new-api 把 `https://gateway.example.com/home/v1` 配为 OpenAI 兼容渠道的 Base URL，
   请求经隧道到达家庭端 vLLM；家庭端也可以是 new-api 本身（`target.url = http://127.0.0.1:3000`）。

## 2. 文件说明

| 文件 | 作用 |
| --- | --- |
| `server.toml` | 网关配置：监听、TLS、trust_proxy、限流、API Key、隧道 `home`/`newapi-local`、静态路由 `/newapi` |
| `agent.toml` | 家庭端配置：隧道 id/token、心跳与重连、协议声明、TLS 校验、上游 target |

两份配置的 `[[tunnels]].id` / `token` 与 `[agent].id` / `token` 已互相对应：
示例中为 `home` / `agt_change_me`（仅用于演示，上线必须替换）。

## 3. 演示目录准备

以下命令在仓库根目录执行，演示配置放在独立的 `demo/` 目录中，证书与配置同级：

```bash
mkdir -p demo && cp examples/server.toml examples/agent.toml demo/
cd demo

# 生成 CA + 服务端证书（域名替换为你自己的；自签证书仅用于演示/内网）
divertlink gen-certs --dir certs --domain gateway.example.com
```

家庭端机器（或另一个目录）同样准备：

```bash
mkdir -p ~/divertlink-demo/certs && cd ~/divertlink-demo
scp <网关用户名>@<网关地址>:<演示目录>/agent.toml .
scp <网关用户名>@<网关地址>:<演示目录>/certs/ca.crt certs/
```

## 4. 启动与验证

### 4.1 网关（公网服务器）

```bash
cd demo
# 先校验配置（会检查字段、路由冲突与必填项）
divertlink check -c server.toml

# 启动（生产建议交由 systemd 管理，见 docs/deployment.md）
divertlink server -c server.toml
```

> 监听 443 需要 root 或 `CAP_NET_BIND_SERVICE`。本地快速验证可把
> `tunnel_listen` / `ingress_listen` 改为 `127.0.0.1:8443`，客户端相应改用 8443。

### 4.2 agent（家庭端）

```bash
cd ~/divertlink-demo
divertlink check -c agent.toml
divertlink agent -c agent.toml
```

### 4.3 验证

```bash
# 网关管理端点（在网关本机执行）
curl -fsS http://127.0.0.1:9090/healthz
curl -fsS http://127.0.0.1:9090/readyz
curl -sS  http://127.0.0.1:9090/status

# 经隧道调用家庭端 vLLM（客户端执行；自签证书时加 --cacert certs/ca.crt）
curl -sS https://gateway.example.com/home/v1/models \
  -H "Authorization: Bearer dlk_change_me"

# 流式验证
curl -N -sS https://gateway.example.com/home/v1/chat/completions \
  -H "Authorization: Bearer dlk_change_me" \
  -H "Content-Type: application/json" \
  -H "Accept: text/event-stream" \
  -d '{"model":"Qwen2.5-7B-Instruct","stream":true,
       "messages":[{"role":"user","content":"从 1 数到 10"}]}'

# 静态路由到本机 new-api（需要本机 :3000 运行 new-api）：
# 示例静态路由设置 require_auth = false，因此这里携带的是 new-api 自己识别的令牌；
# 若保持默认 require_auth = true，还需同时携带网关 API Key。
curl -sS https://gateway.example.com/newapi/api/status \
  -H "Authorization: Bearer <new-api 令牌>"
```

## 5. 上线前检查

- 替换 `dlk_change_me`、`dlk_ops_change_me`、`agt_change_me`、`agt_newapi_change_me`
  等示例令牌；`divertlink token --prefix dlk_ --count 3` 可批量生成。
- 证书 SAN 覆盖网关域名与所有 `hosts` 路由域名；使用 Let's Encrypt 时确保续期后重启。
- 核对 `allow_tunnels`、`allow_ips`、`rate_limit`、`max_concurrent` 是否符合最小权限原则。
- `[[static_routes]]` 默认 `require_auth = true`（走网关 API Key）；示例设为 `false`
  交给 new-api 自校验，此时确保 `upstream` 只指向回环/内网，且
  new-api 等上游自己校验客户端令牌；需要网关级鉴权时请改用 `[[tunnels]]`。
- 示例默认让 `http_listen` / `admin_listen` 监听回环；不需要时写
  `http_listen = "off"` / `admin_listen = "off"` 即可关闭（也接受 `false`），
  端口冲突时也可改成其他回环端口。
- 若在网关前部署 nginx/Caddy 并依赖真实客户端 IP，按需打开 `[server].trust_proxy`；
  公网直连时保持 `false`。
- 家庭端 `target.url` 只监听回环地址，不要额外把家庭服务暴露到局域网或公网。
- 不要在生产环境开启 `insecure = true`。

更多内容见 [快速开始](../docs/quickstart.md)、[配置参考](../docs/configuration.md)、
[协议与调用](../docs/protocols.md)。
