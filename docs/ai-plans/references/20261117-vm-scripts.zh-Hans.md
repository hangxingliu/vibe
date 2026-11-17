# VM 内脚本索引

本文索引所有在客体机（guest）内运行的 shell 代码。宿主机从不运行这些代码。改脚本前先按本表找到源头，再读对应模块。

行号会变，本文只写文件和符号名。用 `grep` 定位符号。

## 送入方式

### 层级关系

```text
send_text                      串口键入（最底层）
├── LoginAction::Send          直接键入一行文本
│   ├── write_file_into_vm / write_user_file_into_vm   生成 base64 写文件命令
│   ├── ssh_key_command / env_login_actions / motd_login_action / ...
│   ├── ` . '<provision-env.sh>'`        source 会话目录中的导出文件
│   └── 其他拼接命令（挂载、遮蔽、export、cd ...）
├── LoginAction::Script / UserScript
│   └── script_command                     一行 eval 渲染后的 script_wrapper.sh，等 VIBE_SCRIPT_STATUS_*
├── LoginAction::StatusCommand
│   └── status_command_and_marker          包装单行命令，等 VIBE_STEP_STATUS_*
│       ├── GuestFile::sh_command           sh -c "$(cat <会话文件>)"，作为 command 内容嵌入
│       └── run_guest_script → sh_command    sh -c "$(echo b64 | base64 --decode)"，作为 command 内容嵌入
├── LoginAction::SecretSend                见到标记后键入密码
├── LoginAction::LegacyLogin (legacy_login) 键入 root、密码、就绪探针
└── LoginAction::SwitchUser (switch_user)   键入 su 命令和切换探针

LoginAction::Expect            不键入，只等输出

seed ISO (build_seed)          不走 send_text；Alpine 维护 VM 自行运行

会话目录 (SessionDir)          不走 send_text；run_vm 前写好，VirtioFS 只读共享
```

所有 `LoginAction` 由 [`ActionRunner::run`](../../../src/session/actions.rs) 分派。等待输出的部分走 [`OutputMonitor`](../../../src/vm_io.rs)。

### 各方式说明

| 方式 | 入口 | 基于 | 说明 |
| --- | --- | --- | --- |
| 串口键入 | `send_text`（[`src/session/actions.rs`](../../../src/session/actions.rs)） | `VmInput` 通道 | 末尾补 `\n`，按 256 字节分块，每块间隔 10 ms，写入 `hvc0`。命令以空格开头，配合 `HISTCONTROL=ignorespace` 不进历史。 |
| 单行文本 | `LoginAction::Send`（[`src/session.rs`](../../../src/session.rs)） | `send_text` | 原样键入，不等结果。 |
| base64 写文件 | `write_file_into_vm`、`write_user_file_into_vm`（[`src/session/shell.rs`](../../../src/session/shell.rs)） | `Send` | 只生成字符串：`echo '<b64>' \| base64 --decode > 路径` 再 `chmod +x`。非 root 用户经 `runuser` 由该用户自己写。 |
| 会话目录 | `SessionDir`、`GuestFile`（[`src/session_dir.rs`](../../../src/session_dir.rs)） | VirtioFS 只读共享 | `<cache>/sessions/<pid>-<nonce>/`，0700。`run_vm` 前写好全部文件。guest 路径 `/mnt/root/virtiofs-shared/<tag>/<文件>`，不 bind mount，只有 root 可读。正常停止后删除；否则保留并打印路径，下次创建时清理 pid 已死的目录。不写密钥。 |
| 会话文件子进程 | `GuestFile::sh_command`（[`src/session_dir.rs`](../../../src/session_dir.rs)） | `StatusCommand` / `Send` | `sh -c "$(cat '<guest 路径>')" <名字> <参数>`。root 读文件，`runuser` 后也能用；stdin 不承载脚本。 |
| 脚本包装 | `script_command`（[`src/session/shell.rs`](../../../src/session/shell.rs)） | `send_text` | `Script` / `UserScript` 用。键入一行 ` eval "$(echo '<b64>' \| base64 --decode)"`。见“包装器”。 |
| 状态命令包装 | `status_command_and_marker`（[`src/session/shell.rs`](../../../src/session/shell.rs)） | `send_text` | `StatusCommand` 用。见“包装器”。 |
| base64 子进程 | `sh_command`（[`src/guest_template.rs`](../../../src/guest_template.rs)），`run_guest_script`（[`src/vm_user.rs`](../../../src/vm_user.rs)）调用 | `StatusCommand` | `sh -c "$(echo '<b64>' \| base64 --decode)" <name> <args>`。不落盘，stdin 空闲。只用于 VirtioFS 挂载前的脚本。 |
| 密码键入 | `SecretSend`（[`src/session/actions.rs`](../../../src/session/actions.rs) 的 `secret_send`） | `send_text` | 前一条 `Send` 已关回显并打印标记。见到标记才键入密码。 |
| 登录与切换 | `legacy_login`、`switch_user`（[`src/session/login.rs`](../../../src/session/login.rs)） | `send_text` | 键入用户名、密码、探针，读 `OutputMonitor` 判断结果。 |
| seed ISO | `build_seed`（[`src/seed.rs`](../../../src/seed.rs)） | `hdiutil` | 把 `maint.sh` 写成 `user-data`。Alpine 的 tiny-cloud 执行它。结果经 `hvc1` 帧回传。 |

