# Vibe：项目上下文与指南

Vibe 是一款轻量级、零配置的工具，旨在 ARM 架构的 Mac 上启动 Linux 虚拟机，为 LLM 智能体（Agent）提供沙箱环境。该项目专注于速度、安全性和极简的资源占用。

## 项目概述

- **目的：** 为 LLM 智能体（例如 Claude Code、Codex 或 Gemini CLI）提供安全、隔离的环境来运行命令，避免对宿主系统造成风险。
- **核心技术：** 使用 **Rust** 构建，基于 Apple 的 **Virtualization framework**（通过 `objc2` 互操作）。
- **客户机操作系统：** 采用 **Debian Linux**（nocloud）基础镜像，并针对快速启动（约 10 秒）进行了优化。
- **架构：**
    - **单二进制文件：** 整个工具为一个独立的二进制文件。其中内嵌了 `vibe-usernet` 网络辅助程序和配置脚本。
    - **直连控制台：** 将宿主机终端直接连接到虚拟机的串口控制台。
    - **目录共享：** 使用 **VirtioFS** 实现宿主机与客户机之间的高性能目录共享。
    - **网络：** 默认的 `nat` 模式会启动内置的 Go 用户态网络辅助程序 `vibe-usernet`（基于 `helpers/gvisor-tap-vsock` 子模块并在 `helpers/vibe-usernet` 中构建），在宿主机网络切换时仍可保持连接。`vznat` 则使用 Apple 的 `VZNATNetworkDeviceAttachment`。
- **磁盘准备：** 较新的 Debian nocloud 镜像不再提供空 root 密码并在首次启动时暂停。`vibe provision`（包括隐式创建首个 `default.raw`）在运行脚本前，会先在无头（headless）Alpine 维护虚拟机中处理其基础镜像的私有工作副本（清除 `/etc/machine-id`，清空 root 密码字段）。直接运行磁盘（`disk.raw`、`.vibe/instance.raw`、`--image`）时绝不会对其进行探测或修改。
- **安全性：** 若缺失 `com.apple.security.virtualization` 权限（entitlement），会自动为其进行自签名。

## 核心文件与目录

