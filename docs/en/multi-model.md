# Multiple models and agent replicas

[中文](../multi-model.md) · [English index](README.md) · [Example topology](../../examples/multi-model/README.md)

DivertLink routes by Host and path, not the JSON `model` field. Choose a layout based on whether you need independent credentials/processes, a few local ports, or model-aware billing and routing.

## Separate machines or independently managed models

Add a gateway `[[tunnels]]` table for each model with its own random token and mount, for example `qwen` at `/qwen` and `llama` at `/llama`. Each agent matches its tunnel ID/token and points `[target].url` to its own model service. Generate secrets with `divertlink token --prefix agt_`; store them privately rather than copying placeholders into production.

Clients use `https://gateway.example.com/qwen/v1` and `/llama/v1`. A gateway API key can restrict access using `allow_tunnels = ["qwen"]`. Set suitable key limits and tunnel concurrency per model. A route with `hosts` also requires a matching Host header, DNS and TLS certificate.

## One agent, several local services

Keep one gateway mount `/mixed`, and add local prefixes on its agent:

```toml
[target]
url = "http://127.0.0.1:8000"

[[targets]]
name = "qwen"
prefix = "/qwen"
url = "http://127.0.0.1:8001"

[[targets]]
name = "llama"
prefix = "/llama"
url = "http://127.0.0.1:8002"
```

Client `/mixed/qwen/v1/models` becomes `/v1/models` at port 8001: the gateway strips `/mixed`, then the agent strips `/qwen`. Longest complete-segment prefixes win; unmatched paths use `[target]`. Check `x-divertlink-target` in responses when diagnosing the selected local route.

All these local targets share one tunnel's permissions and stream budget. `allow_tunnels` does not isolate prefixes inside a tunnel. Use separate tunnels for independent access control. Configure target-specific headers privately when local services need different credentials.

## Model-name routing or quotas

Point an agent's `[target].url` to an existing new-api/LiteLLM service. Let that service handle `model` routing, accounting and provider-specific transforms. Alternatively, configure new-api as a client of separate DivertLink mounts. See [API integration](protocols.md).

## Replicas of the same model

Run several agents with the **same** gateway tunnel ID/token and equivalent model/API capabilities. The gateway keeps multiple sessions and selects them round-robin, with failover when a session cannot open a stream. This does not resume a generation or SSE stream that has already failed.

`max_streams` is shared by all sessions of a tunnel; `agent.max_inflight` is per agent. Choose values based on real backend capacity, not just the number of machines. Inspect `/status` for `agents` and `connected`, then verify each route. `/readyz` alone cannot prove every model or replica works. Rotate a leaked tunnel token across every replica; a stolen token can let another agent join the pool.
