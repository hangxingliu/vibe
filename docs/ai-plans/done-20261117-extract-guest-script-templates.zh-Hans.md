# 抽离 VM 内脚本模板到 `src/guest/`

本文是实现计划，不是已完成功能的说明。基于 2026-11-17 工作区（`HEAD` 为 `9c2b434`）。本文只新增计划文档，不改产品代码。

背景索引见 [`references/20261117-vm-scripts.zh-Hans.md`](references/20261117-vm-scripts.zh-Hans.md)。下文的“调研文档”都指它。

## 已定方向（Q&A）

需求点已全部回复。下列问答记录方向选择，实现时以此为准。标“假设”的条目是本文补的决定，用户可改。

### 会话目录（P0）

**Q：脚本怎样进入 guest？**
A：不再逐行键入脚本正文。宿主机把脚本写成文件，放进会话目录。会话目录经 VirtioFS 只读共享给 guest。guest 中 root 从 staging 路径读取并执行。

**Q：会话目录在哪里？**
A：`<cache>/sessions/<pid>-<nonce>/`，模式 0700。`<cache>` 是 `ctx.cache_dir`（默认 `~/.cache/vibe`）。用现有 `cache::private_dir(&cache_dir.join("sessions"), "")` 创建。

**Q：guest 中路径是什么？**
A：`/mnt/root/virtiofs-shared/<tag>/<文件>`。`<tag>` 是会话共享的 `DirectoryShare::tag()`。不做 bind mount。`/mnt/root` 为 0700，只有 root 可读。

**Q：哪些文件进会话目录？**
A：

| 文件 | 写入者 | provision | run |
| --- | --- | --- | --- |
| `install_bindfs.sh` | `build_provision_plan` | 有 | 无 |
| `provision-env.sh` | `build_provision_plan` | 有 | 无 |
| `mount_apt_cache.sh` | `run_run` | 无 | 非 root `--vm-user` 时有 |
| `install_ssh_key.sh` | `run_run` | 无 | `--ssh-key` 时有 |
| `scripts/NNN-<名字>.sh` | `SessionDir::stage_scripts` | 每个 `Script`/`UserScript` 一个 | 每个 `--script` 一个 |

built-in 脚本按 `# vibe:run-as-user` 拆成两个文件：`NNN-base.root.sh`、`NNN-base.user.sh`。`NNN` 是三位动作序号，`<名字>` 取 basename，非 `[A-Za-z0-9._-]` 字符换成 `_`。

**Q：会话目录何时删除？**
A：`run_vm` 返回且 `outcome.stopped_normally` 为真时删除。其他情况保留，并在宿主机打印目录路径。

**Q：保留下来的目录何时清理？（假设）**
A：下一次创建会话目录前调用 `cache::sweep_stale(<cache>/sessions)`，删除 pid 已死的目录。失败现场因此保留到下一次 `vibe` 启动。

**Q：根密码会进会话目录吗？**
A：不会。`RootPassword` 仍只经 `SecretSend` 键入。会话目录中不写任何密钥。SSH 公钥不是密钥，可以写。

**Q：会话共享为什么只读？**
A：guest 只读脚本，不写回。只读共享防止 guest 改动宿主机上的现场文件。

**Q：会话共享出现在 `describe_vm_files` 的列表中吗？（假设）**
A：不出现。它是内部共享。失败时单独打印路径。

**Q：维护 VM（Alpine）和 `src/vm_checks/` 怎么办？**
A：它们不经 `run_run`/`provision_image`，没有会话目录。`Script` 动作没有文件时，包装器回退到内嵌 base64（现有行为）。

### Q1. 包装器（`script_command`）的送入方式

**Q：包装器怎样送入？**
A：方案 A。Rust 渲染 `src/guest/script_wrapper.sh` 后整体 base64，只键入一行 ` eval "$(echo '<b64>' | base64 --decode)"`。

**Q：P0 之后体积还是问题吗？**
A：不是。脚本正文改从文件读，base64 中只剩包装器本身（约 2 KB）。回退到内嵌脚本时才有约 1.8 倍体积。

**Q：包装器为什么不直接执行共享中的文件？**
A：vm-user 读不到 `/mnt/root`。包装器仍把脚本复制到私有目录 `/tmp/vibe-scripts-<uid>`。root 用 `cat` 读源文件。vm-user 分支经 `runuser ... sh -c 'cat >"$1" && chmod +x "$1"'` 写入，符号链接防护不变。

**Q：控制台显示什么？**
A：只显示一行 `eval`。`docs/how-provisioning-scripts-work.md` 中 “The console shows the wrapper” 一句要改。宿主机上的脚本文件替代原来的可观测性。

### Q2. 其他三个脚本（`install_bindfs`、`mount_apt_cache`、`ssh_key`）的执行方式

**Q：怎样执行？**
A：按 P0 写成会话目录中的文件。root shell 用 `sh -c "$(cat '<guest 路径>')" <名字> <参数>` 在子进程执行。

**Q：为什么不用 `sh <文件>` 或 `sh -s`？**
A：`sh -c "$(cat ...)"` 由 root 读文件，所以 `runuser` 后的 vm-user 也能执行。stdin 不承载脚本，`apt-get` 吃不掉脚本正文。`StatusCommand` 本来就把 stdin 设为 `/dev/null`。

**Q：guest 运行时才知道的值（vm-user 的 HOME）怎样传？**
A：作为位置参数，由 root shell 展开，例如 `"$VIBE_VM_HOME"`。配置区块只放宿主机已知的字面值。

### Q3. `DEBIAN_FRONTEND`

**Q：`install_bindfs` 搬进子进程后，配置脚本怎样继承 `DEBIAN_FRONTEND`？**
A：按 P0。provision 写 `provision-env.sh`，root shell 用 ` . '<guest 路径>'` 加载。该文件含 `export DEBIAN_FRONTEND=noninteractive` 和现有的 `VIBE_PROVISION_*`、`VIBE_GIT_SHA`、`VIBE_BUILD_DATE` 导出，替换现有的多行 `export` 键入。`install_bindfs.sh` 自己也导出该变量。

**Q：用户可见行为变了吗？**
A：没有。provision 脚本仍继承 `DEBIAN_FRONTEND=noninteractive`。run 的 `--script` 原本就不继承，现在也不继承。

### Q4. root 与非 root 合并范围

**Q：合并哪些？**
A：只合并 `script_command`（`__vibe_as_vm_user=0|1`）和 `ssh_key_command`。`write_file_into_vm` 与 `write_user_file_into_vm` 不动。

### Q5. 额外抽离范围

**Q：必做四项之外做哪些？**
A：方案 A，做第 1、2 项。

- 第 1 项（T11）：`CHECK_VM_USER_HOME_SCRIPT`、`CLEAR_MACHINE_ID_SCRIPT`、`REPL_FINALIZE_SCRIPT` 移成 `src/guest/*.sh`。它们是 `Script` 动作，T10 之后自动进会话目录。
- 第 2 项（T12）：`run_guest_script` 改用 `sh_command`。

**Q：第 2 项为什么不用会话目录？**
A：`check_vm_user_actions` 和 `ensure_vm_user_actions` 在 root 准备阶段运行，早于 VirtioFS 挂载。挪动挂载顺序不在本轮范围。所以改用 `sh_command`（`sh -c "$(echo b64 | base64 --decode)"`），去掉 `sh -s` 的 stdin 隐患。

### Q6. region 标记

**Q：标记怎样写，缺失怎么办？**
A：模板写 `# region NAME` / `# endregion NAME`。匹配允许 `#` 后零或多个空格和行首缩进。内部模板缺区块或区块重复时返回错误，由单元测试兜底。`base.sh` 的 `INJECT_PROXY_CODE` 保持“找不到就不替换”。

