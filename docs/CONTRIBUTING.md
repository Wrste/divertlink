# 文档维护 / Documentation maintenance

[中文目录](README.md) · [English index](en/README.md)

## 中文

- 中文首页为 `README.md`，英文首页为 `README.en.md`；主题映射以两个文档目录为准。中文安装位于 `quickstart.md` 第 2 节，英文拆成 `en/installation.md` 和 `en/quickstart.md`。
- 修改 CLI、配置字段、默认值、资产命名或发布状态时，同步更新两种语言的对应说明、示例和技能参考。事实来源是所选版本的 `src/config.rs`、`assets/*.example.toml`、`--help` 及实际 Release 资产，不从 CI 矩阵推断可下载包。
- 新页面在顶部提供中文/English及目录链接；链接指向仓库相对路径。翻译不是逐字拷贝，但命令、端口、路径、默认值、权限与行为必须一致。尚未翻译的进阶中文资料应明确标注。
- 示例使用占位域名/模型名，真实密钥只通过私有文件或环境传递。区分网关 Key、隧道 token 与上游 Key。不要让默认示例跳过 TLS 校验、覆盖现有配置或公开管理端口。
- 修改命令后在适用平台验证；无法实际运行的平台如实记录。修改技能辅助脚本运行 `python3 -m unittest discover -s tests -p 'test_deploy_skill.py' -v`，修改打包内容后运行 `python3 scripts/package-skill.py`。`dist/` 生成包不加入 Git。
- 提交前检查相对链接、代码块和 `git diff --check`。文档独立修改不要求重跑全部 Rust 测试；配置/运行逻辑变化应执行对应测试。

## English

- `README.md` is the Chinese home and `README.en.md` is English. Both documentation indexes map corresponding guides; Chinese installation lives in quickstart section 2, while English installation has its own page.
- Synchronize CLI/config defaults, commands, asset names and publication state across both languages and the deployment skill when these change. Use the selected source revision, annotated templates, binary help and actual Release assets as evidence; a CI target is not proof of a published asset.
- New guides need language and index navigation using repository-relative links. Task-oriented translations may differ in organization, but operational behavior must agree. Mark supplementary Chinese-only references honestly.
- Use example domains/models and private files/environment for secrets. Distinguish gateway API keys, tunnel tokens and upstream keys. Do not normalize disabled TLS checks, forced overwrites or publicly exposed management endpoints.
- Exercise changed commands on applicable platforms and record untested ones. Run the skill helper tests above after script changes and rebuild the portable ZIP after packaging changes. Generated `dist/` artifacts stay out of Git.
- Check links, fenced code and `git diff --check` before committing. Run relevant implementation tests when behavior changes; documentation alone does not need the entire Rust suite.
