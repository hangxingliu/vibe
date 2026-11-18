# Vibe：Agent 指令

Vibe 是一个 Rust 单文件程序，在 ARM Mac 上启动 Debian Linux 虚拟机，用来隔离 LLM agent。它通过 `objc2` 调用 Apple 的 Virtualization 框架。终端直接连到虚拟机串口控制台。宿主机目录通过 VirtioFS 共享进客户机。

## 命令

- 构建：`cargo build --locked`（CI 发布用 `cargo build --release --locked`）。
- 测试：`cargo test`。
- 检查：`cargo fmt --check`、`cargo clippy`；改过的 shell 脚本跑 `shellcheck` 和 `shfmt -d`。工具版本见 `mise.toml`。
- 帮助文本：修改后把 `vibe --help` 的输出复制到 `USAGE.md` 的帮助代码块。去掉末尾动态生成的 "Cache directory" 和 "Image directories" 部分。

## 测试

- 完成 `docs/ai-plans/` 中某个计划的代码工作后，删除只用于验证重构或中间步骤的测试。
- 保留守护核心行为的测试：解析、状态机、磁盘读取、文件锁、下载、代理规则，以及下文的不变量。
- 不要测试常量、固定字符串，或注释已写清的简单函数。
- 不要测试仅供测试使用的辅助代码。

## 模块地图

修改模块前先读它。每行列出入口符号。

- `src/main.rs`：常量、内置脚本表、`run_main`（构造 `CommandContext` 并分发）。
- `src/cli.rs`：CLI 解析（`lexopt`）、`CliArgs`、`CliCommand`、`parse_cli_from`。测试在 `src/cli/tests.rs`。
- `src/help.rs`：`help_text`、`REPO_URL`、分节渲染器（`render_section`，90 列上限）、`provisioning_scripts_banner`。
- `src/run_command.rs`、`src/provision_command.rs`：`run_run` 与 `run_provision`，即 `run_main` 的两个命令体。
- `src/shares.rs`：缓存共享（`CacheShare`、`cache_share_specs`、`legacy_cache_notice`）、项目目录遮蔽（`project_masks_and_shares`）、`DirectoryShare`、`describe_vm_files`。
- `src/signing.rs`：`ensure_signed`。
- `src/vm_user.rs`：`VmUser`、`GuestPath`，客户机检查脚本 `src/guest/check_vm_user.sh` 和 `src/guest/ensure_vm_user.sh`（退出码 10-15 对应 `VmUserProblem`）。
- `src/session_plan.rs`：`SessionPlan` 各阶段与 `expand_session`。共享暂存、bind 挂载、遮蔽、bindfs 挂载 apt 缓存、切换到 vm-user。
- `src/session.rs`：`LoginAction`、`legacy_login_prefix`，并重新导出 `src/session/*`：`password.rs`（`RootPassword`）、`failure.rs`（`LoginFailure`）、`shell.rs`（脚本包装 `script_command`）、`login.rs`（`legacy_login`、`switch_user`）、`actions.rs`（登录线程）。
- `src/vm_run.rs`、`src/vm_lifecycle.rs`、`src/vm_config.rs`、`src/vm_io.rs`：启动并驱动虚拟机、无头运行、虚拟机配置、终端 I/O 与输入闸门。
- `src/provision.rs`：`ensure_default_image`、`provision_image`、`build_provision_script`、`split_run_as_user`。
- `src/disk_prepare.rs`、`src/maintenance.rs`、`src/alpine.rs`、`src/seed.rs`、`src/guest/maint.sh`、`src/control.rs`：用 Alpine 维护虚拟机处理新的 Debian 基础镜像。
- `src/image_probe.rs`、`src/gpt.rs`、`src/ext4ro.rs`（含 `src/ext4ro/`）、`src/cache.rs`：只读磁盘检查、文件锁、克隆。
- `src/images.rs`、`src/image_dirs.rs`：镜像下载、命名、跨镜像目录查找。
- `src/networking.rs`、`src/proxy.rs`：网络模式、`vibe-usernet` 参数、代理解析。
- `src/vm_checks/`（维护检查在 `src/vm_checks/maintenance/`）：供 `tests/` 中 `#[ignore]` 集成测试使用的虚拟机检查。不属于 CLI。
- `provisioning/*.sh`：内置脚本，由 `build.rs` 嵌入。`base.sh` 总会运行。
- `helpers/vibe-usernet/`：Go 网络辅助程序，由 `build.rs` 基于 `helpers/gvisor-tap-vsock` 子模块构建。

