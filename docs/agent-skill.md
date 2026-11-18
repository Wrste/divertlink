# 智能体部署技能包

**中文** · [English](en/agent-skill.md) · [文档目录](README.md)

项目附带 [`divertlink-deploy`](../skills/divertlink-deploy/SKILL.md) 技能，供支持 `SKILL.md` 的智能体安装使用。它指导智能体检查实际主机、选择已发布二进制或源码构建、生成双端配置、处理 TLS 与上游鉴权、选择 systemd / Docker / 前台运行，并逐条验证隧道和入口。可用中文或英文提出部署任务。

技能包本身不包含 DivertLink 二进制、服务器账号、私钥或令牌，也不会赋予主机访问权限。部署时复用你已经授权的本机/SSH 环境；已有配置、证书与密钥默认保留。当前文档和技能准备工作不会连接或改动你的生产服务器。

## 安装到 Codex

从仓库根目录复制完整技能文件夹到技能目录，保留里面的 `scripts/`、`references/`、`agents/`。Linux/macOS：

```bash
skill_root="${CODEX_HOME:-$HOME/.codex}/skills"
mkdir -p "$skill_root"
# 若目标已存在，先检查差异，决定如何备份/更新，不直接覆盖。
if [ -e "$skill_root/divertlink-deploy" ]; then
  echo '技能已存在，请先检查并备份后更新。'
else
  cp -R skills/divertlink-deploy "$skill_root/"
fi
```

Windows PowerShell（在仓库根目录）：

```powershell
$codexRoot = if ($env:CODEX_HOME) { $env:CODEX_HOME } else { Join-Path $HOME '.codex' }
$skillRoot = Join-Path $codexRoot 'skills'
$destination = Join-Path $skillRoot 'divertlink-deploy'
New-Item -ItemType Directory -Force -Path $skillRoot | Out-Null
if (Test-Path $destination) { throw '技能已存在，请先检查并备份后更新。' }
Copy-Item -Recurse .\skills\divertlink-deploy $destination
```

其他支持技能的智能体可将该文件夹放入其技能发现目录，或让智能体读取 `SKILL.md`。辅助脚本要求 **Python 3.9+ 标准库**；HTTPS 验证默认 TLS 1.3 入口还需要支持 TLS 1.3 的 SSL 后端。macOS 系统 Python 可能使用旧 LibreSSL，应换用带现代 OpenSSL 的 Python；仅管理端点的 HTTP 检查不受影响。不使用辅助脚本时，DivertLink 的 Unix Release 本身不需要 Python。

## 生成可分享的 ZIP

```bash
python3 scripts/package-skill.py
# dist/divertlink-deploy-skill.zip
# dist/divertlink-deploy-skill.zip.sha256
```

ZIP 内只有一个 `divertlink-deploy/` 根目录，包含技能、参考资料、两个辅助脚本、UI 元数据及项目双许可证。打包脚本只收录明确类型的文件，不收录缓存、运行日志、二进制或本地凭据。解压后把完整文件夹放到上述技能目录；不要只复制 `SKILL.md`。

## 示例任务

首次部署：

> 使用 $divertlink-deploy，在已授权 SSH 别名 cloud-gateway 的 Linux x86_64 服务器上部署网关，域名 gateway.example.com；本机 macOS ARM64 作 agent，模型地址 http://127.0.0.1:11434。先检查端口和现有服务，使用 v0.1.1；网关用 systemd，保留现有证书。完成后验证 home 隧道、入口鉴权和模型列表，不发起推理请求。

已有 nginx 占用 443：

> 使用 $divertlink-deploy，接入现有 nginx 的 HTTPS 入口，agent 直接连接 8443，HTTP 回源保持 loopback。检查 SSE 配置，保留已有站点、证书和密钥。

升级或多模型：

> 使用 $divertlink-deploy，检查现有网关与两个 agent 的配置，为 qwen 和 llama 分别设置独立隧道、挂载路径和 API Key 权限。先备份，逐条验证，失败时恢复原配置；不输出密钥。

示例中的主机、域名、版本和模型地址需要换成实际值。技能会先利用可读环境信息，再询问阻塞部署的缺失信息。只提供文档/计划的任务不等于授权修改线上服务。

## 二进制安装辅助脚本

以下路径相对仓库根目录；安装后改成技能所在目录。先查看计划，不下载、不写文件：

```bash
python3 skills/divertlink-deploy/scripts/install_binary.py \
  --version 0.1.1 --bin-dir "$HOME/.local/bin" --dry-run
# 确认目标平台和目录后执行安装：
python3 skills/divertlink-deploy/scripts/install_binary.py \
  --version 0.1.1 --bin-dir "$HOME/.local/bin"
```

脚本自动识别 OS/CPU，下载固定版本及 SHA256 文件，校验归档，仅安装其中的可执行文件，并执行 `version` 检查。macOS 还执行 `codesign --verify --strict`。已有文件默认拒绝覆盖；有意升级并停止相关进程后才使用 `--replace`。它不修改 PATH、服务、防火墙或配置。离线公证票据验证应使用 [DMG 安装](quickstart.md#2-安装-divertlink)。

v0.1.1 的 Linux x86_64/ARM64 和 macOS Intel/ARM64 产物将在本次 Release 构建；Windows ZIP 是否可用需以 Release 页面为准。辅助脚本会检查实际资产，缺失时应按[多平台源码安装](quickstart.md#25-从源码安装所有平台)构建。

## 部署结果验证脚本

在网关主机执行，或通过已授权的 SSH 本地转发访问管理端点。API Key 放在私有环境变量 `DIVERTLINK_API_KEY` 中，不作为脚本参数或聊天内容：

```bash
python3 skills/divertlink-deploy/scripts/verify.py \
  --admin-url http://127.0.0.1:9090 --tunnel-id home \
  --ingress-url https://gateway.example.com/home/v1 \
  --key-env DIVERTLINK_API_KEY --ca /private/path/ca.crt
```

使用公网 CA 时省略 `--ca`。脚本检查 `/healthz`、`/readyz`、指定隧道是否连接、未认证模型列表请求是否返回 401，以及使用网关 Key 后是否返回模型列表。结果仅输出检查状态，不输出响应内容或密钥；全部通过退出码为 0，失败为 1。它拒绝非回环管理 URL、远程明文入口和重定向，不会关闭 TLS 验证。

只验证管理端点时省略 `--ingress-url`；这不代表 API 已接通。Anthropic 或自定义服务不一定实现 `/v1/models`，应另选有效端点验证鉴权。模型列表成功不证明 SSE 正常；需要流式验证时，由智能体在任务允许范围内执行。仓库的 `scripts/smoke.sh` 使用临时 mock 服务，可无模型配额验证本地链路。

## 维护与验证

技能入口负责决策与关键约束，`references/` 保存配置/部署细节；不要把真实配置和密钥写回技能包。修改后运行：

```bash
python3 -m unittest discover -s tests -p 'test_deploy_skill.py' -v
python3 scripts/package-skill.py
```

维护规范及中英文映射见[文档贡献说明](CONTRIBUTING.md)。