## 目标与范围

目标：

- Rust 中拼接的 guest 脚本抽成 `src/guest/*.sh` 模板。模板开头用 `# region INJECT_..._CONFIG` 区块声明配置。Rust 只替换这个区块。
- 区块替换从 `build_provision_script` 抽成通用函数。
- 新增会话目录（P0）。provision 与 run 的脚本经它进入 guest，不再整段键入。

收益：

- 脚本可读，可用 `shellcheck`、`shfmt` 检查。
- 脚本可在宿主机上用 stub 命令单独测试（参照 `src/vm_user.rs` 的 `check_with_stubs`）。
- root 与 vm-user 两套脚本合成一套。
- 键入量大减。失败后宿主机保留实际执行的脚本。

不在本轮范围：

- 改变挂载、遮蔽、登录、切换用户的顺序。
- 改变标记协议（`VIBE_SCRIPT_STATUS_*`、`VIBE_STEP_STATUS_*`）。
- 改变 `provisioning/*.sh` 的内容。
- 合并 `write_file_into_vm` 与 `write_user_file_into_vm`。

## 当前代码事实与修改入口

行号会变，以符号名为准。

| 入口 | 当前行为 | 计划中的变化 |
| --- | --- | --- |
| `build_provision_script`（`src/provision.rs`） | 内联正则替换 `INJECT_PROXY_CODE` | 改用 `replace_region_if_present`，替换串用 `regex::NoExpand` |
| `build_provision_plan`（同上） | 多行 `export VIBE_PROVISION_*` 键入 | 改为写 `provision-env.sh` 并 ` . ` 加载；写 `install_bindfs.sh` |
| `provision_image`（同上） | 无 cache 目录参数 | 新增 `cache_dir` 参数，创建并收尾会话目录 |
| `run_run`（`src/run_command.rs`） | 无会话目录 | 创建会话目录；写 `mount_apt_cache.sh`、`install_ssh_key.sh`；暂存 `--script` |
| `DirectoryShare`（`src/shares.rs`） | 每个共享都 bind mount | 新增 `staging_only` 字段；`mount_actions` 与 `describe_vm_files` 跳过它 |
| `SessionPlan.restrict_apt_cache`（`src/session_plan.rs`） | `bool` | 改为 `apt_cache_script: Option<GuestFile>`；原读取处用 `.is_some()` |
| `script_command`（`src/session/shell.rs`） | `format!` 拼两套分支，逐行键入 | 渲染 `script_wrapper.sh`，一行 `eval`；有文件时 `cat` 文件，无文件时内嵌 base64 |
| `ROOT_MISE_VARIABLES`（同上） | Rust 常量 | 移入模板，删除常量 |
| `LoginAction::Script`/`UserScript`（`src/session.rs`） | `{ name, content }` | 新增 `file: Option<GuestFile>`；`content` 保留给标签与测试 |
| `mount_apt_cache_action`（`src/session_plan.rs`） | 当前 shell 中 `bindfs` 或 `mount --bind` | `__vibe_notice=$(<GuestFile 的 sh 命令>)` |
| `install_bindfs_action`（同上） | 当前 shell 中 `export DEBIAN_FRONTEND` 后 `apt-get` | 子进程执行 `install_bindfs.sh` |
| `ssh_key_command`（`src/run_command.rs`） | root 与非 root 两套 `format!` | 一个 `install_ssh_key.sh`；非 root 前加 `runuser -u "$VIBE_VM_USER" --` |
| `run_guest_script`（`src/vm_user.rs`） | `echo b64 \| base64 --decode \| sh -s -- <name>` | `sh_command(script, name, &[user])` |
| `actions_thread_reports_the_index_of_a_failed_script`（`src/session/actions.rs`） | 循环读键入行直到含 `VIBE_SCRIPT_STATUS_` | 该行不再出现，改为读一行 |
| `src/session/shell.rs`、`src/session_plan.rs`、`src/run_command.rs` 现有测试 | 断言拼出的命令字符串 | 改为断言渲染结果，或改成宿主机行为测试 |

## 设计概要

### 新模块 `src/session_dir.rs`

```rust
/// Absolute guest path of a file in the session share. Shell-safe.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct GuestFile(String);

impl GuestFile {
    /// `sh -c "$(cat '<path>')" <name> <args>`: root reads the file, so the
    /// command also works after `runuser`. stdin stays free.
    pub(crate) fn sh_command(&self, name: &str, args: &[String]) -> String;
    pub(crate) fn as_shell(&self) -> String;
}

/// `<cache>/sessions/<pid>-<nonce>/`, shared read-only and never bind-mounted.
pub(crate) struct SessionDir { host: PathBuf, share: DirectoryShare, next: usize }

impl SessionDir {
    /// Sweeps dead-pid directories, then makes a new private one.
    pub(crate) fn create(cache_dir: &Path) -> io::Result<Self>;
    pub(crate) fn share(&self) -> &DirectoryShare;
    /// Writes `rel` (mode 0600) and returns its guest path.
    pub(crate) fn write(&mut self, rel: &str, content: &str) -> io::Result<GuestFile>;
    /// Writes every `Script`/`UserScript` of `plan` to `scripts/NNN-<name>.sh`
    /// and sets its `file`.
    pub(crate) fn stage_scripts(&mut self, plan: &mut SessionPlan) -> io::Result<()>;
    /// Removes the directory when the VM stopped normally; otherwise keeps it
    /// and returns its path for the error message. `Err` when the remove fails.
    pub(crate) fn finish(self, stopped_normally: bool) -> io::Result<Option<PathBuf>>;
}
```

规则：

- 所有文件在 `run_vm` 之前写完。不依赖 VirtioFS 对运行中写入的可见性。
- 会话共享放在共享列表末尾，不影响现有挂载顺序。
- `stage_scripts` 只处理 `SessionPlan` 的 `root_actions` 和 `user_actions`。`expand_session` 不新增 `Script`。

### 新模块 `src/guest_template.rs`

```rust
/// Replaces the lines between `# region NAME` and `# endregion NAME`
/// with `body`. The marker lines stay. Errs when the region is missing
/// or appears more than once.
pub(crate) fn replace_region(template: &str, name: &str, body: &str) -> Result<String, RegionError>;

/// Like `replace_region`, but returns `template` unchanged when the region
/// is missing. Only for user-editable scripts such as `provisioning/base.sh`.
pub(crate) fn replace_region_if_present(template: &str, name: &str, body: &str) -> String;

/// `NAME='value'` with the value single-quoted for sh.
pub(crate) fn shell_assign(name: &str, value: &str) -> String;

/// Root shell text that runs `script` in a child `sh` with `args`:
/// `sh -c "$(echo '<b64>' | base64 --decode)" <name> <args>`.
/// Only for scripts that run before the VirtioFS mount.
pub(crate) fn sh_command(script: &str, name: &str, args: &[String]) -> String;
```

规则：

- 正则用 `regex::escape(name)` 拼接，`(?ms)` 模式，非贪婪匹配区块内容。
- 替换用 `regex::NoExpand`。注入内容常含 `$VIBE_VM_HOME` 等文本。
- 内部模板缺区块属于编程错误。调用点用 `.expect("<模板名> has region <NAME>")`，单元测试覆盖每个模板。
- `shell_single_quote` 留在 `src/session/shell.rs`，`shell_assign` 调用它。
- 模板常量放在调用方旁边，例如 `const MOUNT_APT_CACHE_SCRIPT: &str = include_str!("guest/mount_apt_cache.sh");`。

### 配置区块的约定

- 区块名统一为 `INJECT_<脚本名>_CONFIG`。
- 区块默认值让脚本可以单独运行，便于宿主机测试。必须由宿主机给出的值默认为空，脚本开头用 `: "${NAME:?NAME is empty}"` 检查。
- 宿主机已知的字面值（staging 路径、代理 URL、SSH 公钥、源文件路径）用 `shell_assign` 注入。
- guest 运行时才知道的值（vm-user 的 HOME）作为位置参数传入，由 root shell 展开。
- 在当前 shell 中 `eval` 的模板（包装器）变量和函数都用 `__vibe_` 前缀，结束时 `unset`/`unset -f`。不能写 `exit` 和 `set -e`，否则会关掉登录 shell。

### 方言

- `script_wrapper.sh`：bash（在交互 bash 中 `eval`）。首行 `#!/usr/bin/env bash`，加 `# shellcheck shell=bash`。
- 子进程模板：`#!/bin/sh`，POSIX sh，与 `check_vm_user.sh` 一致。guest 中由 dash 执行。

