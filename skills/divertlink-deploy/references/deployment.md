# Deployment modes

## Linux systemd

Clone the complete source tree from `https://github.com/Wrste/divertlink.git` to obtain `deploy/install.sh` and `deploy/systemd/`. Select an existing release version explicitly. On each host inspect the appropriate plan:

```bash
sudo ./deploy/install.sh --server --version 0.1.2 --domain gateway.example.com --no-start --dry-run
sudo ./deploy/install.sh --server --version 0.1.2 --domain gateway.example.com --no-start
```

The script installs binary `/usr/local/bin/divertlink`, configs `/etc/divertlink`, state `/var/lib/divertlink`, user/group `divertlink` and unit templates. Existing configs are preserved unless `--force-config` is used. `--no-start` enables boot startup but skips the immediate restart. For an existing config, back it up privately before editing, and remember already running services are not stopped by this flag.

Gateway: choose public CA certificates for a public deployment, or explicitly use generated self-signed CA with client trust. Stage replacement certificate/key copies with permissions readable by the service group, `check -c`, then start the gateway. Generated `ca.key` is root-only; do not transfer it. Allow the selected TLS ingress/tunnel port, keep 8080/9090 loopback. The template grants only low-port binding capability to server, not to agent.

Home agent: inspect gateway tunnel token securely, then set it using a private config/management channel. The script's `--token` option is available, but avoid putting real tokens in agent tool arguments or chat; an alternative is install agent with `--no-start`, then edit `agent.toml` privately to use the gateway token. No port forwarding is required. Copy the public CA when self-signed. Set `target.url`, check, start `divertlink-agent`, and check the selected tunnel on the gateway.

```bash
sudo ./deploy/install.sh --agent --version 0.1.2 --gateway gateway.example.com:443 --no-start
# Set matching tunnel token, model URL and CA/pin before starting.
sudo /usr/local/bin/divertlink check -c /etc/divertlink/agent.toml
sudo systemctl start divertlink-agent
sudo systemctl status divertlink-agent --no-pager
sudo journalctl -u divertlink-agent -n 50 --no-pager
```

The agent installer generates an independent token if none is supplied; that token does not automatically match the gateway. Never infer success from unit installation alone.

## Manual foreground / macOS / Windows

Use the skill installer for available binaries or build with current Rust stable and native C/C++ linker (`cargo build --release --locked`). Windows needs MSVC Build Tools + Windows SDK; initial v0.1.2 has no Windows ZIP. No additional runtime is needed by Unix release binaries. macOS DMG is signed, notarized and stapled; tar.gz contains the same signed executable. Do not disable Gatekeeper globally.

On a fresh gateway directory, use `init --role both` to obtain matching configs. Transfer only `agent.toml` and `certs/ca.crt` to the home agent; for mTLS also transfer that agent's client cert/key and adjust absolute paths. The agent never needs `server.toml`, server key or CA key. Choose the correct agent binary for its own OS/CPU.

Run `divertlink server -c <absolute-server-config>` and `divertlink agent -c <absolute-agent-config>` in separate managed processes. Ctrl+C stops a foreground process. `console --open` is interactive; exiting console stops its children. For unattended operation use an existing service manager; do not claim `nohup` alone provides reboot persistence. DivertLink is not a native Windows Service; use a configured service wrapper or Task Scheduler when asked, and verify the wrapper's restart behavior.

## Docker / Compose

Prefer the repository `deploy/docker-compose.yml`, keeping loopback admin publishing. Check whether `ghcr.io/wrste/divertlink:<version>` exists before pulling. Otherwise build a local image (`make docker TAG=latest`) and use the matching tag; the initial release did not publish GHCR images. Prepare configs/certs, mount them read-only, grant container uid/gid 65532 read access, set container HTTP/admin listeners to 0.0.0.0:8080/9090 while keeping host published ports loopback-only, then `docker compose ... config` before `up -d`.

The agent in a container cannot reach a host model through its own `127.0.0.1`. Use a reachable host/network address appropriate for that Docker host, and avoid publicly exposing the model port. Reuse user-selected networking rather than changing host firewall rules blindly.

## nginx / Caddy already owns 443

Keep proxy HTTPS on 443, gateway agent TLS on 8443, loopback `http_listen` on 8080, admin on 9090. Agents connect directly to 8443 over DivertLink TLS; API clients use proxy 443. Gateway still needs its own certificate for agent TLS even if Caddy issues a different ingress certificate. Do not point the agent at a normal HTTP reverse proxy endpoint. Enable `trust_proxy` only for the trusted proxy path and retain SSE no-buffer/flush configuration from repository examples.

## Verification and recovery

`verify.py --admin-url http://127.0.0.1:9090 --tunnel-id <id>` must run on the gateway, or through an SSH loopback forward such as `ssh -L 19090:127.0.0.1:9090 <gateway>`. Add public ingress checks with the correct gateway API key environment variable; self-signed ingress needs `--ca`.

| Symptom | Inspect |
| --- | --- |
| 401 | Gateway API key and selected upstream auth mode; do not use tunnel token as API key. |
| Agent TLS failure | Reachable TLS port, SNI/SAN, correct CA/pin, cert validity, clock. |
| 503 / readiness false | Agent service/logs, matching ID/token, outbound reachability, selected tunnel enabled. |
| 404 | Full-segment mount, restrictive hosts list, actual upstream path and model protocol. |
| 502 / 504 | Model service address from the agent host/container, upstream authentication and timeouts. |
| SSE buffered | Proxy compression/buffering and flush; validate with a scoped streaming request. |

Back up the current binary/config/certs before upgrades, validate replacement then restart only the affected unit. Restore the previous binary/config and restart if verification fails. Do not rotate credentials or regenerate CA material as an automatic retry.
