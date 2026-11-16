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

- `src/main.rs`：入口点、CLI 参数解析（通过 `lexopt`）、帮助文本，以及 run/provision 的 `main()` 分发（项目目录共享和登录操作）。缓存共享（`CacheShare`）由 `cache_share_specs` 从 `MISE_CACHE_SHARE`、`SYS_CACHE_SHARES` 和 `AGENT_CONF_SHARES` 中选取（`--[no-]mount-sys-cache`、`--[no-]mount-mise`、`--[no-]mount-agents-conf`）；`apt` 为 `Global`（`<cache>/apt`），其他为 `User`（`<cache>/user-<vm-user>/<name>`），`legacy_cache_notice` 提示旧的顶层缓存目录；`check_implicit_provision` 在隐式 provision `default.raw` 之前拒绝 `--no-mount-*`，以及没有 root 密码的非 root `--vm-user`；`describe_vm_files` 在 VM 启动前打印共享目录与写入的文件。`--vm-user` 与 `--root-password` 是共用选项（可写在 `provision` 前后，后一个生效）。`--vm-user` 选择相对客户机路径（apt 以外的缓存共享、项目共享、`.tmux`）所在 HOME 的账号；`run_session_plan` 构建 run 的 `SessionPlan`，`project_masks_and_shares` 生成 `.vibe`/`.git` 遮蔽与额外共享（不遮蔽 `.env`）；显式 provision 且 `--vm-user` 不是 root 时必须提供 root 密码（`check_provision_root_password`）。会话以 root 登录并挂载共享，非 root 用户时在用户动作之前切换到该用户（`switch_user`）；`existing_image_repl_actions` 对已有镜像的 `--repl` 做同样的事。`--root-password`，或在省略该选项时的 `VIBE_ROOT_PASSWORD`，是传给登录流程的密码。其中的 `#[cfg(test)] mod tests` 覆盖 CLI 解析和这些辅助函数。
- `src/vm_user.rs`：`--vm-user` 的 `VmUser`。名称匹配 `^[a-z_][a-z0-9_-]*$`，长度为 1–32。默认是 `root`，`--vm-user root` 与默认值相同。`GuestPath`（绝对路径，或相对于该用户 HOME 的路径：root 为 `/root/...`，其他用户为 `"${VIBE_VM_HOME}"/...`）。`check_vm_user_actions`（run、已有镜像的 `--repl`）与 `ensure_vm_user_actions`（provision）运行 `src/guest/check_vm_user.sh` / `src/guest/ensure_vm_user.sh`，并导出 `VIBE_VM_USER`、`VIBE_VM_UID`、`VIBE_VM_GID`、`VIBE_VM_HOME`；`VmUserProblem` 对应检查脚本的退出码 10–15。
- `src/session_plan.rs`：`SessionPlan`（阶段：登录、`root_prepare`、挂载与遮蔽、`root_actions`、切换到非 root 用户（`switch_user_actions`：关闭 `legacy_tiocsti`，然后是 `LoginAction::SwitchUser`）、`user_actions`、尾部）与 `expand_session`。共享先挂到 `/mnt/root/virtiofs-shared`（`/mnt/root` 为 0700），再 bind mount 到目标；非 root 用户 HOME 下缺失的目录以该用户为属主创建；遮蔽为 root 所有的 `mode=0555` tmpfs；非 root 的 run 用 `bindfs` 挂载 apt 缓存（不可用时回退并在宿主机警告）。另含 `install_bindfs_action`（每次执行脚本的 provision）、宿主机提示和 root 无密码警告。共享的布局、VirtioFS 权限模型与屏蔽的边界见 `docs/file-shares-between-vm.md`。
- `src/vm_io.rs`：宿主机终端和控制台 I/O：`OutputMonitor`（可取消的等待，返回 `WaitResult::{Found, Timeout, Closed}`、`wait_for_any`、`close()`）、`spawn_vm_io`、raw 模式、终端尺寸调整转发以及输入门控（`decide_user_input`：丢弃键盘输入直到自动操作完成；门控关闭时连续收到两次 Ctrl-C 则请求停止）。
- `src/vm_config.rs`：Virtualization framework 配置。`VmSpec`（`DiskSpec`、`SerialRole`、`ShareDevice`）、`create_vm_configuration`、`default_interactive_spec`（历史沿用的单磁盘交互布局）、EFI 变量存储以及 `nsurl_from_path`。
- `src/vm_lifecycle.rs`：启动已配置的虚拟机（`start_vm`）、请求停止（`request_stop`）、强制停止（`force_stop`）以及 `run_headless`（日志文件、控制帧、各阶段超时期限）。
- `src/vm_run.rs`：`run_vm()`：引导磁盘启动，执行 root 登录和内部初始化，随后执行登录操作。返回 `RunOutcome`（所有操作完成、虚拟机正常停止以及仍在运行的调用方操作前缀）；操作错误使用 `[action N: label]`（N 从 1 开始统计调用方的操作），Ctrl-C 和强制停止使用 `[shutdown]`，虚拟机配置或启动错误使用 `[boot]`；若虚拟机未能进入 root shell 则报错并输出 `[boot]`/`[login]` 诊断信息。在加入操作线程之前会关闭 `OutputMonitor`，因此客户机关机时宿主机能立即退出。`drive_interactive` 与 `src/vm_checks/devices.rs` 共用。两者都接收 `SessionPlan`；`drive_interactive` 打印步骤提示与 root 无密码警告，并把检查脚本的退出码 10–15 转为 `LoginFailure::VmUser`。
- `src/session.rs`：`LoginAction`、登录操作线程、`legacy_login_prefix()`（root 登录、在 `Password:` 提示下按需键入密码、就绪探测、内部设置，包括写入 `/root/.bash_logout`；从不写入或 source `/root/.profile`）、`RootPassword`（`Debug` 与 `Display` 输出 `***`；`expose()` 只在键入时使用）、`write_bashrc_actions()`（把 `src/bashrc.sh` 写入 `/root/.profile` 并 source；由 provision 与 `--write-bashrc` 使用）、`set_root_password_actions()`（一条不含密码的 `chpasswd` 命令，随后是 `SecretSend`）、`LoginAction::StatusCommand`（stdin 来自 `/dev/null`，在 `VIBE_STEP_STATUS_<id>` 之后报告 `<status> <notice>`）、`LoginFailure` 诊断信息（`PasswordRequired`、`AuthFailed`、`AuthTimeout` 和 `VmUser` 不建议 `vibe provision`）、`LoginAction::SwitchUser`（`su --login --pty "$VIBE_VM_USER"`，随后探测用户名与 uid；`LoginFailure::SwitchFailed`）、`write_user_file_into_vm` 与 `write_user_bashrc_action`（用户 HOME 中的文件，以该用户身份写入）、`Script` 与 `UserScript` 的包装 `script_command`（按 uid 区分的 `/tmp/vibe-scripts-<uid>`；`UserScript` 经 `runuser` 与 `bash --login -c 'exec "$0"'` 以 vm-user 运行）、`LoginAction::label()` 与 `action_prefix()`（绝不包含可能含环境变量的 `--send` 文本，也不包含密码）、`DiskSource` 与 `needs_provision_hint()`，以及构建客户机 shell 命令的辅助函数。
- `src/image_dirs.rs`：`ImageDirs`，在 `run_main` 中由 `VIBE_EXTRA_IMAGE_DIR`（`:` 分隔的绝对路径）和 `VIBE_PREFERRED_IMAGE_DIR`（默认缓存目录或 extra 中的某一项，必须存在）构造一次。查找顺序为首选目录、默认目录、其余 extra 目录；不存在的 extra 目录会被跳过。提供 `find_file`、`existing` 和 `describe`（run/provision 开始时以及 `--help` 末尾打印的目录列表）。错误使用 `[image-dirs]` 前缀。共享目录、`vibe-usernet` 和维护日志始终位于默认目录。
- `src/images.rs`：基础镜像下载与验证、镜像名称/路径（`find_named_image`、`new_image_path`、跨镜像目录查找的 `locate_debian_base`）以及实例磁盘。`fetch_verified` 和 `extract_disk_raw` 与 Alpine 缓存共用。每次 curl 下载都使用 `proxy::download_proxy()` 给出的参数，并打印 `Host downloads proxy:` 一行。Debian 下载保留了文件大小检查；其错误信息使用 `[download]` 前缀。
- `src/alpine.rs`：固定版本的 Alpine 维护镜像（`ensure_alpine_image`）：下载、SHA-512、解压并检查分区 2 的 ext4 UUID。下载与修复只在首选镜像目录中进行；其他镜像目录中的 raw 在 UUID 匹配时以只读方式复用，否则打印警告并跳过。缓存的原始镜像为只读输入。错误使用 `[download]` 前缀。
- `src/seed.rs`：NoCloud seed ISO（`build_seed`、`Manifest::serialize`）。写入 `meta-data`、`user-data` 和 `manifest`（`policy=minimal`），随后使用卷标 `cidata` 执行 `hdiutil makehybrid`。
- `src/guest/maint.sh`：由 tiny-cloud 作为 `user-data` 运行的 POSIX 维护脚本。用于清除 `/etc/machine-id` 和 root 密码字段。不会打印 shadow 文件内容。
- `src/maintenance.rs`：`process_disk`：宿主机安全检查、Alpine 克隆、seed ISO、无头维护虚拟机、步骤顺序检查、日志发布（写入默认目录下的 `logs/`，开头带有完整路径的头部）以及 `verify_processed`。`maint/` 创建在 Alpine raw 同目录下，失败时回退到首选镜像目录。`PrepareError` 显示为 `[prepare] <stage> (last PREP-STEP: ...)`，若为宿主机复检则显示为 `[verify] host-verify`，随后输出已发布的日志路径，若设置了 `VIBE_KEEP_FAILED_WORK=1`，还会附带保留的 `maint/<pid>-<nonce>/` 目录。Provision 会在 `ensure_bootable` 中调用它。运行路径不会调用。
- `src/disk_prepare.rs`：provision 工作副本的 `ensure_bootable`：探测、跳过或发出警告（针对不受支持的磁盘输出 `[probe]` 警告），或者运行 Alpine 维护。不被运行流程使用。
- `src/provision.rs`：配置流程（`ensure_default_image`、`provision_image`）。复制基础镜像，在扩展磁盘前调用 `ensure_bootable`（`vznat` 模式下 base 脚本把 `GuestProxy` 写入 `/etc/apt/apt.conf.d/99timeout`，并连同 `no_proxy` 一起导出），发布前清除 `/etc/machine-id`。会运行脚本的 provision（包括新镜像的 `--repl` 和隐式 `default.raw`）在登录之后、`.profile` 与脚本之前用 `chpasswd` 设置给出的 root 密码。run 以及已有镜像的 `--repl` 只用该密码登录。`build_provision_script` 为非 root 的 `--vm-user` 拆分内置脚本（root 部分为 `Script`，用户部分为 `UserScript`）；`provision_written_files` 列出 `/root/.profile` 与用户的 `.profile`；非 root 的 provision 在清除 machine-id 之前执行 `check-vm-user-home`（用户 HOME 中没有属主为 root 的文件），其 `--repl` 会切换到该用户。错误使用 `[prepare]`、`[action N: label]`、`[shutdown]` 和 `[verify]`。设置密码失败时为 `[login] setup set root password failed with exit code N`。设置 `VIBE_KEEP_FAILED_WORK=1` 时，失败的工作副本 `<name>.tmp.<pid>` 会被保留并打印路径；否则会被删除。
- `src/gpt.rs`：只读 GPT 解析器（`read_gpt`、`select_layout`、`scan_labels`）以及 ext4、FAT 和 ISO9660 的文件系统标签探测器。Alpine 解压、维护和配置（通过 `ensure_bootable`）会调用它。运行路径不会调用。
- `src/ext4ro.rs`：使用 `pread` 的受限只读 ext4 读取器（`Ext4Ro::open`、`read_file`、`lstat`）。符号链接直接返回而不进行追踪。Provision 通过 `image_probe` 读取。运行路径不会调用。
- `src/control.rs`：维护控制帧（`PREP-READY`、`PREP-STEP`、`PREP-RESULT`）、增量式 `FrameReader` 和 `new_nonce`。`run_headless` 将 hvc1 接入 `FrameReader`。步骤顺序由 `maintenance::observe_frame` 强制约束。
- `src/cache.rs`：`FileLock`（`flock`）、排他性临时文件、带复制退避策略的 `clonefile`、同目录 `replace`、`private_dir` 以及 `sweep_stale`。Alpine 下载会获取 `alpine.lock`。维护过程通过同级的 `{disk}.lock` 锁定工作副本（磁盘镜像本身不受 flock 锁定；由 Virtualization.framework 进行挂载），并将 Alpine 原始镜像克隆到其同目录下的 `maint/<pid>-<nonce>/`。`ensure_bootable` 调用该路径，完成后移除同级锁文件。
- `src/image_probe.rs`：`probe_disk`、纯逻辑 `classify` 和 `verify_processed`。不会存储 shadow 明文。`process_disk` 使用 `verify_processed` 进行复检。Provision 会探测其工作副本和生成的最终镜像。运行路径不会调用它。`VIBE_TRACE_PROBE` 会记录每次调用以供测试。
- `tests/fixtures/make-ext4.sh`：重新构建 `ext4ro` 和 `image_probe` 测试中使用的小型 gzip ext4 镜像。这些镜像被 gitignore。当任一所需镜像缺失时，上述测试会运行此脚本。
- `src/networking.rs`：`NetworkMode`（通过内置 `vibe-usernet` 辅助程序的 `Nat`，通过 Apple NAT 挂载的 `VzNat`）、`usernet_args`（总是显式传递各个代理开关）、DNS 选项、端口发布，以及辅助程序的启动握手（`ready` 之前的 `dns-upstreams <mode> <description>` 行）。
- `src/proxy.rs`：VM 与宿主机下载共用的代理解析。`HostProxyEnv`（`https_proxy` > `HTTPS_PROXY` > `http_proxy` > `HTTP_PROXY`，bypass 为 `no_proxy` > `NO_PROXY`）、URL 规范化与 `redact_proxy_url`、`BypassList`（`--proxy-bypass` 替换内置列表）、`check_cli_proxy_args`（解析时检查）、`resolve_vm_proxy`（`VmProxyPlan`：`nat` 用的 `VmProxy`、`vznat` 用的 `GuestProxy`、摘要与警告），以及 `DownloadProxy`（curl 的 `--proxy`/`--noproxy`，在 `run_main` 中设置一次，由 `images.rs` 读取）。行为说明见 `docs/network-proxy.zh-Hans.md`。
- `src/vm_checks/`：供 `#[ignore]` 集成测试使用的虚拟机检查，通过 `VIBE_VM_CHECK` 选择，并由 `main()` 在进程主线程上运行（Virtualization.framework 需要主线程，而 `cargo test` 的线程不是主线程）。`devices.rs`（`read-only-disk`、`headless-timeout`）、`maintenance.rs`（`maintenance-<name>`，例如 `maintenance-all`）、`prepare.rs`（对 `VIBE_VM_CHECK_DISK` 执行 `probe`、`prepare`，Alpine 缓存位于 `VIBE_VM_CHECK_CACHE`）和 `vm_user.rs`（`vm-user`、`root-password`：在 `VIBE_VM_CHECK_DISK` 的克隆上以子进程驱动 `vibe` CLI，额外参数来自 `VIBE_VM_CHECK_ARGS`）。不属于 CLI 的一部分。
- `provisioning/base.sh`：用于为默认 Debian 镜像初始化必要工具（gcc、rust、mise 等）的 Shell 脚本。
- `provisioning/*.sh`：内置脚本可以包含一行 `# vibe:run-as-user`。非 root 的 `--vm-user` 时，`split_run_as_user`（`src/provision.rs`）以 root 运行该行之前的部分，以该用户运行之后的部分（加上 shebang 行）；每部分自带头部。自定义脚本从不拆分，始终以 root 运行。
- `src/bashrc.sh`：客户机端 bash 配置。
- `src/entitlements.plist`：macOS 虚拟化所需的权限配置（entitlements）。
- `build.rs`：使用 `go build` 构建 Go 语言的 `vibe-usernet` 辅助程序（在非 Apple 宿主机上为存根；`helpers/vibe-usernet` 或子模块的 `pkg/`、`go.mod`、`go.sum` 变化时重新构建），将其与配置脚本一同嵌入，并记录 git SHA 和构建日期。
- `helpers/vibe-usernet/`、`helpers/gvisor-tap-vsock/`：用户态网络辅助程序的源码（后者为 git 子模块）。

