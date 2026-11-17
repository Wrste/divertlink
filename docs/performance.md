# 性能与容量

DivertLink 的性能目标是：**LLM 推理的算力瓶颈在家庭端 GPU，网关只做极轻量的
HTTP 转发**。理解架构中的性能设计、知道如何测量基线、按容量调参，比记住某个固定
QPS 数字更重要。

- [1. 架构中的性能设计](#1-架构中的性能设计)
- [2. 延迟与开销预期](#2-延迟与开销预期)
- [3. 容量估算](#3-容量估算)
- [4. 压测方法](#4-压测方法)
- [5. 调优参数](#5-调优参数)
- [6. 操作系统与内核调优](#6-操作系统与内核调优)
- [7. 常见瓶颈](#7-常见瓶颈)

## 1. 架构中的性能设计

| 设计 | 性能收益 | 说明 |
| --- | --- | --- |
| 零缓冲流式转发 | SSE 首 token 延迟低，不在网关攒包 | 响应体按 chunk 透传，`stream_idle_timeout` 只用于回收僵死连接 |
| 单连接多路复用（yamux） | 无逐请求 TLS/握手成本，稳定不受 NAT 空闲回收影响 | 一个公网请求对应一条 yamux 数据流，流内是标准 HTTP/1.1 |
| 端到端背压 | 慢客户端/慢上游不会无限堆积内存 | yamux 流控把压力传回上游，写入阻塞而非缓存膨胀 |
| `Bytes` 零拷贝 | 减少大 body（如长上下文、音频上传）的拷贝与分配 | 内部实现使用 `tokio`/`hyper` 的字节缓冲类型 |
| 连接池 | 复用家庭端到 target 的连接，避免重复建连 | agent 以连接池方式访问 `target.url` |
| `TCP_NODELAY` | 小包低延迟（尤其 SSE 逐 token 输出） | 隧道与上游连接关闭 Nagle 合并 |
| 单端口 ALPN 分流 | 防火墙只开 443，减少额外代理跳数 | 隧道与入口共用一个 TLS 监听 |
| 逐跳头清洗与 `Connection: close` | 上游不需要处理长连接语义，转发行为确定 | 每个数据流对应一个 HTTP/1.1 请求/响应 |

> 多路复用窗口（yamux 流窗口、会话窗口）由传输库内部管理，配置文件**未暴露窗口大小
> 参数**；能控制并发上限的是 `max_streams`、`max_inflight` 与 `max_connections`
> （见第 5 节），另有 yamux 流上限由实现按 `max_streams_per_tunnel` 推算
> （约为其 2 倍并夹在 512–4096 之间）。

## 2. 延迟与开销预期

一次请求的端到端延迟由四段组成：

```text
客户端 ──公网 RTT──► 网关（TLS 终止 + 认证 + 路由） ──公网 RTT──► 家庭端 ──本机 RTT──► 上游
```

- **网关处理开销**：认证、限流、路由、头清洗与转发，通常在**亚毫秒到数毫秒**量级；
  与 GPU 推理耗时（数百毫秒到数十秒）相比可忽略。
- **隧道额外 RTT**：请求要先从网关到家庭端，再回传；首 token 延迟 ≈ 公网段两次 RTT +
  上游首 token 时间。家庭宽带上行带宽、跨运营商链路是主要变量。
- **流式开销**：SSE 数据逐块转发，网关不攒包；若出现“最后一次性返回”，先怀疑
  前置 nginx/CDN 缓冲，而不是网关。
- **吞吐开销**：网关不解析 JSON、不做压缩/解压，转发本身接近内存拷贝速度；实际吞吐
  取决于家庭端服务的生成速度与连接质量。

以上为设计预期，**请以第 4 节在你的链路上实测为准**。

## 3. 容量估算

### 3.1 有效并发公式

一条隧道同时在途的请求数由以下上限共同约束（取最小值）：

```text
有效并发 = min(
    隧道并发（[[tunnels]].max_streams，未配置时继承 [limits].max_streams_per_tunnel）,
    [agent].max_inflight,
    yamux 流上限（实现按 max_streams_per_tunnel*2 推算，夹在 512..=4096）,
    上游服务自身并发/GPU 承载能力
)
```

注意 `max_streams` 已经覆盖 `max_streams_per_tunnel`（前者默认继承后者），两者不是
独立相乘的闸门。`[limits].max_connections` 限制的是**每个监听器**各自的并发 TCP
连接数（客户端/new-api 到网关入口是一份、agent 到隧道端口是另一份、明文入口与管理
端点也各一份），不是 yamux 数据流数；HTTP/2 客户端可在一条 TCP 连接上并发多个请求。

### 3.2 带宽估算

LLM 纯文本场景的流量远小于算力消耗，可用以下量级估算（参考值，实际取决于模型与客户端）：

| 项目 | 估算 |
| --- | --- |
| 单个 token 的文本体积 | 约 2–5 字节（英文）/ 约 3–9 字节（UTF-8 中文，按分词而定） |
| SSE 单块开销 | 每个 chunk 携带 `data: {...}` JSON 与换行，非流式 chunk 化时开销占比更高 |
| 单流流量 | 以 50 token/s、每 chunk 约 150–400 字节计，约 **10–25 KB/s**；客户端处理慢时更低 |
| 100 并发流 | 约 **1–3 MB/s** 下行，远低于千兆网卡 |
| 大 Prompt/上传 | 受 `max_request_bytes` 限制（默认 64 MiB），是带宽与内存的主要突发来源 |

结论：**大多数场景的瓶颈是 GPU 与上游并发，而不是网关带宽**；真正需要提前规划的是
大 body（长上下文、批量 embedding、音频）与并发突发。

### 3.3 场景参考

| 场景 | 典型并发流 | 网关参考规格 | 建议配置 |
| --- | --- | --- | --- |
| 个人使用 | 1–5 | 1 核 1 GB 起 | 默认配置即可 |
| 小团队（< 20 人） | 10–30 | 2 核 2 GB | `max_streams` 32–64，`max_concurrent` 按人 |
| new-api 渠道 / 对外服务 | 50–200 | 2–4 核 4 GB | `max_streams` 128–256，按 Key 限流；监控 p99 |
| 高并发网关 | 300+ | 4 核 8 GB 起 | 按第 4 节压测扩容；关注 fd 上限与 TLS 握手 CPU |

> 规格为量级参考，不构成承诺；请按自己的压测结果扩容。家庭端 GPU 与上行带宽通常
> 先于网关成为瓶颈。

## 4. 压测方法

### 4.1 准备

- **不要在生产 Key 上直接压测**：新建一个专用 `[[api_keys]]` 条目（例如
  `name = "bench"`、不配置 `rate_limit`、`max_concurrent = 0`），压完删除。
- 压测家庭端推理有真实 GPU 成本：固定较小的 `max_tokens`，或使用本机 mock 上游
  （返回固定 JSON/SSE 的小服务）先测**纯隧道开销**。
- 自签证书压测时加 `--insecure`（oha）或 `--cacert`（curl），避免把证书校验算进失败率。
- 打开网关 `/metrics` 与系统监控（CPU、内存、网络、fd），压测时同步观察。

### 4.2 非流式吞吐与延迟

使用 [oha](https://github.com/hatoo/oha)（Rust 压测工具，参数与 `hey` 类似）：

```bash
oha -z 30s -c 8 --no-tui \
  -m POST \
  -H "Authorization: Bearer dlk_BENCH_KEY" \
  -T "application/json" \
  -d '{
    "model": "Qwen2.5-7B-Instruct",
    "messages": [{"role": "user", "content": "用 50 字介绍你自己"}],
    "max_tokens": 64,
    "stream": false
  }' \
  https://gateway.example.com/home/v1/chat/completions
```

参数说明：

| 参数 | 含义 |
| --- | --- |
| `-z 30s` | 持续压测 30 秒（也可用 `-n 500` 固定请求数） |
| `-c 8` | 并发 worker 数；逐步提升做并发阶梯 |
| `-m POST` / `-T` / `-d` | 方法、Content-Type、请求体 |
| `--no-tui` | 输出静态摘要，便于记录/对比 |
| `--json` | 机器可读结果，适合归档与回归对比 |
| `--insecure` | 自签证书实验环境跳过校验 |

解读输出：

- **Requests/sec**：网关+上游整体吞吐；与上游 GPU 吞吐对比，判断瓶颈位置。
- **Success rate**：出现非 2xx 时先看状态码分布（401/403/429/502/504 见
  [故障排查](troubleshooting.md)）。
- **Latency 分布（p50 / p90 / p99）**：p99 明显劣化通常意味着排队、并发打满或限流命中。
- **Size/req**：确认请求/响应体大小符合预期，避免误压大 body。

### 4.3 流式首 token 延迟（TTFB）

SSE 场景光看总耗时不够，应单独测首字节时间：

```bash
for i in $(seq 1 20); do
  curl -N -sS -o /dev/null \
    -w '%{http_code} %{time_starttransfer} %{time_total}\n' \
    --cacert ca.crt \
    -H "Authorization: Bearer dlk_BENCH_KEY" \
    -H "Content-Type: application/json" \
    -H "Accept: text/event-stream" \
    -d '{"model":"Qwen2.5-7B-Instruct","stream":true,
         "messages":[{"role":"user","content":"从 1 数到 40"}],"max_tokens":128}' \
    https://gateway.example.com/home/v1/chat/completions
done
```

用 awk 汇总 p50 量级（示例，按需改为更精确的统计）：

```bash
... | awk '{print $2}' | sort -n | awk '{a[NR]=$1} END {print "min",a[1],"p50",a[int((NR+1)/2)],"max",a[NR]}'
```

判读：

- `time_starttransfer` 小、`time_total` 明显更大 → 真流式；
- 两者接近且都很大 → 被缓冲（检查前置代理与客户端 `-N`）；
- 逐次 TTFB 抖动大 → 家庭端排队、GPU 抢占或链路抖动。

### 4.4 并发阶梯

从低到高逐级加压，每级稳定运行 30 秒并记录：

```bash
for c in 1 2 4 8 16 32; do
  echo "=== concurrency $c ==="
  oha -z 30s -c "$c" --no-tui -m POST -T "application/json" \
    -H "Authorization: Bearer dlk_BENCH_KEY" \
    -d '{"model":"Qwen2.5-7B-Instruct","messages":[{"role":"user","content":"hi"}],"max_tokens":16}' \
    https://gateway.example.com/home/v1/chat/completions \
  | grep -E 'Requests/sec|Success rate|Latency|p99|Status code'
done
```

观察转折点：吞吐不再增长、p99 陡增、出现 429/502/504 时的并发值，即为当前链路容量。
把 `max_streams` / `max_inflight` / `max_connections` / 上游并发中的一个或多个提高到
转折点之上，再复测确认瓶颈转移方向。

### 4.5 需要同步观察的指标

| 位置 | 观察项 |
| --- | --- |
| 网关 `/metrics` | 请求数、错误率、隧道在线状态、流式时延等（指标名以实际输出为准） |
| 网关系统 | CPU（TLS/拷贝）、RSS、网卡带宽、fd 使用量、TCP 重传 |
| agent | 进程 CPU/内存、与网关连接是否稳定、重连日志 |
| 家庭端服务 | GPU 利用率、KV cache 占用、请求排队时间、并发上限 |
| 链路 | 客户端↔网关、网关↔家庭端的 RTT 与丢包（`mtr`/`ping`） |

## 5. 调优参数

| 参数 | 位置 | 调优建议 |
| --- | --- | --- |
| `max_streams_per_tunnel` | `[limits]` | 网关侧单隧道并发总闸门；按 3.1 节的有效并发公式设置 |
| `max_streams` | `[[tunnels]]` | 单隧道更细的并发上限（未配置时继承 `max_streams_per_tunnel`）；对外重点隧道可单独放宽，其余收紧 |
| `max_inflight` | `[agent]` | 家庭端同时处理的请求上限；通常与 `max_streams` 对齐，避免网关放行但家庭端排队 |
| `max_connections` | `[limits]` | **每个监听器**各自的并发连接上限（隧道/入口、明文入口、管理端点分别计数）；客户端使用长连接/HTTP2 时可适当调低，防连接洪水 |
| `max_request_bytes` | `[limits]` | 大 Prompt/上传场景调大（如 128MiB），同时评估内存；仅文本对话可调小防滥用 |
| `request_timeout` | `[limits]` / `[[tunnels]]` | 覆盖最长生成耗时（含排队）；过小会截断长回答，过大则慢请求长期占用 |
| `stream_idle_timeout` | `[limits]` | 应大于上游两次输出之间的最大间隔；长思考模型/工具调用可能长时间无输出，需调大。**注意 agent 对 target 响应体有固定 300s 空闲上限（不可配置）**，把本值调到 300s 以上不会延长实际可空闲时间 |
| `heartbeat` / `heartbeat_timeout` | `[agent]` | 心跳超时建议 ≥ 3×心跳间隔；公网抖动大时可适当放宽，缩短则更快发现断线 |
| `connect_timeout` | `[agent]` | 家庭宽带上行慢/跨境链路可适当调大（如 15s） |
| `reconnect_min` / `reconnect_max` / `reconnect_jitter` | `[agent]` | 大量 agent 同时断线时保留 jitter，避免惊群；单机场景可缩短 `reconnect_min` 加快恢复 |
| `max_concurrent` / `rate_limit` | `[[api_keys]]` | 压测时先临时放宽，生产按配额收紧；压测专用 Key 用完删除 |
| `level` | `[log]` | 压测/高负载时用 `warn`/`info`，`debug`/`trace` 会增加 IO 与 CPU |

> 所有并发/超时参数之间要互相匹配：例如网关 `request_timeout` 小于上游实际生成时间，
> 表现为稳定的 504；`max_inflight` 小于网关 `max_streams`，表现为请求在家庭端排队。

## 6. 操作系统与内核调优

网关与家庭端在高并发下建议检查/调整以下项（按发行版实际路径与持久化方式配置）：

| 项目 | 建议 |
| --- | --- |
| 文件描述符 | systemd `LimitNOFILE=65535`；Docker `ulimits.nofile`；`ulimit -n` 验证 |
| 监听队列 | `net.core.somaxconn` 调大（如 4096）；`net.ipv4.tcp_max_syn_backlog` |
| 本地端口范围 | `net.ipv4.ip_local_port_range` 调大，避免高连接率下端口耗尽 |
| TIME_WAIT/连接回收 | `net.ipv4.tcp_tw_reuse = 1`（客户端/网关出向连接场景）；结合 `ss -s` 观察 |
| conntrack | 启用防火墙/NAT 时调大 `nf_conntrack_max`，或为 443 加 `NOTRACK` 规则 |
| 文件句柄/内存 | `fs.file-max`、`vm.overcommit_memory` 按主机实际情况评估 |
| 拥塞控制 | 公网链路可评估 `bbr`；跨境高 RTT 链路收益明显 |
| 内核 / 库 | 保持更新；TLS 使用 rustls（ring），不依赖系统 OpenSSL |

调整后务必复测并观察稳定性，不要一次改动多个变量。

## 7. 常见瓶颈

| 瓶颈 | 表现 | 处理 |
| --- | --- | --- |
| 家庭端 GPU/上游并发 | 吞吐上不去、家庭端排队、网关指标正常 | 降低并发或扩容 GPU；调整 `max_inflight` 与 `max_streams` |
| 家庭宽带上行 | 首 token 慢、大 body 上传慢、TTFB 抖动 | 升级上行带宽；减少单请求体积；就近选择网关地域 |
| 公网 RTT/丢包 | p99 抖动、偶发超时 | 用 `mtr` 定位链路；换网关地域/运营商；调整心跳与超时 |
| TLS 握手 CPU | 高新建连接率时网关 CPU 高 | 客户端复用连接（HTTP/2/长连接）；提高 `max_connections` 前先确认 CPU |
| 单 TCP 连接带宽 | 单流大响应变慢、多流互相挤占 | 多路复用在单连接上共享带宽；大文件场景评估多条隧道/分片 |
| 慢客户端背压 | 内存不涨但上游写入阻塞，整条流变慢 | 客户端需及时读取流；中间代理关闭缓冲 |
| 限流/并发限制 | 429 或请求排队 | 压测时放宽 bench Key 限制；生产按配额设计 |
| 日志/审计 IO | 高 QPS 下 CPU 与磁盘升高 | 网关用 `json` 集中采集、降低级别、异步采集 |
| 系统 fd/backlog 限制 | 新连接被拒、`accept` 错误 | 按第 6 节调整 `LimitNOFILE` 与 `somaxconn` |
| 前置代理缓冲 | 流式变批量、TTFB 等于总耗时 | nginx `proxy_buffering off` 等，见 [部署文档](deployment.md) |

> 测量优先：先用第 4 节的方法确认瓶颈在网关、链路还是上游，再改配置；不要凭感觉
> 同时调大所有上限，否则只会把瓶颈转移成超时与内存问题。