### 已知陷阱

- `bindfs` 留在后台。它若继承 `$(...)` 的 stdout，命令替换会一直等它退出。模板中 `bindfs` 的 stdout 必须重定向（`>&2`）。
- `regex::Regex::replace` 默认展开 `$name`。必须用 `NoExpand`。
- `"$(cat file)"` 会去掉末尾换行，对脚本无影响。脚本大小远低于 `ARG_MAX`。
- VirtioFS 上的文件属主是宿主机 uid。guest root 读取不受影响，vm-user 读不到 `/mnt/root`。所有读源文件的动作都由 root 执行。
- `install_bindfs` 搬进子进程后，root shell 的 `DEBIAN_FRONTEND` 来自 `provision-env.sh`（Q3）。
- 会话目录写入失败时，带 `[prepare]` 前缀报错。

## 渐进式任务拆分

规则：

- 每个任务只改一小块。完成后 `cargo build --locked`、`cargo test`、`cargo clippy`、`cargo fmt --check` 都要通过。
- 新增或修改的 `.sh` 要过 `shellcheck` 和 `shfmt -d`。宿主机 PATH 中没有这两个工具，用 `mise exec -- shellcheck ...`、`mise exec -- shfmt -d ...`。
- 每个任务开头列出“需要读的上下文”。只读这些即可完成该任务。
- T01–T04 不改变 guest 行为。T05 起每个任务只接入一个调用点。

### T01. 新增 `src/guest_template.rs`：区块替换 ✅ 已完成 (Completed)

- 需要读的上下文：`build_provision_script`（`src/provision.rs`）；本文“新模块 `src/guest_template.rs`”；Q6。
- 改动：
    - 新建 `src/guest_template.rs`，实现 `replace_region`、`replace_region_if_present`、`RegionError`。
    - `src/main.rs` 加 `mod guest_template;`。
- 测试（同文件 `#[cfg(test)] mod tests`）：
    - 替换区块内容，标记行保留，区块外文本不变。
    - 空区块（两行标记相邻）可替换。
    - 注入内容含 `$HOME`、`${VIBE_VM_HOME}`、`$1` 时原样保留。
    - 区块缺失、只有开始标记、出现两次：`replace_region` 返回错误，`replace_region_if_present` 原样返回。
    - 名字是另一名字的前缀（`CONFIG` 与 `CONFIG_2`）时不误匹配。
- 依赖：无。
- 完成说明：`replace_region` 用字符串拼接写回，不经过 `Regex::replace`，所以 `$` 原样保留。标记行保留，允许行首缩进和 `#` 后零个或多个空白。缺区块与重复区块都是 `RegionError`。这些测试守卫替换行为，验收后保留。

### T02. `build_provision_script` 改用 `replace_region_if_present` ✅ 已完成 (Completed)

- 需要读的上下文：`build_provision_script`；测试 `guest_proxy_is_saved_to_apt_and_exported`、`no_guest_proxy_or_other_scripts_write_nothing`。
- 改动：删除内联正则，调用 `replace_region_if_present(&content, "INJECT_PROXY_CODE", &proxy_code)`。
- 行为：不变。代理 URL 含 `$` 时不再被破坏。
- 测试：现有两个测试保持通过，不新增。
- 依赖：T01。
- 完成说明：`build_provision_script` 调用 `replace_region_if_present(&content, "INJECT_PROXY_CODE", &proxy_code)`。注入内容已自带末尾换行，标记行之间不再多一个空行。代理命令不变。`guest_proxy_is_saved_to_apt_and_exported` 与 `no_guest_proxy_or_other_scripts_write_nothing` 通过。

### T03. 新增 `shell_assign` 与 `sh_command` ✅ 已完成 (Completed)

- 需要读的上下文：`run_guest_script`（`src/vm_user.rs`）；`shell_single_quote`、`shell_word`（`src/session/shell.rs`）。
- 改动：在 `src/guest_template.rs` 中实现两个函数。参数用 `shell_word` 转义。
- 测试：用宿主机 `bash -c` 执行输出：
    - 脚本拿到 `$0` 与参数，参数含空格和单引号时不变形。
    - 脚本中的 `cat` 读到测试给的 stdin，读不到脚本自身。
    - `shell_assign` 的值含单引号、换行、`$` 时，`eval` 后变量与原值相同。
- 依赖：T01。
- 完成说明：两个函数在 `src/guest_template.rs`。参数经 `shell_word`。宿主机 `bash -c` 测试通过。运行路径尚未调用，函数带 `#[cfg_attr(not(test), allow(dead_code))]`。接入调用点后删掉这个 allow。测试验收后保留。

### T04. 新增 `src/session_dir.rs` 并接入 run 与 provision（不写脚本） ✅ 已完成 (Completed)

- 需要读的上下文：`cache::private_dir`、`cache::sweep_stale`（`src/cache.rs`）；`DirectoryShare`、`describe_vm_files`（`src/shares.rs`）；`mount_actions`（`src/session_plan.rs`）；`run_run`（`src/run_command.rs`）；`provision_image`、`ensure_default_image`（`src/provision.rs`）；`provision_command.rs` 中调用 `provision_image` 处；本文“新模块 `src/session_dir.rs`”。
- 改动：
    - 新建 `src/session_dir.rs`：`GuestFile`、`SessionDir`（`create`、`share`、`write`、`finish`）。`stage_scripts` 留到 T10。
    - `DirectoryShare` 新增 `staging_only: bool`，现有构造器设 `false`。新增构造器 `DirectoryShare::staging_only(host)`，只读。
    - `mount_actions` 跳过 `staging_only` 共享，但它仍触发 staging 挂载。`describe_vm_files` 跳过它。
    - `run_run` 与 `provision_image` 创建会话目录，把共享追加到末尾。`run_vm` 返回后调用 `finish(outcome.stopped_normally)`。保留时在错误信息或 stderr 中打印 `Vibe kept the session scripts at <path>`。
    - `provision_image` 新增 `cache_dir` 参数。
- 测试：
    - `SessionDir::create` 清理死 pid 目录，保留活 pid 目录（复用 `sweep_stale` 的测试思路，只测接线，不重测 `sweep_stale`）。
    - `write` 返回 `/mnt/root/virtiofs-shared/<tag>/<rel>`，宿主机文件内容一致。
    - `finish(true)` 删除目录，`finish(false)` 保留并返回路径。
    - `mount_actions` 对 `staging_only` 共享不生成 bind mount，但生成 staging 挂载。
