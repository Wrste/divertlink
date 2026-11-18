# DivertLink

[中文](README.md) · **English** · [Documentation](docs/en/README.md) · [Releases](https://github.com/Wrste/divertlink/releases)

Expose LLM services running on a home network through a gateway with a public IP. The home agent opens an outbound encrypted tunnel; API clients connect to the gateway over HTTPS. No public IP or inbound port forwarding is needed at home.

```text
API client → HTTPS gateway :443 → TLS + yamux tunnel → home agent → local model
```

DivertLink forwards HTTP semantics and streaming bodies without interpreting model JSON. Use it with OpenAI-compatible APIs, Anthropic, vLLM, Ollama, LM Studio, llama.cpp, new-api and similar services. The gateway terminates TLS and must be trusted.

## Install

[v0.1.1](https://github.com/Wrste/divertlink/releases/tag/v0.1.1) includes directly runnable Linux and macOS binaries. Unix users do not need Rust, Python, Node.js or Docker to run the release executable.

| Platform | Asset suffix | Notes |
| --- | --- | --- |
| Linux x86_64 | `x86_64-unknown-linux-musl.tar.gz` | Statically linked |
| Linux ARM64 | `aarch64-unknown-linux-musl.tar.gz` | Statically linked |
| macOS Apple Silicon | `aarch64-apple-darwin.dmg` or `.tar.gz` | Developer ID signed and Apple notarized |
| macOS Intel | `x86_64-apple-darwin.dmg` or `.tar.gz` | Developer ID signed and Apple notarized |
| Windows x64 | Source build for now | MSVC Build Tools and Rust stable required to build |

Download the matching asset and its `.sha256` file. Archives contain a directory named after the asset, with the executable inside. See [platform installation](docs/en/installation.md) for download commands, checksum verification, PATH setup and source builds. macOS DMGs have stapled tickets for offline verification.

## Get started

Install the executable on both hosts, then follow the [complete quickstart](docs/en/quickstart.md). On a new gateway project directory:

```bash
umask 077
mkdir -p "$HOME/divertlink"
cd "$HOME/divertlink"
divertlink init --role both --domain gateway.example.com
divertlink check -c server.toml
divertlink server -c server.toml
```

`init` generates random API keys, matching tunnel tokens, a CA and a server certificate. Transfer **only** `agent.toml` and `certs/ca.crt` to the home host. Set `[target].url` to its actual local model endpoint. Choose the gateway tunnel's upstream authentication mode: `none` for an unauthenticated model, or replacement with its private upstream key. See the quickstart before starting both processes. On that host:

```bash
divertlink check -c agent.toml
divertlink agent -c agent.toml
```

Use `https://gateway.example.com/home/v1` as an OpenAI SDK base URL, and the gateway API key as the client key. The gateway's `/home` mount is removed before forwarding. Self-signed deployments require the generated CA on API clients too; use a public CA certificate for public production ingress. Keep the admin endpoint on loopback.

For the built-in UI, run `divertlink console --open`. It serves a local web console at `http://127.0.0.1:17653/`, with project profiles, process controls, logs and TLS probes. Closing the console stops processes it started; use systemd or another process manager for unattended operation.

## Features

- TLS 1.3 agent tunnels; shared port 443 through ALPN (`divertlink/1`, `h2`, `http/1.1`).
- Unbuffered SSE forwarding, backpressure and cancellation on client disconnect.
- Distinct gateway API keys and agent tokens, optional mTLS/SPKI pinning, rate/concurrency/body limits.
- Multiple tunnel mounts/hosts, agent-side path routing and multiple agents per tunnel.
- Protocol-transparent HTTP forwarding; use new-api/LiteLLM for model-name routing, billing or quotas.
- Loopback health, readiness, Prometheus metrics and JSON status endpoints.
- One executable containing gateway, agent, certificate/config setup and browser/PWA console.

WebSocket, gRPC and HTTP/3 forwarding are not supported. A failed stream cannot be resumed transparently. Certificates and configuration are loaded at startup; restart after changes.

## Deploy with an agent

The portable [divertlink-deploy skill](skills/divertlink-deploy/SKILL.md) helps coding agents inspect hosts, install verified binaries, configure TLS/routing, deploy with systemd/Docker or foreground processes, and verify the requested tunnel and ingress authentication. It includes Python standard-library installer and verifier helpers; they do not replace DivertLink's own runtime.

See [skill installation and examples](docs/en/agent-skill.md). It supports Chinese and English tasks. It does not grant access to servers or credentials by itself.

## Documentation

[English documentation index](docs/en/README.md) · [中文文档目录](docs/README.md)

| Topic | Guide |
| --- | --- |
| Installation and first tunnel | [Install](docs/en/installation.md) · [Quickstart](docs/en/quickstart.md) |
| Configure gateway and agent | [Configuration](docs/en/configuration.md) |
| systemd, Docker, reverse proxies and upgrades | [Deployment](docs/en/deployment.md) |
| Multiple models and replicas | [Multi-model](docs/en/multi-model.md) |
| Authentication, TLS and recovery | [Security](docs/en/security.md) · [Troubleshooting](docs/en/troubleshooting.md) |
| Client URLs and SDKs | [API integration](docs/en/protocols.md) |
| Local graphical console | [Console](docs/en/console.md) |
| Build, sign and notarize releases | [Releasing](docs/en/releasing.md) |

## Development

Use current Rust stable (package MSRV is 1.88; locked dependencies may require newer Rust) and the platform C/C++ toolchain:

```bash
git clone https://github.com/Wrste/divertlink.git
cd divertlink
cargo build --release --locked
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked
```

The repository-local development toolchain, when present, is enabled in Bash with `. ./scripts/env.sh`. Validate deployment assets with `make deploy-check`; run `scripts/smoke.sh` against a built binary for a temporary mock-model end-to-end test. Documentation contribution rules are in [docs/CONTRIBUTING.md](docs/CONTRIBUTING.md).

## License

Dual licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option.
