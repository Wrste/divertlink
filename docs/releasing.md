# 构建、签名与公证

**中文** · [English](en/releasing.md) · [文档目录](README.md)

DivertLink 的 Release 是直接运行的命令行二进制，不依赖 Node.js、Python 或容器运行时。
Linux 使用 musl 静态链接，macOS 使用原生 target；Windows 仍由 GitHub Actions 构建。

## Linux

在 Linux 主机上构建当前架构：

```bash
rustup target add x86_64-unknown-linux-musl
# Debian/Ubuntu 构建 musl 时需安装 C 工具链：sudo apt-get install build-essential musl-tools
./scripts/release-unix.sh x86_64-unknown-linux-musl
# 交叉构建 ARM64 时，先安装 cross 并准备 Docker，再安装 Rust target：
# cargo install cross --locked
rustup target add aarch64-unknown-linux-musl
./scripts/release-unix.sh aarch64-unknown-linux-musl --build-tool cross
```

脚本生成 `dist/divertlink-<version>-<target>.tar.gz` 和 SHA256 文件。解压后直接运行：

```bash
tar -xzf dist/divertlink-0.1.1-x86_64-unknown-linux-musl.tar.gz
./divertlink-0.1.1-x86_64-unknown-linux-musl/divertlink version
```

Linux 资产是 musl 静态链接，适用于常见的 glibc/musl 发行版；仍需匹配 CPU 架构。
也可在 Mac 上安装 Zig 与 `cargo-zigbuild`，安装对应 Rust target 后用
`--build-tool zigbuild` 交叉编译上述两个 Linux target。最终用户不需要安装构建工具。
示例中的 `0.1.1` 替换为 Cargo.toml 对应版本。

## macOS 本机编译

先安装 Xcode Command Line Tools 和 Rust stable。Apple Silicon 使用
`aarch64-apple-darwin`，Intel 使用 `x86_64-apple-darwin`：

```bash
rustup target add aarch64-apple-darwin
./scripts/release-unix.sh aarch64-apple-darwin
# Intel Mac：
rustup target add x86_64-apple-darwin
./scripts/release-unix.sh x86_64-apple-darwin
```

压缩包内的 `divertlink` 及 `DivertLink Console.command` 可以直接运行：

```bash
tar -xzf dist/divertlink-0.1.1-aarch64-apple-darwin.tar.gz
./divertlink-0.1.1-aarch64-apple-darwin/divertlink version
./divertlink-0.1.1-aarch64-apple-darwin/"DivertLink Console.command"
```

## macOS Developer ID 签名

本机钥匙串中应有 `Developer ID Application` 证书。先查看名称：

```bash
security find-identity -v -p codesigning
```

签名身份可通过环境变量传入，脚本不会把它写入仓库：

```bash
export MACOS_SIGN_IDENTITY='Developer ID Application: 你的公司 (TEAMID)'
./scripts/release-unix.sh aarch64-apple-darwin --sign
tar -xzf dist/divertlink-0.1.1-aarch64-apple-darwin.tar.gz
codesign --verify --strict --verbose=2 \
  divertlink-0.1.1-aarch64-apple-darwin/divertlink
```

`--sign` 使用 hardened runtime、时间戳和 Developer ID 签名。普通 tar.gz 内的二进制
可直接执行；用户首次从网络下载时，macOS 仍可能根据下载来源显示 Gatekeeper 提示。

## Apple 公证与 DMG

公证需要 Apple Developer Program 账号。建议在当前 shell 中配置短期环境变量：

```bash
export APPLE_ID='你的 Apple ID'
export APPLE_TEAM_ID='你的 Team ID'
export APPLE_APP_SPECIFIC_PASSWORD='xxxx-xxxx-xxxx-xxxx'
```

已在当前 shell 配置这些环境变量时直接运行发布命令，不需要重新输入。
也可以把凭据保存到钥匙串后只设置 `NOTARY_KEYCHAIN_PROFILE`；不要把密码写入脚本、
`.env` 或 Git。运行：

```bash
export MACOS_SIGN_IDENTITY='Developer ID Application: 你的公司 (TEAMID)'
./scripts/release-unix.sh aarch64-apple-darwin --notarize
```

脚本会签名二进制，生成 DMG，提交 `xcrun notarytool` 并等待 `Accepted`，随后 stapler
把票据附加到 DMG 并执行 `spctl` 验证。最终得到：

- `*.dmg`：已签名、已公证、已 stapled，适合 macOS 分发；
- `*.tar.gz`：同一签名二进制的直接运行包；裸二进制和 tar.gz 本身不能 stapler，离线
  验证请使用 DMG。

公证失败时脚本保留 `*.notary.json`，按提交 ID 查看日志：

```bash
xcrun notarytool log <submission-id> --apple-id "$APPLE_ID" \
  --team-id "$APPLE_TEAM_ID" --password "$APPLE_APP_SPECIFIC_PASSWORD"
```

## Release 发布流程

GitHub Actions 的 `release.yml` 支持手动构建（只生成构建产物，不发布），以及推送
`vX.Y.Z` tag（必须与 Cargo.toml 版本一致）。tag 构建会把 Linux/Windows 资产放进
草稿 Release；macOS runner 的未签名构建只作为 `unsigned-*` 工作流产物保留，不进入
Release，避免覆盖本机公证包。

正式发布前在有 Developer ID 证书的 Mac 上运行上面的 `--notarize`，将生成的 DMG、
tar.gz 和 SHA256 文件上传到草稿，确认签名、公证和版本后再发布。
这样私钥和 Apple 凭据始终留在本机，不进入 GitHub Actions。