- 依赖：无。
- 完成说明：
    - `SessionDir::finish` 返回 `io::Result<Option<PathBuf>>`。删除失败时错误带 `[prepare]` 和路径。
    - `close_session` 在保留目录时向 stderr 打印 `Vibe kept the session scripts at <path>`。`run_run` 与 `provision_image` 在 `run_vm` 返回后调用它。`run_vm` 返回错误时也保留并打印。
    - 创建失败的错误前缀是 `[prepare]`。
    - `motd_login_action` 同样跳过 `staging_only`。内部共享不进客户机共享表。
    - 结构体还没有 `next`。`stage_scripts` 实现时再加，避免未使用字段。
    - `write`、`GuestFile::sh_command`、`as_str`、`as_shell` 运行路径尚未调用，带 `#[cfg_attr(not(test), allow(dead_code))]`。接入后删掉。
    - 已有镜像的 `provision --repl` 不经 `provision_image`，不创建会话目录。
    - 会话目录测试、挂载测试、`describe_vm_files` 测试、MOTD 测试守卫接线行为，验收后保留。没有只为阶段性验证而写的用例，不需要清理。

### T05. `install_bindfs_action` 改用 `src/guest/install_bindfs.sh` ✅ 已完成 (Completed)

- 需要读的上下文：`install_bindfs_action`、测试 `bindfs_install_uses_the_guest_proxy_only_in_vznat_mode`（`src/session_plan.rs`）；`build_provision_plan`；T04 的 `SessionDir`；Q2、Q3。
- 模板草稿：

```sh
#!/bin/sh
# Installs bindfs during provision when the image does not have it.
# Runs as root inside the guest.

# region INJECT_INSTALL_BINDFS_CONFIG
APT_PROXY_URL=""
# endregion INJECT_INSTALL_BINDFS_CONFIG

set -eu
command -v bindfs >/dev/null && exit 0
export DEBIAN_FRONTEND=noninteractive
if [ -n "$APT_PROXY_URL" ]; then
	set -- -o "Acquire::http::Proxy=$APT_PROXY_URL" -o "Acquire::https::Proxy=$APT_PROXY_URL"
else
	set --
fi
apt-get "$@" install --yes bindfs </dev/null && exit 0
apt-get "$@" update </dev/null
apt-get "$@" install --yes bindfs </dev/null
```

- Rust：
    - `build_provision_plan` 接收 `&mut SessionDir`，渲染模板（`shell_assign("APT_PROXY_URL", &guest.apt_url)`），写入 `install_bindfs.sh`。
    - `install_bindfs_action(file: &GuestFile)` 的命令为 `file.sh_command("install_bindfs.sh", &[])`。
    - root shell 暂时仍需 `DEBIAN_FRONTEND`：本任务在 `install_bindfs_action` 后键入 ` export DEBIAN_FRONTEND=noninteractive`。T10 用 `provision-env.sh` 取代它。
- 测试：
    - 渲染结果含 `APT_PROXY_URL='socks5h://...'`；无代理时为 `APT_PROXY_URL=''`。
    - 宿主机行为测试：PATH 中放 stub `apt-get`（记录参数，第一次 `install` 失败），不放 `bindfs`。断言顺序为 install、update、install，三次都带代理参数。放 stub `bindfs` 时 `apt-get` 未被调用。
    - 更新 `provision.rs` 中依赖动作下标的测试（`scripted_provision_*`、`non_root_provision_*`）。
- 依赖：T03、T04。
- 完成说明：
    - `build_provision_plan` 增加 `session: &mut SessionDir`。`provision_image` 在调用它之前创建会话目录。计划构建失败时 `close_session(session, false)`，再返回该错误。写文件失败的前缀是 `[prepare]`。
    - 渲染用 `shell_assign("APT_PROXY_URL", ...)`。`vznat` 时值为 `guest.apt_url`，否则 `APT_PROXY_URL=''`。命令是 `file.sh_command("install_bindfs.sh", &[])`。
    - 脚本在子进程里自己 `export DEBIAN_FRONTEND=noninteractive`。root shell 仍在该动作之后键入同一导出。T10 的 `provision-env.sh` 再删掉这次键入。
    - 渲染结果始终含模板里的 `Acquire::` 选项字符串。代理是否生效由 `APT_PROXY_URL` 是否为空决定，由宿主机 stub `apt-get` 断言参数，不靠“文本里没有 Acquire”来判断。
    - `shell_assign` 与 `GuestFile::sh_command`、`as_shell`、`write` 已在运行路径上，去掉了 `dead_code` allow。`guest_template::sh_command` 仍给参数加引号，运行路径不调用，allow 还在。`GuestFile::as_str` 仍只给测试用。
    - 宿主机行为测试与 provision 动作下标测试验收后保留。没有只为阶段性验证而写的用例。

### T06. `mount_apt_cache_action` 改用 `src/guest/mount_apt_cache.sh` ✅ 已完成 (Completed)

- 需要读的上下文：`mount_apt_cache_action`、`notice_message`、`SessionPlan.restrict_apt_cache` 的所有读取处（`src/session_plan.rs`、`src/run_command.rs`、`src/shares.rs`、`src/provision.rs` 测试）；测试 `other_users_get_owned_home_dirs_and_a_bindfs_apt_cache`；`check_vm_user_actions`（`__vibe_notice=$(...)` 写法）；`docs/file-shares-between-vm.md` 的 bindfs 小节。
- 模板草稿：

```sh
#!/bin/sh
# Mounts the shared apt cache so that only _apt (root's apt) can change it.
# Runs as root inside the guest. Prints the notice word on stdout when it
# falls back to a writable bind mount.

# region INJECT_MOUNT_APT_CACHE_CONFIG
APT_CACHE_DIR="/var/cache/apt"
STAGING_DIR=""
NO_BINDFS_NOTICE="apt-no-bindfs"
# endregion INJECT_MOUNT_APT_CACHE_CONFIG

set -eu
: "${STAGING_DIR:?STAGING_DIR is empty}"
mkdir -p "$APT_CACHE_DIR"
if command -v bindfs >/dev/null && apt_uid=$(id -u _apt 2>/dev/null); then
	# bindfs stays in the background; keep it off the stdout that the host reads.
	bindfs -o nonempty -u "$apt_uid" -g 0 -p 0755,af-x "$STAGING_DIR" "$APT_CACHE_DIR/" >&2
	exit 0
fi
echo 'Warning: bindfs is not available; the shared apt cache is writable by every user in this VM.' >&2
mount --bind "$STAGING_DIR" "$APT_CACHE_DIR"
echo "$NO_BINDFS_NOTICE"
```

- Rust：
    - `SessionPlan.restrict_apt_cache: bool` 改为 `apt_cache_script: Option<GuestFile>`。原读取处用 `.is_some()`。
    - `run_run` 在非 root 时渲染并写入 `mount_apt_cache.sh`。`STAGING_DIR`、`APT_CACHE_DIR`、`NO_BINDFS_NOTICE` 分别来自 apt 共享的 staging 路径、`APT_CACHE_GUEST`、`APT_NO_BINDFS_NOTICE`。
    - apt 共享的 staging 路径在写文件时就已知（`STAGING_DIR` 加 `share.tag()`），无需运行时参数。
    - 命令为 `format!("__vibe_notice=$({})", file.sh_command("mount_apt_cache.sh", &[]))`。
- 行为差异：回退时 notice 在 `mount --bind` 成功后才输出。`mount` 失败时会话本来就停止，结果相同。
- 测试：
    - 命令以 `__vibe_notice=$(sh -c "$(cat ` 开头；渲染结果区块含 staging 路径。
    - 宿主机行为测试（临时目录作 `APT_CACHE_DIR`，stub `bindfs`、`id`、`mount`）：
        - 有 `bindfs` 和 `_apt`：调用 `bindfs`，参数正确，stdout 为空。
        - stub `bindfs` 后台 `sleep` 并继承 stdout 时，`$(...)` 仍立即返回。
        - 无 `bindfs` 或无 `_apt`：调用 `mount --bind`，stdout 为 `apt-no-bindfs`。
