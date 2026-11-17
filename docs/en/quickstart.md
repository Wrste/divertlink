# Connect your first gateway and agent

[中文](../quickstart.md) · [English index](README.md) · [Installation](installation.md)

You need a public gateway reachable on TCP 443, a home machine able to connect outbound, a running model service, and a hostname resolving to the gateway. Install the correct binary on **both** machines. The model needs no public IP or inbound forwarding. If 443 is already occupied, use the [reverse proxy layout](deployment.md#nginx-or-caddy-on-443).

## Prepare matching configuration

On the gateway, choose a new private directory and initialize once:

```bash
umask 077
mkdir -p "$HOME/divertlink-gateway"
divertlink init --role both --domain gateway.example.com --dir "$HOME/divertlink-gateway"
```

`init` prints generated credentials; run it in a private terminal. It creates matching `server.toml` and `agent.toml`, plus a private CA and server certificate under `certs/`. Do not use `--force` on an existing deployment.

The generated tunnel ID is `home`, mount `/home`, and model URL `http://127.0.0.1:8000`. In the gateway's `[[tunnels]]` table choose the upstream authentication mode:

```toml
# For a local model without authentication:
upstream_auth_header = "none"
```

For an authenticated model use `upstream_auth_header = "authorization"` and set `upstream_api_key` to its real key in the private file. `passthrough` deliberately forwards the client's gateway key to the model; it only works when that key is accepted there. See [authentication](security.md).

Copy **only** `agent.toml` and the public `certs/ca.crt` to a private directory on the home machine, preserving that relative layout. Use an already authorized SSH/file-transfer channel. Never transfer `server.toml`, `server.key`, or `ca.key` to the agent. For example, after creating the private destination directory:

```bash
scp "$HOME/divertlink-gateway/agent.toml" home-host:divertlink-agent/agent.toml
scp "$HOME/divertlink-gateway/certs/ca.crt" home-host:divertlink-agent/certs/ca.crt
```

On the home machine create `~/divertlink-agent/certs` first with private permissions. Edit `[target].url` in its agent config to match the actual service:

| Service | Typical base URL |
| --- | --- |
| vLLM / llama.cpp | `http://127.0.0.1:8000` (check the service's port) |
| Ollama | `http://127.0.0.1:11434` |
| LM Studio | `http://127.0.0.1:1234` |
| new-api | `http://127.0.0.1:3000` |

Keep `[agent].id/token` matched to the gateway, and its `server` set to `gateway.example.com:443`. Paths are relative to the config directory, not the current shell directory.

## Start both processes

Gateway:

```bash
divertlink check -c "$HOME/divertlink-gateway/server.toml"
divertlink server -c "$HOME/divertlink-gateway/server.toml"
```

Home machine, in another terminal:

```bash
divertlink check -c "$HOME/divertlink-agent/agent.toml"
divertlink agent -c "$HOME/divertlink-agent/agent.toml"
```

On Unix, binding port 443 requires permission. Use the [systemd installer](deployment.md#linux-systemd) on Linux. For an unprivileged trial set **both** gateway TLS listeners to `0.0.0.0:8443`, update the agent's server port and client URLs, and allow that TCP port. Do not run an entire desktop console as root. Keep admin 9090 and plaintext ingress 8080 on loopback.

`check` validates TOML; process startup and a real request are still required to validate certificates and connectivity. Ctrl+C stops foreground processes. For reboot persistence use a [service manager](deployment.md).

## Verify the route

On the gateway:

```bash
curl -fsS http://127.0.0.1:9090/healthz
curl -fsS http://127.0.0.1:9090/readyz
curl -fsS http://127.0.0.1:9090/status
```

`/healthz` proves the process is alive. `/readyz` is true with at least one connected tunnel, or no configured tunnels; inspect `/status` for **your** `home` tunnel and `connected: true`.

On the client, put the public CA certificate in a trusted local path. Set `DIVERTLINK_API_KEY` privately to `[[api_keys]].key` from `server.toml`, then:

```bash
curl --cacert /private/path/ca.crt -fsS \
  https://gateway.example.com/home/v1/models \
  -H "Authorization: Bearer $DIVERTLINK_API_KEY"
# Without the header the same URL must return 401.
```

For a public CA certificate clients use normal system trust and omit `--cacert`. Switching certificates also requires [updating agent trust](security.md#tls-trust). Never use `-k` as the deployed solution.

After selecting an actual model name, an optional streaming test consumes model resources:

```bash
curl --cacert /private/path/ca.crt -N -fsS \
  https://gateway.example.com/home/v1/chat/completions \
  -H "Authorization: Bearer $DIVERTLINK_API_KEY" \
  -H 'Content-Type: application/json' \
  -d '{"model":"YOUR_MODEL","stream":true,"messages":[{"role":"user","content":"Hello"}]}'
```

Tokens should arrive incrementally. Anthropic-only servers may not implement `/v1/models`; use their appropriate endpoint. The [agent skill verifier](agent-skill.md) automates readiness, selected-tunnel and OpenAI model-list checks without printing keys.

For a self-contained local test without a GPU or model quota, run the repository's `./scripts/smoke.sh -b /path/to/divertlink`. It starts a temporary mock model, gateway and agent, validates authentication, SSE and admin endpoints, then cleans up. If a step fails, use [troubleshooting](troubleshooting.md).