## 标记协议

宿主机靠标记行判断进度。标记拆成两个 `printf` 参数，回显的命令行永远不含完整标记。

| 标记 | 生成处 | 下一行内容 |
| --- | --- | --- |
| `VIBE_READY_<nonce>` | `readiness_probe_command_and_marker`（[`src/session/login.rs`](../../../src/session/login.rs)） | `id -u`，必须为 `0` |
| `VIBE_SWITCHED_<nonce>` | `switch_probe_command_and_marker`（[`src/session/login.rs`](../../../src/session/login.rs)） | `<用户名> <uid>`，uid 不能为 `0` |
| `VIBE_PW_<nonce>` | `set_root_password_actions`（[`src/session/password.rs`](../../../src/session/password.rs)） | 无。出现后宿主机键入密码 |
| `VIBE_PW_DONE_<nonce>` | 同上 | `chpasswd` 退出码 |
| `VIBE_SCRIPT_STATUS_script_<N>` | `script_command`（[`src/session/shell.rs`](../../../src/session/shell.rs)） | 脚本退出码 |
| `VIBE_STEP_STATUS_step_<N>` | `status_command_and_marker`（[`src/session/shell.rs`](../../../src/session/shell.rs)） | `<退出码> <notice>`，notice 可空 |
| `VIBE_PROVISION_SUCCESS` | `build_provision_plan`（[`src/provision.rs`](../../../src/provision.rs)） | 无。`Expect` 等它 |
| `VIBE1 <nonce> PREP-*` | [`src/guest/maint.sh`](../../../src/guest/maint.sh) 的 `emit`，写 `hvc1` | 由 [`src/control.rs`](../../../src/control.rs) 的 `FrameReader` 解析 |

## 包装器

### `script_command(id, script, file, as_vm_user)`

模板 [`src/guest/script_wrapper.sh`](../../../src/guest/script_wrapper.sh)。Rust 用 `replace_region` 填 `INJECT_SCRIPT_WRAPPER_CONFIG`（`__vibe_script_id`、`__vibe_script_src` 或 `__vibe_script_b64`、`__vibe_as_vm_user`），整体 base64 后只键入一行 ` eval "$(echo '<b64>' | base64 --decode)"`。控制台只显示这一行。有 `file` 时 `cat` 会话目录中的源文件；无 `file`（Alpine 维护、`vm_checks`）时解码内嵌 base64。模板在当前 shell 中执行，不 `exit`，不 `set -e`，结束时 `unset` 所有 `__vibe_*`。

步骤：

1. 若 uid 为 0：`free -h`，清页缓存，压缩内存。
2. 建或复用私有目录 `/tmp/vibe-scripts-<uid>`（mode 700，非符号链接，属主为当前 uid）。
3. 把源脚本复制为 `<id>.sh` 并 `chmod +x`。写失败则 status 为 1。
4. 以 `</dev/null` 运行脚本，保存退出码。
5. 成功且有 `~/.local/bin/mise` 时重新 `mise activate bash`。
6. 打印空行、`VIBE_SCRIPT_STATUS_<id>`、退出码。

`__vibe_as_vm_user=1` 时差异：

- 目录为 `/tmp/vibe-scripts-$VIBE_VM_UID`，root 建目录后 `chown` 给该用户。
- root 读源文件（vm-user 进不了 `/mnt/root`），经管道交给 `runuser ... sh -c 'cat >"$1" && chmod +x "$1"'` 由用户自己写。
- 运行：`cd "$VIBE_VM_HOME" && runuser -u "$VIBE_VM_USER" -- env -u MISE_* HOME=... bash --login -c 'exec "$0"' <脚本>`。用 `exec` 避免 `~/.bash_logout` 覆盖退出码。

超时：`SCRIPT_ACTION_TIMEOUT`（24 小时，[`src/session/actions.rs`](../../../src/session/actions.rs)）。

### `status_command_and_marker(id, command)`

```sh
 { <command>
} </dev/null
 __vibe_status=$?; printf ... '<marker>' "$__vibe_status" "${__vibe_notice:-}"; unset __vibe_status __vibe_notice
```