- 依赖：T03、T04。
- 完成说明：
    - `SessionPlan.restrict_apt_cache: bool` 已改为 `apt_cache_script: Option<GuestFile>`。读取处用 `.is_some()`。`describe_vm_files` 的参数仍是 `bool`，调用处传入 `apt_cache_script.is_some()`。
    - 只在非 root 且共享列表里有 apt 共享时写入 `mount_apt_cache.sh`。没有 apt 共享时为 `None`。以前每个非 root 用户都设 `restrict_apt_cache`；没有 apt 共享时本来也没有可限制的挂载。
    - `STAGING_DIR` 在写文件时拼好：`STAGING_DIR` + `share.tag()`。`APT_CACHE_DIR` 来自 `APT_CACHE_GUEST`，`NO_BINDFS_NOTICE` 来自 `APT_NO_BINDFS_NOTICE`。
    - 命令是 `__vibe_notice=$(sh -c "$(cat ...)")`。`$(...)` 去掉末尾换行，宿主机读到的词是 `apt-no-bindfs`。回退提示只在 `mount --bind` 成功之后打印。`bindfs` 的标准输出转到标准错误，避免 `$(...)` 等待后台进程。
    - 宿主机行为测试验收后保留。没有只为阶段性验证而写的用例。

### T07. `ssh_key_command` 合并为 `src/guest/install_ssh_key.sh` ✅ 已完成 (Completed)

- 需要读的上下文：`ssh_key_command`、测试 `run_session_writes_tmux_profile_and_ssh_key_for_the_target_user`（`src/run_command.rs`）；`GuestPath::to_shell`（`src/vm_user.rs`）；Q2、Q4。
- 模板草稿（以目标用户身份运行，`$1` 是目标 HOME）：

```sh
#!/bin/sh
# Installs the --ssh-key public key as the only authorized key of the user
# that runs this script. $1 is that user's HOME.

# region INJECT_INSTALL_SSH_KEY_CONFIG
SSH_PUBLIC_KEY=""
# endregion INJECT_INSTALL_SSH_KEY_CONFIG

set -eu
target_home=${1:?target HOME is missing}
: "${SSH_PUBLIC_KEY:?SSH_PUBLIC_KEY is empty}"
umask 077
mkdir -p "$target_home/.ssh"
chmod 700 "$target_home/.ssh"
printf '%s\n' "$SSH_PUBLIC_KEY" >"$target_home/.ssh/authorized_keys"
chmod 600 "$target_home/.ssh/authorized_keys"
```

- Rust：
    - `run_run` 渲染模板（`shell_assign("SSH_PUBLIC_KEY", key.trim())`），写入 `install_ssh_key.sh`。
    - 参数为 `GuestPath::new("").to_shell(vm_user)`：root 为 `/root`，其他用户为 `"${VIBE_VM_HOME}"`。参数由 root shell 展开，不再用 `shell_word` 转义。`sh_command` 需要支持“已是 shell 表达式”的参数，或在调用处拼接。实现时二选一，保持单一写法。
    - root：` ` + `file.sh_command(...)`。非 root：` runuser -u "$VIBE_VM_USER" -- ` + `file.sh_command(...)`。`$(cat ...)` 由 root shell 展开，vm-user 不需要读 `/mnt/root`。
- 行为差异：root 分支多 `umask 077`；文件末尾多一个换行。对 sshd 等价。
- 测试：
    - root 命令不含 `runuser`；非 root 命令以 ` runuser -u "$VIBE_VM_USER" -- sh -c ` 开头；非 root 动作中不出现 `/root/`。
    - 宿主机行为测试：`$1` 指向临时目录，执行渲染后的脚本。`.ssh` 为 700，`authorized_keys` 为 600，内容为公钥加换行。公钥含单引号时内容不变。
- 依赖：T03、T04。
- 完成说明：
    - `run_run` 用 `shell_assign("SSH_PUBLIC_KEY", key.trim())` 渲染并写入 `install_ssh_key.sh`。root 命令是 ` ` + `file.sh_command`。非 root 是 ` runuser -u "$VIBE_VM_USER" -- ` + `file.sh_command`。
    - `GuestFile::sh_command` 的参数按调用方已经写好的 shell 词原样拼接，不再套 `shell_word`。HOME 参数是 `GuestPath::new("").to_shell(vm_user)`。root 得到 `/root/`：`Path::new("/root").join("")` 保留末尾斜杠。脚本里 `/root/` 与 `/root` 是同一目录（`/root//.ssh`）。非 root 得到 `"${VIBE_VM_HOME}"`，由 root shell 展开，不再用 `shell_word` 包一层。
    - 非 root 动作字符串含 `/mnt/root/virtiofs-shared`（`STAGING_DIR`）。测试不能断言“不含 `/root/`”。断言不含 `/root/.`，并且 HOME 参数是 `"${VIBE_VM_HOME}"`。
    - root 也使用同一脚本，因此多了 `umask 077`，`authorized_keys` 以换行结尾。
    - `$(cat ...)` 在 root shell 展开，vm-user 不读 `/mnt/root`。
    - 宿主机行为测试与命令形状测试验收后保留。没有只为阶段性验证而写的用例。

### T08. 新建 `src/guest/script_wrapper.sh`（暂不接入） ✅ 已完成 (Completed)

- 需要读的上下文：`script_command` 及其文档注释（`src/session/shell.rs`）；`docs/how-vm-boot.md` 的 `Script` 与 `UserScript` 两段；Q1。
- 模板草稿（在当前 shell 中执行，不能 `exit`，不能 `set -e`）：

```bash
#!/usr/bin/env bash
# shellcheck shell=bash
# Runs one Script or UserScript action. Evaluated in the current shell, so
# it reactivates mise there and prints the status line on its terminal.

# region INJECT_SCRIPT_WRAPPER_CONFIG
__vibe_script_id=""
__vibe_script_src=""
__vibe_script_b64=""
__vibe_as_vm_user=0
__vibe_script_dir_base="/tmp/vibe-scripts"
# endregion INJECT_SCRIPT_WRAPPER_CONFIG

# Prints the script: from the session share, or from the inline base64.
__vibe_script_text() {
	if [ -n "$__vibe_script_src" ]; then
		cat -- "$__vibe_script_src"
	else
		printf '%s' "$__vibe_script_b64" | base64 --decode
	fi
}

if [ "$(id -u)" = 0 ]; then
	free -h
	echo 3 >/proc/sys/vm/drop_caches
	echo 1 >/proc/sys/vm/compact_memory
fi
if [ "$__vibe_as_vm_user" = 1 ]; then
	__vibe_script_dir="$__vibe_script_dir_base-$VIBE_VM_UID"
	# root makes the directory; the user writes the script into it.
	...
else
	__vibe_script_dir="$__vibe_script_dir_base-$(id -u)"
	...
fi
...
printf '\n%s%s\n%s\n' 'VIBE_SCRIPT_STATUS_' "$__vibe_script_id" "$__vibe_status"
unset -f __vibe_script_text
unset __vibe_script_id __vibe_script_src __vibe_script_b64 __vibe_as_vm_user __vibe_script_dir_base __vibe_script_dir __vibe_script __vibe_status
```

