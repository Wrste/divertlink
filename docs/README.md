# 中文文档目录

**中文** · [English](en/README.md) · [项目首页](../README.md) · [发行下载](https://github.com/Wrste/divertlink/releases)

| 任务 | 中文 | English |
| --- | --- | --- |
| 安装及首次接通 | [多平台安装与快速开始](quickstart.md) | [Installation](en/installation.md) · [Quickstart](en/quickstart.md) |
| 配置字段及默认值 | [配置参考](configuration.md) | [Configuration](en/configuration.md) |
| systemd / Docker / 反向代理 | [部署与运维](deployment.md) | [Deployment](en/deployment.md) |
| 多模型、多机及副本 | [多模型部署](multi-model.md) | [Multi-model](en/multi-model.md) |
| TLS、鉴权及密钥 | [安全指南](security.md) | [Security](en/security.md) |
| URL、SDK 与 new-api | [协议与调用](protocols.md) | [API integration](en/protocols.md) |
| 图形控制台与 PWA | [图形化客户端](console.md) | [Console](en/console.md) |
| 错误与连接排查 | [故障排查](troubleshooting.md) | [Troubleshooting](en/troubleshooting.md) |
| 编译、签名与公证 | [发布指南](releasing.md) | [Releasing](en/releasing.md) |
| 智能体自动部署技能 | [安装与使用技能包](agent-skill.md) | [Agent deployment skill](en/agent-skill.md) |

中文保留完整字段及运维手册，英文提供对应的安装、配置和操作指南。二者命令、默认值与发布状态应保持一致；配置事实以 `src/config.rs`、`assets/*.example.toml` 和所安装版本的 `--help` 为准。

进阶资料：[常见问题](faq.md) · [性能与容量](performance.md) · [设计契约](DESIGN.md) · [配置示例](../examples/README.md)。这些资料当前主要为中文，英文入口在相应主题指南中提供背景；不将链接到中文的页面标作英文翻译。

维护中英双语文档的方法见[文档贡献说明](CONTRIBUTING.md)。