`command` 可设 `__vibe_notice` 为一个词。宿主机把它转成警告（`notice_message`，[`src/session_plan.rs`](../../../src/session_plan.rs)）。退出码非 0 则会话停止。

## 独立脚本文件

| 文件 | 嵌入点 | 运行环境 | 作用 |
| --- | --- | --- | --- |
| [`src/bashrc.sh`](../../../src/bashrc.sh) | `BASHRC_SCRIPT`（[`src/session/shell.rs`](../../../src/session/shell.rs)） | Debian，写到 `~/.profile` | 颜色别名、无限历史、PATH（cargo/bun/local）、mise 激活、关闭各 agent 遥测、`stty -ixon`。 |
| [`src/bash_logout.sh`](../../../src/bash_logout.sh) | `BASH_LOGOUT_SCRIPT`（[`src/session/shell.rs`](../../../src/session/shell.rs)） | Debian，写到 `/root/.bash_logout` | 写历史。非 SSH、非 tmux、无 tmux server 时 `systemctl poweroff`。 |
| [`src/guest/check_vm_user.sh`](../../../src/guest/check_vm_user.sh) | `CHECK_VM_USER_SCRIPT`（[`src/vm_user.rs`](../../../src/vm_user.rs)） | Debian，root，`sh_command`（内嵌 base64） | 检查 `--vm-user` 账户。退出码 10-15 对应 `VmUserProblem`。属特权组时打印 `groups=...`。 |
| [`src/guest/ensure_vm_user.sh`](../../../src/guest/ensure_vm_user.sh) | `ENSURE_VM_USER_SCRIPT`（[`src/vm_user.rs`](../../../src/vm_user.rs)） | Debian，root，`sh_command`（内嵌 base64） | provision 时账户缺失则 `useradd`。已有账户不改。输出走 stderr。 |
| [`src/guest/script_wrapper.sh`](../../../src/guest/script_wrapper.sh) | `SCRIPT_WRAPPER`（[`src/session/shell.rs`](../../../src/session/shell.rs)） | Debian，当前 shell 中 `eval`（bash） | `Script` / `UserScript` 包装器。区块 `INJECT_SCRIPT_WRAPPER_CONFIG`。 |
| [`src/guest/install_bindfs.sh`](../../../src/guest/install_bindfs.sh) | `render_install_bindfs`（[`src/session_plan.rs`](../../../src/session_plan.rs)） | Debian，root，会话文件子进程 | provision 时缺 `bindfs` 则 `apt-get install`。区块 `INJECT_INSTALL_BINDFS_CONFIG`（`APT_PROXY_URL`）。自己导出 `DEBIAN_FRONTEND`。 |
| [`src/guest/mount_apt_cache.sh`](../../../src/guest/mount_apt_cache.sh) | `run_run`（[`src/run_command.rs`](../../../src/run_command.rs)）写入，`mount_apt_cache_action`（[`src/session_plan.rs`](../../../src/session_plan.rs)）执行 | Debian，root，会话文件子进程 | 非 root 运行时用 `bindfs` 挂 apt 缓存；缺 `bindfs` 或 `_apt` 时退回 bind mount 并输出 `apt-no-bindfs`。区块 `INJECT_MOUNT_APT_CACHE_CONFIG`。 |
| [`src/guest/install_ssh_key.sh`](../../../src/guest/install_ssh_key.sh) | `ssh_key_command`（[`src/run_command.rs`](../../../src/run_command.rs)） | Debian，目标用户，会话文件子进程 | 写 `~/.ssh/authorized_keys`（700/600）。`$1` 为目标 HOME。非 root 前加 `runuser`。区块 `INJECT_INSTALL_SSH_KEY_CONFIG`。 |
| [`src/guest/provision-env.sh`](../../../src/guest/provision-env.sh) | `render_provision_env`（[`src/provision.rs`](../../../src/provision.rs)） | Debian，root shell ` . ` 加载 | 导出 `DEBIAN_FRONTEND` 与 `VIBE_PROVISION_*`、`VIBE_GIT_SHA`、`VIBE_BUILD_DATE`。区块 `INJECT_PROVISION_ENV_CONFIG`。 |
| [`src/guest/check_vm_user_home.sh`](../../../src/guest/check_vm_user_home.sh) | `CHECK_VM_USER_HOME_SCRIPT`（[`src/provision.rs`](../../../src/provision.rs)） | Debian，root，`Script` | 非 root provision 后检查 HOME 中无 root 属主文件。 |
| [`src/guest/clear_machine_id.sh`](../../../src/guest/clear_machine_id.sh) | `CLEAR_MACHINE_ID_SCRIPT`（[`src/provision.rs`](../../../src/provision.rs)） | Debian，root，`Script` | 非 `--repl` 发布前清空 machine-id。 |
| [`src/guest/repl_finalize.sh`](../../../src/guest/repl_finalize.sh) | `REPL_FINALIZE_SCRIPT`（[`src/provision.rs`](../../../src/provision.rs)） | Debian，root，`Script` | `--repl` 时建临时单元，关机时清 machine-id。 |
| [`src/guest/maint.sh`](../../../src/guest/maint.sh) | `USER_DATA`（[`src/seed.rs`](../../../src/seed.rs)） | Alpine 维护 VM，busybox ash | 挂 seed、解析 manifest，依次执行 `self`、`target`、`fsck`、`inspect`、`apply`、`verify`、`unmount`。清空目标盘 machine-id 和 root 密码字段。`shadow-selftest` 参数跑自测。 |
| [`provisioning/*.sh`](../../../provisioning/) | `BUILTIN_PROVISION_SCRIPTS`（[`build.rs`](../../../build.rs) 生成） | Debian | 内置配置脚本，见下节。 |

