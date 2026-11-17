# Deployment and operations

[中文](../deployment.md) · [English index](README.md) · [First connection](quickstart.md)

Choose gateway and agent roles separately. Only the gateway needs a reachable public TLS port; the home agent connects outbound. Reuse existing service managers and certificates where possible.

## Linux systemd

Use a complete repository checkout: `deploy/install.sh` loads templates from `deploy/systemd/`, so downloading that script alone is insufficient. Requires Linux, systemd and root. Inspect the plan before installing:

```bash
git clone https://github.com/Wrste/divertlink.git
cd divertlink
sudo ./deploy/install.sh --server --version 0.1.0 \
  --domain gateway.example.com --no-start --dry-run
sudo ./deploy/install.sh --server --version 0.1.0 \
  --domain gateway.example.com --no-start
```

This installs `/usr/local/bin/divertlink`, private configuration/certificates in `/etc/divertlink`, state in `/var/lib/divertlink`, the `divertlink` service account and systemd units. Existing configuration is preserved unless `--force-config` is explicitly used. **`--no-start` still enables boot startup**; it only skips the immediate start/restart, and does not stop already running services. Installer output can contain generated credentials; keep it private.

Inspect `/etc/divertlink/server.toml`, configure the selected TLS certificates and authentication, and ensure the non-root service can read them (`root:divertlink`, mode `0640` for copied config/private key). The server unit grants low-port binding capability. The agent unit does not need it.

```bash
sudo /usr/local/bin/divertlink check -c /etc/divertlink/server.toml
sudo systemctl start divertlink-server
sudo systemctl status divertlink-server --no-pager
sudo journalctl -u divertlink-server -n 50 --no-pager
```

On the home machine, use the same full checkout:

```bash
sudo ./deploy/install.sh --agent --version 0.1.0 \
  --gateway gateway.example.com:443 --no-start
# Privately edit agent.toml: match gateway ID/token, choose target.url and TLS trust.
sudo /usr/local/bin/divertlink check -c /etc/divertlink/agent.toml
sudo systemctl start divertlink-agent
```

Without `--token` the installer creates an independent random token; it will not match the gateway automatically. Set the matching value in the private config before starting. With self-signed TLS transfer only the public `ca.crt` to the agent and set its CA path. Avoid passing real tokens in command histories or agent tool arguments. No inbound home port is needed.

For offline installation use `--binary /path/to/archive.tar.gz` or an extracted executable from a release you have already verified. Open only the chosen gateway TLS ports in the host firewall and cloud security group. Keep 8080/9090 loopback.

## macOS and Windows

Use the [native installation instructions](installation.md), then run each role with an absolute config path:

```bash
divertlink server -c /private/path/server.toml
divertlink agent -c /private/path/agent.toml
```

Foreground operation and `console --open` are useful interactively. Exiting console stops its managed children. For unattended macOS operation use a user-selected launchd service with absolute binary/config/log paths and verify restart and login/reboot behavior. DivertLink is not itself a native Windows Service: use Task Scheduler or a configured service wrapper, checking account permissions and restart behavior. `nohup` alone does not provide boot persistence.

Use an unprivileged TLS port such as 8443 for desktop trials; change both gateway TLS listeners, the agent endpoint and API URL consistently.

## Docker Compose

The initial v0.1.0 release has **no published GHCR image**. Build locally from the repository instead of assuming the Compose image can be pulled:

```bash
make docker TAG=latest
# Prepare deploy/config/server.toml and deploy/certs/ first.
docker compose -f deploy/docker-compose.yml config
docker compose -f deploy/docker-compose.yml up -d
docker compose -f deploy/docker-compose.yml logs -f gateway
```

The Makefile's local image tag matches Compose's default `ghcr.io/wrste/divertlink:latest`. For another tag set `DIVERTLINK_VERSION` consistently. See [Compose](../../deploy/docker-compose.yml) and [Dockerfile](../../Dockerfile).

Set the container's HTTP/admin listeners to `0.0.0.0:8080` and `0.0.0.0:9090` so published ports reach them; the Compose **host** mappings bind these ports to `127.0.0.1`. Keep TLS on 443. Mount configuration and certificates read-only, readable by container uid/gid 65532. Do not expose admin to the public network. The image's health check proves process liveness, not a working model route.

A containerized agent's `127.0.0.1` refers to itself. Use a reachable model service/container address or host address appropriate to the platform; do not publish the home model publicly. Bridge networking can obscure client IPs; review `allow_ips` and trusted proxy settings. Linux host networking has different exposure rules; inspect the comments in Compose before changing it.

## nginx or Caddy on 443

If an existing HTTP reverse proxy owns 443, keep it and set gateway listeners explicitly:

```toml
[server]
tunnel_listen = "0.0.0.0:8443"
ingress_listen = "off"
http_listen = "127.0.0.1:8080"
admin_listen = "127.0.0.1:9090"
trust_proxy = true
```

API clients use the proxy's HTTPS 443; the proxy forwards HTTP to loopback 8080. Agents connect **directly** to gateway 8443, which still needs its own valid TLS certificate and a firewall allowance. A regular HTTP reverse proxy cannot carry the `divertlink/1` TLS tunnel. Set `trust_proxy` only when direct ingress is controlled and forwarded headers come from that trusted proxy.

Use [nginx ingress](../../deploy/nginx/ingress.conf) (`proxy_buffering off`, compression off, suitable timeouts) or [Caddyfile](../../deploy/Caddyfile) (`flush_interval -1`) to preserve streaming. Adapt domains and certificate paths to the real host. Caddy's public certificate does not automatically configure the gateway's agent TLS listener.

## Upgrade, rollback and removal

Record the running version and back up configuration, CA/server keys and the previous binary to a private location. Verify a pinned replacement's checksum/signature, stop only affected processes, replace the binary, validate configuration, restart, then verify each selected tunnel and authenticated route. Preserve credentials and CA across upgrades; do not rerun forced initialization.

If verification fails, restore the old binary and the matching backup configuration/certificates, restart and verify again. Keep failure logs with secrets redacted.

To remove a systemd deployment, stop/disable the relevant units, remove those unit files and run `systemctl daemon-reload`; remove the binary only if no other role needs it. Delete configuration/state and the service account only after deciding whether to retain private backups. Installation does not create a general-purpose uninstall command. See [verification](quickstart.md#verify-the-route) and [troubleshooting](troubleshooting.md).
