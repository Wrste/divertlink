# Configuration decisions

Use `divertlink init --role both --domain <gateway-host> --dir <new-dir>` to obtain matching random credentials and CA material. The directory must be private before init because generated files inherit the process umask. Do not use a complete example with placeholder secrets in production.

| Setting | Operational meaning |
| --- | --- |
| `server.tunnel_listen` / `server.ingress_listen` | Both default to `0.0.0.0:443`; identical means ALPN sharing. mTLS requires different ports. |
| `server.http_listen` / `server.admin_listen` | Default `127.0.0.1:8080` / `127.0.0.1:9090`. Keep loopback; `"off"` or `false` disables, removing the field restores its default. |
| `server.trust_proxy` | False by default. True only behind a trusted local/private proxy with direct ingress controlled. |
| Server `tls.cert` / `tls.key` | PEM leaf/full chain and private key, required for TLS listeners. SAN must cover the hostname. |
| Server `tls.client_ca` | Optional mTLS; paired agent `tls.client_cert` / `tls.client_key` required. `init --mtls` creates files but does not enable server mTLS or split ports. |
| `api_keys[].key` | Public client authentication via Bearer or `x-api-key`; distinct from tunnel token. `allow_tunnels` restricts tunnel IDs, empty means all. |
| `tunnels[].id/token` | Must match `agent.id/token`. Multiple agents with the same pair join the session pool. |
| `tunnels[].mount` | Path prefix stripped before forwarding. Match by full path segment. A route needs mount, hosts or `default=true`. |
| `tunnels[].hosts` | Restrictive Host allow-list; requires DNS and certificate SAN. If also using mount both constraints apply. |
| `tunnels[].upstream_auth_header` | `passthrough` (default), `none`, `authorization` or `x-api-key`. Replacement uses `upstream_api_key`. |
| `tunnels[].public_paths` | Relative to mount, bypasses gateway key checks. Only explicit non-sensitive read-only endpoints, not `/v1`. |
| `agent.server` | Gateway TLS `host:port`; `https://` and `tls://` also accepted. HTTP/WS tunnels unsupported. |
| Agent `tls.ca` / `tls.pin` | Trust a private CA with `ca`; otherwise bundled public roots are used. Optional `pin` additionally checks SPKI after normal chain/hostname/validity validation. Remove or update stale pins during certificate migration. |
| Agent `tls.servername` | SNI override if connecting by IP but certificate is for a DNS name. |
| `target.url` | Default model base URL, e.g. Ollama `http://127.0.0.1:11434`, vLLM `:8000`, LM Studio `:1234`, new-api `:3000`. |
| `targets[]` | Additional `name`, `prefix`, `url`; longest complete segment prefix wins, stripped before forwarding. Unmatched paths use `target.url`. |

Paths resolve relative to the TOML file directory. Optional paths and pin values must be omitted/commented to disable them, not set to `""`. Unknown fields are rejected. Durations use strings such as `"10m"`; bytes use `"64MiB"`. Config and cert changes require process restart; there is no reload signal.

For a public API key configure a bounded `rate_limit` (e.g. `"600/min"`), `max_concurrent` and optionally `allow_ips`; use per-client keys. `max_streams` is shared across all sessions in a tunnel; agent `max_inflight` is per agent.

Multi-model choices:

- Separate machines/models: separate tunnel IDs and mounts (`/qwen`, `/llama`) with independent tokens.
- One agent, multiple local ports: one tunnel mount (`/mixed`) plus agent prefixes (`/qwen`, `/llama`); public URL `/mixed/qwen/v1` becomes local `/v1`.
- Model-name routing, billing or quotas: point `target.url` to existing new-api/LiteLLM. DivertLink never inspects JSON `model`.
- Same model replicas: same tunnel ID/token across agents, gateway selects sessions; there is no guarantee of resuming an already failed SSE stream.

Complete maintained schema: `src/config.rs`, `assets/*.example.toml` in the chosen repository revision. Fetch/read those only when advanced fields are needed.
