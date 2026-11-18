# Build, sign and publish binaries

[中文](../releasing.md) · [English index](README.md) · [Install a release](installation.md)

Unix release executables run without Rust/Python/Node at runtime. Use current Rust stable and platform build tools for building. Versioned output goes to ignored `dist/`; never add signing credentials or private keys to the repository.

## Linux static executables

On Linux with the appropriate C/musl toolchain:

```bash
rustup target add x86_64-unknown-linux-musl
./scripts/release-unix.sh x86_64-unknown-linux-musl
```

For ARM64, add `aarch64-unknown-linux-musl` and build natively or use `--build-tool cross` with `cross` and Docker installed. macOS cross-builds can use Zig plus `cargo-zigbuild` and `--build-tool zigbuild`. Adding a Rust target alone does not install its cross C linker.

The result is `dist/divertlink-<version>-<target>.tar.gz` and `.tar.gz.sha256`. Check the checksum, extract, and run the nested `divertlink version`; users need only a compatible Linux kernel and CPU.

## macOS native build and signing

Install Xcode Command Line Tools and Rust. Apple Silicon uses `aarch64-apple-darwin`, Intel uses `x86_64-apple-darwin`:

```bash
rustup target add aarch64-apple-darwin
./scripts/release-unix.sh aarch64-apple-darwin
security find-identity -v -p codesigning
```

With a Developer ID Application identity available in the local keychain:

```bash
export MACOS_SIGN_IDENTITY='Developer ID Application: YOUR ORGANIZATION (TEAMID)'
./scripts/release-unix.sh aarch64-apple-darwin --sign
```

Signing enables hardened runtime and a timestamp. Verify the extracted executable with `codesign --verify --strict`. Select a specific identity if multiple valid identities exist.

## Apple notarization

Configure `APPLE_ID`, `APPLE_TEAM_ID`, `APPLE_APP_SPECIFIC_PASSWORD` privately in the current environment, or use a stored keychain credential profile with `NOTARY_KEYCHAIN_PROFILE`. Do not print credentials, commit `.env` files, export signing private keys to CI, or ask users to paste passwords into chat.

```bash
./scripts/release-unix.sh aarch64-apple-darwin --notarize
```

The script signs, builds a DMG, waits for notarytool `Accepted`, staples the ticket to the DMG and verifies it with `spctl`. It also creates a tar.gz of the same signed executable and checksums. Repeat for Intel when both architectures are required. A DMG can carry an offline notarization ticket; a bare binary/tar.gz cannot be stapled.

On failure retain the generated `.notary.json` submission metadata and inspect the Apple notarization log using the existing private credentials. Fix the concrete issue before retrying. Do not publish a failed/unsigned build as a signed release.

## GitHub workflow and publication

[release.yml](../../.github/workflows/release.yml) supports manual build-only runs and `vX.Y.Z` tags matching `Cargo.toml`. A tag creates a **draft** with Linux/Windows assets. Hosted macOS runner builds remain unsigned workflow artifacts and are excluded from the Release so they cannot overwrite local notarized packages.

Upload the locally verified DMG/tar.gz and SHA256 files to the draft, check versions/architectures and notarization, then publish. The previous [v0.1.0 release](https://github.com/Wrste/divertlink/releases/tag/v0.1.0) had Linux and notarized macOS packages; verify the new release assets after publication. Installation docs must describe actual published assets rather than the workflow matrix.

The project deployment skill can be packaged independently with `python3 scripts/package-skill.py`. Its ZIP contains only the self-contained skill and project licenses, not runtime binaries or local credentials. See [agent skill](agent-skill.md).