## 构建与运行

### 前置要求
- 基于 ARM 架构的 Mac（M1/M2/M3/M4）。
- macOS 13 (Ventura) 或更高版本。
- `codesign` 工具（Xcode / Command Line Tools 的一部分）。

### 命令
- **构建：** `cargo build --release`
- **运行：** `cargo run -- [OPTIONS] [disk-image.raw]`
- **测试：** `cargo test`（CLI 解析由 `src/main.rs` 中的 `#[cfg(test)] mod tests` 覆盖；镜像目录的解析与查找在 `src/image_dirs.rs` 和 `src/images.rs` 中包含测试；`OutputMonitor`、输入门控和登录/操作逻辑在 `src/vm_io.rs` 和 `src/session.rs` 中包含 `#[cfg(test)] mod tests`；GPT、ext4、控制帧、缓存辅助函数和镜像分类在 `src/gpt.rs`、`src/ext4ro.rs`、`src/control.rs`、`src/cache.rs` 和 `src/image_probe.rs` 中包含 `#[cfg(test)] mod tests`；`VmSpec` 结构和无头模式各阶段超时在 `src/vm_config.rs` 和 `src/vm_lifecycle.rs` 中包含测试；Alpine 缓存、seed manifest 和维护步骤顺序在 `src/alpine.rs`、`src/seed.rs` 和 `src/maintenance.rs` 中包含测试。准备决策、machine-id 完成脚本和 provision 的代理注入在 `src/disk_prepare.rs` 和 `src/provision.rs` 中包含测试。代理解析和 `usernet_args` 在 `src/proxy.rs` 和 `src/networking.rs` 中包含测试。`--vm-user` 的校验、`GuestPath` 和客户机检查脚本（用桩命令运行）的测试在 `src/vm_user.rs`；挂载、遮蔽和阶段顺序在 `src/session_plan.rs`；run 会话在 `src/main.rs`。`--root-password` 的解析、登录分支，以及 provision 的 `chpasswd` 动作在 `src/main.rs`、`src/session.rs` 和 `src/provision.rs` 中包含测试。真实磁盘镜像测试被标记为 `#[ignore]`，需要设置 `VIBE_TEST_DEBIAN_NEW_RAW`、`VIBE_TEST_DEBIAN_OLD_RAW` 和 `VIBE_TEST_ALPINE_RAW`。虚拟机的启动测试偏重集成测试且需要 Mac 宿主机；`tests/vm_devices.rs`（`VIBE_VM_CHECK=read-only-disk|headless-timeout`）、`tests/alpine_maintenance.rs`（`VIBE_VM_CHECK=maintenance-all`）和 `tests/disk_prepare.rs`（`VIBE_VM_CHECK=prepare`）以及 `tests/vm_user.rs`（`VIBE_VM_CHECK=vm-user|root-password`，需要 `VIBE_VM_CHECK_DISK`；VM 代理放进 `VIBE_VM_CHECK_ARGS`）被标记为 `#[ignore]`，它们会启动 `vibe` 二进制，使检查在其主线程上运行。维护检查使用 `VIBE_E2FS_SBIN` 或 `PATH` 中的 e2fsprogs。ext4 与探测测试在所需的 `tests/fixtures/*.img.gz` 缺失时会运行 `tests/fixtures/make-ext4.sh`；该脚本通过 `MKFS_EXT4`、`/tmp/e2fs-install`、Homebrew `e2fsprogs` 或 `PATH` 查找 `mkfs.ext4`、`debugfs` 和 `e2fsck`）。

