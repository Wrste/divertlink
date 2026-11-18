# Troubleshooting

[中文](../troubleshooting.md) · [English index](README.md)

Check the actual failing hop: API client → gateway, gateway → agent, then agent → model. Record versions and review redacted process/service logs before changing configuration. `divertlink check -c <file>` validates configuration but does not make network requests or load every runtime resource.

| Symptom | Next check |
| --- | --- |
| Download 404 | Confirm version, CPU and available assets on Releases; Windows ZIP/GHCR availability depends on the published Release assets |
| Permission denied on 443 | Use the Linux systemd capability or an unprivileged port on both listeners and agent/client URLs |
| Agent cannot connect | DNS, outbound firewall, gateway listen port and cloud security group |
| TLS unknown issuer / name mismatch | Correct CA, full certificate chain, SNI/SAN, validity and system clock; never bypass verification |
| Pin mismatch | Verify intended public key through a trusted channel; update a rotated pin, do not disable checks blindly |
| mTLS failure | Separate tunnel/ingress listeners, gateway client CA and paired agent client cert/key |
| Agent rejected | Match `agent.id/token` with an enabled gateway tunnel; API keys are unrelated |
| 401 | Missing/invalid gateway key, or an upstream rejection after authentication; inspect auth replacement mode |
| 403 | Key's `allow_tunnels`/`allow_ips`, source IP visibility and proxy trust |
| 404 | Eligible Host, mount, full-segment target prefix and upstream path; avoid duplicated `/v1` |
| 405 / 413 | Allowed HTTP methods / configured request-body limit |
| 429 | Gateway/global/key limits, tunnel stream capacity, or an upstream limit; inspect logs and metrics |
| 502 / 504 | Model reachable from the agent's host/container, correct upstream key, response-header and idle timeouts |
| 503 | Requested tunnel has no usable session or is disabled; inspect its `/status` entry |
| SSE arrives at once | Front-proxy buffering/compression, flush settings and client buffering (`curl -N`) |
| Container alive but admin unreachable | Container listener must bind its interface; host published admin port must stay loopback |
| One replica works but another fails | Verify each agent's model capabilities, target URL and session status |

On the gateway:

```bash
curl -fsS http://127.0.0.1:9090/healthz
curl -fsS http://127.0.0.1:9090/readyz
curl -fsS http://127.0.0.1:9090/status
curl -fsS http://127.0.0.1:9090/metrics
# Linux units:
sudo journalctl -u divertlink-server -n 100 --no-pager
# Run on the agent host for that role:
sudo journalctl -u divertlink-agent -n 100 --no-pager
```

`/readyz` accepts any connected tunnel (or an empty tunnel configuration), so inspect the requested ID in `/status`. Liveness alone does not prove an upstream responds. The [skill verifier](agent-skill.md) checks a selected tunnel and optional authenticated OpenAI models endpoint. It deliberately refuses remote plaintext ingress and redirects.

A normal HTTP reverse proxy or CDN cannot forward the agent's custom TLS ALPN. Use direct gateway TLS, or a deliberately configured TCP/TLS passthrough infrastructure that preserves the required protocol. Do not route the agent to a standard nginx/Caddy HTTPS virtual host.

For temporary detail set `[log].level = "debug"` and restart the affected process. Keep output private and redact credentials before sharing. Retry after a concrete correction; repeated identical failures require a new diagnosis, not forced CA regeneration or token rotation. Restore the previous binary/configuration if an upgrade caused a regression.

Use `./scripts/smoke.sh -b /path/to/divertlink` to isolate binary behavior with a temporary mock model. A passing local smoke test does not establish real DNS, firewall, certificate renewal or host service behavior. Unsupported WebSocket/gRPC/HTTP3 requests need another transport.
