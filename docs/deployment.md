# DivertLink 部署与运维指南

**中文** · [English](en/deployment.md) · [文档目录](README.md)

本文面向部署、运维与集成人员，覆盖 gateway（公网服务器）与 agent（家庭侧）的安装、
三种网络拓扑、证书、systemd / Docker 部署、new-api 对接、防火墙、升级回滚与排障。

- 架构背景见 [`docs/DESIGN.md`](DESIGN.md)；**配置字段与校验语义以 `src/config.rs` 和
  [`docs/configuration.md`](configuration.md) 为准**，CLI 以 `divertlink --help` 为准。
- 仓库自带资产：`Dockerfile`、`deploy/docker-compose.yml`、`deploy/systemd/`、
  `deploy/nginx/ingress.conf`、`deploy/Caddyfile`、`deploy/install.sh`、
  `.github/workflows/`、`Makefile`。

## 目录

1. [部署模型与端口](#1-部署模型与端口)
2. [快速开始（单端口 443 共用）](#2-快速开始单端口-443-共用)
3. [三种拓扑](#3-三种拓扑)
4. [systemd 部署](#4-systemd-部署)
5. [Docker 部署](#5-docker-部署)
6. [证书获取与管理](#6-证书获取与管理)
7. [new-api / one-api 对接](#7-new-api--one-api-对接)
8. [防火墙与网络](#8-防火墙与网络)
9. [升级与回滚](#9-升级与回滚)
10. [常见排障](#10-常见排障)
11. [附录：配置字段速查](#11-附录配置字段速查)

---

## 1. 部署模型与端口

DivertLink 由两个进程组成：

| 角色 | 位置 | 作用 |
| --- | --- | --- |
| `divertlink server`（gateway） | 有固定公网 IP 的服务器 | 监听 443（或分离端口），对外提供 TLS HTTP(S) 入口；接受 agent 的加密隧道 |
| `divertlink agent` | 家庭宽带内网 | 主动向 gateway 建立 TLS + yamux 隧道，把本地推理服务映射到公网 |

agent **主动出站**连接 gateway，因此家庭侧不需要公网 IP、端口映射或 DDNS。

默认端口（均可在配置中修改）：

| 字段 | 默认值 | 用途 | 是否应对公网开放 |
| --- | --- | --- | --- |
| `[server].tunnel_listen` | `0.0.0.0:443` | agent 隧道（TLS） | 是（agent 网络） |
| `[server].ingress_listen` | `0.0.0.0:443` | 公网 HTTP(S) 入口 | 是（API 客户端） |
| `[server].http_listen` | `127.0.0.1:8080` | 可选明文入口，供本机 nginx/Caddy 终止 TLS 后回源 | 否 |
| `[server].admin_listen` | `127.0.0.1:9090` | `/healthz` `/readyz` `/metrics` `/status` | **否**，仅本机 |

- `tunnel_listen` 与 `ingress_listen` 相同（默认都是 443）时，gateway 在同一个端口按
  TLS ALPN 分流：agent 使用 `divertlink/1`，HTTP 客户端使用 `h2` / `http/1.1`，
  防火墙只需放行 443。
- 若启用 mTLS（`[tls].client_ca` 非空），`tunnel_listen` 与 `ingress_listen` 必须是不同监听。
- 管理端点只监听回环，切勿映射到公网；容器部署时也只发布到 `127.0.0.1`。

前置条件：

- gateway：Linux（systemd 或 Docker）、固定公网 IP、可解析的域名（推荐）、TLS 证书。
- agent：Linux + systemd（或 Docker/手工运行）、能访问 gateway 的隧道端口、本地推理服务地址。
- 客户端：new-api / one-api / OpenAI SDK / Anthropic SDK 等。
- 部署后自检：`scripts/smoke.sh` 需要 bash、curl、python3（仅自检使用，不进入运行时）。

Windows、macOS 与 Linux 的下载、校验、PATH 和源码构建步骤见
[多平台安装指南](quickstart.md#2-安装-divertlink)。本篇 systemd 示例适用于 Linux。
维护者构建、签名和公证发行包见[发布指南](releasing.md)。

---

## 2. 快速开始（单端口 443 共用）

以下假设网关域名 `llm.example.com`、版本 `0.1.1`、agent 隧道 id `home`、mount `/home`。
单端口 ALPN 共用模式不需要 nginx/Caddy。

[v0.1.1](https://github.com/Wrste/divertlink/releases/tag/v0.1.1) 已提供 Linux/macOS
发行包，可直接执行下面的 `--version 0.1.1` 在线安装示例。也可按
[源码安装](quickstart.md#25-从源码安装所有平台)构建，再将 `--version 0.1.1`
替换为 `--binary ./target/release/divertlink` 离线安装。

### 2.1 在公网服务器安装 gateway

```bash
git clone https://github.com/Wrste/divertlink.git
cd divertlink
sudo ./deploy/install.sh --server --version 0.1.1 --domain llm.example.com
```

脚本会：下载并校验 release 二进制（SHA256）→ 安装到 `/usr/local/bin/divertlink` → 创建
`divertlink` 系统用户 → 创建 `/etc/divertlink`、`/etc/divertlink/certs`、
`/var/lib/divertlink` → 调用 `divertlink init` 生成 `/etc/divertlink/server.toml`
配置骨架与自签证书 → 安装并启用 `divertlink-server.service`。

首次生成配置时脚本会打印**隧道 token**与**入口 API Key**（只显示一次），请保存。
自签证书只用于让服务立即可用：公网部署请按 [2.2](#22-放置-tls-证书)替换为 Let's Encrypt 等可信证书。
脚本生成配置后会自动执行 `divertlink check` 校验，校验失败会直接报错退出。

离线安装：

```bash
sudo ./deploy/install.sh --server --binary ./divertlink-0.1.1-x86_64-unknown-linux-musl.tar.gz
```

### 2.2 放置 TLS 证书

公网入口必须使用可信证书（Let's Encrypt 等），否则 agent 与客户端需要额外配置 CA/pin。
证书放到 `server.toml` 的 `[tls].cert` / `[tls].key` 指向的路径（默认
`/etc/divertlink/certs/server.crt`、`server.key`）：

```bash
# 示例：certbot standalone 申请（需先临时停掉占用 80 端口的服务）
sudo certbot certonly --standalone -d llm.example.com
sudo install -d -m 0750 -o root -g divertlink /etc/divertlink/certs
sudo install -m 0644 -o root -g divertlink \
  /etc/letsencrypt/live/llm.example.com/fullchain.pem /etc/divertlink/certs/server.crt
sudo install -m 0640 -o root -g divertlink \
  /etc/letsencrypt/live/llm.example.com/privkey.pem /etc/divertlink/certs/server.key
sudo divertlink check -c /etc/divertlink/server.toml
sudo systemctl restart divertlink-server
```

证书细节（自动续期、自签/私有 CA、mTLS）见[第 6 节](#6-证书获取与管理)。

### 2.3 在家庭侧安装 agent

在 gateway 上取隧道 token：

```bash
sudo sed -n 's/^token = "\([^"]*\)".*/\1/p' /etc/divertlink/server.toml | head -n 1
```

在家庭机器上：

```bash
git clone https://github.com/Wrste/divertlink.git
cd divertlink
sudo ./deploy/install.sh --agent --version 0.1.1 \
  --gateway llm.example.com:443 \
  --token '<上一步的 token>'
```

然后编辑 `/etc/divertlink/agent.toml`，把 `[target].url` 改成家庭侧推理服务地址，
例如 vLLM `http://127.0.0.1:8000`、Ollama `http://127.0.0.1:11434`、
LM Studio `http://127.0.0.1:1234`、new-api `http://127.0.0.1:3000`：

```bash
sudo divertlink check -c /etc/divertlink/agent.toml
sudo systemctl restart divertlink-agent
sudo systemctl status divertlink-agent
journalctl -u divertlink-agent -f
```

agent 日志出现握手/会话建立且无重连错误，即表示隧道可用。

若网关使用自签证书（安装脚本默认生成）而非 Let's Encrypt，需要让 agent 信任它：
把网关的 `/etc/divertlink/certs/ca.crt` 复制到家庭机的 `/etc/divertlink/certs/ca.crt`，
并在 `agent.toml` 的 `[tls]` 中启用 `ca = "certs/ca.crt"`。可额外配置 `tls.pin`，
但 pin 不替代 CA 校验，自签部署仍需 CA：

```bash
sudo install -d -m 0750 -o root -g divertlink /etc/divertlink/certs
sudo install -m 0644 -o root -g divertlink ca.crt /etc/divertlink/certs/ca.crt
sudo divertlink check -c /etc/divertlink/agent.toml
sudo systemctl restart divertlink-agent
```

### 2.4 验证

```bash
# 管理端点（在 gateway 本机）
curl -fsS http://127.0.0.1:9090/healthz
curl -fsS http://127.0.0.1:9090/readyz
curl -fsS http://127.0.0.1:9090/status

# 通过公网入口调用（API Key 使用安装时打印的 dlk_...）
curl -sS https://llm.example.com/home/v1/models \
  -H "Authorization: Bearer dlk_xxx"
```

`/readyz` 在没有配置隧道或至少一条隧道连接时就绪；需在 `/status` 中确认指定 ID 的
`connected = true`，并逐条验证实际 API。自签入口请求需要 `--cacert /实际路径/ca.crt`。

### 2.5 部署后自检（scripts/smoke.sh）

仓库自带端到端冒烟脚本，可在**同机**验证二进制、配置、TLS、SSE、认证与管理端点是否正常
（它会用 `divertlink init` 生成临时配置与自签证书，并监听随机空闲端口，不会影响现有部署）：

```bash
# 依赖：bash、curl、python3
./scripts/smoke.sh -b /usr/local/bin/divertlink
# 输出形如：冒烟测试通过：15 项检查全部成功
```

覆盖项：OpenAI 兼容非流式请求、`GET /v1/models`、SSE 逐块实时到达（无缓冲）、
缺少/错误 API Key 返回 401、`x-api-key` 认证、`public_paths` 免认证路径、
管理端点 `/healthz` `/readyz` `/metrics` `/status`、明文 `http_listen` 入口。
失败时会打印 gateway/agent/mock 上游日志；加 `-k` 可保留临时目录用于排查。

---

### 2.6 图形控制台（可选，推荐用于第一次上手）

不想手改 TOML 时，可以用内置控制台在浏览器里完成配置与启停：

```bash
divertlink console --open      # 打开 http://127.0.0.1:17653/
```

- 「＋ 新建项目」生成双端配置与证书，并自动登记为 profile；
- 选中配置点「启动」，日志、在线隧道、流量、证书有效期都在同一界面；
- 「连接探测」可验证 agent → 网关的 TLS 握手与 SPKI pin；
- 控制台**只监听回环地址**，是交互式管理工具，不替代 systemd/Docker 常驻托管；
  远程使用 `ssh -L 17653:127.0.0.1:17653 user@gateway` 本地转发即可。

Windows / macOS / Linux 双击启动脚本见 `scripts/desktop/`，浏览器里还可「安装」为
PWA 桌面应用。完整说明（含安全模型与参考的开源 UI 项目）见
[图形化客户端](console.md)。

> 长期运行仍然使用 systemd（Linux）或 Windows 服务（如
> `nssm install divertlink "C:\divertlink\divertlink.exe" agent -c "C:\divertlink\agent.toml"`）；
> 控制台进程退出时，由它启动的子进程也会一起停止。

## 3. 三种拓扑

### 3.1 拓扑 A：单端口 443 ALPN 共用（推荐）

```
客户端 ──TLS(ALPN h2/http1.1)──┐
                              ├─ gateway:443 ──yamux 隧道── agent ──▶ 家庭侧推理服务
agent ──TLS(ALPN divertlink/1)┘
```

`server.toml`：

```toml
[server]
tunnel_listen = "0.0.0.0:443"
ingress_listen = "0.0.0.0:443"
http_listen = "127.0.0.1:8080"
admin_listen = "127.0.0.1:9090"
```

`agent.toml`：

```toml
[agent]
server = "llm.example.com:443"
```

要点：

- 公网只开放 443；agent 不需要额外端口。
- TLS 证书必须包含 agent 与客户端使用的域名；同一端口按 ALPN 分流，不需要反向代理。
- 适合绝大多数部署：协议透明，SSE 由 gateway 直接透传，少一层缓冲风险。

### 3.2 拓扑 B：隧道与入口分离端口

适合希望把公网 API 入口与 agent 隧道分开治理（独立防火墙策略、独立监听、
或由外层负载均衡只转发 ingress 端口）的场景。

`server.toml`：

```toml
[server]
tunnel_listen = "0.0.0.0:443"       # agent 隧道（可限制来源 IP）
ingress_listen = "0.0.0.0:8443"      # 公网 HTTP(S) 入口
http_listen = "127.0.0.1:8080"
admin_listen = "127.0.0.1:9090"
```

客户端访问 `https://llm.example.com:8443/home/...`，agent 连接 `llm.example.com:443`。
防火墙放行 443、8443。两张监听使用同一份 `[tls]` 证书；证书需同时覆盖两个访问方式。

> mTLS 场景必须使用本拓扑：`[tls].client_ca` 非空时 tunnel/ingress 不能同端口。

### 3.3 拓扑 C：nginx / Caddy 终止 TLS

把公网 TLS 交给 nginx 或 Caddy，gateway 只提供明文 `http_listen` 回源。
适用于已有 Web 入口、需要统一证书管理/访问日志/额外安全策略的环境。

同机部署（nginx/Caddy 占用 443）：

```toml
[server]
tunnel_listen = "0.0.0.0:8443"        # agent 直连 8443；与 ingress 共用 8443（ALPN 分流）
ingress_listen = "0.0.0.0:8443"
http_listen = "127.0.0.1:8080"        # nginx/Caddy 回源
admin_listen = "127.0.0.1:9090"
```

```bash
sudo cp deploy/nginx/ingress.conf /etc/nginx/conf.d/divertlink.conf
# 修改 server_name 与证书路径
sudo nginx -t && sudo systemctl reload nginx
```

Caddy 版本：

```bash
sudo cp deploy/Caddyfile /etc/caddy/Caddyfile
# 修改域名后：
sudo caddy validate --config /etc/caddy/Caddyfile
sudo systemctl reload caddy
```

若 nginx/Caddy 在**另一台机器**上，则 gateway 的 `tunnel_listen` 可保持 443，
只需把反代 upstream 指向 gateway 内网地址，并将 `http_listen` 修改为可被该机器访问的监听
（如 `0.0.0.0:8080`，仅在内网放行）。

关键约束：

- SSE 流式必须关闭代理缓冲：nginx 的 `proxy_buffering off;`、Caddy 的
  `flush_interval -1`（示例文件已包含），并**不要启用 gzip/encode**。
- 代理的读超时不得小于 `[limits].request_timeout`（默认 `10m`）。
- 纯明文拓扑（只有 `http_listen` + `[[static_routes]]`，TLS 由 nginx/Caddy 终止）不需要
  `tls.cert`/`tls.key`：这两项仅在启用 `tunnel_listen` 或 `ingress_listen` 时必填。
- 请求体上限与 `[limits].max_request_bytes`（默认 `64MiB`）保持一致：
  nginx `client_max_body_size 64m`、Caddy `request_body { max_size 64MB }`。
- 客户端 IP：nginx/Caddy 会添加 `X-Forwarded-For`。只有 `server.toml` 里设置
  `[server].trust_proxy = true` 时 gateway 才采信该头（`allow_ips`、按 IP 限流、审计日志
  才会看到真实客户端 IP）；该开关只能在可信代理后面开启，否则客户端可伪造来源 IP。
  不信任代理时保持默认 `trust_proxy = false`，或优先使用拓扑 A/B（gateway 直接终止 TLS）。
- 拓扑 C 的 `server.toml` 示例（在拓扑 A/B 基础上把 ingress 交给代理）：

  ```toml
  [server]
  tunnel_listen = "0.0.0.0:443"     # agent 直连（或按同机部署改 8443）
  # ingress_listen 整行删除 = 关闭 TLS 入口，公网流量全部走下面的 http_listen
  http_listen = "127.0.0.1:8080"    # nginx/Caddy 回源
  admin_listen = "127.0.0.1:9090"
  trust_proxy = true                # 仅当 nginx/Caddy 与 gateway 同机/可信
  ```

  > 关闭监听可用 `"off"` / `"none"` / `"disabled"` / `""` / `false` 任一写法
  > （如 `ingress_listen = "off"`、`admin_listen = false`）；删除字段则回落到默认值
  > （默认值是开启，不是关闭）。

### 3.4 TLS 直通 / 共用 443 模式说明

- **推荐**：让 gateway 自己终止 TLS（拓扑 A），利用 ALPN 在 443 上同时承载隧道与
  HTTP 入口，不需要任何反代。
- **TLS 直通**：若 443 必须与其它站点共用，且 gateway 必须自己终止 TLS，可用 nginx
  `stream` 模块的 `ssl_preread on;` 做纯 TCP 转发（nginx 不终止 TLS，ALPN 原样交给
  gateway，例如转发到 `127.0.0.1:8443`）。注意 nginx 的 `http` 与 `stream` 不能同时
  监听同一端口，故这种模式只能把 443 整体交给 `stream`，HTTP 站点需另行分流。
  `deploy/nginx/ingress.conf` 末尾有示例与注意事项。
- **代理层看到的源 IP**：多一层纯 TCP 透传后，gateway 只能看到代理地址；若需要真实
  客户端 IP 做 `allow_ips`，需使用拓扑 A/B，或在代理层完成访问控制。

---

## 4. systemd 部署

### 4.1 安装脚本

```bash
sudo ./deploy/install.sh --server|--agent|--both [选项]
```

| 选项 | 说明 |
| --- | --- |
| `--server` / `--agent` / `--both` | 必选（三选一），安装角色；`--both` 同机安装两端 |
| `--version <x.y.z>` | release 版本，默认查询 GitHub 最新 release |
| `--binary <路径>` | 离线安装：本地 release `tar.gz` 或已解压二进制 |
| `--prefix <目录>` | 二进制前缀，默认 `/usr/local`（即 `/usr/local/bin/divertlink`） |
| `--config-dir <目录>` | 配置目录，默认 `/etc/divertlink` |
| `--state-dir <目录>` | 运行时状态目录，默认 `/var/lib/divertlink` |
| `--user <用户名>` | systemd 服务用户，默认 `divertlink` |
| `--domain <域名>` | server：写入自签证书 SAN；agent：未给 `--gateway` 时的默认网关 |
| `--gateway <host[:port]>` | agent 连接的服务端地址，默认补 `:443` |
| `--token <token>` | 预置隧道 token（两端必须一致），缺省自动生成随机 token |
| `--unit-dir <目录>` | systemd 单元模板目录，默认脚本旁 `systemd/` |
| `--no-start` | 只启用开机自启，不立即启动/重启 |
| `--force-config` | 覆盖已存在配置骨架（会重新生成配置与自签证书） |
| `--dry-run` | 只打印计划，不修改系统 |

安装脚本用 `divertlink init` 生成配置骨架与自签证书，因此生成的 TOML 一定与程序内置
示例/校验规则一致；`--token` 与 `--gateway` 只在**本次生成**配置时写入，已有配置默认保留。

脚本幂等：重复运行会升级二进制、刷新 systemd 单元并重启服务；已有配置与证书保留。
release 资产带 SHA256，脚本下载后强制校验；离线可用 `--binary` 指向本地 tar.gz 或裸二进制。

### 4.2 手工安装（不使用脚本时）

```bash
# 1) 二进制
sudo install -m 0755 divertlink /usr/local/bin/divertlink

# 2) 服务用户
sudo groupadd --system divertlink
sudo useradd --system --gid divertlink --no-create-home \
  --home-dir /var/lib/divertlink --shell /usr/sbin/nologin divertlink

# 3) 目录
sudo install -d -m 0755 /usr/local/bin
sudo install -d -m 0750 -o root -g divertlink /etc/divertlink /etc/divertlink/certs
sudo install -d -m 0750 -o divertlink -g divertlink /var/lib/divertlink

# 4) 配置（参考 /etc/divertlink 下由 init 生成的示例，或按 DESIGN 手写）
sudo divertlink init --role server --dir /etc/divertlink --domain llm.example.com
sudo divertlink check -c /etc/divertlink/server.toml

# 5) systemd 单元
sudo install -m 0644 deploy/systemd/divertlink-server.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now divertlink-server.service
```

agent 同理，使用 `deploy/systemd/divertlink-agent.service` 和 `agent.toml`。

### 4.3 目录与权限

| 路径 | 权限/属主 | 内容 |
| --- | --- | --- |
| `/usr/local/bin/divertlink` | `0755 root:root` | 静态 musl 二进制 |
| `/etc/divertlink/` | `0750 root:divertlink` | 配置目录 |
| `/etc/divertlink/server.toml` / `agent.toml` | `0640 root:divertlink` | 含 token/API Key，勿公开读 |
| `/etc/divertlink/certs/` | `0750 root:divertlink` | TLS 证书与私钥 |
| `/etc/divertlink/certs/server.key` | `0640 root:divertlink` | 服务器私钥 |
| `/etc/divertlink/certs/ca.key` | `0600 root:root` | 自签 CA 私钥（仅签发时使用，服务不读） |
| `/etc/divertlink/certs/ca.crt` / `server.crt` | `0644 root:divertlink` | 证书/CA（agent 需要 ca.crt） |
| `/var/lib/divertlink/` | `0750 divertlink:divertlink` | 运行时状态 |

### 4.4 systemd 单元与安全加固

`deploy/systemd/divertlink-server.service` 要点：

- `User=divertlink`、`NoNewPrivileges=yes`；
- 只有 server 需要 `<1024` 端口，故 `AmbientCapabilities=CAP_NET_BIND_SERVICE` +
  `CapabilityBoundingSet=CAP_NET_BIND_SERVICE`；agent 单元将二者置空；
- `ProtectSystem=strict`、`ProtectHome=yes`、`PrivateTmp=yes` 等内核/文件系统收敛；
- `ReadWritePaths` 只放行状态目录 `/var/lib/divertlink`；配置与证书对服务只读
  （证书轮换由 root 更新文件后重启服务）；
- `Restart=on-failure`、`RestartSec=5s`、`StartLimitBurst=5`；
- 单元内使用 `EnvironmentFile=-/etc/divertlink/server.env` / `agent.env`（可选），
  可写 `DIVERTLINK_CONFIG`、`DIVERTLINK_SERVER`、`DIVERTLINK_TOKEN`、`DIVERTLINK_ID` 等。

查看加固评分：

```bash
systemd-analyze security divertlink-server.service
```

若把配置/状态放到非默认目录，请同步修改单元中的 `ReadWritePaths`、`WorkingDirectory`、
`StateDirectory`（安装脚本已对 `--prefix`/`--config-dir` 做替换）。

### 4.5 常用运维命令

```bash
sudo systemctl status divertlink-server.service
sudo journalctl -u divertlink-server.service -f
sudo systemctl restart divertlink-server.service
sudo divertlink check -c /etc/divertlink/server.toml
sudo divertlink server -c /etc/divertlink/server.toml --check   # 只校验退出
/usr/local/bin/divertlink version
```

agent 侧把 `server` 换成 `agent`；`divertlink agent -c agent.toml --once` 可单次运行便于调试，
`--quiet` 把日志级别降到 warn（server/agent 均支持），`--check` 只校验配置后退出。

---

## 5. Docker 部署

### 5.1 镜像

镜像为多阶段构建：alpine/musl 编译静态二进制，运行层为
`gcr.io/distroless/static-debian12:nonroot`（含 CA 证书、无 shell、非 root uid 65532），
内置静态 busybox 仅用于健康检查。

```bash
# 本地构建（当前平台），标签与下方 Compose 默认值一致
make docker TAG=latest

# 多架构构建（需 buildx；CI 的 release.yml 会按需推送到 ghcr）
docker buildx build --platform linux/amd64,linux/arm64 \
  -t ghcr.io/wrste/divertlink:dev .
```

可选：在 GitHub 仓库 Settings → Variables 设置 `ENABLE_GHCR=true`，
打 `v*.*.*` tag 后 release 工作流会推送 `ghcr.io/<owner>/divertlink` 多架构镜像。
未发布镜像时先执行上面的本地构建，再运行 Compose；不要直接拉取不存在的远程版本。

### 5.2 Compose 部署 gateway

```bash
cd divertlink
mkdir -p deploy/config deploy/certs

# 准备配置与证书：可在任意 Linux 机器用 `sudo ./deploy/install.sh --server --domain <域名>`
# 生成配置骨架与证书后复制过来（或直接手写）。相对证书路径按配置文件所在目录解析，
# 因此 certs/server.crt 与 /etc/divertlink/certs/server.crt 两种写法都能用：
sudo cp /etc/divertlink/server.toml deploy/config/
sudo cp -L /etc/divertlink/certs/server.crt deploy/certs/
sudo cp -L /etc/divertlink/certs/server.key deploy/certs/
sudo chown -R 65532:65532 deploy/config deploy/certs
chmod 0640 deploy/config/server.toml deploy/certs/server.key

docker compose -f deploy/docker-compose.yml up -d
docker compose -f deploy/docker-compose.yml ps
docker compose -f deploy/docker-compose.yml logs -f gateway
```

说明：

- `server.toml` 中 `[tls].cert` / `[tls].key` 可以写相对路径 `certs/server.crt` /
  `certs/server.key`（按配置文件所在目录解析，容器内等价于
  `/etc/divertlink/certs/server.crt`、`/etc/divertlink/certs/server.key`），
  也可以直接写这两个绝对路径；`deploy/install.sh` 生成的配置使用相对路径。
- 443 端口需要非 root 绑定：compose 同时设置了
  `cap_add: NET_BIND_SERVICE` 与 `sysctls: net.ipv4.ip_unprivileged_port_start=0`；
  内核 < 5.7 或不允许该 sysctl 时，去掉 sysctls、保留 cap 即可。
- 8080/9090 只发布到 `127.0.0.1`，不要暴露公网。
- 默认 bridge 网络下网关看到的客户端 IP 是 Docker 网关地址，`allow_ips` /
  按 IP 限流会失真；需要真实 IP 时改用 host 网络（见 compose 文末注释）。
- Compose bridge 模式下，容器内 `http_listen` / `admin_listen` 分别设为
  `"0.0.0.0:8080"` / `"0.0.0.0:9090"`，使宿主机端口映射可达；
  **宿主机**的 8080/9090 映射仍必须绑定 `127.0.0.1`。healthcheck 只证明进程存活。

不使用 Compose 的等价命令：

```bash
docker run -d --name divertlink-gateway \
  --restart unless-stopped \
  --read-only --tmpfs /tmp:size=16m \
  --cap-add NET_BIND_SERVICE \
  --sysctl net.ipv4.ip_unprivileged_port_start=0 \
  -p 443:443 -p 127.0.0.1:8080:8080 -p 127.0.0.1:9090:9090 \
  -v "$PWD/deploy/config:/etc/divertlink:ro" \
  -v "$PWD/deploy/certs:/etc/divertlink/certs:ro" \
  ghcr.io/wrste/divertlink:latest \
  server -c /etc/divertlink/server.toml
```

`deploy/docker-compose.yml` 内附 new-api 上游三种接法的注释；new-api 与 gateway
同机/同 compose 网络时，推荐 `[[static_routes]] upstream = "http://new-api:3000"`。

### 5.3 Docker 注意点

- 容器内 nonroot 用户（uid/gid 65532）必须能读配置与私钥；host 上相应 chown/chmod。
- 镜像根文件系统 `read_only: true`，日志走 stdout/stderr（`docker logs`）。
- 证书更新后重建/重启容器：`docker compose restart gateway`。
- agent 一般建议用 systemd 部署在家庭机；Docker 跑 agent 时需注意访问宿主机
  本地推理服务要使用 `--network host` 或 `host.docker.internal`（按平台支持）。

---

## 6. 证书获取与管理

### 6.1 公网可信证书（推荐 Let's Encrypt）

gateway 的 `tunnel_listen` / `ingress_listen` 必须使用客户端与 agent 信任的证书。
若两个 TLS 监听都关闭、只保留 `http_listen` 并把 TLS 交给前置 nginx/Caddy（拓扑 C），
则 `[tls]` 段可以完全不配置证书（`cert`/`key` 仅在启用上述 TLS 监听时必填）。
证书常见获取方式：

```bash
# 方式 1：standalone（需临时释放/停用占用 80 端口的服务）
sudo certbot certonly --standalone -d llm.example.com

# 方式 2：webroot（已有 80 端口 Web 服务一起放行 /.well-known/acme-challenge/）
sudo certbot certonly --webroot -w /var/www/html -d llm.example.com

# 方式 3：DNS-01（通配符/无法开放 80）
sudo certbot certonly --manual --preferred-challenges dns -d '*.llm.example.com' -d llm.example.com
```

把 `fullchain.pem`、`privkey.pem` 安装到配置引用路径：

```bash
sudo install -m 0644 -o root -g divertlink \
  /etc/letsencrypt/live/llm.example.com/fullchain.pem /etc/divertlink/certs/server.crt
sudo install -m 0640 -o root -g divertlink \
  /etc/letsencrypt/live/llm.example.com/privkey.pem /etc/divertlink/certs/server.key
sudo divertlink check -c /etc/divertlink/server.toml
sudo systemctl restart divertlink-server
```

自动续期（certbot 已自带 timer）建议加 deploy hook，在续期后重启服务加载新证书：

```bash
sudo certbot renew --dry-run
sudo tee /etc/letsencrypt/renewal-hooks/deploy/divertlink.sh >/dev/null <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
install -m 0644 -o root -g divertlink \
  /etc/letsencrypt/live/llm.example.com/fullchain.pem /etc/divertlink/certs/server.crt
install -m 0640 -o root -g divertlink \
  /etc/letsencrypt/live/llm.example.com/privkey.pem /etc/divertlink/certs/server.key
systemctl restart divertlink-server.service
EOF
sudo chmod 0755 /etc/letsencrypt/renewal-hooks/deploy/divertlink.sh
```

也可以让脚本直接在 certbot live 目录上做软链（注意 `--keep-until-expiring` 与路径权限）。

### 6.2 自签 / 私有 CA 证书

无公网域名或需要端到端私有信任时，用 `divertlink gen-certs` 生成 CA、服务器、客户端证书：

```bash
# 默认生成 CA + 服务器证书（推荐）：ca.crt/ca.key + server.crt/server.key
sudo divertlink gen-certs --dir /etc/divertlink/certs \
  --domain llm.example.com --client --force
# 只要单张自签证书（省掉 CA 环节）时加 --self-signed（该模式不能配 --client）
# sudo divertlink gen-certs --dir /etc/divertlink/certs --domain llm.example.com --self-signed --force
sudo chown root:divertlink /etc/divertlink/certs/*.crt /etc/divertlink/certs/server.key
sudo chmod 0644 /etc/divertlink/certs/*.crt
sudo chmod 0640 /etc/divertlink/certs/server.key /etc/divertlink/certs/client.key
sudo chown root:root /etc/divertlink/certs/ca.key
sudo chmod 0600 /etc/divertlink/certs/ca.key
```

命令会输出证书路径与 **SPKI pin**。把服务器证书/私钥放到 `[tls]` 引用路径，
再把 CA 证书复制到 agent 的 `/etc/divertlink/certs/ca.crt`、或在 agent 的
`[tls].pin` 填入 SPKI pin：

```toml
# agent.toml（自签 CA 场景二选一）
[tls]
ca = "/etc/divertlink/certs/ca.crt"
# 或只信任指定公钥：
# pin = "<gen-certs 输出的 SPKI SHA-256>"
insecure = false
```

`insecure = true` 仅在本地调试使用，会失去证书校验，严禁生产。

### 6.3 手工/商业证书

- 证书链按「服务器证书 → 中间 CA」顺序拼接为 PEM 放入 `[tls].cert`；私钥单独文件。
- 私钥权限 `0640 root:divertlink`，证书 `0644`。
- 证书 SAN 必须包含 agent 使用的 `[agent].server` host 以及客户端访问域名。
- `[tls].min_version` 可选 `1.2`/`1.3`，默认 1.3；它只作用于网关监听器（隧道的 TLS 服务端
  与公网入口），即老客户端可协商 1.2。**agent 的 TLS 客户端固定 TLS 1.3**，与 `min_version` 无关。
- 更换证书后执行 `divertlink check -c ...` 并重启；agent 会自动重连恢复。

### 6.4 mTLS（可选）

`[tls].client_ca` 指向客户端 CA 后，隧道/入口将校验客户端证书。按 DESIGN 要求，
启用时 `tunnel_listen` 与 `ingress_listen` 必须是不同监听（用拓扑 B）。

agent 侧用 `divertlink gen-certs --client`（或 `divertlink init --mtls`）生成客户端证书，
并在 `agent.toml` 的 `[tls]` 中配置（两个字段必须同时出现，配置校验会强制检查）：

```toml
[tls]
ca = "certs/ca.crt"
client_cert = "certs/client.crt"
client_key = "certs/client.key"
```

`divertlink check -c agent.toml` 只校验字段与文件路径的组合关系，不会读取证书内容；
正式启用前请确认证书链/私钥匹配，否则所有隧道连接都会握手失败。

### 6.5 证书安全

- 证书、私钥、CA、pin 一律不入库（仓库 `.gitignore` 已忽略 `certs/`、`*.pem`、`*.key`、`*.crt`）。
- 私钥只给 `root:divertlink` 可读；备份与权限审计纳入例行检查。
- 证书续期后核对文件时间与 `systemctl status`，避免旧证书继续服务。

---

## 7. new-api / one-api 对接

DivertLink 协议透明，入口同时兼容 OpenAI、Anthropic、vLLM 等 REST API，SSE 逐块透传。
以下三种接法可组合使用。

### 7.1 接法 ①：new-api 把 DivertLink 当上游渠道（推荐）

在 new-api 后台「渠道」新增：

| 渠道类型 | Base URL | 密钥 | 说明 |
| --- | --- | --- | --- |
| OpenAI | `https://llm.example.com/home` | `dlk_...`（server.toml 的 `[[api_keys]].key`） | 请求 `/v1/chat/completions` 等 |
| Anthropic | `https://llm.example.com/home` | 同上（同时支持 `x-api-key` 头） | 请求 `/v1/messages` |
| 自定义/OpenAI 兼容（vLLM） | `https://llm.example.com/home` | 同上，或按 `upstream_api_key` 替换 | `/health`、`/tokenize`、`/metrics` 等按需 |

- `mount = "/home"` 时，公网 URL 前缀是 `/home`；mount 在转发前被剥离，
  因此上游看到的标准路径是 `/v1/...`。
- 若给隧道配置了 `hosts = ["home.llm.example.com"]`，也可直接用
  `https://home.llm.example.com` 作为 Base URL（需该域名解析与证书覆盖）。
- 客户端认证：网关接受 `Authorization: Bearer <api_key>` 或 `x-api-key: <api_key>`；
  默认把原始认证头透传给家庭端（便于 vLLM `--api-key` / new-api 渠道鉴权），
  需要替换时在 `[[tunnels]]` 设置 `upstream_api_key` 与
  `upstream_auth_header = "passthrough" | "authorization" | "x-api-key" | "none"`。
- 多个隧道/多 API Key 时，用 `allow_tunnels` 把 key 绑定到隧道，用
  `allow_ips`、`rate_limit`、`max_concurrent` 做来源与配额控制。

```bash
curl -sS https://llm.example.com/home/v1/chat/completions \
  -H "Authorization: Bearer dlk_xxx" \
  -H "Content-Type: application/json" \
  -d '{"model":"qwen2.5-7b-instruct","messages":[{"role":"user","content":"hi"}],"stream":true}'
```

### 7.2 接法 ②：gateway 用 static_routes 反代到 new-api

new-api 部署在 gateway 本机或同 compose 网络，网关直接把某前缀反代给它（不走隧道）：

```toml
[[static_routes]]
mount = "/newapi"
upstream = "http://127.0.0.1:3000"
strip_prefix = true
require_auth = true       # 默认值；客户端需带网关 API Key
# require_auth = false    # 仅当 new-api 自行鉴权、且 upstream 只暴露在回环/内网时使用
```

客户端访问 `https://llm.example.com/newapi/...`，网关剥离 `/newapi` 后转发到 new-api。

`require_auth` 语义（默认 `true`）：与隧道一样校验网关 API Key，并应用该 key 的
`allow_ips` / `rate_limit` / `max_concurrent`（`allow_tunnels` 对 static route 不适用）。
只有显式设为 `false` 才完全透传、不做网关认证与限流——这会把认证责任全部交给 new-api，
因此只应在 `upstream` 指向回环/内网且 new-api 自身强制鉴权时使用。
Compose 部署时用 `upstream = "http://new-api:3000"`（服务名解析见
`deploy/docker-compose.yml` 注释）。注意该路径同样经过网关入口认证与限流，
若 new-api 期望自己的令牌，请在 new-api 侧配置渠道密钥或在网关侧调整认证策略。

### 7.3 接法 ③：agent target 指向家庭侧 new-api

家庭机运行 new-api，只让 agent 通过 tunnel 暴露它：

```toml
# agent.toml
[target]
url = "http://127.0.0.1:3000"
```

这样 new-api 不直接暴露公网，统一由 gateway 做 API Key、限流、审计；
外部使用 `https://llm.example.com/home/...`（base URL 与接法 ① 相同）。
适合个人/小团队把 new-api 部署在家庭内网，同时保留网关侧的统一入口。

### 7.4 选择建议

| 场景 | 推荐接法 |
| --- | --- |
| new-api 需要调用家庭侧模型 / 聚合多个家庭隧道 | ① |
| new-api 本身就跑在网关服务器上，希望被统一入口代理 | ② |
| new-api 在家庭内网、不希望公网直连 | ③（或 ① 与 ③ 组合） |
| 既有多家庭模型又有中心 new-api | ① + ② 组合，按 mount/host 路由 |

---

## 8. 防火墙与网络

最小放行集合：

| 端口 | 来源 | 说明 |
| --- | --- | --- |
| 443/tcp | 公网 | 拓扑 A：隧道 + HTTP 入口共用 |
| 8443/tcp | agent 与 API 客户端 | 拓扑 B/C 中的分离端口（按实际配置） |
| 80/tcp | 公网 | 仅 Let's Encrypt HTTP-01 / 跳转需要 |
| 8080/tcp | 仅回环 | 不要对公网开放 |
| 9090/tcp | 仅回环 | 绝不对公网开放 |
| 22/tcp | 管理来源 | SSH 加固/限制来源 |

`ufw` 示例（Ubuntu/Debian）：

```bash
sudo ufw default deny incoming
sudo ufw default allow outgoing
sudo ufw allow 22/tcp
sudo ufw allow 443/tcp
sudo ufw allow 80/tcp          # 仅签发/续期证书需要，可临时开放
sudo ufw enable
sudo ufw status verbose
```

`firewalld` 示例（RHEL/CentOS/Fedora）：

```bash
sudo firewall-cmd --permanent --add-service=ssh
sudo firewall-cmd --permanent --add-port=443/tcp
sudo firewall-cmd --permanent --add-port=80/tcp
sudo firewall-cmd --reload
sudo firewall-cmd --list-all
```

`nftables` 示例（默认拒绝入站，仅放行必要端口）：

```nft
table inet filter {
  chain input {
    type filter hook input priority filter; policy drop;
    ct state established,related accept
    iif "lo" accept
    tcp dport { 22, 443 } accept
    icmp type echo-request accept
  }
}
```

注意事项：

- **Docker 会绕过 ufw**：`ports` 发布到 0.0.0.0 的端口不经过 ufw 过滤。使用 Docker 时
  用 `DOCKER-USER` 链限制来源，或改用 host 网络 + 系统防火墙统一管理；
  compose 已把 8080/9090 只绑定到 `127.0.0.1`。
- 拓扑 B/C 中的 ingress 端口如果交给云负载均衡/四层代理，记得同时放行健康检查来源。
- 管理端点 `admin_listen` 保持 `127.0.0.1:9090`；远程查看用 SSH 隧道：
  `ssh -L 9090:127.0.0.1:9090 user@gateway`。
- IPv6 环境需同步放行（`ufw allow 443/tcp` 默认双栈；nftables 示例的 `inet` 表已覆盖）。
- agent 侧只需出站访问 gateway 隧道端口，无需入站放行。

---

## 9. 升级与回滚

### 9.1 升级前

1. 备份配置与证书：

   ```bash
   sudo tar -C / -czf /root/divertlink-backup-$(date +%F).tar.gz \
     etc/divertlink var/lib/divertlink
   ```

2. 记下当前版本与 release 资产：
   ```bash
   divertlink version
   ```

3. 下载新版本并校验（脚本已自动做 SHA256）：
   ```bash
   sha256sum -c divertlink-<version>-<target>.tar.gz.sha256
   ```

### 9.2 systemd 升级

```bash
cd divertlink && git pull   # 或重新下载部署资产
sudo ./deploy/install.sh --server --version 0.2.0
```

脚本会替换二进制、刷新 systemd 单元并重启服务；配置默认保留，如新版需要新字段，
先查看 release notes 再按需合并（必要时用 `--force-config` 生成新骨架对照）。

建议顺序：

1. 先升级 agent（各家庭节点可依次滚动，agent 会自动重连）；
2. 再升级 gateway（重启期间隧道短暂中断，agent 按指数退避自动恢复，
   客户端可能出现短暂 502/连接错误，建议客户端带重试）；
3. 验证：`/healthz`、`/readyz`、`/status`、一条真实 chat completions（含 SSE）；
   条件允许时跑一次 `./scripts/smoke.sh -b /usr/local/bin/divertlink` 做端到端自检；
4. 保留旧二进制以便回滚：升级前 `cp /usr/local/bin/divertlink /usr/local/bin/divertlink.bak`。

### 9.3 回滚

```bash
# 回滚到上一个 release（配置通常是兼容的）
sudo ./deploy/install.sh --server --version 0.1.1
sudo systemctl status divertlink-server
```

或手工回滚二进制后重启：

```bash
sudo install -m 0755 /usr/local/bin/divertlink.bak /usr/local/bin/divertlink
sudo systemctl restart divertlink-server
```

若升级伴随配置变更导致无法启动：

```bash
sudo tar -C / -xzf /root/divertlink-backup-<date>.tar.gz etc/divertlink var/lib/divertlink
sudo divertlink check -c /etc/divertlink/server.toml
sudo systemctl restart divertlink-server
```

### 9.4 Docker 升级/回滚

```bash
# 固定版本 tag 部署，避免 latest 漂移
DIVERTLINK_VERSION=0.2.0 docker compose -f deploy/docker-compose.yml up -d
docker compose -f deploy/docker-compose.yml ps

# 回滚
DIVERTLINK_VERSION=0.1.1 docker compose -f deploy/docker-compose.yml up -d
```

- 证书更新后重启容器加载新证书。
- 镜像为只读根文件系统，配置/证书通过卷挂载；升级不会改动卷内容。
- 离线环境可先 `docker save` / `docker load`，或使用本地构建的镜像 tag。

### 9.5 证书轮换

按[第 6 节](#6-证书获取与管理)更新 `/etc/divertlink/certs/server.crt|key` 后：

```bash
sudo divertlink check -c /etc/divertlink/server.toml
sudo systemctl restart divertlink-server   # agent 自动重连
```

---

## 10. 常见排障

先收集三类信息：

```bash
sudo systemctl status divertlink-server divertlink-agent
sudo journalctl -u divertlink-server -n 100 --no-pager
sudo journalctl -u divertlink-agent -n 100 --no-pager
sudo divertlink check -c /etc/divertlink/server.toml
```

| 现象 | 可能原因 | 排查与处理 |
| --- | --- | --- |
| agent 反复重连 / TLS handshake 失败 | 证书域名不匹配、CA/pin 配错、`insecure=false` 却用自签证书 | 核对 `[agent].server` 是否在证书 SAN；自签时配置 `[tls].ca` 或 `pin`；核对系统时间（证书有效期） |
| agent 连接超时 / connection refused | 防火墙未放行、端口写错、gateway 未监听 | 在 agent 机器 `nc -vz llm.example.com 443`；`ss -tlnp | grep 443`；检查云安全组 |
| hello 被拒 / 会话建立后立即断开 | token 不匹配、隧道 id 不存在、`enabled=false` | 对比 server `[[tunnels]].token` 与 agent `[agent].token`；确认 id/mount；确认 `enabled = true` |
| 公网请求 401 | API Key 错误、header 形式不对、来源 IP 不在 `allow_ips` | 分别试 `Authorization: Bearer` 与 `x-api-key`；核对 `[[api_keys]].key` 与允许隧道/IP |
| 404 | mount/hosts 路由不匹配、Base URL 少了前缀 | 请求 `https://域名/home/...`；检查 `mount`、`hosts`、`default`；确认 Base URL 含 `/home` |
| 413 Request Entity Too Large | nginx `client_max_body_size` 小于网关限制 | 二者都调到 ≥ 请求体（默认 64MiB）：网关 `max_request_bytes`、nginx/Caddy 对应上限 |
| SSE 变成整块返回/首字延迟高 | 代理启用了缓冲/压缩 | nginx `proxy_buffering off;`、Caddy `flush_interval -1`、关闭 gzip/encode；确认没有外层 CDN 缓冲 |
| 429 / 请求被限流 | `rate_limit`、`max_concurrent`、`max_connections` 触发 | 在 `[[api_keys]]` / `[[tunnels]]` / `[limits]` 调整；检查 `/metrics` |
| 502 / 503 | agent 离线、target 服务未启动、上游拒绝 | `curl http://127.0.0.1:9090/status` 看隧道状态；在 agent 机器直接 `curl target`；检查 agent 日志 |
| `/metrics` `/status` 打不开 | admin 监听在回环、被防火墙挡、端口不同 | 在 gateway 本机 `curl 127.0.0.1:9090/...`；远程用 SSH 隧道；确认 `admin_listen` |
| systemd 启动失败由于权限 | 服务用户读不到配置/私钥；自定义目录未加入 `ReadWritePaths` | `sudo -u divertlink test -r /etc/divertlink/server.toml`；核对属主/模式；调整单元路径 |
| Docker 内 443 绑定失败 | 非 root 绑定特权端口被拒 | 确认 `cap_add: NET_BIND_SERVICE` 或 `sysctl net.ipv4.ip_unprivileged_port_start=0` |
| Docker 内配置/证书读取失败 | 挂载文件属主不对（容器用户 65532） | `sudo chown -R 65532:65532 deploy/config deploy/certs`；私钥 0640 |
| 证书续期后仍用旧证书 | 未重启服务 / 未更新文件 | 检查文件 mtime；`systemctl restart divertlink-server`；核对 certbot deploy hook |
| 端口被占用 | 同机 nginx/Caddy 已绑定 443，或旧进程未退出 | `ss -tlnp`；按拓扑 C 改 `tunnel_listen` 或让 nginx 反代 |
| `divertlink check` 报 "invalid socket address" | 监听字段写成非法地址 | 关闭监听用 `"off"`/`false`；写地址必须是 `host:port`（如 `0.0.0.0:443`）。`tls.pin`/`tls.ca` 不需要时删除或注释 |
| install.sh 生成配置后 `check` 失败 | 使用了旧版脚本或手工改坏配置 | 用 `--force-config` 重新生成，或 `divertlink init` 生成对照；见 [4.1](#41-安装脚本) |

调试技巧：

```bash
# 单次运行 agent，前台看完整日志
sudo divertlink agent -c /etc/divertlink/agent.toml --once

# 校验配置后退出，不进服务
sudo divertlink server -c /etc/divertlink/server.toml --check

# 检查 ALPN/TLS（需 openssl）
openssl s_client -connect llm.example.com:443 -alpn divertlink/1 -servername llm.example.com
openssl s_client -connect llm.example.com:443 -alpn h2 -servername llm.example.com

# 管理端点
curl -fsS http://127.0.0.1:9090/healthz
curl -fsS http://127.0.0.1:9090/metrics

# 查看监听与隧道连接
ss -tlnp | grep -E ':(443|8080|9090)'
```

日志为 JSON 时（`[log].format = "json"`）可用 `journalctl -o cat` 配合 `jq` 过滤；
常规排障建议临时把 `[log].level` 设为 `debug`（或 `trace`）后重启复现，复现后改回 `info`；
日志过多时可用 `divertlink server/agent ... --quiet` 只保留 warn/error。

另可在同机运行 `./scripts/smoke.sh -b /usr/local/bin/divertlink` 验证"二进制 + 全链路"是否正常，
它不会读写 `/etc/divertlink`，适合排除部署环境（证书/路由/防火墙以外）的软件问题。

---

## 11. 附录：配置字段速查

完整示例与语义以 `src/config.rs`、[`docs/configuration.md`](configuration.md) 与
`divertlink init` 生成的配置为准。

### 11.1 server.toml

| 表/字段 | 默认/示例 | 说明 |
| --- | --- | --- |
| `[server] name` | `divertlink-gateway` | 实例名 |
| `[server] tunnel_listen` | `0.0.0.0:443` | agent 隧道的 TLS 监听；`"off"`/`false` 关闭 |
| `[server] ingress_listen` | `0.0.0.0:443` | 公网 HTTP(S) 入口；与 tunnel 相同则 ALPN 共用；`"off"`/`false` 关闭 |
| `[server] http_listen` | `127.0.0.1:8080` | 可选明文入口（前置代理回源）；`"off"`/`false` 关闭 |
| `[server] admin_listen` | `127.0.0.1:9090` | 管理端点；`"off"`/`false` 关闭 |
| `[server] trust_proxy` | `false` | 是否采信 `X-Forwarded-For`（仅可信反代后面开启） |
| `[tls] cert` / `key` | `certs/server.crt` / `certs/server.key` | TLS 证书/私钥；仅启用 `tunnel_listen`/`ingress_listen` 时必填 |
| `[tls] client_ca` | `""` | 可选 mTLS 客户端 CA；启用时 tunnel/ingress 必须不同监听 |
| `[tls] min_version` | `"1.3"` | `1.2` 或 `1.3` |
| `[limits] max_request_bytes` | `64MiB` | 请求体上限 |
| `[limits] request_timeout` | `10m` | 请求总超时 |
| `[limits] stream_idle_timeout` | `120s` | 流式空闲超时 |
| `[limits] header_read_timeout` | `10s` | 读取客户端请求头超时 |
| `[limits] max_streams_per_tunnel` | `256` | 单隧道并发流上限 |
| `[limits] max_connections` | `1024` | 连接上限 |
| `[log] level` / `format` | `info` / `text` | `text` 或 `json` |
| `[[api_keys]] name` | 任意 | API Key 名称 |
| `[[api_keys]] key` | `dlk_...`（`init`/`token` 生成） | 公网入口密钥 |
| `[[api_keys]] allow_tunnels` | `[]` | 允许的隧道 id，空 = 全部 |
| `[[api_keys]] allow_ips` | `[]` | 来源 CIDR，空 = 不限 |
| `[[api_keys]] rate_limit` | `600/min` | 空 = 不限 |
| `[[api_keys]] max_concurrent` | `0` | 0 = 不限 |
| `[[tunnels]] id` | `home` | 隧道 id，对应 agent |
| `[[tunnels]] token` | `agt_...`（`init`/`token` 生成） | 隧道 token，对应 agent |
| `[[tunnels]] mount` | `/home` | 入口前缀，转发前剥离 |
| `[[tunnels]] hosts` | `[]` | 可选 Host 路由 |
| `[[tunnels]] default` | `false` | 无匹配时兜底（全局仅一个） |
| `[[tunnels]] protocols` | `["openai","anthropic","vllm"]` | 声明协议能力 |
| `[[tunnels]] upstream_api_key` | `""` | 空 = 透传客户端认证头 |
| `[[tunnels]] upstream_auth_header` | `passthrough` | `passthrough`/`authorization`/`x-api-key`/`none` |
| `[[tunnels]] public_paths` | `["/health"]` | 免认证只读路径前缀 |
| `[[tunnels]] max_streams` | `128` | 单隧道客户端并发流上限 |
| `[[tunnels]] request_timeout` | `10m` | 隧道侧请求超时 |
| `[[tunnels]] enabled` | `true` | 是否启用 |
| `[[static_routes]] mount` | `/newapi` | 本地反代前缀 |
| `[[static_routes]] upstream` | `http://127.0.0.1:3000` | 固定上游地址 |
| `[[static_routes]] strip_prefix` | `true` | 是否剥离 mount |
| `[[static_routes]] require_auth` | `true` | 是否校验网关 API Key（`false` 仅当 upstream 自行鉴权且仅回环/内网可达） |
| `[[static_routes]] tls_ca` | 无（不配置） | upstream 为 https 时的额外 CA 文件；不配置用系统根证书 |
| `[[static_routes]] headers` | `{}` | 附加到该路由上游请求的静态头 |

### 11.2 agent.toml

| 字段 | 默认/示例 | 说明 |
| --- | --- | --- |
| `[agent] id` | `home` | 对应服务端隧道 id |
| `[agent] server` | `gateway.example.com:443` | 服务端隧道地址，接受 `https://`/`tls://` 前缀 |
| `[agent] token` | `agt_change_me` | 对应服务端隧道 token |
| `[agent] heartbeat` | `5s` | 心跳间隔 |
| `[agent] heartbeat_timeout` | `15s` | 心跳超时 |
| `[agent] heartbeat_mode` | `app` | `app`/`transport`/`both`/`none` |
| `[agent] connect_timeout` | `10s` | 连接超时 |
| `[agent] max_inflight` | `128` | 并发在途上限 |
| `[agent] reconnect_min` / `max` / `jitter` | `1s`/`60s`/`0.3` | 指数退避参数（`jitter` 是浮点数，不能加引号） |
| `[agent] protocols` | `["openai","anthropic","vllm"]` | 向网关宣告的协议能力 |
| `[tls] ca` | `certs/ca.crt` | 自签 CA；空 = 系统根证书 |
| `[tls] pin` | `""` | 可选 SPKI SHA-256（base64/hex） |
| `[tls] servername` | `""` | 默认取 server host |
| `[tls] insecure` | `false` | 仅调试 |
| `[tls] client_cert` / `client_key` | 无（不配置即关闭） | mTLS 客户端证书/私钥，必须成对出现；不需要时删除或注释 |
| `[target] url` | `http://127.0.0.1:8000` | 本地推理服务 |
| `[target] request_timeout` / `connect_timeout` | `10m`/`5s` | 上游超时 |
| `[target] tls_ca` | `""` | target 为 https 时的额外 CA |
| `[target] headers` | `{}` | 附加到上游请求的静态头 |
| `[log] level` / `format` | `info` / `text` | 日志级别/格式 |

环境变量覆盖：`DIVERTLINK_CONFIG`；agent 侧另有 `DIVERTLINK_SERVER`、
`DIVERTLINK_TOKEN`、`DIVERTLINK_ID`（可写入 systemd 的 `EnvironmentFile`）。

### 11.3 CLI 速查

```text
divertlink server   [-c server.toml] [--check] [--quiet]
divertlink agent    [-c agent.toml]  [--check] [--once] [--quiet]
divertlink init     [--role server|agent|both] [--dir .] [--domain example.com] [--mtls] [--force]
divertlink gen-certs --dir certs [--domain ...] [--self-signed] [--client] [--force]
divertlink token    [--prefix dlk_] [--count 1] [--bytes 24]
divertlink check    -c config.toml
divertlink version
```

`-c/--config` 也接受环境变量 `DIVERTLINK_CONFIG`；agent 另有 `DIVERTLINK_SERVER` /
`DIVERTLINK_TOKEN` / `DIVERTLINK_ID` 覆盖。

### 11.4 交付物与排障入口

| 路径 | 用途 |
| --- | --- |
| `Dockerfile` / `.dockerignore` | 静态 musl 多阶段镜像（distroless nonroot） |
| `deploy/docker-compose.yml` | gateway compose 示例（卷、健康检查、new-api 注释） |
| `deploy/systemd/divertlink-server.service` | gateway systemd 单元（加固、cap） |
| `deploy/systemd/divertlink-agent.service` | agent systemd 单元 |
| `deploy/nginx/ingress.conf`、`deploy/Caddyfile` | TLS 终止反代示例（SSE 配置） |
| `deploy/install.sh` | Linux 一键安装（幂等、离线、SHA256 校验、`--server/--agent/--both`） |
| `deploy/check-assets.sh` | 部署资产自检（YAML/install.sh/compose/关键契约，`make deploy-check`） |
| `scripts/smoke.sh` | 端到端冒烟自检（`make smoke`；部署后 `-b /usr/local/bin/divertlink`） |
| `.github/workflows/ci.yml` | fmt/clippy/test/doc 多平台 CI |
| `.github/workflows/release.yml` | tag 触发多平台打包 + 可选 ghcr 多架构镜像 |

管理端点（`admin_listen`）：`/healthz` 存活、`/readyz` 就绪、`/metrics` Prometheus
文本、`/status` 隧道状态 JSON；均为只读，且只应监听回环。

本地/CI 等价检查：

```bash
make check         # fmt-check + clippy -D warnings + test + doc + deploy-check
make smoke         # 构建 debug 二进制并跑端到端冒烟
make deploy-check  # 只校验部署资产（YAML、install.sh、compose、systemd/nginx 契约）
```
