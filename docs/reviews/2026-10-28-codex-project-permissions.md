# 2026-10-28 Codex 项目权限对齐

## 范围与设计

用户要求本仓 Codex 权限对齐同级 `tier`，减少频繁命令申请。本次仅新增项目级
`.codex/config.toml`，不复制 `tier` 的跨仓开发指令，不改全局配置或 `tier` 跟踪文件。

- 三个权限键与 `tier/.codex/config.toml` 完全相同：`sandbox_mode = "danger-full-access"`、
  `approval_policy = "on-request"`、`approvals_reviewer = "auto_review"`。
- `sandbox_workspace_write.writable_roots = ["../../tier"]` 镜像关联仓库范围；以本配置的
  `.codex/` 为基准，解析为 `/Users/zhaozhe/Documents/projects/tier`。此表仅在切换为
  `workspace-write` 时适用，不约束 `danger-full-access` 模式。
- 两项目在现有全局配置中均已为 `trusted`。项目配置在重新开启会话后加载，不改变当前
  受管理会话的边界；`on-request` 加 `auto_review` 不代表永不审批。
- 不编译、不发布、不部署、不重启 Codex 或 homeway，不访问生产实例。

## 技术门（独立，开工前）

非作者子 agent `permissions_review` 只读核验源配置、两个项目的信任条目和相对路径。
结论：**通过，无阻塞项**。权限键、fallback 范围及项目级边界符合授权；保留新会话与
信任要求注释，不复制无关指令，不扩大为全局改动。

## 代码门（独立，成件后）

非作者子 agent `permissions_review` 实读 8 行成件并独立执行 TOML 键值与路径检查。
结论：**通过，无阻塞项**。三个权限值与 `tier` 相同，顶层无额外键，fallback 子表只有
预期的关联路径；注释未宣称永不审批。`tier` 与全局配置准备前后 SHA256 不变，`tier`
配置的 Git 状态为空；未复制 `developer_instructions`。Rust/Go 直译痕迹检查不适用：
本次只有 TOML 与评审文档。

## 验证与落盘

- 准备阶段使用 Python `tomllib` 解析临时成件，比较三个权限值、精确键集合、fallback
  子表与相对路径，全部通过；独立评审者重复验证通过。`git diff --check` 通过。
- 8 行成件 SHA256：`6aec281cb3155f6e1a1b3af9c15151be84b5a2cb70ad963afcfda12a82647a35`。
- 源 `tier` 配置 SHA256：`d852526c3e573305f4b9e40c93ae510f51b080b83180d1f6602f1a0a326d9c05`。
- 主会话经一次定点审批写入受保护的 `.codex/config.toml`，实际文件 SHA256 与上述成件
  完全一致。落盘后 `tomllib` 再验三个权限值、精确键集合与 fallback 解析全部通过；
  全局两个项目仍为 `trusted`，`tier` 配置的 Git 状态为空，未修改全局或 `tier`。
- 主会话实测 Codex CLI `0.164.0` 的 `codex features list` 退出码 **0**，无配置错误；
  当前沙箱不能创建 PATH aliases 的警告不影响该结果。先前
  `codex --strict-config features list` 退出码 **1**，原因是该命令不支持此选项，非配置错误。
- 本次只验证配置落盘与原生 CLI 接受；当前受管理线程仍保持原权限，新配置在重新开启
  项目会话后生效。未运行程序测试套件，未重启 Codex 或 homeway。