区块替换用 [`src/guest_template.rs`](../../../src/guest_template.rs)：`replace_region`（内部模板，缺区块或重复即报错）、`replace_region_if_present`（`provisioning/base.sh`）、`shell_assign`（`NAME='value'`）。

`run_vm` 前写入会话目录的文件：provision 有 `install_bindfs.sh`、`provision-env.sh`；run 有 `mount_apt_cache.sh`（非 root 且有 apt 共享）、`install_ssh_key.sh`（`--ssh-key`）；两者都有 `scripts/NNN-<名字>.sh`（`SessionDir::stage_scripts`，拆分的内置脚本用 `.root.sh` / `.user.sh`；非 root vm-user 的 run `user_actions` 不暂存，保持内嵌 base64，因为该用户读不到 `/mnt/root`）。

`maint.sh` 失败时发 `PREP-RESULT fail <code> <step>`。code：`2` seed，`3` self，`4` target，`5` fsck，`6` inspect，`7` apply，`8` verify，`9` unmount。详见 [`docs/alpine-os-purpose.md`](../../alpine-os-purpose.md)。

## 内置配置脚本

`build_provision_script`（[`src/provision.rs`](../../../src/provision.rs)）处理每个脚本：

- `base` 且有 guest 代理时，用 `replace_region_if_present` 替换 `# region INJECT_PROXY_CODE` 区块。注入 apt 代理（写 `APT_PROXY_FILE`，[`src/proxy.rs`](../../../src/proxy.rs)）和 `generate_http_proxy_env_export_cmds` 导出。
- root 用户，或自定义脚本：整段作为 `Script` 以 root 运行。
- 非 root `--vm-user` 的内置脚本：按 `# vibe:run-as-user` 拆分（`split_run_as_user`）。前段为 `Script`（root），后段为 `UserScript`（vm-user）。后段只继承 `#!` 行。

| 脚本 | root 段 | user 段 |
| --- | --- | --- |
| [`base.sh`](../../../provisioning/base.sh) | 写镜像历史 `/root/vibe-image-history.txt`；非首跑则退出。apt 超时、代理注入、Debian fast-forward 源、`full-upgrade`、装基础包、sshd 配置并禁用、`growpart`/`resize2fs`、主机名 `vibe`。 | 首跑才执行：装 mise，写 `~/.config/mise/config.toml`，`mise install`，写 `~/.local/state/vibe/base-user.done`。 |
| [`rust.sh`](../../../provisioning/rust.sh) | `apt install sccache` | rustup（minimal + rustfmt、clippy） |
| [`claude.sh`](../../../provisioning/claude.sh) | 无 | 追加 `claude` 到 mise 配置，`mise install` |
| [`codex.sh`](../../../provisioning/codex.sh) | 无 | `mise use -g npm:@openai/codex` |
| [`pi.sh`](../../../provisioning/pi.sh) | 无 | `mise use -g npm:@earendil-works/pi-coding-agent` |
| [`golang.sh`](../../../provisioning/golang.sh) | 无 | `mise use -g go@latest` |
| [`copilot.sh`](../../../provisioning/copilot.sh) | 无 | `curl gh.io/copilot-install \| bash` |
| [`gemini.sh`](../../../provisioning/gemini.sh) | 无 | antigravity 安装脚本 |
| [`grok.sh`](../../../provisioning/grok.sh) | 无 | `x.ai/cli/install.sh` |

所有脚本自带 `retryable` / `exec_retry`（最多重试 3 次，间隔 10 秒）。