- `src/main.rs`：入口点、CLI 参数解析（通过 `lexopt`）、帮助文本，以及 run/provision 的 `main()` 分发（项目目录共享和登录操作）。缓存共享由 `cache_share_specs` 从 `MISE_CACHE_SHARE`、`SYS_CACHE_SHARES` 和 `AGENT_CONF_SHARES` 中选取（`--[no-]mount-sys-cache`、`--[no-]mount-mise`、`--[no-]mount-agents-conf`）；`check_implicit_provision` 在隐式 provision `default.raw` 之前拒绝 `--no-mount-*`；`describe_vm_files` 在 VM 启动前打印共享目录与写入的文件。其中的 `#[cfg(test)] mod tests` 覆盖 CLI 解析和这些辅助函数。
- `src/vm_io.rs`：宿主机终端和控制台 I/O：`OutputMonitor`（可取消的等待，返回 `WaitResult::{Found, Timeout, Closed}`、`wait_for_any`、`close()`）、`spawn_vm_io`、raw 模式、终端尺寸调整转发以及输入门控（`decide_user_input`：丢弃键盘输入直到自动操作完成；门控关闭时连续收到两次 Ctrl-C 则请求停止）。
- `src/vm_config.rs`：Virtualization framework 配置。`VmSpec`（`DiskSpec`、`SerialRole`、`ShareDevice`）、`create_vm_configuration`、`default_interactive_spec`（历史沿用的单磁盘交互布局）、EFI 变量存储以及 `nsurl_from_path`。
- `src/vm_lifecycle.rs`：启动已配置的虚拟机（`start_vm`）、请求停止（`request_stop`）、强制停止（`force_stop`）以及 `run_headless`（日志文件、控制帧、各阶段超时期限）。
- `src/vm_run.rs`：`run_vm()`：引导磁盘启动，执行 root 登录和内部初始化，随后执行登录操作。返回 `RunOutcome`（所有操作完成、虚拟机正常停止以及仍在运行的调用方操作前缀）；操作错误使用 `[action N: label]`（N 从 1 开始统计调用方的操作），Ctrl-C 和强制停止使用 `[shutdown]`，虚拟机配置或启动错误使用 `[boot]`；若虚拟机未能进入 root shell 则报错并输出 `[boot]`/`[login]` 诊断信息。在加入操作线程之前会关闭 `OutputMonitor`，因此客户机关机时宿主机能立即退出。`drive_interactive` 与 `src/vm_checks/devices.rs` 共用。
- `src/session.rs`：`LoginAction`、登录操作线程、`legacy_login_prefix()`（root 登录、`Password:` 检测、就绪探测、内部设置，包括写入 `/root/.bash_logout`；从不写入或 source `/root/.profile`）、`write_bashrc_actions()`（把 `src/bashrc.sh` 写入 `/root/.profile` 并 source；由 provision 与 `--write-bashrc` 使用）、`LoginFailure` 诊断信息、`LoginAction::label()` 与 `action_prefix()`（绝不包含可能包含环境变量的 `--send` 文本）、`DiskSource` 与 `needs_provision_hint()`，以及构建客户机 shell 命令的辅助函数。
- `src/image_dirs.rs`：`ImageDirs`，在 `run_main` 中由 `VIBE_EXTRA_IMAGE_DIR`（`:` 分隔的绝对路径）和 `VIBE_PREFERRED_IMAGE_DIR`（默认缓存目录或 extra 中的某一项，必须存在）构造一次。查找顺序为首选目录、默认目录、其余 extra 目录；不存在的 extra 目录会被跳过。提供 `find_file`、`existing` 和 `describe`（run/provision 开始时以及 `--help` 末尾打印的目录列表）。错误使用 `[image-dirs]` 前缀。共享目录、`vibe-usernet` 和维护日志始终位于默认目录。
- `src/images.rs`：基础镜像下载与验证、镜像名称/路径（`find_named_image`、`new_image_path`、跨镜像目录查找的 `locate_debian_base`）以及实例磁盘。`fetch_verified` 和 `extract_disk_raw` 与 Alpine 缓存共用。Debian 下载保留了文件大小检查；其错误信息使用 `[download]` 前缀。
- `src/alpine.rs`：固定版本的 Alpine 维护镜像（`ensure_alpine_image`）：下载、SHA-512、解压并检查分区 2 的 ext4 UUID。下载与修复只在首选镜像目录中进行；其他镜像目录中的 raw 在 UUID 匹配时以只读方式复用，否则打印警告并跳过。缓存的原始镜像为只读输入。错误使用 `[download]` 前缀。
- `src/seed.rs`：NoCloud seed ISO（`build_seed`、`Manifest::serialize`）。写入 `meta-data`、`user-data` 和 `manifest`（`policy=minimal`），随后使用卷标 `cidata` 执行 `hdiutil makehybrid`。
- `src/guest/maint.sh`：由 tiny-cloud 作为 `user-data` 运行的 POSIX 维护脚本。用于清除 `/etc/machine-id` 和 root 密码字段。不会打印 shadow 文件内容。
- `src/maintenance.rs`：`process_disk`：宿主机安全检查、Alpine 克隆、seed ISO、无头维护虚拟机、步骤顺序检查、日志发布（写入默认目录下的 `logs/`，开头带有完整路径的头部）以及 `verify_processed`。`maint/` 创建在 Alpine raw 同目录下，失败时回退到首选镜像目录。`PrepareError` 显示为 `[prepare] <stage> (last PREP-STEP: ...)`，若为宿主机复检则显示为 `[verify] host-verify`，随后输出已发布的日志路径，若设置了 `VIBE_KEEP_FAILED_WORK=1`，还会附带保留的 `maint/<pid>-<nonce>/` 目录。Provision 会在 `ensure_bootable` 中调用它。运行路径不会调用。
- `src/disk_prepare.rs`：provision 工作副本的 `ensure_bootable`：探测、跳过或发出警告（针对不受支持的磁盘输出 `[probe]` 警告），或者运行 Alpine 维护。不被运行流程使用。
- `src/provision.rs`：配置流程（`ensure_default_image`、`provision_image`）。复制基础镜像，在扩展磁盘前调用 `ensure_bootable`，发布前清除 `/etc/machine-id`。错误使用 `[prepare]`、`[action N: label]`、`[shutdown]` 和 `[verify]`。设置 `VIBE_KEEP_FAILED_WORK=1` 时，失败的工作副本 `<name>.tmp.<pid>` 会被保留并打印路径；否则会被删除。
- `src/gpt.rs`：只读 GPT 解析器（`read_gpt`、`select_layout`、`scan_labels`）以及 ext4、FAT 和 ISO9660 的文件系统标签探测器。Alpine 解压、维护和配置（通过 `ensure_bootable`）会调用它。运行路径不会调用。
- `src/ext4ro.rs`：使用 `pread` 的受限只读 ext4 读取器（`Ext4Ro::open`、`read_file`、`lstat`）。符号链接直接返回而不进行追踪。Provision 通过 `image_probe` 读取。运行路径不会调用。
- `src/control.rs`：维护控制帧（`PREP-READY`、`PREP-STEP`、`PREP-RESULT`）、增量式 `FrameReader` 和 `new_nonce`。`run_headless` 将 hvc1 接入 `FrameReader`。步骤顺序由 `maintenance::observe_frame` 强制约束。
- `src/cache.rs`：`FileLock`（`flock`）、排他性临时文件、带复制退避策略的 `clonefile`、同目录 `replace`、`private_dir` 以及 `sweep_stale`。Alpine 下载会获取 `alpine.lock`。维护过程通过同级的 `{disk}.lock` 锁定工作副本（磁盘镜像本身不受 flock 锁定；由 Virtualization.framework 进行挂载），并将 Alpine 原始镜像克隆到其同目录下的 `maint/<pid>-<nonce>/`。`ensure_bootable` 调用该路径，完成后移除同级锁文件。
- `src/image_probe.rs`：`probe_disk`、纯逻辑 `classify` 和 `verify_processed`。不会存储 shadow 明文。`process_disk` 使用 `verify_processed` 进行复检。Provision 会探测其工作副本和生成的最终镜像。运行路径不会调用它。`VIBE_TRACE_PROBE` 会记录每次调用以供测试。
- `tests/fixtures/make-ext4.sh`：重新构建 `ext4ro` 和 `image_probe` 测试中使用的小型 gzip ext4 镜像。这些镜像被 gitignore。当任一所需镜像缺失时，上述测试会运行此脚本。
- `src/networking.rs`：`NetworkMode`（通过内置 `vibe-usernet` 辅助程序的 `Nat`，通过 Apple NAT 挂载的 `VzNat`）、代理和 DNS 选项，以及端口发布。
- `src/vm_checks/`：供 `#[ignore]` 集成测试使用的虚拟机检查，通过 `VIBE_VM_CHECK` 选择，并由 `main()` 在进程主线程上运行（Virtualization.framework 需要主线程，而 `cargo test` 的线程不是主线程）。`devices.rs`（`read-only-disk`、`headless-timeout`）、`maintenance.rs`（`maintenance-<name>`，例如 `maintenance-all`）和 `prepare.rs`（对 `VIBE_VM_CHECK_DISK` 执行 `probe`、`prepare`，Alpine 缓存位于 `VIBE_VM_CHECK_CACHE`）。不属于 CLI 的一部分。
- `provisioning/base.sh`：用于为默认 Debian 镜像初始化必要工具（gcc、rust、mise 等）的 Shell 脚本。
- `src/bashrc.sh`：客户机端 bash 配置。
- `src/entitlements.plist`：macOS 虚拟化所需的权限配置（entitlements）。
- `build.rs`：使用 `go build` 构建 Go 语言的 `vibe-usernet` 辅助程序（在非 Apple 宿主机上为存根），将其与配置脚本一同嵌入，并记录 git SHA 和构建日期。
- `helpers/vibe-usernet/`、`helpers/gvisor-tap-vsock/`：用户态网络辅助程序的源码（后者为 git 子模块）。