## 开发规范

- **极简依赖：** 坚持“小二进制文件”理念。避免引入笨重的 crate。当前依赖主要为 `objc2`、`lexopt`、`libc` 和 `regex`。
- **资深工程规范：** 代码应保持整洁、符合 Rust 惯用法，并遵循现有模式（例如精准克制地使用 `objc2`）。
- **不使用 Emoji：** CLI 和文档均避免使用 Emoji，以保持专业、极简的风格。
- **以人为本：** 文档（如 README）由人编写，供人阅读。
- **macOS 互操作性：** 注意 macOS 特有的行为，特别是权限（entitlements）和安全边界。

## 常见任务

- **添加默认挂载：** 在 `main.rs` 的 `SYS_CACHE_SHARES` 或 `AGENT_CONF_SHARES` 中添加 `CacheShare`（由 `cache_share_specs` 选取；每个用户一份的缓存用 `CacheShare::user`，所有用户共用的用 `CacheShare::global`），或修改 `run_main()` 中 `CliCommand::Run` 分支的项目目录共享。客户机路径用 `GuestPath` 表示：相对路径位于 `--vm-user` 的 HOME 下，绝对路径（如 apt 缓存）对所有用户相同。
- **更新客户机预配配置：** 编辑 `provisioning/base.sh`。注意，这仅在创建 `default.raw` 的*首次*运行时生效。
- **调整网络设置：** 查看 `src/networking.rs` 和 `NetworkMode` 枚举。代理规则在 `src/proxy.rs`（Rust 侧）以及 `helpers/gvisor-tap-vsock/pkg/services/{noproxy,netproxy,dns,forwarder}` 和 `pkg/virtualnetwork/proxy.go`（辅助程序侧）；修改后同步更新 `docs/network-proxy.zh-Hans.md`。
- **修复权限问题：** `main.rs` 包含一个 `ensure_signed()` 函数，用于处理自签名和重新执行。
- **更新变更日志：** `CHANGELOG.md` 遵循 [Keep a Changelog 1.1.0](https://keepachangelog.com/en/1.1.0/)，使用英文编写。readme 不再列出与原始仓库的差异，而是链接到 `CHANGELOG.md`。
    - 将每一项用户可感知的变更（CLI 选项、默认值、客户机行为、预配脚本、用户会设置的环境变量、支持的镜像）添加到 `## [Unreleased]` 小节中的 `Added`、`Changed`、`Deprecated`、`Removed`、`Fixed` 或 `Security` 下；缺少对应子小节时新建。
    - 破坏现有用法的变更以 `**Breaking:**` 开头，并说明替代用法。
    - 不记录用户察觉不到的内部细节：重构、测试、模块结构、编辑器或工具配置、内部或仅用于测试的环境变量。
    - 发布时，更新 `Cargo.toml` 中的 `version`，将 `[Unreleased]` 重命名为 `## [X.Y.Z] - YYYY-MM-DD`，在其上方新增一个空的 `## [Unreleased]`，并更新文件末尾的比较链接。