## 不变量

- 运行路径对磁盘只读。只有 provision 会探测或修改磁盘（`ensure_bootable`）。运行磁盘时从不调用 `gpt`、`ext4ro` 或 `image_probe`。
- Virtualization.framework 代码必须在进程主线程上运行。`cargo test` 的线程不是主线程；集成测试通过 `VIBE_VM_CHECK` 启动 `vibe` 二进制。
- 绝不打印 root 密码。`RootPassword` 打印为 `***`。不要把密码放进客户机命令行；用 `SecretSend` 键入。动作标签从不包含 `--send` 文本。
- 错误以所属阶段为前缀：`[boot]`、`[login]`、`[action N: label]`、`[shutdown]`、`[prepare]`、`[verify]`、`[download]`、`[image-dirs]`、`[probe]`。
- 先以 root 登录并挂载共享，再切换到非 root 的 `--vm-user`。相对 `GuestPath` 解析到该用户的 HOME 下。
- `apt` 保持为 `CacheShare::global`。其他缓存为 `CacheShare::user`（`<cache>/user-<vm-user>/<name>`）。
- 内置脚本用 `# vibe:run-as-user` 标记用户级部分的开始。自定义脚本从不拆分，以 root 运行。
- 依赖保持精简：`objc2` 系列、`lexopt`、`libc`、`regex`、`base64`。新增 crate 前先征询。
- CLI 输出和文档不用 emoji。readme 由人撰写；改动保持简短并沿用其语气。

## 集成测试

这些测试标记为 `#[ignore]`，需要 Mac 宿主机。用 `cargo test -- --ignored <name>` 运行。

- `tests/vm_devices.rs`：无额外输入。
- `tests/alpine_maintenance.rs`：e2fsprogs 来自 `VIBE_E2FS_SBIN` 或 `PATH`。
- `tests/disk_prepare.rs`：`VIBE_VM_CHECK_DISK`、`VIBE_VM_CHECK_CACHE`。
- `tests/vm_user.rs`：`VIBE_VM_CHECK_DISK`（root 无密码）；代理和 DNS 选项放进 `VIBE_VM_CHECK_ARGS`。
- 真实镜像单元测试：`VIBE_TEST_DEBIAN_NEW_RAW`、`VIBE_TEST_DEBIAN_OLD_RAW`、`VIBE_TEST_ALPINE_RAW`。
- `tests/fixtures/make-ext4.sh` 重建被 gitignore 的 ext4 测试镜像。缺少镜像时测试会自动运行它。

## 常见任务

- 添加默认挂载：在 `src/shares.rs` 的 `SYS_CACHE_SHARES` 或 `AGENT_CONF_SHARES` 中添加 `CacheShare`。按用户存放的数据用相对客户机路径。
- 修改客户机初始化：编辑 `provisioning/base.sh`。已有的 `default.raw` 不会变化。
- 修改代理规则：编辑 `src/proxy.rs` 以及辅助程序侧的 `helpers/gvisor-tap-vsock/pkg/services/{noproxy,netproxy,dns,forwarder}` 和 `pkg/virtualnetwork/proxy.go`。更新 `docs/network-proxy.md`。
- 修改共享或遮蔽：更新 `docs/file-shares-between-vm.md`。
- 修改启动、登录或 provision 流程：更新 `docs/how-vm-boot.md`。
- 每个 `*.zh-Hans.md` 与对应英文文件保持同步。

## 变更日志

`CHANGELOG.md` 遵循 [Keep a Changelog 1.1.0](https://keepachangelog.com/en/1.1.0/)，用英文编写。readme 链接到它，不列出与上游的差异。

- 把每个用户可见的变更加到 `## [Unreleased]` 下的 `Added`、`Changed`、`Deprecated`、`Removed`、`Fixed` 或 `Security`。用户可见指 CLI 选项、默认值、客户机行为、provisioning 脚本、用户设置的环境变量或支持的镜像。
- 破坏性变更加 `**Breaking:**` 前缀，并写明替代做法。
- 跳过重构、测试、模块布局、工具配置，以及内部或仅测试用的环境变量。
- 发布时：提升 `Cargo.toml` 的 `version`，把 `[Unreleased]` 改名为 `## [X.Y.Z] - YYYY-MM-DD`，并在其上方新增空的 `## [Unreleased]`。
