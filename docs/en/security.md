# TLS and authentication

[中文](../security.md) · [English index](README.md) · [Configuration](configuration.md)

The gateway terminates TLS and can read requests. This is an encrypted reverse tunnel, not end-to-end encryption through an untrusted gateway. Keep the model on loopback or a protected private network. Home machines need outbound access only.

## Three separate credentials

| Credential | Used between | Configuration |
| --- | --- | --- |
| Gateway API key | API client → gateway | `api_keys[].key` |
| Tunnel token | Agent → gateway | `tunnels[].token` ↔ `agent.token`, with matching IDs |
| Upstream key | Gateway/agent → model | `upstream_api_key` or private target headers |

Use per-client gateway keys with bounded `rate_limit`, `max_concurrent`, and appropriate `allow_tunnels`/`allow_ips`. Generate random credentials with `divertlink token`; placeholders are not secure credentials. The console token is a separate management credential.

`upstream_auth_header = "none"` strips client authentication before an unauthenticated model. `authorization` injects `Bearer <upstream_api_key>`; `x-api-key` injects that header. The default `passthrough` forwards the client's own gateway key; choose it only when the upstream should accept that key. Do not use tunnel tokens as client API keys.

A stolen tunnel token can let an unauthorized agent join the tunnel's session pool and receive traffic. Rotate it on the gateway and every associated agent; review `/status` and logs. API-key rotation can overlap old/new key entries during client migration, followed by removal of the old key and a gateway restart.

## TLS trust

Agent tunnels require TLS 1.3. Public HTTPS defaults to TLS 1.3 but `tls.min_version = "1.2"` can allow older API clients. The certificate SAN must match the hostname; for an IP connection use the correct agent `tls.servername` if needed.

- **Private/self-signed CA:** distribute only the public `ca.crt` over a trusted channel; set agent `tls.ca` to it. Clients use `--cacert` or install that CA in their own trust configuration. Keep `ca.key` private on the certificate administration host.
- **Public CA:** configure gateway `tls.cert` with its full chain and `tls.key` with the matching private key. The agent uses bundled public roots when `ca` is absent. Remove old private CA settings, and update/remove old pins deliberately.
- **SPKI pin:** `tls.pin` is an additional public-key hash check after normal chain, hostname and validity validation. It does not make an untrusted self-signed certificate trusted. `gen-certs` prints the pin. A new key pair changes it; reissuing with the same key preserves the SPKI hash.

Never deploy `insecure = true` or `curl -k` as a fix. Certificate files must be readable by the service identity but private keys must not be public. Back up certificate material privately and restart after renewal; DivertLink has no built-in ACME or certificate hot reload.

## Mutual TLS

`divertlink init --mtls` generates client material but does **not** enable server-side mTLS automatically. Configure gateway `tls.client_ca`, agent `tls.client_cert/client_key`, and use different gateway tunnel and public ingress ports. Keep tunnel token authentication as well. Transfer only that agent's client certificate/key and required CA, never the server key or CA key.

## Exposure and management

Keep admin 9090 and console 17653 on loopback. Use authorized SSH forwarding for remote access. Admin endpoints expose operational metadata and are not a public authentication surface. Enable `trust_proxy` only behind a trusted proxy with direct ingress controlled.

`public_paths` bypasses gateway authentication for matching path prefixes relative to the mount; allow only explicit harmless endpoints. Do not whitelist `/v1`. Static routes are authenticated by default; `allow_tunnels` does not restrict static routes. If turning their authentication off, the private upstream must authenticate users itself.

Use a private configuration directory (`umask 077` before init on Unix). `init` outputs generated credentials, so keep its terminal/logs private. Do not commit real TOML, tokens, private keys, `.env` credentials, signing identities' private material or notarization passwords. Report credential locations rather than values when an agent performs deployment.

Before exposing a route, verify TLS normally, reject an unauthenticated request, authenticate with the intended key, confirm the selected tunnel and exercise the requested endpoint. Model-list success does not establish that streaming works. Review [troubleshooting](troubleshooting.md) before changing trust or credentials.