## 构建与运行

### 前置要求
- 基于 ARM 架构的 Mac（M1/M2/M3/M4）。
- macOS 13 (Ventura) 或更高版本。
- `codesign` 工具（Xcode / Command Line Tools 的一部分）。

### 命令
- **构建：** `cargo build --release`
- **运行：** `cargo run -- [OPTIONS] [disk-image.raw]`
- **测试：** `cargo test`（CLI 解析由 `src/main.rs` 中的 `#[cfg(test)] mod tests` 覆盖；镜像目录的解析与查找在 `src/image_dirs.rs` 和 `src/images.rs` 中包含测试；`OutputMonitor`、输入门控和登录/操作逻辑在 `src/vm_io.rs` 和 `src/session.rs` 中包含 `#[cfg(test)] mod tests`；GPT、ext4、控制帧、缓存辅助函数和镜像分类在 `src/gpt.rs`、`src/ext4ro.rs`、`src/control.rs`、`src/cache.rs` 和 `src/image_probe.rs` 中包含 `#[cfg(test)] mod tests`；`VmSpec` 结构和无头模式各阶段超时在 `src/vm_config.rs` 和 `src/vm_lifecycle.rs` 中包含测试；Alpine 缓存、seed manifest 和维护步骤顺序在 `src/alpine.rs`、`src/seed.rs` 和 `src/maintenance.rs` 中包含测试。准备决策和 machine-id 完成脚本在 `src/disk_prepare.rs` 和 `src/provision.rs` 中包含测试。真实磁盘镜像测试被标记为 `#[ignore]`，需要设置 `VIBE_TEST_DEBIAN_NEW_RAW`、`VIBE_TEST_DEBIAN_OLD_RAW` 和 `VIBE_TEST_ALPINE_RAW`。虚拟机的启动测试偏重集成测试且需要 Mac 宿主机；`tests/vm_devices.rs`（`VIBE_VM_CHECK=read-only-disk|headless-timeout`）、`tests/alpine_maintenance.rs`（`VIBE_VM_CHECK=maintenance-all`）和 `tests/disk_prepare.rs`（`VIBE_VM_CHECK=prepare`）被标记为 `#[ignore]`，它们会启动 `vibe` 二进制，使检查在其主线程上运行。维护检查使用 `VIBE_E2FS_SBIN` 或 `PATH` 中的 e2fsprogs。ext4 与探测测试在所需的 `tests/fixtures/*.img.gz` 缺失时会运行 `tests/fixtures/make-ext4.sh`；该脚本通过 `MKFS_EXT4`、`/tmp/e2fs-install`、Homebrew `e2fsprogs` 或 `PATH` 查找 `mkfs.ext4`、`debugfs` 和 `e2fsck`）。

