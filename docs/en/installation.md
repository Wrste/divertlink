# Installation

[中文](../quickstart.md#2-安装-divertlink) · [English index](README.md) · [Next: quickstart](quickstart.md)

Install on both the public gateway and home agent. The hosts may use different operating systems. Release executables include all three modes: `server`, `agent`, `console`. Unix binaries need no Rust, Python, Node.js or Docker runtime.

## Download availability

[v0.1.2](https://github.com/Wrste/divertlink/releases/tag/v0.1.2) provides:

| Host | Target suffix | Format |
| --- | --- | --- |
| Linux Intel/AMD 64-bit | `x86_64-unknown-linux-musl` | `.tar.gz` |
| Linux ARM64 | `aarch64-unknown-linux-musl` | `.tar.gz` |
| macOS Apple Silicon | `aarch64-apple-darwin` | `.dmg`, `.tar.gz` |
| macOS Intel | `x86_64-apple-darwin` | `.dmg`, `.tar.gz` |

Windows x64 ZIP is not included in this release; use a source build below. Do not assume an asset exists just because it is present in the CI build matrix. No 32-bit or Windows ARM64 prebuilt package is provided.

## Linux

Use Bash, `curl`, `tar` and `sha256sum`. On Debian/Ubuntu install prerequisites with `sudo apt-get install ca-certificates curl tar coreutils`. Check `uname -m`, then run:

```bash
(
  set -eu
  release_version=0.1.2
  case "$(uname -m)" in
    x86_64) release_target=x86_64-unknown-linux-musl ;;
    aarch64|arm64) release_target=aarch64-unknown-linux-musl ;;
    *) echo 'Unsupported prebuilt architecture' >&2; exit 1 ;;
  esac
  release_name="divertlink-${release_version}-${release_target}"
  release_base="https://github.com/Wrste/divertlink/releases/download/v${release_version}"
  release_dir="$(mktemp -d)"
  cd "$release_dir"
  curl -fLO "${release_base}/${release_name}.tar.gz"
  curl -fLO "${release_base}/${release_name}.tar.gz.sha256"
  sha256sum -c "${release_name}.tar.gz.sha256"
  tar -xzf "${release_name}.tar.gz"
  mkdir -p "$HOME/.local/bin"
  install -m 0755 "${release_name}/divertlink" "$HOME/.local/bin/divertlink"
)
export PATH="$HOME/.local/bin:$PATH"
divertlink version
```

Add `export PATH="$HOME/.local/bin:$PATH"` to `~/.bashrc` (or `~/.zshrc` for Zsh), then reopen the shell. For a global installation:

```bash
sudo install -m 0755 "$HOME/.local/bin/divertlink" /usr/local/bin/divertlink
```

The musl executable is statically linked. It must still match the CPU and run on a compatible Linux kernel. For systemd installation use the full repository and [deployment instructions](deployment.md).

## macOS

`uname -m` prints `arm64` on a native Apple Silicon terminal and `x86_64` on Intel. On Apple Silicon, avoid choosing an Intel build accidentally from a Rosetta terminal.

Download the matching DMG and `.dmg.sha256` from the release page. For Apple Silicon:

```bash
shasum -a 256 -c divertlink-0.1.2-aarch64-apple-darwin.dmg.sha256
open divertlink-0.1.2-aarch64-apple-darwin.dmg
```

The archive and checksum must be in the current directory. Double-click `DivertLink Console.command` in the mounted image to launch the UI, or run the contained executable from Terminal. Copy it to a permanent directory for continued use. The DMG is signed, notarized and stapled; it supports offline ticket validation.

For terminal installation, download and verify the tar.gz instead:

```bash
(
  set -eu
  release_version=0.1.2
  case "$(uname -m)" in
    arm64) release_target=aarch64-apple-darwin ;;
    x86_64) release_target=x86_64-apple-darwin ;;
    *) echo 'Unsupported prebuilt architecture' >&2; exit 1 ;;
  esac
  release_name="divertlink-${release_version}-${release_target}"
  release_base="https://github.com/Wrste/divertlink/releases/download/v${release_version}"
  release_dir="$(mktemp -d)"
  cd "$release_dir"
  curl -fLO "${release_base}/${release_name}.tar.gz"
  curl -fLO "${release_base}/${release_name}.tar.gz.sha256"
  shasum -a 256 -c "${release_name}.tar.gz.sha256"
  tar -xzf "${release_name}.tar.gz"
  codesign --verify --strict "${release_name}/divertlink"
  mkdir -p "$HOME/.local/bin"
  install -m 0755 "${release_name}/divertlink" "$HOME/.local/bin/divertlink"
)
export PATH="$HOME/.local/bin:$PATH"
divertlink version
```

Persist PATH in `~/.zshrc` (Bash login shells: `~/.bash_profile`). The tar.gz contains the same signed executable but cannot carry a stapled ticket itself; first Gatekeeper evaluation may require network access. Do not globally disable Gatekeeper.

## Source builds, including Windows

Install [Git](https://git-scm.com/downloads), [Rust stable](https://rustup.rs/) and native build tools. The package declares MSRV 1.88, but locked dependencies may require a newer compiler; use current stable.

| Platform | Build tools |
| --- | --- |
| Debian/Ubuntu | `sudo apt-get install build-essential git curl ca-certificates` |
| Fedora | `sudo dnf install gcc gcc-c++ make git curl` |
| Alpine | `sudo apk add build-base git curl ca-certificates` |
| macOS | `xcode-select --install` |
| Windows x64 | [Visual Studio Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/), Desktop development with C++, MSVC and Windows SDK; rustup default MSVC toolchain |

After installing Rust reopen the terminal, then:

```text
git clone https://github.com/Wrste/divertlink.git
cd divertlink
cargo build --release --locked
```

Linux/macOS binary: `target/release/divertlink`. Copy it to `~/.local/bin` as above. Windows PowerShell:

```powershell
$installDir = Join-Path $env:LOCALAPPDATA "DivertLink\bin"
New-Item -ItemType Directory -Force -Path $installDir | Out-Null
Copy-Item .\target\release\divertlink.exe $installDir -Force
$env:Path = "$installDir;$env:Path"
divertlink version
```

Add `%LOCALAPPDATA%\DivertLink\bin` to the **user** Path in Windows Environment Variables, then reopen PowerShell. In the current directory use `.\divertlink.exe`, not bare `divertlink.exe`. Missing Visual C++ runtime DLLs should be resolved using the [official Microsoft x64 redistributable](https://learn.microsoft.com/en-us/cpp/windows/latest-supported-vc-redist), not a third-party DLL download.

## Verify, upgrade or remove

Run `divertlink version` and `divertlink --help` on both hosts. On a desktop, `divertlink console --open` opens `http://127.0.0.1:17653/`; Ctrl+C stops it. Continue with [quickstart](quickstart.md) to create configuration.

A gateway on Linux binding 443 needs a service capability or root; prefer the supplied systemd unit. For a test use 8443 and update both listeners, the agent endpoint and client URLs. Agents only open outbound connections.

Before upgrading stop the affected process, back up the executable/configuration, verify the replacement checksum and restart. Binary replacement does not migrate your configuration or certificates. Never remove `Cargo.lock` to work around a source build error. To uninstall a user installation, stop processes, delete only the installed executable and remove its PATH entry; delete configurations/certificates only when no longer needed.

A 404 download indicates an unavailable asset or wrong version/target. `Exec format error` or `Bad CPU type` usually indicates a CPU/OS mismatch. A checksum mismatch must stop installation. See [troubleshooting](troubleshooting.md).
