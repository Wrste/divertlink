# Agent deployment skill

[中文](../agent-skill.md) · [English index](README.md)

The portable [`divertlink-deploy`](../../skills/divertlink-deploy/SKILL.md) skill helps an agent inspect hosts, install verified binaries, configure TLS/authentication/routing, choose systemd/Docker/foreground operation and verify the requested routes. It understands Chinese and English tasks. It includes no runtime binary, host access or credentials.

## Install

From a repository checkout on Linux/macOS, copy the **whole** skill directory, including scripts and references:

```bash
skill_root="${CODEX_HOME:-$HOME/.codex}/skills"
mkdir -p "$skill_root"
if [ -e "$skill_root/divertlink-deploy" ]; then
  echo 'Skill already exists; inspect and back it up before updating.'
else
  cp -R skills/divertlink-deploy "$skill_root/"
fi
```

Windows PowerShell:

```powershell
$codexRoot = if ($env:CODEX_HOME) { $env:CODEX_HOME } else { Join-Path $HOME '.codex' }
$skillRoot = Join-Path $codexRoot 'skills'
$destination = Join-Path $skillRoot 'divertlink-deploy'
New-Item -ItemType Directory -Force -Path $skillRoot | Out-Null
if (Test-Path $destination) { throw 'Inspect and back up the existing skill before updating.' }
Copy-Item -Recurse .\skills\divertlink-deploy $destination
```

Other agents supporting `SKILL.md` can use their own discovery directory or read the entrypoint directly. Optional helper scripts use Python 3.9+ standard library. Verifying the default HTTPS ingress additionally requires a TLS 1.3 capable SSL backend; macOS system Python may use old LibreSSL, so use a modern Python/OpenSSL build. HTTP-only loopback management checks are unaffected. DivertLink's Unix executable itself does not require Python.

## Portable package

```bash
python3 scripts/package-skill.py
```

This writes `dist/divertlink-deploy-skill.zip` and its `.sha256`. Extract the single `divertlink-deploy/` folder into your agent's skill directory. The package includes instructions, references, helpers, UI metadata and MIT/Apache licenses. An explicit file allow-list excludes runtime binaries, logs, caches and local credentials. Do not copy only `SKILL.md`.

## Example requests

> Use $divertlink-deploy to deploy v0.1.2 on my already authorized SSH host cloud-gateway (Linux x86_64), using gateway.example.com and systemd. Run the agent on this macOS ARM64 host against http://127.0.0.1:11434. Inspect occupied ports and preserve existing certificates. Verify the home tunnel, unauthenticated rejection and model listing; do not generate model output.

> Use $divertlink-deploy with my existing nginx on 443. Keep its sites and certificates, use agent TLS on 8443 and loopback HTTP ingress, and verify proxy streaming settings.

> Use $divertlink-deploy to add separate qwen and llama mounts and per-client tunnel permissions to an existing gateway. Back up first, verify every route and restore the previous configuration if verification fails. Report secret locations, never values.

Replace example domains/hosts/versions with real ones. The skill inspects available facts before asking for blockers and uses existing authorization. Documentation or planning requests do not authorize live deployment. The skill preserves existing credentials and certificate material rather than regenerating them on failure.

## Binary installer

Paths below are relative to the repository; after installation resolve them from the skill's directory:

```bash
python3 skills/divertlink-deploy/scripts/install_binary.py \
  --version 0.1.2 --bin-dir "$HOME/.local/bin" --dry-run
python3 skills/divertlink-deploy/scripts/install_binary.py \
  --version 0.1.2 --bin-dir "$HOME/.local/bin"
```

Dry-run prints the OS/CPU, pinned URL and destination with no download/write. Installation validates the published SHA256, reads only the executable from the archive and verifies `version`; macOS also verifies its code signature. It refuses an existing binary unless `--replace` is explicitly selected for an upgrade after stopping its processes. It does not edit PATH, services, firewall, certificates or configuration. Use the notarized DMG when offline ticket validation is needed.

v0.1.2 has Linux x86_64/ARM64 and macOS Intel/ARM64 packages. Windows ZIP is not published; use a [source build](installation.md#source-builds-including-windows) if the release asset is absent. Python is a helper dependency, not a runtime dependency.

## End-to-end verifier

Run on the gateway or through an authorized loopback SSH forward. Set `DIVERTLINK_API_KEY` privately in the environment; never put its value in the command or chat:

```bash
python3 skills/divertlink-deploy/scripts/verify.py \
  --admin-url http://127.0.0.1:9090 --tunnel-id home \
  --ingress-url https://gateway.example.com/home/v1 \
  --key-env DIVERTLINK_API_KEY --ca /private/path/ca.crt
```

Omit `--ca` for a public CA. It checks liveness/readiness, the selected tunnel, 401 without authentication and an OpenAI model list with the gateway key. It emits only statuses, returning 0 for success and 1 for failure. It refuses remote admin URLs, remote plaintext ingress and redirects; TLS verification remains enabled.

Omit ingress only for management checks, not as proof that an API works. Anthropic/custom upstreams may lack `/v1/models`; verify the suitable endpoint manually, including unauthenticated rejection. Model listing does not verify streaming. Use a scoped streaming request when authorized, or repository `scripts/smoke.sh` with a temporary mock model to avoid spending model quota.

## Maintenance

Keep operational decisions in `SKILL.md`, detailed mode guidance in `references/` and deterministic helpers in `scripts/`. Never add live host configuration or secrets. Run:

```bash
python3 -m unittest discover -s tests -p 'test_deploy_skill.py' -v
python3 scripts/package-skill.py
```

See [bilingual contribution guidelines](../CONTRIBUTING.md).
