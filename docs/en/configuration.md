# Configuration reference

[中文完整参考](../configuration.md) · [English index](README.md)

Use `divertlink init --role both --dir <new-directory> --domain <gateway-host>` for matching credentials, then edit the relevant tables. Annotated templates: [server](../../assets/server.example.toml), [agent](../../assets/agent.example.toml). The authoritative schema for the checked-out version is [src/config.rs](../../src/config.rs).

TOML rejects unknown fields. Duration strings include `"5s"` and `"10m"`; byte sizes include `"64MiB"`. Relative certificate paths resolve from the **config file's directory**. Omit/comment optional path and pin fields to disable them; empty strings are invalid. Configuration and certificate changes require a process restart; there is no hot-reload signal.

## Gateway

`[server]`:

| Field | Default | Meaning |
| --- | --- | --- |
| `name` | `"divertlink-gateway"` | Display name |
| `tunnel_listen` | `"0.0.0.0:443"` | Agent TLS listener |
| `ingress_listen` | `"0.0.0.0:443"` | Public HTTPS; same address as tunnel enables ALPN sharing |
| `http_listen` | `"127.0.0.1:8080"` | Plaintext ingress for a trusted local proxy |
| `admin_listen` | `"127.0.0.1:9090"` | Health, readiness, status and metrics |
| `trust_proxy` | `false` | Trust forwarded client IP only behind a controlled proxy |
| `shutdown_grace` | `"10s"` | Graceful request/stream draining |

Each listener accepts `"off"` or `false` to disable it. Omitting it restores its default. Listener addresses are socket IP addresses with ports, not DNS hostnames.

`[tls]`: `cert = "certs/server.crt"`, `key = "certs/server.key"`, `min_version = "1.3"` by default. Public HTTPS can allow `"1.2"`; the agent tunnel stays TLS 1.3. Optional `client_ca` enables agent mTLS and requires separate tunnel and ingress listeners.

`[limits]`:

| Field | Default |
| --- | --- |
| `max_request_bytes` | `"64MiB"` |
| `request_timeout` | `"10m"` (response headers) |
| `stream_idle_timeout` | `"120s"` (quiet response body) |
| `header_read_timeout` | `"10s"` |
| `max_streams_per_tunnel` | `256` |
| `max_connections` | `1024` per listener |
| `allowed_methods` | `GET, POST, PUT, PATCH, DELETE, HEAD, OPTIONS` |
| `global_rate_limit` | unset; e.g. `"3000/min"`, before authentication |

Each `[[api_keys]]` requires unique `name` and `key`. Optional `allow_tunnels = []` and `allow_ips = []` mean unrestricted tunnel IDs and source IPs respectively; `allow_ips` accepts CIDRs. `rate_limit` is optional, and `max_concurrent = 0` means unlimited. Clients send `Authorization: Bearer <key>` or `x-api-key: <key>`.

Each `[[tunnels]]` requires `id` and `token`, matched by the agent:

| Field | Default / behavior |
| --- | --- |
| `mount` | Optional path prefix, stripped before forwarding; matches complete segments |
| `hosts` | `[]`; a nonempty list restricts Host headers |
| `default` | `false`; at most one fallback, cannot combine with mount |
| `protocols` | `[]`; protocol metadata, not JSON inspection |
| `upstream_api_key` | unset; used by authentication replacement |
| `upstream_auth_header` | `"passthrough"`; also `"none"`, `"authorization"`, `"x-api-key"` |
| `public_paths` | `[]`; relative-to-mount prefixes bypass gateway authentication |
| `max_streams` | inherits `limits.max_streams_per_tunnel`, shared across sessions |
| `request_timeout` | inherits gateway header timeout |
| `enabled` | `true` |

Provide mount, hosts or a default route. Host restrictions are applied before path selection; eligible path routes use the longest full-segment mount, then fallback. Multiple agents with the same ID/token join a round-robin session pool; they must offer equivalent services. Existing failed streams are not resumable.

Optional `[[static_routes]]` directly proxies HTTP(S) without an agent: required `mount` and `upstream`; `strip_prefix = true`, `require_auth = true`, optional `tls_ca` and `headers = {}`. `allow_tunnels` does not scope static routes. Disable gateway authentication only if the private upstream enforces its own client authentication.

## Agent

`[agent]` requires `id`, `server` and `token`. `server` accepts TLS `host:port`, `tls://host:port` or `https://host:port`; an ordinary HTTP proxy is not a tunnel endpoint.

| Field | Default |
| --- | --- |
| `heartbeat` / `heartbeat_timeout` | `"5s"` / `"15s"` |
| `heartbeat_mode` | `"app"`; also `"transport"`, `"both"`, `"none"` |
| `connect_timeout` | `"10s"` |
| `max_inflight` | `128` per agent |
| `reconnect_min` / `reconnect_max` | `"1s"` / `"60s"` |
| `reconnect_jitter` | `0.3` |
| `protocols` | `["openai", "anthropic", "vllm"]` metadata |

Agent `[tls]` has optional `ca`, `pin`, `servername`, `client_cert`, `client_key`; `insecure = false`. Without `ca`, the agent uses bundled public roots. With `ca`, it uses the supplied CA bundle. An optional pin adds an SPKI check **after normal certificate chain, hostname and validity checks**; it does not replace CA validation. Obtain the CA and pin through a trusted channel. See [security](security.md).

`[target]` defaults to `url = "http://127.0.0.1:8000"`, `request_timeout = "10m"`, `connect_timeout = "5s"`, optional `tls_ca`, `headers = {}`. This is the local model service, not the public gateway.

Each additional `[[targets]]` requires unique `name`, `prefix`, `url`; `enabled = true`. It inherits target timeouts unless overridden; TLS CA and headers are configured separately per target (unset CA uses public roots, headers default to empty). At most 64 additional targets are accepted. The longest full-segment prefix wins and is stripped; `/` is not an additional prefix. Unmatched requests fall back to `[target]`.

## Logging and environment

Both roles use `[log]`: `level = "info"`, `format = "text"` or `"json"`. Keep secret-bearing init output and private configuration out of shared logs.

`DIVERTLINK_CONFIG` supplies the server/agent config path when `-c` is absent; agent also accepts `DIVERTLINK_SERVER`, `DIVERTLINK_TOKEN`, `DIVERTLINK_ID`. Confirm precedence against your binary's `--help` when scripting.

```bash
divertlink check -c /private/path/server.toml
divertlink check -c /private/path/agent.toml
# Once warnings have been reviewed/resolved:
divertlink check --strict -c /private/path/server.toml
```

Validation does not establish reachability or prove certificate loading. Complete the [quickstart verification](quickstart.md#verify-the-route) after startup. For advanced validation rules and worked examples consult the [expanded Chinese reference](../configuration.md).