provision 前置导出的环境变量（`build_provision_plan` 写 `provision-env.sh`，root shell ` . ` 加载）：`DEBIAN_FRONTEND=noninteractive`、`VIBE_PROVISION_IMAGE`、`VIBE_PROVISION_BASE`、`VIBE_GIT_SHA`、`VIBE_BUILD_DATE`、`VIBE_PROVISION_SCRIPTS`、`VIBE_PROVISION_VM_USER`。

## provision 执行顺序

入口：`run_provision`（[`src/provision_command.rs`](../../../src/provision_command.rs)）调用 `provision_image`（[`src/provision.rs`](../../../src/provision.rs)）。

1. 宿主机复制基础盘为工作副本。
2. `ensure_bootable`（[`src/disk_prepare.rs`](../../../src/disk_prepare.rs)）探测副本。仅当探测结果需要处理时，`process_disk`（[`src/maintenance.rs`](../../../src/maintenance.rs)）启动 Alpine 维护 VM，运行 [`maint.sh`](../../../src/guest/maint.sh)。
3. 宿主机扩容副本，创建会话目录并写入脚本。`run_vm`（[`src/vm_run.rs`](../../../src/vm_run.rs)）启动 Debian，`expand_session` 按下列顺序展开 `build_provision_plan` 的动作。
4. 登录（`legacy_login_prefix`）：
   1. `legacy_login`：键入 `root`、密码（可选）、就绪探针。
   2. ` export HISTCONTROL=ignorespace`、` unset HISTFILE`。
   3. ` stty -F /dev/hvc0 sane`。
   4. 后台尺寸同步循环（读 `/dev/hvc1`）。
   5. 写 `/root/.bash_logout`（[`src/bash_logout.sh`](../../../src/bash_logout.sh)）。
5. root 准备：
   1. 有 `--root-password`：`stty -echo ... chpasswd`，再 `SecretSend` 键入密码。
   2. 非 root `--vm-user`：`StatusCommand` `create vm-user`（[`ensure_vm_user.sh`](../../../src/guest/ensure_vm_user.sh) 加 [`check_vm_user.sh`](../../../src/guest/check_vm_user.sh)），再导出 `VIBE_VM_*`。
6. 挂载：VirtioFS 暂存，逐个 bind mount。provision 不设遮蔽，不用 `bindfs` 挂 apt 缓存。
7. root 动作：
   1. `StatusCommand` `install bindfs`（会话文件 `install_bindfs.sh`）。
   2. 写 `/root/.profile`（[`src/bashrc.sh`](../../../src/bashrc.sh)）并 `. /root/.profile`。
   3. 非 root：写 vm-user 的 `~/.profile`。
   4. ` . '<provision-env.sh>'`。
   5. 配置脚本，依次运行（编号 `[action N]`）：`base` root 段、`base` user 段、其余脚本各自的 root 段和 user 段。root 用户或自定义脚本不拆分。
   6. 非 root：`check-vm-user-home`（`check_vm_user_home.sh`）。
   7. `finalize-machine-id`：非 `--repl` 用 `clear_machine_id.sh`，`--repl` 用 `repl_finalize.sh`。
   8. ` echo VIBE_PROVISION_SUCCESS`，`Expect` 该标记。
   9. 非 `--repl`：` systemctl poweroff; sleep 100`。
8. 切换：仅 `--repl` 且非 root。`sysctl ... legacy_tiocsti=0`，`su --login --pty`，切换探针。
9. 收尾：mise 激活、fzf 键绑定。
10. `--repl` 结束时，root shell `exit` 触发 `.bash_logout` 关机。`repl_finalize.sh` 的单元在关机时清 machine-id。

已有镜像的 `provision --repl`（无 `--replace`）走短路径（`existing_image_repl_actions`，[`src/provision_command.rs`](../../../src/provision_command.rs)）：登录、`check vm-user`、导出 `VIBE_VM_*`、挂载、写 `.profile`、切换（非 root）、收尾。不跑配置脚本，不设 root 密码，不调 Alpine。

## run 执行顺序

入口：`run_run`（[`src/run_command.rs`](../../../src/run_command.rs)）。

1. 镜像为 `default` 且 `default.raw` 不存在时，先跑 `ensure_default_image`（[`src/provision.rs`](../../../src/provision.rs)）。这就是一次完整 provision，按上节顺序执行所有内置脚本。
2. 宿主机克隆模板镜像。run 路径不探测、不修改磁盘，不调 Alpine。创建会话目录并写入脚本。
3. `run_vm` 启动 Debian，`expand_session` 按下列顺序展开 `run_session_plan` 的动作。
4. 登录：同 provision 第 4 步。
5. root 准备：非 root `--vm-user` 时 `StatusCommand` `check vm-user`（只检查，不建账户），再导出 `VIBE_VM_*`。
6. 挂载：
   1. VirtioFS 暂存目录。
   2. 每个共享：建挂载点，bind mount（会话共享除外）。非 root 时 apt 缓存改用会话文件 `mount_apt_cache.sh`（`bindfs`）。
   3. tmpfs 遮蔽（`.vibe`，git 模式为 no 时的 `.git`）。
