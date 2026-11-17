# DivertLink 桌面启动器

本目录提供三个平台的双击启动脚本，用于打开 **内置控制台**（`divertlink console`）。
它们不是 Electron/Tauri 应用：控制台是编进同一个二进制的本地 Web UI，浏览器/PWA
窗口就是界面，因此没有额外运行时与安装包。

| 平台 | 文件 | 使用方式 |
| --- | --- | --- |
| macOS | `divertlink-console.command` | 双击；首次可能需要在「系统设置 → 隐私与安全性」允许 |
| Windows | `divertlink-console.bat` | 双击；会自动打开默认浏览器 |
| Linux | `divertlink-console.desktop` | 复制到 `~/.local/share/applications/`，在应用菜单中打开 |

三者都会按以下顺序查找二进制：

1. 环境变量 `DIVERTLINK_BIN`
2. `PATH` 中的 `divertlink`
3. 仓库内 `target/release/divertlink`
4. 仓库内 `target/debug/divertlink`

若都没有，请先构建：

```bash
cargo build --release
```

## Linux 应用菜单

```bash
mkdir -p ~/.local/share/applications
cp scripts/desktop/divertlink-console.desktop ~/.local/share/applications/
# 确保 divertlink 在 PATH 中（例如 sudo install -m755 target/release/divertlink /usr/local/bin/）
update-desktop-database ~/.local/share/applications 2>/dev/null || true
```

若二进制不在 `PATH`，把 `.desktop` 里的 `Exec=` 改成绝对路径。

## macOS 双击

```bash
chmod +x scripts/desktop/divertlink-console.command
open scripts/desktop/divertlink-console.command
```

首次双击若被 Gatekeeper 拦截：右键 →「打开」→「打开」。

## Windows 双击

直接双击 `divertlink-console.bat`。若希望无终端窗口常驻，可把它注册为计划任务或
使用 NSSM 包装（长期运行请优先使用 Windows 服务；控制台适合临时查看与操作）。

## 相关文档

- [图形化客户端：内置控制台](../../docs/console.md)
- [快速开始](../../docs/quickstart.md)
- [部署文档](../../docs/deployment.md)
