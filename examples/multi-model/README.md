# 多模型 / 多对一示例

本目录演示把**多个大模型**收敛到一个公网 DivertLink 网关的三种方式（完整说明见
[../../docs/multi-model.md](../../docs/multi-model.md)）：

| 文件 | 角色 | 说明 |
| --- | --- | --- |
| `server.toml` | 网关 | 3 条隧道（`/qwen`、`/llama`、`/mixed`）+ 3 个 API Key（按模型限权/限流） |
| `agent-qwen.toml` | 家庭机器 A | 单模型 Qwen（vLLM `:8000`），隧道 `qwen` |
| `agent-llama.toml` | 家庭机器 B | 单模型 Llama（vLLM `:8001`），隧道 `llama` |
| `agent-mixed.toml` | 家庭机器 C | 一台机器多个本地服务：默认 `:8000` + `/qwen`→`:8001` + `/llama`→`:8002` |

## 拓扑

```text
           /qwen/*    ── tunnel qwen   ── agent-qwen   ── vLLM :8000 (Qwen)
客户端 ─► 网关 :443 ─┬ /llama/*  ── tunnel llama  ── agent-llama  ── vLLM :8001 (Llama)
                    └ /mixed/*  ── tunnel mixed  ── agent-mixed  ─┬ :8000 默认上游
                                                                  ├ :8001 /qwen/*
                                                                  └ :8002 /llama/*
```

另外：把同一份 `agent-qwen.toml` 复制到多台机器（id/token 相同）即可对 `/qwen`
做多机 round-robin + 故障转移。

## 快速开始

```bash
# 1) 生成证书（网关机器），把 server.toml 里的 cert/key 指向生成的证书
divertlink gen-certs --dir certs --domain gw.example.com
divertlink check -c server.toml

# 2) 网关
divertlink server -c server.toml

# 3) 各家庭机器（各自拷贝对应 agent.toml + certs/ca.crt）
divertlink agent -c agent-qwen.toml    # 机器 A
divertlink agent -c agent-llama.toml   # 机器 B
divertlink agent -c agent-mixed.toml   # 机器 C

# 4) 验证
curl -fsS https://gw.example.com/qwen/v1/models  -H "Authorization: Bearer <team-qwen 的 key>"
curl -fsS https://gw.example.com/llama/v1/models -H "Authorization: Bearer <team-llama 的 key>"
curl -i  https://gw.example.com/mixed/qwen/v1/models -H "Authorization: Bearer <all-models 的 key>"
# 观察响应头：x-divertlink-target: qwen
```

## 说明与注意事项

1. 示例中的 `dlk_*_REPLACE_ME_*` / `agt_*_REPLACE_ME_*` 必须替换为随机密钥：
   `divertlink token --prefix dlk_`、`divertlink token --prefix agt_`；
2. `certs/` 不在仓库中（`.gitignore` 已忽略），请用 `gen-certs` 或 Let's Encrypt 生成；
3. 每条隧道可独立设置 `max_streams` / `request_timeout` / `hosts`；
4. `/mixed` 的前缀路由只对**路径**生效，不解析请求体里的模型名；按模型名路由请用
   new-api/LiteLLM 作为家庭端统一入口（见文档方案 C）；
5. `check` 会对示例占位密钥与会提示告警（不影响启动），上线前务必替换并可用
   `divertlink check --strict` 做门禁。