7. root 动作：
   1. `--mount-tmux-conf`：写 `~/.tmux.conf`。
   2. `--write-bashrc`：root 写并 source `/root/.profile`；非 root 写 vm-user 的 `~/.profile`。
   3. `--ssh-key`：会话文件 `install_ssh_key.sh` 写 `authorized_keys`，` systemctl start ssh`，` ip -4 -br addr`。
8. 切换（非 root）：`sysctl ... legacy_tiocsti=0`，`su --login --pty`，切换探针。vm-user 登录 shell 读自己的 `~/.profile`。
9. 用户动作（非 root 时在 vm-user shell 中）：
   1. 非 root：` export HISTCONTROL=ignorespace; unset HISTFILE`。
   2. ` cd <project>`。
   3. `--env` 导出。
   4. ` export HISTFILE=...`。
   5. MOTD 共享表（无共享时 ` clear`）。
   6. 有 guest 代理：导出 `http_proxy` 等。
   7. 用户的 `--send` / `--expect` / `--script`，按命令行顺序（编号 `[action N]`）。
10. 收尾：mise 激活、fzf 键绑定。之后终端交给用户。
11. 用户 `exit`：非 root 先回到 root shell，再 `exit` 才触发 `.bash_logout` 关机。

## Rust 拼接片段明细

上两节的每一步对应下表片段。阶段顺序由 `expand_session`（[`src/session_plan.rs`](../../../src/session_plan.rs)）决定。

### 1. 登录

| 片段 | 位置 | 作用 |
| --- | --- | --- |
| 键入 `root`、密码 | `legacy_login`（[`src/session/login.rs`](../../../src/session/login.rs)） | 串口登录。密码不进日志。 |
| 就绪探针 | `readiness_probe_command_and_marker`（同上） | 确认 root shell。 |
| ` export HISTCONTROL=ignorespace`、` unset HISTFILE` | `legacy_login_prefix`（[`src/session.rs`](../../../src/session.rs)） | 自动命令不进历史。 |
| ` stty -F /dev/hvc0 sane` | 同上 | 修正串口终端默认值。 |
| 后台 `while read rows cols ... < /dev/hvc1` | 同上 | 同步宿主机终端尺寸到 `hvc0`。 |
| 写 `/root/.bash_logout` | 同上 | 见 [`src/bash_logout.sh`](../../../src/bash_logout.sh)。 |

### 2. root 准备

| 片段 | 位置 | 场景 |
| --- | --- | --- |
| `stty -echo; printf 标记; read __vibe_pw; ... chpasswd` | `set_root_password_actions`（[`src/session/password.rs`](../../../src/session/password.rs)） | provision 带 `--root-password`。密码经 `SecretSend` 键入，不上命令行。 |
| `__vibe_notice=$(sh -c "$(echo b64 \| base64 --decode)" check_vm_user.sh <user>)` | `check_vm_user_actions`（[`src/vm_user.rs`](../../../src/vm_user.rs)） | run、已有镜像的 `provision --repl`。标签 `check vm-user`。 |
| `__vibe_notice=$(<ensure> && <check>)` | `ensure_vm_user_actions`（[`src/vm_user.rs`](../../../src/vm_user.rs)） | 跑脚本的 provision。标签 `create vm-user`。 |
| ` export VIBE_VM_USER=... VIBE_VM_UID=... VIBE_VM_GID=... VIBE_VM_HOME=...` | `vm_user_env_action`（[`src/vm_user.rs`](../../../src/vm_user.rs)） | 上两者之后。后续片段依赖这些变量。 |

非 root 用户的 HOME 下路径渲染为 `"${VIBE_VM_HOME}"/<rel>`（`GuestPath::to_shell`，[`src/vm_user.rs`](../../../src/vm_user.rs)）。切换用户后改用 `"$HOME"/<rel>`（`GuestPath::to_user_shell`）。

### 3. 挂载