## 开发规范

- **极简依赖：** 坚持“小二进制文件”理念。避免引入笨重的 crate。当前依赖主要为 `objc2`、`lexopt`、`libc` 和 `regex`。
- **资深工程规范：** 代码应保持整洁、符合 Rust 惯用法，并遵循现有模式（例如精准克制地使用 `objc2`）。
- **不使用 Emoji：** CLI 和文档均避免使用 Emoji，以保持专业、极简的风格。
- **以人为本：** 文档（如 README）由人编写，供人阅读。
- **macOS 互操作性：** 注意 macOS 特有的行为，特别是权限（entitlements）和安全边界。

## 常见任务

- **添加默认挂载：** 在 `main.rs` 的 `SYS_CACHE_SHARES` 或 `AGENT_CONF_SHARES` 中添加缓存共享（由 `cache_share_specs` 选取），或修改 `run_main()` 中 `CliCommand::Run` 分支的项目目录共享。
- **更新客户机预配配置：** 编辑 `provisioning/base.sh`。注意，这仅在创建 `default.raw` 的*首次*运行时生效。
- **调整网络设置：** 查看 `src/networking.rs` 和 `NetworkMode` 枚举。
- **修复权限问题：** `main.rs` 包含一个 `ensure_signed()` 函数，用于处理自签名和重新执行。
