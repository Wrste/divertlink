# 快速开始

本文从零开始，把家庭端的 LLM 服务安全地映射到一台有固定公网 IP 的服务器上，并验证
非流式与流式调用。安装完成后配置与验证约 10 分钟；熟悉命令后可按 [README 的 30 秒快速开始](../README.md#30-秒快速开始) 精简执行。

- [1. 开始前准备](#1-开始前准备)
- [2. 安装 DivertLink](#2-安装-divertlink)
- [3. 生成配置与证书](#3-生成配置与证书)
- [4. 配置并启动网关](#4-配置并启动网关)
- [5. 配置并启动家庭端 agent](#5-配置并启动家庭端-agent)
- [6. 放行防火墙与 DNS](#6-放行防火墙与-dns)
- [7. 验证](#7-验证)
- [8. 常见错误](#8-常见错误)
- [9. 下一步](#9-下一步)
- [10. 端到端冒烟验证（可选）](#10-端到端冒烟验证可选)

## 1. 开始前准备

| 角色 | 要求 |
| --- | --- |
| 网关服务器 | 有公网固定 IP（或固定域名解析到该 IP）；Linux/macOS/Windows 均可；能监听 443（需 root 或端口能力）；建议 2 核 2 GB 以上 |
| 域名（推荐） | 一条 A 记录指向网关公网 IP，例如 `gateway.example.com`；自签证书或 Let's Encrypt 都需要它 |
| 家庭端机器 | 能访问互联网（家庭宽带/CGNAT 均可，**不需要公网 IP、不需要端口映射**）；能运行 vLLM / Ollama / LM Studio / llama.cpp / new-api 之一 |
| 家庭端服务 | 已在监听，例如 vLLM 默认 `http://127.0.0.1:8000`，new-api 默认 `http://127.0.0.1:3000` |
| 二进制 | `divertlink` 可执行文件（见第 2 节） |

网络拓扑：

```text
客户端 ──► https://gateway.example.com:443 ──► divertlink server（公网固定 IP）
                                                   │  ALPN "divertlink/1" 加密长连接
                                                   ▼
                                   divertlink agent（家庭宽带，主动外连）
                                                   │  HTTP
                                                   ▼
                                   vLLM / new-api（127.0.0.1:8000/3000）
```

> 下文命令中的 `gateway.example.com`、`home.llm.example.com`、`alice@192.168.1.50`、
> `dlk_...`、`agt_...` 均为占位值，必须替换为你的实际域名、账号与令牌。

## 2. 安装 DivertLink

- [选择安装方式与平台](#21-选择安装方式与平台)
- [Linux 发行包](#22-linux-安装发行包)
- [macOS 发行包](#23-macos-安装发行包)
- [Windows 发行包](#24-windows-安装发行包)
- [源码安装（当前可用）](#25-从源码安装所有平台)
- [Linux systemd 安装](#26-linux-systemd-安装)
- [验证与常见问题](#27-安装验证与常见问题)
- [443 端口权限](#28-监听-443-的权限)

### 2.1 选择安装方式与平台

网关与家庭端**都需要安装**，两端可以使用不同操作系统。同一个可执行文件包含
`server`、`agent` 和 `console`，无需分别下载服务端和客户端，也无需 Node.js 或 Python。

- 下载入口：[Wrste/divertlink Releases](https://github.com/Wrste/divertlink/releases)。
- **当前仓库尚无 Release**，请先使用 [2.5 从源码安装](#25-从源码安装所有平台)；
  下方发行包命令适用于对应版本正式发布之后，不会自动触发发布。
- 命令中的 `0.1.0` 是版本示例，执行前应在 Releases 确认该版本和资产存在。
  下载 URL 的标签带 `v`（如 `v0.1.0`），文件名的版本不带 `v`。

| 操作系统 / CPU | Release 资产（`<version>` 替换为实际版本） |
| --- | --- |
| Linux Intel / AMD 64 位（`uname -m` 为 `x86_64`） | `divertlink-<version>-x86_64-unknown-linux-musl.tar.gz` |
| Linux ARM 64 位（`aarch64` / `arm64`） | `divertlink-<version>-aarch64-unknown-linux-musl.tar.gz` |
| macOS Apple Silicon（M 系列，`arm64`） | `divertlink-<version>-aarch64-apple-darwin.tar.gz` |
| macOS Intel（`x86_64`） | `divertlink-<version>-x86_64-apple-darwin.tar.gz` |
| Windows Intel / AMD 64 位 | `divertlink-<version>-x86_64-pc-windows-msvc.zip` |

每个压缩包另有同名 `.sha256` 文件。**压缩包内还有一层同名目录**，二进制不在
解压目录根部。32 位 Linux/Windows、Windows ARM64 暂无预构建资产；源码构建也需
目标平台的 Rust 工具链和 C/C++ 编译环境，未列入发行构建矩阵的平台需自行验证。

### 2.2 Linux 安装发行包

适用于 Ubuntu、Debian、Fedora、Rocky Linux、Alpine 等发行版的上述两种架构。
准备 `bash`、`curl`、`tar` 和 `sha256sum`；Debian/Ubuntu 可执行
`sudo apt-get update && sudo apt-get install -y ca-certificates curl tar coreutils`。
以下命令在 **Bash** 中执行，使用用户目录安装，无需 root：

```bash
(
  set -eu
  release_version="0.1.0"  # 替换为 Releases 中已有的版本
  case "$(uname -m)" in
    x86_64) release_target="x86_64-unknown-linux-musl" ;;
    aarch64|arm64) release_target="aarch64-unknown-linux-musl" ;;
    *) echo "暂无该架构的发行包，请从源码构建" >&2; exit 1 ;;
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

确认版本输出后，将 `export PATH="$HOME/.local/bin:$PATH"` 添加到 `~/.bashrc`
（Zsh 用 `~/.zshrc`），重新打开终端后仍可直接调用。也可使用绝对路径
`~/.local/bin/divertlink version`。

如需供所有用户或 systemd 使用，在上述安装后执行：

```bash
sudo install -m 0755 "$HOME/.local/bin/divertlink" /usr/local/bin/divertlink
/usr/local/bin/divertlink version
```

要创建专用服务用户、配置与开机自启，见 [2.6 Linux systemd 安装](#26-linux-systemd-安装)。

### 2.3 macOS 安装发行包

打开「终端」，执行 `uname -m`：M 系列原生终端输出 `arm64`，Intel 输出 `x86_64`。
M 系列若在 Rosetta 终端中显示 `x86_64`，建议切回原生终端安装 ARM 版本。
系统自带的 `curl`、`tar`、`shasum` 即可完成安装：

```bash
(
  set -eu
  release_version="0.1.0"  # 替换为 Releases 中已有的版本
  case "$(uname -m)" in
    arm64) release_target="aarch64-apple-darwin" ;;
    x86_64) release_target="x86_64-apple-darwin" ;;
    *) echo "暂无该架构的发行包，请从源码构建" >&2; exit 1 ;;
  esac
  release_name="divertlink-${release_version}-${release_target}"
  release_base="https://github.com/Wrste/divertlink/releases/download/v${release_version}"
  release_dir="$(mktemp -d)"
  cd "$release_dir"
  curl -fLO "${release_base}/${release_name}.tar.gz"
  curl -fLO "${release_base}/${release_name}.tar.gz.sha256"
  shasum -a 256 -c "${release_name}.tar.gz.sha256"
  tar -xzf "${release_name}.tar.gz"
  mkdir -p "$HOME/.local/bin"
  install -m 0755 "${release_name}/divertlink" "$HOME/.local/bin/divertlink"
)
export PATH="$HOME/.local/bin:$PATH"
divertlink version
```

将 `export PATH="$HOME/.local/bin:$PATH"` 添加到 `~/.zshrc`（macOS Bash 登录终端用 `~/.bash_profile`），
重新打开终端验证。也可用 `~/.local/bin/divertlink` 的绝对路径启动。
正式 macOS 发布包由 Developer ID 签名并经过 Apple 公证。优先下载同架构的 `.dmg`，
校验对应 `.sha256` 后双击挂载，在终端运行卷内的 `divertlink`，或双击
`DivertLink Console.command` 打开控制台；需要长期使用时，将二进制复制到上述安装目录。
DMG 已附加公证票据，适合离线验证；tar.gz 中为同一签名二进制，首次评估可能需要联网。

若使用自行构建的未签名包且系统提示无法验证开发者，先确认来源，再按
「系统设置 → 隐私与安全性」中的提示处理；不要全局关闭 Gatekeeper。

安装完成后运行 `divertlink console --open` 即可打开图形控制台。它是浏览器界面，
不是 `.app` 安装包；使用 PWA 与双击启动器的方法见 [图形化客户端](console.md)
和 [桌面启动器](../scripts/desktop/README.md)。

维护者需要构建可直接运行的 Linux/macOS 发布包时，参见[构建、签名与公证](releasing.md)。

### 2.4 Windows 安装发行包

适用于 64 位 Intel / AMD Windows。在「设置 → 系统 → 系统信息」确认系统类型，
打开 **PowerShell 5.1 或更新版本**，无需管理员权限。下面安装到
`%LOCALAPPDATA%\DivertLink\bin`：

```powershell
$ErrorActionPreference = "Stop"
$releaseVersion = "0.1.0"  # 替换为 Releases 中已有的版本
$releaseName = "divertlink-$releaseVersion-x86_64-pc-windows-msvc"
$releaseBase = "https://github.com/Wrste/divertlink/releases/download/v$releaseVersion"
$releaseDir = Join-Path ([IO.Path]::GetTempPath()) ("divertlink-" + [guid]::NewGuid())
$installDir = Join-Path $env:LOCALAPPDATA "DivertLink\bin"
New-Item -ItemType Directory -Force -Path $releaseDir, $installDir | Out-Null
$archivePath = Join-Path $releaseDir "$releaseName.zip"
$checksumPath = "$archivePath.sha256"
Invoke-WebRequest -UseBasicParsing "$releaseBase/$releaseName.zip" -OutFile $archivePath
Invoke-WebRequest -UseBasicParsing "$releaseBase/$releaseName.zip.sha256" -OutFile $checksumPath
$expectedHash = ((Get-Content $checksumPath -Raw).Trim() -split '\s+')[0]
$actualHash = (Get-FileHash $archivePath -Algorithm SHA256).Hash
if ($actualHash -ine $expectedHash) { throw "SHA256 校验失败，停止安装" }
Expand-Archive -Path $archivePath -DestinationPath $releaseDir
Copy-Item (Join-Path $releaseDir "$releaseName\divertlink.exe") $installDir -Force
$env:Path = "$installDir;$env:Path"
divertlink version
divertlink console --open
```

最后一条命令会打开默认浏览器；按 `Ctrl+C` 停止控制台。配置 agent 时，在 PowerShell
使用 `divertlink agent -c "C:\实际目录\agent.toml"`；使用当前目录里的程序时必须加
`.\`，例如 `.\divertlink.exe version`。

要让新终端也能找到程序，打开「编辑账户的环境变量 → 用户变量 → Path → 编辑 → 新建」，
添加 `%LOCALAPPDATA%\DivertLink\bin`，保存后重新打开 PowerShell。无需修改系统 Path。
也可始终使用绝对路径：

```powershell
& "$env:LOCALAPPDATA\DivertLink\bin\divertlink.exe" version
```

如提示缺少 `VCRUNTIME140.dll` 等运行库，从
[微软官方页面](https://learn.microsoft.com/zh-cn/cpp/windows/latest-supported-vc-redist)
安装 x64 Visual C++ Redistributable 后重试。

### 2.5 从源码安装（所有平台）

尚无 Release、下载不到对应资产或需要修改代码时，使用此方式。
安装 [Git](https://git-scm.com/downloads) 与 [Rust stable](https://rustup.rs/)，
使用当前 stable 工具链（项目声明 Rust 最低版本为 1.88；锁定依赖可能要求更新版本）。
源码编译还需要本机链接器与 C/C++ 构建工具，发行包用户无需安装这些工具。

| 平台 | 编译前准备 |
| --- | --- |
| Ubuntu / Debian | `sudo apt-get update && sudo apt-get install -y build-essential git curl ca-certificates`，然后按 rustup 官网安装 Rust |
| Fedora | `sudo dnf install gcc gcc-c++ make git curl`，然后按 rustup 官网安装 Rust |
| Alpine | `sudo apk add build-base git curl ca-certificates`，按 rustup 官网安装适用于本机的 Rust 工具链 |
| macOS | 执行 `xcode-select --install` 安装 Command Line Tools，然后按 rustup 官网安装 Rust |
| Windows | 安装 [Visual Studio 2022 Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/)，选择「使用 C++ 的桌面开发」（含 MSVC 与 Windows SDK），然后运行 rustup 官网的 `rustup-init.exe`，选择默认 MSVC 工具链 |

Linux / macOS 在安装好上表依赖后，可按 rustup 官方方式下载并运行安装器：

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs -o rustup-init.sh
sh rustup-init.sh --default-toolchain stable
```

按提示选择默认安装，完成后重新打开终端；如果系统已有 Rust，执行
`rustup update stable`。Windows 在安装器中同样选择 stable/MSVC，安装完成后重开
PowerShell；Git 未在 PATH 中时重开终端或检查 Git 安装选项。

装好 Rust 后重新打开终端，执行以下命令（Windows 使用 PowerShell）：

```text
git --version
rustc --version
cargo --version
git clone https://github.com/Wrste/divertlink.git
cd divertlink
cargo build --release --locked
```

Linux / macOS 的产物为 `target/release/divertlink`，复制到用户目录并验证：

```bash
mkdir -p "$HOME/.local/bin"
install -m 0755 target/release/divertlink "$HOME/.local/bin/divertlink"
export PATH="$HOME/.local/bin:$PATH"
divertlink version
```

PATH 的永久设置见对应平台章节；Linux 全局安装也可使用
`sudo install -m 0755 target/release/divertlink /usr/local/bin/divertlink`。
Windows 的产物为 `target\release\divertlink.exe`，在仓库目录执行：

```powershell
$installDir = Join-Path $env:LOCALAPPDATA "DivertLink\bin"
New-Item -ItemType Directory -Force -Path $installDir | Out-Null
Copy-Item .\target\release\divertlink.exe $installDir -Force
$env:Path = "$installDir;$env:Path"
divertlink version
```

本仓库已有本地工具链时，开发者可在 Bash 中先执行 `. ./scripts/env.sh`；普通用户按
rustup 官方方式安装即可。编译遇到网络超时请检查 crates.io 访问，遇到链接器缺失请先
补齐上表中的构建工具；不要删除 `Cargo.lock` 来绕过 `--locked`。

### 2.6 Linux systemd 安装

仅适用于 **Linux + systemd + root/sudo 权限**，macOS、Windows、WSL 中未启用
systemd 的环境应使用上述手工安装。需先取得完整仓库，因为脚本会读取
`deploy/systemd/` 中的服务模板，不能只下载 `install.sh` 单个文件执行。

在克隆的仓库根目录运行；目前没有 Release 时，先完成上一节源码构建，再离线安装：

```bash
# 网关：安装本地编译好的二进制，并生成配置与自签证书
sudo ./deploy/install.sh --server --binary ./target/release/divertlink \
  --domain gateway.example.com

# 家庭端：在家庭机器的仓库执行，token 必须替换为网关打印的隧道 token
# 先生成配置，按第 5 节配置 target、CA/pin 后再启动
sudo ./deploy/install.sh --agent --binary ./target/release/divertlink \
  --gateway gateway.example.com:443 --token 'agt_REPLACE_WITH_GATEWAY_TOKEN' --no-start
```

Release 发布后，可将 `--binary ./target/release/divertlink` 换成
`--version 0.1.0`（实际发布版本），脚本会自动选择架构、下载并校验压缩包。
默认二进制为 `/usr/local/bin/divertlink`，配置为 `/etc/divertlink/server.toml` 或
`/etc/divertlink/agent.toml`。脚本会启用开机自启，默认立即启动；`--no-start` 只启用，
暂不启动。agent 配置完成后执行：

```bash
sudo divertlink check -c /etc/divertlink/agent.toml
sudo systemctl start divertlink-agent
sudo systemctl status divertlink-agent
sudo journalctl -u divertlink-agent -f
```

完整生产部署、证书替换与升级回滚见 [部署文档](deployment.md)。Docker 用户见
[Docker 部署](deployment.md#5-docker-部署)，可以从仓库本地构建镜像；预构建镜像
只有在维护者启用 GHCR 并发布成功后才可用。

### 2.7 安装验证与常见问题

在网关与家庭端分别执行：

```text
divertlink version
divertlink --help
divertlink console --open
```

版本命令应输出版本信息，帮助应列出 `server` / `agent` / `console` 等子命令。
桌面环境会打开 `http://127.0.0.1:17653/`；若未自动打开，请在本机浏览器输入此地址。
无桌面的服务器用 `version` 与 `--help` 验证即可。`console` 会持续占用当前终端，
按 `Ctrl+C` 退出；安装成功后继续第 3 节生成配置，或用控制台创建项目。

| 现象 | 处理方法 |
| --- | --- |
| 下载返回 404 | 检查是否已有 Release、版本标签和资产名；暂无 Release 时按 2.5 从源码安装 |
| `command not found` / 无法识别命令 | 按对应章节设置 PATH 并重开终端，或使用二进制绝对路径；Windows 当前目录程序加 `.\` |
| `Exec format error` / `Bad CPU type` / 此应用无法在你的电脑上运行 | 核对系统与 CPU，重新选择平台资产；不能将 Linux 包放到 macOS 或 Windows 原生环境运行 |
| SHA256 校验失败 | 停止安装，删除下载的压缩包及校验文件，重新下载同一版本；不要跳过校验 |
| `Permission denied` | Linux/macOS 执行 `chmod +x /实际路径/divertlink`，并确认所在文件系统允许执行 |
| `Address already in use` | 先停止已有进程；控制台可换端口：`divertlink console --listen 127.0.0.1:17654 --open` |

升级时先停止相关进程或服务，再按同一平台步骤安装新版本。配置和证书保存在项目目录
或 `/etc/divertlink` 中，应单独备份；替换二进制后重新执行 `version`、`check -c` 并启动。
用户目录安装的卸载方法是停止进程后删除对应的 `divertlink` / `divertlink.exe`，再移除
添加的 PATH 项。systemd 用户需先停止并禁用对应服务，再移除服务单元与二进制；
配置、证书和状态目录应在确认不再需要后单独清理。

### 2.8 监听 443 的权限

安装二进制不代表已经配置端口权限。Linux 上非 root 进程绑定 443 可选：

```bash
# 全局安装的二进制：授予绑定低端口能力
sudo setcap 'cap_net_bind_service=+ep' /usr/local/bin/divertlink

# 生产推荐：使用 systemd 单元中的 AmbientCapabilities，见部署文档
# 初次演练也可改用 8443（同时修改 tunnel_listen、ingress_listen、agent 地址与客户端 URL）
```

`setcap` 授权会在升级二进制后失效，需重新执行。macOS 非特权进程可先用 8443 演练；
Windows 若启动失败请检查端口占用与防火墙规则。agent 主动出站，不需要在家庭侧开放
443 或创建入站端口映射。

## 3. 生成配置与证书

以下命令在**网关服务器**上执行。`init` 会生成双端配置示例、自签 CA 与服务端证书，
并打印下一步提示。

```bash
mkdir -p ~/divertlink && cd ~/divertlink
divertlink init --role both --domain gateway.example.com
```

Windows 网关在 PowerShell 中改用以下建目录命令，其余 `divertlink` 子命令相同：

```powershell
New-Item -ItemType Directory -Force -Path "$env:USERPROFILE\divertlink" | Out-Null
Set-Location "$env:USERPROFILE\divertlink"
divertlink init --role both --domain gateway.example.com
```

生成的文件（如目录已有同名文件，`init` 默认不覆盖；确需覆盖加 `--force`）：

```text
~/divertlink/
├── server.toml          # 网关配置（已自动填入随机 API Key 与隧道 token）
├── agent.toml           # 家庭端配置（已自动填入同一 token、ca 与 pin）
└── certs/
    ├── ca.crt           # 自签 CA 证书（agent 用它校验网关证书）
    ├── ca.key           # CA 私钥：只留在网关/离线保存，绝不分发
    ├── server.crt       # 网关服务端证书（SAN 含 gateway.example.com）
    └── server.key       # 网关服务端私钥
```

> `init --role both` 会同时生成 CA 与服务端证书；`--role agent` **不生成任何证书**，
> 需要从网关拷贝 `certs/ca.crt`。若加 `--mtls`，还会生成 `client.crt` / `client.key`
> 并把这些路径写入 `agent.toml`，但**不会自动启用 mTLS**：还要在 `server.toml` 中取消
> `[tls].client_ca` 注释，并把 `tunnel_listen` 与 `ingress_listen` 拆成不同端口
> （用 `--dir` 指定绝对目录时，写入 agent 的客户端证书路径也是绝对路径，拷贝到家庭端后
> 需按实际目录调整）。

`init` 已经写入随机的 API Key（`dlk_...`）和隧道 token（`agt_...`），并打印 API Key 与
next steps；下面的手工字段说明适用于自行搭建或替换令牌的场景。

> 配置里的证书/CA 相对路径以**配置文件所在目录**为基准解析，不是进程工作目录。
> 因此 `~/divertlink/server.toml` 中的 `certs/server.crt` 指向
> `~/divertlink/certs/server.crt`；把配置移到别处时记得同步调整路径或目录布局。

如需手动控制证书参数（例如额外客户端证书、SPKI pin），可单独使用 `gen-certs`：

```bash
# 生成 CA + 服务端证书，并在输出中打印 SPKI pin（可填入 agent.toml 的 [tls].pin）
divertlink gen-certs --dir certs --domain gateway.example.com

# 仅生成自签服务端证书（实验环境）：没有独立 ca.crt，agent 的 [tls].ca 指向
# certs/server.crt 即可（输出里会提示），或改用 pin / insecure（后者禁止生产）
divertlink gen-certs --dir certs --domain gateway.example.com --self-signed

# 生成客户端证书（mTLS 用；必须使用默认的 CA 模式——与 --self-signed 同时使用时
# 客户端证书会被静默忽略，因为自签服务端证书不能充当 mTLS CA）
divertlink gen-certs --dir certs --domain gateway.example.com --client
```

> 正式环境建议使用公网 CA（如 Let's Encrypt）签发的证书：把证书链与私钥路径填入
> `server.toml` 的 `[tls].cert` / `[tls].key`，并删除 `agent.toml` 的 `[tls].ca`
> 配置行以使用系统根证书（**删除该行**，不要写成空字符串）。证书续期后需重启网关。


## 4. 配置并启动网关

编辑 `~/divertlink/server.toml`。`init` 生成的是完整示例（含随机 Key/token、`/home`
隧道与注释掉的静态路由），通常只需核对域名、监听地址与证书路径；手工搭建时至少要配置
以下字段：

```toml
[server]
tunnel_listen = "0.0.0.0:443"     # agent 隧道监听
ingress_listen = "0.0.0.0:443"    # 公网入口；与上面相同则按 ALPN 共用 443
admin_listen = "127.0.0.1:9090"   # 管理端点，只监听本机
trust_proxy = false               # 网关前有可信 nginx/Caddy 时才设 true

[tls]
cert = "certs/server.crt"
key = "certs/server.key"
min_version = "1.3"

[[api_keys]]
name = "default"
key = "dlk_REPLACE_WITH_RANDOM_KEY"      # 客户端访问网关用的 Key

[[tunnels]]
id = "home"                               # 必须与 agent.toml 的 id 一致
token = "agt_REPLACE_WITH_RANDOM_TOKEN"   # 必须与 agent.toml 的 token 一致
mount = "/home"                           # 公网路径前缀
```

> `http_listen`（默认 `127.0.0.1:8080`）与 `admin_listen`（默认 `127.0.0.1:9090`）
> 会随默认值自动监听；它们只适合本机使用，不要暴露到公网。不需要时可用
> `admin_listen = "off"` / `http_listen = "off"`（也接受 `false`）显式关闭；删除字段则
> 回落到默认值。

如需替换或新增令牌（`init` 生成的配置文件里已是随机值，可跳过）：

```bash
divertlink token --prefix dlk_ --count 1   # 生成网关 API Key
divertlink token --prefix agt_ --count 1   # 生成 agent 隧道令牌
```

启动前自检并前台启动，观察日志：

```bash
divertlink check -c server.toml
divertlink server -c server.toml
```

看到监听 443 与配置加载成功的日志后，保持前台运行；生产环境请改用 systemd（见
[部署文档](deployment.md)）。管理端点验证（在网关本机执行）：

```bash
curl -fsS http://127.0.0.1:9090/healthz
curl -fsS http://127.0.0.1:9090/readyz
```

> `http_listen = "127.0.0.1:8080"` 是给本机 nginx/new-api 用的明文入口，不要暴露到公网。
> 若 443 已被 nginx 占用，可让 nginx 终止公网 TLS 并反代到 `http_listen`，同时给 agent
> 隧道分配独立端口；也可用 nginx `stream` 按 ALPN `divertlink/1` 做 TLS 直通，详见
> [部署文档](deployment.md)。
>
> 若配置了 `[[static_routes]]`（静态反代到本机 new-api 等），默认 `require_auth = true`，
> 与隧道一样要求网关 API Key（并执行 `allow_ips`/`rate_limit`/`max_concurrent`）；只有
> 显式设置 `require_auth = false` 时才只透传请求头、由上游自行校验令牌，此时
> `upstream` 只应指向回环/内网地址。详见 [协议与调用 §5.2](protocols.md#52-拓扑-b网关静态路由反代到-new-api)。

> 不想手写配置？运行 `divertlink console --open` 打开内置图形控制台：
> 「＋ 新建项目」会生成双端配置与证书，并可在界面里一键启动、看实时日志、探测 TLS
> 证书。详见 [图形化客户端](console.md)。

## 5. 配置并启动家庭端 agent

### 5.1 把文件拷贝到家庭端机器

在网关服务器上执行（替换家庭端用户名与地址）：

```bash
scp agent.toml alice@192.168.1.50:~/divertlink/
scp certs/ca.crt alice@192.168.1.50:~/divertlink/certs/
```

家庭端创建目录（如果 `certs/` 不存在）：

```bash
mkdir -p ~/divertlink/certs && cd ~/divertlink
```

### 5.2 编辑 agent.toml

`init --role both` 已经填好 `id` / `server` / `token` / `ca` / `pin`，通常只需把
`[target].url` 改成家庭端实际服务地址；下面是各字段含义（手工搭建时照此填写）：

```toml
[agent]
id = "home"
server = "gateway.example.com:443"          # 网关域名或公网 IP
token = "agt_REPLACE_WITH_RANDOM_TOKEN"     # 与 server.toml 的 tunnels.token 一致
heartbeat = "5s"
heartbeat_timeout = "15s"
heartbeat_mode = "app"

[tls]
ca = "certs/ca.crt"      # 自签部署用 gen-certs 生成的 ca.crt；网关用公网 CA 时删除本行
# pin = "<gen-certs 输出的 SPKI pin>"   # 可选：固定网关公钥，防中间人
# servername = "gateway.example.com"    # 可选：默认自动取 server 的 host
# 仅当网关启用 mTLS（server.toml 设置了 [tls].client_ca）时才需要，两者必须同时配置：
# client_cert = "certs/client.crt"
# client_key  = "certs/client.key"

[target]
url = "http://127.0.0.1:8000"   # 家庭端 vLLM；new-api 为 :3000，Ollama 为 :11434
request_timeout = "10m"
connect_timeout = "5s"
```

确认家庭端服务本身可访问：

```bash
curl -fsS http://127.0.0.1:8000/v1/models
```

### 5.3 校验并启动 agent

```bash
divertlink check -c agent.toml
divertlink agent -c agent.toml
```

agent 会主动向网关 443 发起 TLS 连接（ALPN `divertlink/1`），认证通过后网关日志会出现
隧道 `home` 上线相关的记录；`/status` 或 `/readyz` 可看到隧道在线状态。

> agent 掉线会自动按指数退避（带 jitter）重连，无需人工处理；家庭宽带 CGNAT 不影响，
> 因为连接始终由家庭端主动发起并保活。

## 6. 放行防火墙与 DNS

1. **DNS**：在域名服务商处为 `gateway.example.com` 添加 A 记录指向网关公网 IP：

   ```bash
   dig +short gateway.example.com
   # 期望输出网关公网 IP
   ```

2. **云安全组/本机防火墙**：只放行需要的端口：

   ```bash
   # 以 ufw 为例（公网仅放行 443；80 仅在需要 HTTP 跳转/ACME 时放行）
   sudo ufw allow 443/tcp
   sudo ufw status
   ```

   - 公网：`443/tcp`（单端口 ALPN 复用）；如采用独立端口，则放行 `tunnel_listen` 与
     `ingress_listen` 对应端口；`http_listen`、`admin_listen` 只监听回环，不需要放行。
   - 家庭端：无需入站放行，只需能出站访问网关 443。

3. 确认 443 没有被其他服务占用：

   ```bash
   sudo ss -ltnp 'sport = :443'
   ```

## 7. 验证

以下验证在**客户端机器**执行；若使用自签证书，需要拿到 `ca.crt` 并用 `--cacert` 指定
（或把 CA 导入系统信任库），否则 curl 会报证书错误。

### 7.1 非流式请求

```bash
curl -sS https://gateway.example.com/home/v1/models \
  -H "Authorization: Bearer dlk_REPLACE_WITH_RANDOM_KEY"

curl -sS https://gateway.example.com/home/v1/chat/completions \
  --cacert ca.crt \
  -H "Authorization: Bearer dlk_REPLACE_WITH_RANDOM_KEY" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "Qwen2.5-7B-Instruct",
    "messages": [{"role": "user", "content": "你好"}],
    "max_tokens": 32,
    "stream": false
  }'
```

期望返回家庭端模型列表与一段正常的 chat completion JSON。

### 7.2 流式请求（SSE）

```bash
curl -N -sS https://gateway.example.com/home/v1/chat/completions \
  --cacert ca.crt \
  -H "Authorization: Bearer dlk_REPLACE_WITH_RANDOM_KEY" \
  -H "Content-Type: application/json" \
  -H "Accept: text/event-stream" \
  -d '{
    "model": "Qwen2.5-7B-Instruct",
    "messages": [{"role": "user", "content": "从 1 数到 20，每行一个数字"}],
    "stream": true
  }'
```

期望逐行出现 `data: {...}` 并以 `data: [DONE]` 结束。用下面的命令确认是**真流式**
（每行带时间戳；若所有行时间戳几乎相同，说明被缓冲了）：

```bash
curl -N -sS https://gateway.example.com/home/v1/chat/completions \
  --cacert ca.crt \
  -H "Authorization: Bearer dlk_REPLACE_WITH_RANDOM_KEY" \
  -H "Content-Type: application/json" \
  -H "Accept: text/event-stream" \
  -d '{"model":"Qwen2.5-7B-Instruct","stream":true,
       "messages":[{"role":"user","content":"从 1 数到 20"}]}' \
| python3 -u -c '
import sys, time
t0 = time.monotonic()
while True:
    line = sys.stdin.readline()
    if not line:
        break
    if line.startswith("data:"):
        print(f"{time.monotonic() - t0:7.3f}s  {line.rstrip()[:100]}")
'
```

更详细的流式验证方法与 new-api 集成见 [协议与调用](protocols.md#6-sse-流式验证)。

### 7.3 网关本地检查项

```bash
# 隧道是否在线（返回 JSON 快照）
curl -sS http://127.0.0.1:9090/status

# Prometheus 指标（便于接入监控）
curl -sS http://127.0.0.1:9090/metrics

# ALPN 是否正确暴露
openssl s_client -connect gateway.example.com:443 -servername gateway.example.com \
  -alpn divertlink/1 </dev/null 2>/dev/null | grep -E 'ALPN|Verify return code'
```

## 8. 常见错误

| 现象 | 排查方向 |
| --- | --- |
| `bind: permission denied`（443） | 使用 root、`setcap` 或 systemd `AmbientCapabilities`；或先换 8443 演练 |
| 客户端证书校验失败 | 自签证书加 `--cacert ca.crt`；确认 `gen-certs --domain` 与访问域名一致；agent 端检查 `[tls].ca` / `servername` |
| agent 报 CA/证书文件读取失败 | 相对路径按**配置文件所在目录**解析：确认 `agent.toml` 同级有 `certs/ca.crt`，或改写绝对路径 |
| 配置校验报“unknown field” | 配置文件里有多余/拼写错误的字段；按报错删除或修正，`trust_proxy` 等字段大小写敏感 |
| `401 Unauthorized` | 客户端 Key 与 `[[api_keys]].key` 不一致；OpenAI SDK 发 `Authorization: Bearer`，Anthropic SDK 发 `x-api-key`（两者网关都接受） |
| `403 Forbidden` | Key 有效但 `allow_tunnels` 未包含目标隧道，或来源 IP 不在 `allow_ips`；经前置代理时检查 `trust_proxy` |
| `404 Not Found` | URL 少了 mount 前缀（如把 `/home/v1/...` 写成 `/v1/...`）；确认 `[[tunnels]].mount` |
| `413 Payload Too Large` | 请求体超过 `[limits].max_request_bytes`（默认 64 MiB） |
| `429 Too Many Requests` | 命中 `[[api_keys]].rate_limit` 或 `max_concurrent`；按需放宽或拆 Key |
| `502 Bad Gateway` | 隧道已连上但数据流或家庭端 target 失败（target 未启动/地址错/不可达）；看 agent 日志与 `/status` |
| `503 Service Unavailable` | agent 未上线、token 不匹配、隧道被禁用或未连接；看双端日志与 `/status` |
| 流式内容一次性返回 | 前置 nginx/CDN 缓冲；`curl` 未加 `-N`；见 [协议文档 §6](protocols.md#6-sse-流式验证) |
| agent 反复重连 | 网关地址/端口错误、防火墙拦出站、心跳超时过短、时钟偏差；用 `openssl s_client` 验证 TLS 与 ALPN |
| DNS 解析不到 | A 记录未生效或指向错误 IP；`dig` 验证 |

更多按状态码与日志关键词的排查见 [故障排查](troubleshooting.md)。

## 9. 下一步

1. 用 systemd / Docker 托管进程，并配置开机自启与日志轮转：见 [部署文档](deployment.md)。
2. 收紧安全设置：替换所有示例令牌、设置 `allow_tunnels` / `allow_ips` / `rate_limit`，
   按 [安全指南](security.md) 与 [上线检查清单](security.md#9-上线检查清单) 逐项检查。
3. 接入 new-api / one-api 做额度与渠道管理：见 [协议与调用 §5](protocols.md#5-与-new-api--one-api-集成)。
4. 按业务量调整并发、超时与心跳：见 [性能与容量](performance.md)。
5. 了解每个字段：见 [配置参考](configuration.md)。

## 10. 端到端冒烟验证（可选）

如果不想手工一步步排错，仓库自带端到端冒烟脚本 `scripts/smoke.sh`。它会在临时目录中
自动完成一次完整链路验证：

1. `divertlink init --role both` 生成双端配置与自签证书；
2. 改写为随机空闲端口，启动 python mock 模型服务、gateway 与 agent；
3. 用 curl 验证 `/v1/models`、`/v1/chat/completions` 透传，SSE 是否逐块到达（带耗时
   断言，能发现缓冲），免认证的 `/health` 是否放行，未认证请求是否返回 401；
4. 检查管理端点 `/healthz`、`/readyz`、`/status`、`/metrics`。

```bash
# 在仓库根目录执行；自动查找 ./target/release 或 ./target/debug/divertlink
./scripts/smoke.sh

# 也可以显式指定任意路径的二进制（例如已安装到 PATH 的版本）
./scripts/smoke.sh -b /usr/local/bin/divertlink

# 或通过环境变量指定
DIVERTLINK_BIN=/usr/local/bin/divertlink ./scripts/smoke.sh
```

依赖 `bash`、`curl`、`python3`（Linux/macOS/WSL/Git Bash）。脚本自动选择随机空闲端口、
在 `mktemp` 临时目录中运行，退出（含失败）时自动清理子进程与临时目录，不会占用或影响
本机已有服务。找不到二进制时先 `cargo build`，或用 `-b` 指定。