| 片段 | 位置 | 作用 |
| --- | --- | --- |
| `mkdir -p /mnt/root/virtiofs-shared && chmod -R 700 /mnt/root && mount -t virtiofs shared ...` | `mount_actions`（[`src/session_plan.rs`](../../../src/session_plan.rs)） | VirtioFS 挂到仅 root 可达的暂存目录。`chmod` 必须在挂载前。 |
| ` mkdir -p <target>` 或逐级 `install -d -o "$VIBE_VM_UID" ...` | 同上 | 建挂载点。HOME 下的新目录归 vm-user。已有目录不改。 |
| ` mount --bind <staging>/<tag> <target>` | 同上 | 每个共享一次 bind mount。`staging_only` 的会话共享跳过。 |
| `__vibe_notice=$(sh -c "$(cat '<mount_apt_cache.sh>')" ...)` | `mount_apt_cache_action`（同上） | 非 root 运行时挂 apt 缓存，见 [`src/guest/mount_apt_cache.sh`](../../../src/guest/mount_apt_cache.sh)。缺 `bindfs` 或 `_apt` 时退回 bind mount，输出 notice `apt-no-bindfs`。 |
| ` mount -t tmpfs -o mode=0555,uid=0,gid=0 tmpfs <path>` | `mask_actions`（同上） | 用空只读 tmpfs 遮蔽目录。遮蔽列表来自 `project_masks_and_shares`（[`src/shares.rs`](../../../src/shares.rs)）。 |

### 4. root 动作

run（`run_session_plan`，[`src/run_command.rs`](../../../src/run_command.rs)）：

| 片段 | 作用 |
| --- | --- |
| 写 `~/.tmux.conf` | `--mount-tmux-conf`，`write_user_file_into_vm`。 |
| 写 `.profile`（`write_bashrc_actions` 或 `write_user_bashrc_action`） | `--write-bashrc`。root 写后立刻 `. /root/.profile`。 |
| `ssh_key_command` | 运行 [`src/guest/install_ssh_key.sh`](../../../src/guest/install_ssh_key.sh)，写 `authorized_keys`（700/600，`umask 077`）。root 与非 root 共用；非 root 前加 `runuser -u "$VIBE_VM_USER" --`。 |
| ` systemctl start ssh`、` ip -4 -br addr` | 启动 sshd，显示 IP。 |

provision（`build_provision_plan`，[`src/provision.rs`](../../../src/provision.rs)）：

| 片段 | 作用 |
| --- | --- |
| `install_bindfs_action`（[`src/session_plan.rs`](../../../src/session_plan.rs)） | 运行 [`src/guest/install_bindfs.sh`](../../../src/guest/install_bindfs.sh)。缺 `bindfs` 则 `apt-get install`。vznat 模式下 `APT_PROXY_URL` 非空，用 `-o Acquire::*::Proxy` 传代理。 |
| `.profile` 写入 | root 写并 source；非 root 另写一份给 vm-user。 |
| ` . '<provision-env.sh>'` | [`src/guest/provision-env.sh`](../../../src/guest/provision-env.sh)：导出 `DEBIAN_FRONTEND`、`VIBE_PROVISION_*`，供 `base.sh` 写镜像历史。 |
| 各配置脚本 | 见“内置配置脚本”。 |
| [`check_vm_user_home.sh`](../../../src/guest/check_vm_user_home.sh) | 非 root 时运行。HOME 下有 root 属主文件（挂载点除外）则拒绝发布。 |
| [`clear_machine_id.sh`](../../../src/guest/clear_machine_id.sh) | 非 `--repl`。清空 `/etc/machine-id` 和 dbus machine-id。有挂载覆盖或类型异常则失败。 |
| [`repl_finalize.sh`](../../../src/guest/repl_finalize.sh) | `--repl`。用 `systemd-run` 建临时单元 `vibe-finalize-template`，在关机 `ExecStop` 时清 machine-id。脚本放 `/run/vibe-finalize/`，用 `/bin/sh` 调用（`/run` 为 `noexec`）。 |
| ` echo VIBE_PROVISION_SUCCESS` | 成功标记。 |
| ` systemctl poweroff; sleep 100` | 非 `--repl` 时关机。 |

### 5. 切换用户

| 片段 | 位置 | 作用 |
| --- | --- | --- |
| ` sysctl -q -w dev.tty.legacy_tiocsti=0 ...` | `switch_user_actions`（[`src/session_plan.rs`](../../../src/session_plan.rs)） | 禁止 vm-user 往 root 终端回塞输入。 |
| `SWITCH_USER_COMMAND` | [`src/session/login.rs`](../../../src/session/login.rs) | `su --login --pty "$VIBE_VM_USER"`。退出后提示回到 root shell。 |
| 切换探针 | `switch_probe_command_and_marker`（同上） | 每 10 秒重发，直到确认新 shell 身份。 |

### 6. 用户动作

run（`run_session_plan`，[`src/run_command.rs`](../../../src/run_command.rs)）：