- 省略号处照搬现有两个分支：目录检查（`mkdir -m 700`、`-L`、`-O` 或 `stat -c %u`）、写入（root 为 `__vibe_script_text >"$__vibe_script"`；vm-user 为 `__vibe_script_text | runuser ... sh -c 'cat >"$1" && chmod +x "$1"'`）、运行（vm-user 的 `cd "$VIBE_VM_HOME" && runuser ... env -u MISE_SHELL -u __MISE_DIFF -u __MISE_ORIG_PATH -u __MISE_SESSION ... bash --login -c 'exec "$0"'`）、写失败时 status 为 1、成功后重新 `mise activate bash`。
- `__vibe_script_dir_base` 只为宿主机测试而可配。Rust 不注入它。
- 本任务只加文件和宿主机测试，不改 `script_command`。
- 测试（`src/session/shell.rs` 测试模块，用宿主机 `bash`）：
    - root 分支，源为文件：脚本 `exit 3`，输出含 `VIBE_SCRIPT_STATUS_<id>` 与下一行 `3`。
    - root 分支，源为 base64：同上。
    - 源文件不存在：status 为 1，并打印 “cannot write the script”。
    - 私有目录已存在且是符号链接：status 为 1。
    - `eval` 后不残留 `__vibe_*` 变量和函数，shell 未退出（之后 `echo alive` 可执行）。
    - vm-user 分支依赖 `runuser` 和 GNU `stat`，宿主机不测，由 T15 集成测试覆盖。
- 依赖：T01、T03。
- 完成说明：
    - 文件在 `src/guest/script_wrapper.sh`。两个分支都从现有 `script_command` 搬过来。不 `exit`，不 `set -e`。变量前缀 `__vibe_`，结束时 `unset`。状态变量是 `__vibe_status`。
    - `__vibe_script_dir_base` 只给宿主机测试改。Rust 不注入它。
    - `script_command` 没有改，仍自己拼包装器字符串。文件级 `# shellcheck disable=SC2016`：内层 `sh -c` / `bash -c` 的单引号字符串要在内层展开 `$0` 和 `$1`。
    - 宿主机测试只覆盖 root 分支（`__vibe_as_vm_user=0`）和两条失败路径（源文件不存在、脚本目录是符号链接）。断言 `eval` 之后 shell 还在（`echo alive`），并且 `__vibe_` 变量和函数已消失。vm-user 分支未在宿主机测。
    - 这些测试守卫包装器行为，验收后保留。T09 再删掉只断言旧 `script_command` 字符串的两个测试。本任务不删。
    - 该文件尚未接入，changelog 与“控制台不再逐行键入脚本”仍留给 T14。

### T09. `script_command` 改用模板；`Script` 支持文件 ✅ 已完成 (Completed)

- 需要读的上下文：`script_command`、`ROOT_MISE_VARIABLES`、两个包装器测试（`src/session/shell.rs`）；`LoginAction`（`src/session.rs`）；`ActionRunner::script`、测试 `actions_thread_reports_the_index_of_a_failed_script`（`src/session/actions.rs`）；所有构造 `Script {`/`UserScript {` 的位置（`src/provision.rs`、`src/cli.rs`、`src/session.rs` 测试、`src/vm_checks/maintenance/checks.rs`、`src/session/actions.rs` 测试）；T08 的模板。
- 改动：
    - `LoginAction::Script`/`UserScript` 新增 `file: Option<GuestFile>`。现有构造处设 `None`。
    - `script_command(id, content, file, as_vm_user)`：有 `file` 时注入 `__vibe_script_src`，否则注入 `__vibe_script_b64`。返回 ` eval "$(echo '<b64>' | base64 --decode)"`。
    - 删除 `ROOT_MISE_VARIABLES` 及其他不再使用的代码。
- 测试：
    - 删除只断言旧字符串的 `script_wrapper_uses_a_private_directory_per_uid`、`user_script_wrapper_runs_the_script_as_the_vm_user`。行为由 T08 覆盖。
    - 新增一个断言：命令只有一行、以空格开头、不含 `VIBE_SCRIPT_STATUS_`。
    - 修改 `actions_thread_reports_the_index_of_a_failed_script`：读一行键入内容，不再循环查找状态标记。
- 依赖：T08。
- 完成说明：
    - `Script` 与 `UserScript` 增加 `file: Option<GuestFile>`。已有构造处为 `None`。
    - `script_command` 渲染 `script_wrapper.sh`。有 `file` 时注入 `__vibe_script_src`，否则注入 `__vibe_script_b64`。键入一行 ` eval "$(echo '<b64>' | base64 --decode)"`。状态标记不在这一行里。
    - 删除 `ROOT_MISE_VARIABLES`。删除两个只断言旧字符串的测试。失败脚本测试改为读一行键入内容。
    - 无 `file` 时仍内联 base64。Alpine 维护与 `vm_checks` 没有会话目录，走这条路径。
    - 宿主机 bash 对内联 `exit 4` 得到状态 4。一行 eval 与两条注入的测试验收后保留。

### T10. 暂存脚本到会话目录；新增 `provision-env.sh` ✅ 已完成 (Completed)

- 需要读的上下文：`build_provision_plan`、`build_provision_script`（`src/provision.rs`）；`run_run`（`src/run_command.rs`）；T04 的 `SessionDir`；T05 中临时键入的 `DEBIAN_FRONTEND`；Q3。
- 改动：
    - 实现 `SessionDir::stage_scripts`：遍历 `root_actions` 与 `user_actions`，把 `Script`/`UserScript` 的 `content` 写入 `scripts/NNN-<名字>.sh`，并设 `file`。拆分后的 built-in 脚本用 `.root.sh`、`.user.sh` 后缀。
    - `run_run` 与 `provision_image` 在 `run_vm` 前调用它。
    - `build_provision_plan` 写 `provision-env.sh`：现有 `VIBE_PROVISION_*`、`VIBE_GIT_SHA`、`VIBE_BUILD_DATE` 导出，加 `export DEBIAN_FRONTEND=noninteractive`。原多行 `Send` 改为 ` . '<guest 路径>'`。删除 T05 的临时 `export`。
- 测试：
    - `stage_scripts` 后每个 `Script`/`UserScript` 都有 `file`，宿主机文件内容等于 `content`，文件名按序号排列。`--script` 路径含 `/` 和空格时文件名合法。
    - `provision-env.sh` 加载后变量值与旧 `export` 相同（宿主机 `bash` 执行，值含单引号）。
    - 更新依赖动作下标的 provision 测试。
- 依赖：T04、T05、T09。
- 完成说明：
    - `SessionDir` 增加 `next`。`stage_scripts` 按 root 动作再 user 动作写入 `scripts/NNN-<名字>.sh`。相邻同名 `Script` 加 `UserScript` 用 `.root.sh` 与 `.user.sh`。单独 `UserScript` 用 `.user.sh`。
    - 名字取 basename。`[A-Za-z0-9._-]` 以外换成 `_`。不剥掉已有 `.sh`。`/tmp/my script.sh` 得到 `scripts/003-my_script.sh.sh`。这是故意的。
    - `run_run` 与 `provision_image` 在 `run_vm` 前调用。写入失败带 `[prepare]`。失败时关闭会话并保留目录。
    - `build_provision_plan` 写 `provision-env.sh`。`.profile` 之后、脚本之前键入 ` . <guest 路径>`。删掉多行 `export` 和 T05 的临时 ` export DEBIAN_FRONTEND=noninteractive`。
    - 宿主机 bash source 后，单引号、换行和 `$HOME` 与旧导出一致。依赖动作下标的 provision 测试已改。这些测试验收后保留。

### T11.（Q5 第 1 项）三个静态脚本移成文件 ✅ 已完成 (Completed)

