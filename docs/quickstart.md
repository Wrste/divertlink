# 快速开始

本文从零开始，把家庭端的 LLM 服务安全地映射到一台有固定公网 IP 的服务器上，并验证
非流式与流式调用。全程约 10 分钟；熟悉命令后可按 [README 的 30 秒快速开始](../README.md#30-秒快速开始) 精简执行。

- [1. 开始前准备](#1-开始前准备)
- [2. 安装 DivertLink](#2-安装-divertlink)
- [3. 生成配置与证书](#3-生成配置与证书)
- [4. 配置并启动网关](#4-配置并启动网关)
- [5. 配置并启动家庭端 agent](#5-配置并启动家庭端-agent)
- [6. 放行防火墙与 DNS](#6-放行防火墙与-dns)
- [7. 验证](#7-验证)
- [8. 常见错误](#8-常见错误)
- [9. 下一步](#9-下一步)
- [10. 端到端冒烟验证（可选）](#10-端到端冒烟验证可选)

## 1. 开始前准备

| 角色 | 要求 |
| --- | --- |
| 网关服务器 | 有公网固定 IP（或固定域名解析到该 IP）；Linux/macOS/Windows 均可；能监听 443（需 root 或端口能力）；建议 2 核 2 GB 以上 |
| 域名（推荐） | 一条 A 记录指向网关公网 IP，例如 `gateway.example.com`；自签证书或 Let's Encrypt 都需要它 |
| 家庭端机器 | 能访问互联网（家庭宽带/CGNAT 均可，**不需要公网 IP、不需要端口映射**）；能运行 vLLM / Ollama / LM Studio / llama.cpp / new-api 之一 |
| 家庭端服务 | 已在监听，例如 vLLM 默认 `http://127.0.0.1:8000`，new-api 默认 `http://127.0.0.1:3000` |
| 二进制 | `divertlink` 可执行文件（见第 2 节） |

网络拓扑：

```text
客户端 ──► https://gateway.example.com:443 ──► divertlink server（公网固定 IP）
                                                   │  ALPN "divertlink/1" 加密长连接
                                                   ▼
                                   divertlink agent（家庭宽带，主动外连）
                                                   │  HTTP
                                                   ▼
                                   vLLM / new-api（127.0.0.1:8000/3000）
```

> 下文命令中的 `gateway.example.com`、`home.llm.example.com`、`alice@192.168.1.50`、
> `dlk_...`、`agt_...` 均为占位值，必须替换为你的实际域名、账号与令牌。

## 2. 安装 DivertLink

### 2.1 使用发行包（推荐）

从项目 GitHub Releases 页面下载对应平台压缩包，解压后把 `divertlink` 放到
`PATH`（Linux/macOS）：

```bash
# 以 Linux x86_64 为例，<version> 替换为实际版本号，如 0.1.0
tar -xzf divertlink-<version>-x86_64-unknown-linux-musl.tar.gz
sudo install -m 0755 divertlink /usr/local/bin/divertlink
divertlink version
```

### 2.2 从源码构建

需要 Rust 1.88 或更新（`Cargo.toml` 的 `rust-version = "1.88"`）：

```bash
git clone https://github.com/divertlink/divertlink.git
cd divertlink
cargo build --release --locked
sudo install -m 0755 target/release/divertlink /usr/local/bin/divertlink
```

本仓库已内置本地工具链时，可先执行 `. ./scripts/env.sh` 再构建；该脚本仅用于
仓库内开发环境，不影响最终用户安装方式。

### 2.3 监听 443 的权限

Linux 上非 root 进程绑定 443 需要能力授权，任选其一：

```bash
# 方式一：给二进制授予绑定低端口能力
sudo setcap 'cap_net_bind_service=+ep' /usr/local/bin/divertlink

# 方式二（生产推荐）：用 systemd 的 AmbientCapabilities，见 docs/deployment.md
# 方式三：先用非特权端口演练，例如 8443（客户端 URL 相应改端口）
```

> `setcap` 授权会在升级二进制后失效，需要在每次升级后重新执行；生产部署建议使用
> systemd 单元或容器，具体见 [部署文档](deployment.md)。

## 3. 生成配置与证书

以下命令在**网关服务器**上执行。`init` 会生成双端配置示例、自签 CA 与服务端证书，
并打印下一步提示。

```bash
mkdir -p ~/divertlink && cd ~/divertlink
divertlink init --role both --domain gateway.example.com
```

生成的文件（如目录已有同名文件，`init` 默认不覆盖；确需覆盖加 `--force`）：

```text
~/divertlink/
├── server.toml          # 网关配置（已自动填入随机 API Key 与隧道 token）
├── agent.toml           # 家庭端配置（已自动填入同一 token、ca 与 pin）
└── certs/
    ├── ca.crt           # 自签 CA 证书（agent 用它校验网关证书）
    ├── ca.key           # CA 私钥：只留在网关/离线保存，绝不分发
    ├── server.crt       # 网关服务端证书（SAN 含 gateway.example.com）
    └── server.key       # 网关服务端私钥
```

> `init --role both` 会同时生成 CA 与服务端证书；`--role agent` **不生成任何证书**，
> 需要从网关拷贝 `certs/ca.crt`。若加 `--mtls`，还会生成 `client.crt` / `client.key`
> 并把这些路径写入 `agent.toml`，但**不会自动启用 mTLS**：还要在 `server.toml` 中取消
> `[tls].client_ca` 注释，并把 `tunnel_listen` 与 `ingress_listen` 拆成不同端口
> （用 `--dir` 指定绝对目录时，写入 agent 的客户端证书路径也是绝对路径，拷贝到家庭端后
> 需按实际目录调整）。

`init` 已经写入随机的 API Key（`dlk_...`）和隧道 token（`agt_...`），并打印 API Key 与
next steps；下面的手工字段说明适用于自行搭建或替换令牌的场景。

> 配置里的证书/CA 相对路径以**配置文件所在目录**为基准解析，不是进程工作目录。
> 因此 `~/divertlink/server.toml` 中的 `certs/server.crt` 指向
> `~/divertlink/certs/server.crt`；把配置移到别处时记得同步调整路径或目录布局。

如需手动控制证书参数（例如额外客户端证书、SPKI pin），可单独使用 `gen-certs`：

```bash
# 生成 CA + 服务端证书，并在输出中打印 SPKI pin（可填入 agent.toml 的 [tls].pin）
divertlink gen-certs --dir certs --domain gateway.example.com

# 仅生成自签服务端证书（实验环境）：没有独立 ca.crt，agent 的 [tls].ca 指向
# certs/server.crt 即可（输出里会提示），或改用 pin / insecure（后者禁止生产）
divertlink gen-certs --dir certs --domain gateway.example.com --self-signed

# 生成客户端证书（mTLS 用；必须使用默认的 CA 模式——与 --self-signed 同时使用时
# 客户端证书会被静默忽略，因为自签服务端证书不能充当 mTLS CA）
divertlink gen-certs --dir certs --domain gateway.example.com --client
```

> 正式环境建议使用公网 CA（如 Let's Encrypt）签发的证书：把证书链与私钥路径填入
> `server.toml` 的 `[tls].cert` / `[tls].key`，并删除 `agent.toml` 的 `[tls].ca`
> 配置行以使用系统根证书（**删除该行**，不要写成空字符串）。证书续期后需重启网关。


## 4. 配置并启动网关

编辑 `~/divertlink/server.toml`。`init` 生成的是完整示例（含随机 Key/token、`/home`
隧道与注释掉的静态路由），通常只需核对域名、监听地址与证书路径；手工搭建时至少要配置
以下字段：

```toml
[server]
tunnel_listen = "0.0.0.0:443"     # agent 隧道监听
ingress_listen = "0.0.0.0:443"    # 公网入口；与上面相同则按 ALPN 共用 443
admin_listen = "127.0.0.1:9090"   # 管理端点，只监听本机
trust_proxy = false               # 网关前有可信 nginx/Caddy 时才设 true

[tls]
cert = "certs/server.crt"
key = "certs/server.key"
min_version = "1.3"

[[api_keys]]
name = "default"
key = "dlk_REPLACE_WITH_RANDOM_KEY"      # 客户端访问网关用的 Key

[[tunnels]]
id = "home"                               # 必须与 agent.toml 的 id 一致
token = "agt_REPLACE_WITH_RANDOM_TOKEN"   # 必须与 agent.toml 的 token 一致
mount = "/home"                           # 公网路径前缀
```

> `http_listen`（默认 `127.0.0.1:8080`）与 `admin_listen`（默认 `127.0.0.1:9090`）
> 会随默认值自动监听；它们只适合本机使用，不要暴露到公网。不需要时可用
> `admin_listen = "off"` / `http_listen = "off"`（也接受 `false`）显式关闭；删除字段则
> 回落到默认值。

如需替换或新增令牌（`init` 生成的配置文件里已是随机值，可跳过）：

```bash
divertlink token --prefix dlk_ --count 1   # 生成网关 API Key
divertlink token --prefix agt_ --count 1   # 生成 agent 隧道令牌
```

启动前自检并前台启动，观察日志：

```bash
divertlink check -c server.toml
divertlink server -c server.toml
```

看到监听 443 与配置加载成功的日志后，保持前台运行；生产环境请改用 systemd（见
[部署文档](deployment.md)）。管理端点验证（在网关本机执行）：

```bash
curl -fsS http://127.0.0.1:9090/healthz
curl -fsS http://127.0.0.1:9090/readyz
```

> `http_listen = "127.0.0.1:8080"` 是给本机 nginx/new-api 用的明文入口，不要暴露到公网。
> 若 443 已被 nginx 占用，可让 nginx 终止公网 TLS 并反代到 `http_listen`，同时给 agent
> 隧道分配独立端口；也可用 nginx `stream` 按 ALPN `divertlink/1` 做 TLS 直通，详见
> [部署文档](deployment.md)。
>
> 若配置了 `[[static_routes]]`（静态反代到本机 new-api 等），默认 `require_auth = true`，
> 与隧道一样要求网关 API Key（并执行 `allow_ips`/`rate_limit`/`max_concurrent`）；只有
> 显式设置 `require_auth = false` 时才只透传请求头、由上游自行校验令牌，此时
> `upstream` 只应指向回环/内网地址。详见 [协议与调用 §5.2](protocols.md#52-拓扑-b网关静态路由反代到-new-api)。

## 5. 配置并启动家庭端 agent

### 5.1 把文件拷贝到家庭端机器

在网关服务器上执行（替换家庭端用户名与地址）：

```bash
scp agent.toml alice@192.168.1.50:~/divertlink/
scp certs/ca.crt alice@192.168.1.50:~/divertlink/certs/
```

家庭端创建目录（如果 `certs/` 不存在）：

```bash
mkdir -p ~/divertlink/certs && cd ~/divertlink
```

### 5.2 编辑 agent.toml

`init --role both` 已经填好 `id` / `server` / `token` / `ca` / `pin`，通常只需把
`[target].url` 改成家庭端实际服务地址；下面是各字段含义（手工搭建时照此填写）：

```toml
[agent]
id = "home"
server = "gateway.example.com:443"          # 网关域名或公网 IP
token = "agt_REPLACE_WITH_RANDOM_TOKEN"     # 与 server.toml 的 tunnels.token 一致
heartbeat = "5s"
heartbeat_timeout = "15s"
heartbeat_mode = "app"

[tls]
ca = "certs/ca.crt"      # 自签部署用 gen-certs 生成的 ca.crt；网关用公网 CA 时删除本行
# pin = "<gen-certs 输出的 SPKI pin>"   # 可选：固定网关公钥，防中间人
# servername = "gateway.example.com"    # 可选：默认自动取 server 的 host
# 仅当网关启用 mTLS（server.toml 设置了 [tls].client_ca）时才需要，两者必须同时配置：
# client_cert = "certs/client.crt"
# client_key  = "certs/client.key"

[target]
url = "http://127.0.0.1:8000"   # 家庭端 vLLM；new-api 为 :3000，Ollama 为 :11434
request_timeout = "10m"
connect_timeout = "5s"
```

确认家庭端服务本身可访问：

```bash
curl -fsS http://127.0.0.1:8000/v1/models
```

### 5.3 校验并启动 agent

```bash
divertlink check -c agent.toml
divertlink agent -c agent.toml
```

agent 会主动向网关 443 发起 TLS 连接（ALPN `divertlink/1`），认证通过后网关日志会出现
隧道 `home` 上线相关的记录；`/status` 或 `/readyz` 可看到隧道在线状态。

> agent 掉线会自动按指数退避（带 jitter）重连，无需人工处理；家庭宽带 CGNAT 不影响，
> 因为连接始终由家庭端主动发起并保活。

## 6. 放行防火墙与 DNS

1. **DNS**：在域名服务商处为 `gateway.example.com` 添加 A 记录指向网关公网 IP：

   ```bash
   dig +short gateway.example.com
   # 期望输出网关公网 IP
   ```

2. **云安全组/本机防火墙**：只放行需要的端口：

   ```bash
   # 以 ufw 为例（公网仅放行 443；80 仅在需要 HTTP 跳转/ACME 时放行）
   sudo ufw allow 443/tcp
   sudo ufw status
   ```

   - 公网：`443/tcp`（单端口 ALPN 复用）；如采用独立端口，则放行 `tunnel_listen` 与
     `ingress_listen` 对应端口；`http_listen`、`admin_listen` 只监听回环，不需要放行。
   - 家庭端：无需入站放行，只需能出站访问网关 443。

3. 确认 443 没有被其他服务占用：

   ```bash
   sudo ss -ltnp 'sport = :443'
   ```

## 7. 验证

以下验证在**客户端机器**执行；若使用自签证书，需要拿到 `ca.crt` 并用 `--cacert` 指定
（或把 CA 导入系统信任库），否则 curl 会报证书错误。

### 7.1 非流式请求

```bash
curl -sS https://gateway.example.com/home/v1/models \
  -H "Authorization: Bearer dlk_REPLACE_WITH_RANDOM_KEY"

curl -sS https://gateway.example.com/home/v1/chat/completions \
  --cacert ca.crt \
  -H "Authorization: Bearer dlk_REPLACE_WITH_RANDOM_KEY" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "Qwen2.5-7B-Instruct",
    "messages": [{"role": "user", "content": "你好"}],
    "max_tokens": 32,
    "stream": false
  }'
```

期望返回家庭端模型列表与一段正常的 chat completion JSON。

### 7.2 流式请求（SSE）

```bash
curl -N -sS https://gateway.example.com/home/v1/chat/completions \
  --cacert ca.crt \
  -H "Authorization: Bearer dlk_REPLACE_WITH_RANDOM_KEY" \
  -H "Content-Type: application/json" \
  -H "Accept: text/event-stream" \
  -d '{
    "model": "Qwen2.5-7B-Instruct",
    "messages": [{"role": "user", "content": "从 1 数到 20，每行一个数字"}],
    "stream": true
  }'
```

期望逐行出现 `data: {...}` 并以 `data: [DONE]` 结束。用下面的命令确认是**真流式**
（每行带时间戳；若所有行时间戳几乎相同，说明被缓冲了）：

```bash
curl -N -sS https://gateway.example.com/home/v1/chat/completions \
  --cacert ca.crt \
  -H "Authorization: Bearer dlk_REPLACE_WITH_RANDOM_KEY" \
  -H "Content-Type: application/json" \
  -H "Accept: text/event-stream" \
  -d '{"model":"Qwen2.5-7B-Instruct","stream":true,
       "messages":[{"role":"user","content":"从 1 数到 20"}]}' \
| python3 -u -c '
import sys, time
t0 = time.monotonic()
while True:
    line = sys.stdin.readline()
    if not line:
        break
    if line.startswith("data:"):
        print(f"{time.monotonic() - t0:7.3f}s  {line.rstrip()[:100]}")
'
```

更详细的流式验证方法与 new-api 集成见 [协议与调用](protocols.md#6-sse-流式验证)。

### 7.3 网关本地检查项

```bash
# 隧道是否在线（返回 JSON 快照）
curl -sS http://127.0.0.1:9090/status

# Prometheus 指标（便于接入监控）
curl -sS http://127.0.0.1:9090/metrics

# ALPN 是否正确暴露
openssl s_client -connect gateway.example.com:443 -servername gateway.example.com \
  -alpn divertlink/1 </dev/null 2>/dev/null | grep -E 'ALPN|Verify return code'
```

## 8. 常见错误

| 现象 | 排查方向 |
| --- | --- |
| `bind: permission denied`（443） | 使用 root、`setcap` 或 systemd `AmbientCapabilities`；或先换 8443 演练 |
| 客户端证书校验失败 | 自签证书加 `--cacert ca.crt`；确认 `gen-certs --domain` 与访问域名一致；agent 端检查 `[tls].ca` / `servername` |
| agent 报 CA/证书文件读取失败 | 相对路径按**配置文件所在目录**解析：确认 `agent.toml` 同级有 `certs/ca.crt`，或改写绝对路径 |
| 配置校验报“unknown field” | 配置文件里有多余/拼写错误的字段；按报错删除或修正，`trust_proxy` 等字段大小写敏感 |
| `401 Unauthorized` | 客户端 Key 与 `[[api_keys]].key` 不一致；OpenAI SDK 发 `Authorization: Bearer`，Anthropic SDK 发 `x-api-key`（两者网关都接受） |
| `403 Forbidden` | Key 有效但 `allow_tunnels` 未包含目标隧道，或来源 IP 不在 `allow_ips`；经前置代理时检查 `trust_proxy` |
| `404 Not Found` | URL 少了 mount 前缀（如把 `/home/v1/...` 写成 `/v1/...`）；确认 `[[tunnels]].mount` |
| `413 Payload Too Large` | 请求体超过 `[limits].max_request_bytes`（默认 64 MiB） |
| `429 Too Many Requests` | 命中 `[[api_keys]].rate_limit` 或 `max_concurrent`；按需放宽或拆 Key |
| `502 Bad Gateway` | 隧道已连上但数据流或家庭端 target 失败（target 未启动/地址错/不可达）；看 agent 日志与 `/status` |
| `503 Service Unavailable` | agent 未上线、token 不匹配、隧道被禁用或未连接；看双端日志与 `/status` |
| 流式内容一次性返回 | 前置 nginx/CDN 缓冲；`curl` 未加 `-N`；见 [协议文档 §6](protocols.md#6-sse-流式验证) |
| agent 反复重连 | 网关地址/端口错误、防火墙拦出站、心跳超时过短、时钟偏差；用 `openssl s_client` 验证 TLS 与 ALPN |
| DNS 解析不到 | A 记录未生效或指向错误 IP；`dig` 验证 |

更多按状态码与日志关键词的排查见 [故障排查](troubleshooting.md)。

## 9. 下一步

1. 用 systemd / Docker 托管进程，并配置开机自启与日志轮转：见 [部署文档](deployment.md)。
2. 收紧安全设置：替换所有示例令牌、设置 `allow_tunnels` / `allow_ips` / `rate_limit`，
   按 [安全指南](security.md) 与 [上线检查清单](security.md#9-上线检查清单) 逐项检查。
3. 接入 new-api / one-api 做额度与渠道管理：见 [协议与调用 §5](protocols.md#5-与-new-api--one-api-集成)。
4. 按业务量调整并发、超时与心跳：见 [性能与容量](performance.md)。
5. 了解每个字段：见 [配置参考](configuration.md)。

## 10. 端到端冒烟验证（可选）

如果不想手工一步步排错，仓库自带端到端冒烟脚本 `scripts/smoke.sh`。它会在临时目录中
自动完成一次完整链路验证：

1. `divertlink init --role both` 生成双端配置与自签证书；
2. 改写为随机空闲端口，启动 python mock 模型服务、gateway 与 agent；
3. 用 curl 验证 `/v1/models`、`/v1/chat/completions` 透传，SSE 是否逐块到达（带耗时
   断言，能发现缓冲），免认证的 `/health` 是否放行，未认证请求是否返回 401；
4. 检查管理端点 `/healthz`、`/readyz`、`/status`、`/metrics`。

```bash
# 在仓库根目录执行；自动查找 ./target/release 或 ./target/debug/divertlink
./scripts/smoke.sh

# 也可以显式指定任意路径的二进制（例如已安装到 PATH 的版本）
./scripts/smoke.sh -b /usr/local/bin/divertlink

# 或通过环境变量指定
DIVERTLINK_BIN=/usr/local/bin/divertlink ./scripts/smoke.sh
```

依赖 `bash`、`curl`、`python3`（Linux/macOS/WSL/Git Bash）。脚本自动选择随机空闲端口、
在 `mktemp` 临时目录中运行，退出（含失败）时自动清理子进程与临时目录，不会占用或影响
本机已有服务。找不到二进制时先 `cargo build`，或用 `-b` 指定。