| 片段 | 作用 |
| --- | --- |
| ` export HISTCONTROL=ignorespace; unset HISTFILE` | 仅非 root。`su --login` 会清环境。 |
| ` cd <project>` | 进入共享的当前目录。 |
| ` export NAME='value'` | `env_login_actions`（[`src/session/shell.rs`](../../../src/session/shell.rs)），来自 `--env`。 |
| ` export HISTFILE=...` | 恢复历史记录。 |
| ` clear && cat <<'VIBE_MOTD' ...` | `motd_login_action`（同上），打印共享表。无共享时只 `clear`。 |
| `generate_http_proxy_env_export_cmds`（同上） | 导出 `http_proxy` / `https_proxy` / `no_proxy`（大小写各一）。 |
| `--send` / `--expect` / `--script` | [`src/cli.rs`](../../../src/cli.rs) 解析。`--script` 是 `Script`，在当前 shell（可能是 vm-user）中运行，不拆分。 |

### 7. 收尾

`expand_session`（[`src/session_plan.rs`](../../../src/session_plan.rs)）最后追加两行：

- ` [ -x "${HOME}/.local/bin/mise" ] && eval "$(... activate bash)"`
- ` command -v fzf >/dev/null && eval "$(fzf --bash)"`

## 测试专用脚本

位于 [`src/vm_checks/`](../../../src/vm_checks/)，只被 `#[ignore]` 集成测试使用，不属于 CLI。

| 位置 | 作用 |
| --- | --- |
| [`src/vm_checks/devices.rs`](../../../src/vm_checks/devices.rs) | ` echo VIBE_RO_$(cat /sys/block/vda/ro)_...` 检查磁盘只读位。 |
| `legacy_boot`（[`src/vm_checks/maintenance/boot.rs`](../../../src/vm_checks/maintenance/boot.rs)） | ` echo VIBE_SHELL_$(id -u)` 确认准备后的盘能登录。 |
| `shadow_iso`（同上） | 自带 `user-data` 包装器，挂 seed 后运行 `maint.sh shadow-selftest`。 |
| `check_ssh_on`（[`src/vm_checks/maintenance/checks.rs`](../../../src/vm_checks/maintenance/checks.rs)） | `sshd -T` 检查 `permitemptypasswords no`，再试空密码 SSH 登录，应被拒绝。 |
| `VM_USER_CHECK_SCRIPT`（[`src/vm_checks/vm_user.rs`](../../../src/vm_checks/vm_user.rs)） | 经 `--script` 以 vm-user 运行。逐项打印 `VIBE_CHECK_OK/FAIL`：uid、cwd、共享读写、遮蔽、`.git` 只读、暂存目录、`umount`、apt `bindfs`。 |
| [`src/vm_checks/vm_user.rs`](../../../src/vm_checks/vm_user.rs) 的 `--send` 序列 | `exit` 回 root shell、`apt-cache gencaches`、`systemctl poweroff`。 |

## 调研探针

位于本目录。一次性调研用，不随二进制发布。`*-run.sh` 在宿主机运行，同名无后缀脚本经 `vibe --script` 以 root 在 guest 中运行。

| 文件 | 目的 |
| --- | --- |
| [`20261112-vm-user-virtiofs-probe.sh`](20261112-vm-user-virtiofs-probe.sh) | VirtioFS 对不同 UID 的权限行为。 |
| [`20261114-vm-user-virtiofs-uid-probe.sh`](20261114-vm-user-virtiofs-uid-probe.sh) | `mount -o uid=,gid=` 是否可用。 |
| [`20261116-vm-user-q4-layout-probe.sh`](20261116-vm-user-q4-layout-probe.sh) | 复核“root 专用暂存目录 + bind mount”布局。 |

## 修改须知

- 键入的命令以空格开头，保持不进历史。
- 标记拆成两个 `printf` 参数，回显里不能出现完整标记。
- 读 stdin 的命令要接 `</dev/null`，避免吞掉后续键入的行。
- 不在命令行放 root 密码。用 `SecretSend`。
- 非 root 用户 HOME 下的文件由该用户经 `runuser` 写，防符号链接攻击。
- 内置脚本的用户级部分放在 `# vibe:run-as-user` 之后。自定义脚本不拆分，以 root 运行。
- 改 `src/guest/*.sh` 或 `provisioning/*.sh` 后跑 `shellcheck` 和 `shfmt -d`。
- Rust 只替换模板的 `# region INJECT_<名字>_CONFIG` 区块。宿主机已知值用 `shell_assign` 注入；guest 运行时才知道的值作为位置参数传。
- 会话目录中不写密钥。
- 改启动、登录、provision 流程，同步 [`docs/how-vm-boot.md`](../../how-vm-boot.md)。改共享或遮蔽，同步 [`docs/file-shares-between-vm.md`](../../file-shares-between-vm.md)。改配置脚本规则，同步 [`docs/how-provisioning-scripts-work.md`](../../how-provisioning-scripts-work.md)。