- 需要读的上下文：`src/provision.rs` 中 `CHECK_VM_USER_HOME_SCRIPT`、`CLEAR_MACHINE_ID_SCRIPT`、`REPL_FINALIZE_SCRIPT` 及其使用处。
- 改动：新建 `src/guest/check_vm_user_home.sh`、`src/guest/clear_machine_id.sh`、`src/guest/repl_finalize.sh`，内容照搬。常量改为 `include_str!`。文档注释移到脚本头部，Rust 侧留一行说明。T10 之后它们自动进会话目录。
- 测试：不新增。现有 provision 测试保持通过。
- 依赖：无。可与 T05–T10 并行。
- 完成说明：
    - 三个常量改为 `include_str!`。文件是 `src/guest/check_vm_user_home.sh`、`clear_machine_id.sh`、`repl_finalize.sh`。
    - 文档注释在脚本头。Rust 侧各留一行。`repl_finalize.sh` 的 quoted heredoc 正文保持原样。行为不变。不新增测试。
    - `shellcheck` 与 `shfmt --indent 2 -d` 通过。没有对其他已有 `src/guest/*.sh` 执行 `shfmt -w`。

### T12.（Q5 第 2 项）`run_guest_script` 改用 `sh_command` ✅ 已完成 (Completed)

- 需要读的上下文：`run_guest_script`、`check_vm_user_actions`、`ensure_vm_user_actions`（`src/vm_user.rs`）；Q5。
- 改动：`run_guest_script` 调用 `sh_command(script, "<脚本名>", &[user])`，删除重复的 base64 拼接。
- 测试：`check_script_exit_codes_match_the_reported_problems` 保持通过。
- 依赖：T03。
- 完成说明：
    - `run_guest_script` 调用 `sh_command`。名字是 `check_vm_user.sh` 与 `ensure_vm_user.sh`。删掉重复的 base64 与 `sh -s` 拼接。
    - 这两段在 VirtioFS 之前运行，仍内联 base64，不进会话目录。
    - 去掉 `sh_command` 的 `dead_code`。退出码测试仍通过。新断言命令含 `sh -c`、不含 `sh -s`。该断言验收后保留。stdin 必须留空。

### T13. 清理测试与代码 ✅ 已完成 (Completed)

- 按 `AGENTS.md`，删除只为验证重构或中间步骤写的测试。保留模板与会话目录的宿主机行为测试。
- 确认 `shell.rs`、`session_plan.rs`、`run_command.rs`、`provision.rs` 中没有残留的 base64 拼接或未用 import。
- 依赖：T04–T12。
- 完成说明：
    - 两个只断言旧 `script_command` 字符串的测试已在 T09 删除。`shell.rs`、`session_plan.rs`、`run_command.rs`、`provision.rs` 没有脚本正文的 base64 拼接残留。
    - `write_file_into_vm` 与 `write_user_file_into_vm` 仍用 base64。Q4 不合并它们。
    - clippy 没有未用 import。没有只为阶段性验证、做完就无意义的用例。不单列“验收后的清理”。

### T14. 文档同步 ✅ 已完成 (Completed)

- `AGENTS.md` 与 `AGENTS.zh-Hans.md`：
    - 模块表新增 `src/guest_template.rs`（`replace_region`、`sh_command`）与 `src/session_dir.rs`（`SessionDir`、`GuestFile`）。
    - `src/session/shell.rs` 一行改为“`script_command` 渲染 `src/guest/script_wrapper.sh`”。
    - `src/session_plan.rs`、`src/run_command.rs`、`src/provision.rs` 几行列出新模板文件。
    - “Common tasks” 新增：改 guest 脚本时改 `src/guest/*.sh`，区块名见模板头部。
    - “Invariants” 新增：会话目录不写密钥。
- `docs/how-vm-boot.md` 与 `.zh-Hans.md`：
    - 会话目录的位置、只读共享、生命周期。
    - `Script`/`UserScript` 段：包装器来自 `script_wrapper.sh`，一行 `eval`，源脚本来自会话目录。
    - `install bindfs`、`mount apt cache`、`--ssh-key` 处写明模板文件名与子进程执行。
    - provision 经 `provision-env.sh` 导出变量与 `DEBIAN_FRONTEND`。
- `docs/file-shares-between-vm.md` 与 `.zh-Hans.md`：新增会话共享小节（只读、不 bind mount、只有 root 可达）；bindfs 小节指向 `mount_apt_cache.sh`。
- `docs/how-provisioning-scripts-work.md` 与 `.zh-Hans.md`：改写 “The console shows the wrapper”；说明失败后脚本保留在 `<cache>/sessions/<pid>-<nonce>/scripts/`；`INJECT_PROXY_CODE` 一段补“由 `replace_region_if_present` 填充”。
- 调研文档：更新“送入方式”“包装器”“独立脚本文件”三节，新增模板文件行与会话目录；`Rust 拼接片段明细` 对应行指向模板。
- `CHANGELOG.md` 与 `.zh-Hans.md`，`## [Unreleased]` 的 `Changed`：
    - provision 与 `--script` 的脚本经只读共享 `<cache>/sessions/<pid>-<nonce>/` 进入 VM，不再在控制台键入；VM 未正常退出时保留该目录。
    - 控制台不再显示脚本包装器文本，只显示一行 `eval`。
- `USAGE.md`：`--help` 不变，不改。
- 依赖：T13。
- 完成说明：上述文件都已改。changelog 的开放小节是 `## [1.0.0] - Unreleased`，写在它的 `Changed` 下。删除了过时的 “root shell 仍导出 DEBIAN_FRONTEND” 一条。

### T15. 验证 ✅ 已完成 (Completed)

- 单元测试与静态检查：
    - `cargo build --locked`、`cargo test`、`cargo clippy`、`cargo fmt --check`。
    - `mise exec -- shellcheck src/guest/*.sh`、`mise exec -- shfmt -d src/guest/*.sh`。
- 集成测试（需 Mac 宿主机，镜像已在 `~/.cache/vibe`，不要重新下载）：
    - VM 内网络一律经 `socks5://192.168.1.88:3003`：`VIBE_VM_CHECK_ARGS="--proxy socks5://192.168.1.88:3003 --proxy-udp --dns 1.1.1.1"`。手动运行 `vibe` 时也加 `--proxy socks5://192.168.1.88:3003 --proxy-udp`。
    - `cargo test -- --ignored vm_user`（`tests/vm_user.rs`）：覆盖 provision 的 `install bindfs`、`UserScript` 的 vm-user 分支、run 的 `mount apt cache` 与 root 的 apt。
    - 手动跑 `vibe --ssh-key <pub>`（root 与 `--vm-user dev` 各一次）：guest 中 `~/.ssh` 为 700，`authorized_keys` 为 600，属主正确。
    - 用没有 bindfs 的镜像跑一次 `--vm-user dev`：宿主机打印 `apt-no-bindfs` 对应警告。
    - 会话目录：正常退出后 `<cache>/sessions/` 下无本次目录；让一个 `--script` 失败（`exit 1`）后目录保留并打印路径；再启动一次后旧目录被清理。
- 结果写入本文末尾“验收记录”。下载了任何文件都要说明。
- 依赖：T14。
- 完成说明：全部通过。集成测试发现一个缺陷并已修复，见“验收记录”。

## 任务依赖概览

| 任务 | 依赖 |
| --- | --- |
| T01 | 无 |
| T02 | T01 |
| T03 | T01 |
| T04 | 无 |
| T05、T06、T07 | T03、T04 |
| T08 | T01、T03 |
| T09 | T08 |
| T10 | T04、T05、T09 |
| T11 | 无 |
| T12 | T03 |
| T13 | T04–T12 |
| T14 | T13 |
| T15 | T14 |

T05、T06、T07、T08 互不依赖，可按任意顺序做。每做完一个就提交一次。

## 验收记录

T01–T04 已完成（2026-11-17）。未下载镜像或其他文件。

