---
name: divertlink-deploy
description: Deploy, configure, upgrade, and troubleshoot DivertLink gateways and home agents on Linux, macOS, or Windows. Use for encrypted LLM reverse tunnels, multi-model routing, systemd or Docker setup, and end-to-end verification. 中文：自动安装、搭建和排查 DivertLink 网关与家庭端隧道。
---

# DivertLink deployment

Deploy the topology the user requested and verify it against the actual hosts. Respond in the user's language. Keep existing credentials, certificates and routing unless the requested change requires updating them.

## Establish the deployment facts

Inspect available hosts before asking for missing facts: gateway versus home agent, OS/CPU, SSH or local access already authorized, domain and DNS, available listen ports, model service URL, certificate choice and service manager. Ask only for facts that block the chosen deployment. A request to prepare documentation or a plan does not authorize changing live servers.

Use the gateway's own TLS listener on 443 when free; `tunnel_listen` and `ingress_listen` can share it using ALPN. If nginx/Caddy already owns 443, keep that service: use a separate agent TLS port (such as 8443) and loopback HTTP ingress. An HTTP reverse proxy cannot forward the agent's custom TLS tunnel. Read [deployment.md](references/deployment.md) for the applicable mode and [configuration.md](references/configuration.md) before changing TOML.

## Install and configure

1. Prefer an existing verified binary, otherwise a published, pinned release from `Wrste/divertlink`. Inspect its assets; do not assume every platform was published. The initial v0.1.2 has Linux x86_64/ARM64 and signed/notarized macOS Intel/ARM64 assets; Windows ZIP availability must be checked. Use source build only when needed.
2. The helper [install_binary.py](scripts/install_binary.py) uses Python 3.9+ standard library; Python is an agent helper dependency, not a DivertLink runtime dependency. Run `--dry-run` first to inspect the target/URL/destination. A normal install downloads the checksum and archive, installs only the executable, verifies `version`, and refuses an existing destination. `--replace` is for an intended upgrade after stopping the relevant process. It never edits PATH, configuration, firewall, certificates or services. On macOS it verifies the embedded code signature. For a notarized offline container use the DMG documented in the project installation guide.
3. Generate fresh configuration with `divertlink init`; it generates distinct random gateway API keys and agent tunnel tokens. Use a new private directory (`umask 077` on Unix). Suppress secret-bearing `init` stdout from chat/logs. Do not run `init --force` or `install.sh --force-config` over an existing deployment. Parse/edit TOML with a real parser when available; do not globally replace a token, URL or `server` string across tables. Check first, then restart only the affected service.
4. Match gateway `[[tunnels]].id/token` to agent `[agent].id/token`. Agent `target.url` is the model service, not the gateway. With self-signed TLS copy only `ca.crt` and the relevant agent config to the agent; never copy `ca.key` or the server private key. With a public CA remove stale self-signed `ca` and `pin` settings after inspecting them. Never use `insecure=true` or `curl -k` to make a deployment pass.
5. Gateway API keys are separate from tunnel tokens and upstream model keys. Choose the upstream authentication mode deliberately. For a model without authentication use `upstream_auth_header="none"`; otherwise configure replacement with the upstream key or intentionally pass through the client key. Keep secrets in private files/environment; do not print them in the final report or commit them.
6. For systemd, acquire the full project tree (`deploy/install.sh` needs adjacent unit templates), inspect `--dry-run`, and prefer `--no-start` until target and TLS settings are complete. The script still enables boot startup. For Docker first inspect whether a published image exists, otherwise build locally. In a container, `127.0.0.1` refers to that container, not the host.

Example helper usage (resolve paths relative to this installed skill):

```bash
python3 scripts/install_binary.py --version 0.1.2 --bin-dir "$HOME/.local/bin" --dry-run
python3 scripts/install_binary.py --version 0.1.2 --bin-dir "$HOME/.local/bin"
```

Use `$HOME` only as its normal home-directory value. Do not assume the binary directory is in PATH.

## Verify the result

- Run `version` and `check -c` on each host. `check` validates configuration; it does not prove TLS files load or an upstream works. Review warnings; use `--strict` when they have been addressed.
- Start gateway and agent under the selected process manager. Check its logs and the requested tunnel's entry in `/status`. `/readyz` only means at least one configured tunnel is connected (or there are no tunnels); it does not prove every tunnel is ready.
- Run [verify.py](scripts/verify.py) on the gateway or through an authorized SSH forward. It checks loopback `/healthz` and `/readyz`, optionally the selected `--tunnel-id`, unauthenticated ingress rejection and authenticated `/v1/models`. It refuses non-loopback admin URLs, plaintext remote ingress, embedded URL credentials and redirects; TLS validation stays enabled. The default HTTPS ingress requires a TLS 1.3 capable Python SSL backend; old macOS system Python/LibreSSL may need replacement by a modern Python/OpenSSL build. `--ca` adds a self-signed CA. It reads the gateway API key from the named environment variable and emits only check statuses.

```bash
python3 scripts/verify.py --tunnel-id home \
  --ingress-url https://gateway.example.com/home/v1 \
  --key-env DIVERTLINK_API_KEY --ca /private/path/ca.crt
```

For Anthropic-only/custom upstreams without `/v1/models`, omit `--ingress-url` and manually verify an appropriate endpoint and the unauthenticated rejection; do not call the deployment fully verified based on readiness alone. Verify each requested route. Make a small streaming generation request only when within the user's testing scope; a model-list request does not verify SSE. The project `scripts/smoke.sh` uses a temporary mock service to validate the binary without spending model quota.

If startup fails, inspect the concrete error, correct it and retry once. If the same failure persists, stop mutating, preserve logs with secrets redacted, and report the blocker. During upgrades retain the previous binary/config backup and roll back if the upgraded service cannot recover. Do not replace TLS material or rotate credentials just to fix a connection failure.

## Deliver

Report host roles, installed version/architecture, binary and configuration paths, service state, public API base URL, TLS trust mode, checks performed and any incomplete checks. Refer to secrets by storage location or variable name. Distinguish a simulated local test from a successful deployment to the user's actual servers.

Project documentation: [中文](https://github.com/Wrste/divertlink/blob/main/docs/README.md) · [English](https://github.com/Wrste/divertlink/blob/main/docs/en/README.md). This skill is self-contained; use project sources for version-specific fields rather than assuming newer options exist in an older binary.