已跑：`cargo build --locked`、`cargo test`（184 通过，忽略项未跑）、`cargo clippy --all-targets -- --deny warnings`、`cargo fmt --check`。

文档：`AGENTS.md`、`AGENTS.zh-Hans.md` 模块表；`docs/how-vm-boot.md` 与中文版；`docs/file-shares-between-vm.md` 与中文版；`CHANGELOG.md` 与中文版的 `Changed`。T14 仍负责包装器、`provision-env.sh` 和“控制台不再逐行键入脚本”的文档与 changelog 措辞。那些行为还没改。

T05–T08 已完成（2026-11-17）。未下载镜像。未跑 `#[ignore]` 的 VM 集成测试。T09 及之后未做。

已跑：`cargo build --locked --offline`、`cargo test --locked --offline`（`unittests src/main.rs`：191 passed; 0 failed; 3 ignored。`tests/alpine_maintenance.rs`、`tests/disk_prepare.rs`、`tests/vm_devices.rs`、`tests/vm_user.rs` 全部 ignored）、`cargo clippy --locked --offline --all-targets -- --deny warnings`、`cargo fmt --check`。

`mise` 不在 PATH 上，Homebrew 目录当前用户不可写，所以没有用 `mise exec`。经 `HTTPS_PROXY` 从 GitHub 官方发布页下载了 shellcheck v0.11.0（darwin aarch64）和 shfmt v3.14.1（darwin arm64）到 `/tmp`，只检查这四个新脚本：`src/guest/install_bindfs.sh`、`mount_apt_cache.sh`、`install_ssh_key.sh`、`script_wrapper.sh`。`shellcheck` 与 `shfmt -d` 都通过。这两个二进制不在仓库里。

文档：同上几份，另加 `docs/how-provisioning-scripts-work.md` 与中文版的 bindfs 安装步骤。`CHANGELOG` 的 `Changed` 写了 `install_bindfs.sh`、`mount_apt_cache.sh`、`install_ssh_key.sh`。`script_wrapper.sh` 尚未接入，`script_command` 仍自己拼包装器，所以 changelog 不把它写成已生效行为。包装器、`provision-env.sh`、控制台一行 `eval` 仍由 T14 写。

行为备忘：root shell 在 `install bindfs` 之后仍键入 ` export DEBIAN_FRONTEND=noninteractive`，留给 T10 删除。root 的 HOME 参数是 `GuestPath::to_shell` 的 `/root/`（`Path::join("")` 带末尾斜杠），不是 `/root`。非 root 的 ssh 动作含 `/mnt/root/virtiofs-shared`，测试断言的是不含 `/root/.`。apt 脚本只在非 root 且存在 apt 共享时写入。

宿主机行为测试验收后保留。没有只为阶段性验证而写、做完就无意义的用例，不单列清理章节。T09 再删只断言旧 `script_command` 字符串的两个测试。

T09–T13 已完成（2026-11-17）。未下载镜像。未跑 `#[ignore]` 的 VM 集成测试。T14、T15 未做。

已跑：`cargo build --locked --offline`、`cargo test --locked --offline`（`unittests src/main.rs`：193 passed; 0 failed; 3 ignored。`tests/alpine_maintenance.rs`、`tests/disk_prepare.rs`、`tests/vm_devices.rs`、`tests/vm_user.rs` 全部 ignored）、`cargo clippy --locked --offline --all-targets -- --deny warnings`、`cargo fmt --check`。

`/tmp/shellcheck` 已不在。经 `HTTPS_PROXY` 从 GitHub 官方发布页重新下载 shellcheck v0.11.0（darwin aarch64）到 `/tmp`。`/tmp/shfmt`（v3.14.1）仍在。只检查四个新脚本：`src/guest/provision-env.sh`、`check_vm_user_home.sh`、`clear_machine_id.sh`、`repl_finalize.sh`。`shellcheck` 与 `shfmt --indent 2 -d` 通过。这两个二进制不在仓库里。未对其余 `src/guest/*.sh` 执行 `shfmt -w`。

文档：产品文档与 `CHANGELOG` 未改。T14 要写已经生效的行为：包装器改为一行 `eval`、脚本进会话目录、`provision-env.sh`、控制台不再逐行键入包装器。上文“那些行为还没改”只描述 T05–T08 当时的状态。

行为备忘：`/tmp/my script.sh` 暂存为 `scripts/003-my_script.sh.sh`。root shell 不再单独键入 ` export DEBIAN_FRONTEND=noninteractive`。该值由 source `provision-env.sh` 带入。`install_bindfs.sh` 自己仍导出它。`check_vm_user.sh` 与 `ensure_vm_user.sh` 仍内联 base64，因为它们在 VirtioFS 之前运行。

宿主机行为测试验收后保留。没有只为阶段性验证的用例，不单列“验收后的清理”。

T14、T15 已完成（2026-11-17）。未下载镜像或其他文件。shellcheck 与 shfmt 用上次下载到 `/tmp` 的二进制。

已跑：`cargo build --locked`、`cargo test --locked`（`unittests src/main.rs`：194 passed; 0 failed; 3 ignored）、`cargo clippy --locked --all-targets -- --deny warnings`、`cargo fmt --check`。八个新模板 `shellcheck` 与 `shfmt -d` 都通过。

集成测试：`VIBE_VM_CHECK_DISK=~/.cache/vibe/default.raw VIBE_VM_CHECK_ARGS="--proxy socks5://192.168.1.88:3003 --proxy-udp --dns 1.1.1.1" cargo test --locked --test vm_user -- --ignored --test-threads=1`。修复后 2 passed。

发现的缺陷：非 root `--vm-user` 的 run `--script` 在 `user_actions` 中，于 `su` 之后的 vm-user shell 运行。`stage_scripts` 给它暂存了文件，包装器以 dev 身份 `cat /mnt/root/...` 失败（Permission denied）。修复：`SessionDir::stage_scripts` 在 `switch_user` 且 vm-user 非 root 时不暂存 `user_actions`，脚本保持内嵌 base64。provision 的动作都在 `root_actions`，不受影响。新增单元测试 `stage_scripts_keeps_vm_user_shell_scripts_inline`。文档（how-vm-boot、how-provisioning-scripts-work、CHANGELOG、调研文档，含中文版）已注明此例外。

另修：`session_dir` 测试的 `scratch()` 只用 pid 和纳秒时间命名。macOS 时间精度为微秒，并行测试偶发撞名。加了原子计数器。

手动验收（临时镜像由 `default.raw` 克隆后 provision，`--vm-user dev`，均加 `--proxy socks5://192.168.1.88:3003 --proxy-udp`）：

- `--ssh-key`：root 为 `700 root /root/.ssh`、`600 root /root/.ssh/authorized_keys`；dev 为 `700 dev /home/dev/.ssh`、`600 dev /home/dev/.ssh/authorized_keys`。key 各出现一次。
- 删掉 bindfs 后跑 `--vm-user dev`：打印 `bindfs is not installed in this image ...` 警告。
- `--script` 内容为 `exit 1`：打印 `Vibe kept the session scripts at <path>`，目录保留。下一次启动清掉它。正常停止后 `<cache>/sessions/` 为空。
- 控制台每个脚本只显示一行 `eval`。

已有问题，不在本次范围：`src/guest/maint.sh` 有 7 条 shellcheck 告警。HEAD 上一条写错的 `# shellcheck` 指令让检查提前中止，本次改动修正注释后告警才出现。`check_vm_user.sh`、`ensure_vm_user.sh`、`maint.sh` 有 shfmt 差异，HEAD 上已存在。

临时镜像、密钥和工作目录已删。新增测试都守护行为，不单列“验收后的清理”。
