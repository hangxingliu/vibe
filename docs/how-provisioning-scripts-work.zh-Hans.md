# 配置脚本运行机制

`vibe provision` 会复制一份基础磁盘，启动该副本，并在客体机（guest）中运行 Shell 脚本。随后，它将该副本发布为指定名称的镜像。本文档定义了 `provisioning/` 目录下的文件规范以及用户自行传入的脚本路径规范。关于磁盘复制、Alpine 准备工作以及发布检查的详细信息，请参阅[虚拟机启动原理](how-vm-boot.md)。

这些脚本并不在宿主机上运行。宿主机通过串行控制台以 root 身份登录，并将每个脚本输入到该 Shell 中执行。

## 执行哪些脚本

`build.rs` 会在编译时读取所有的 `provisioning/*.sh`。脚本名称为去除扩展名后的文件名（例如 `golang.sh` 变为 `golang`）。文件字节内容通过 `include_str!` 存储。修改内置脚本后，需要重新编译 `vibe`，并配置新镜像以应用更改。已存在的 `.raw` 文件将保持原样。

`vibe --help` 会列出除 `base` 之外的所有内置脚本。文件第二行为描述信息：开头的 `#` 字符及前后的空格会被去除。列表按名称排序。

```text
vibe provision [OPTIONS] [@name | path/to/script.sh ...]
```

`provision` 后面的参数即为要执行的脚本，按照传入的顺序执行。

| 参数 | 加载内容 |
| --- | --- |
| `@name` | 具有该名称的内置脚本。若名称不存在，则输出帮助列表并退出。 |
| `path/to/script.sh` | 解析 CLI 时在宿主机上读取的对应文件。操作名称即为该路径。 |

`provisioning/base.sh` 始终最先执行。附加参数按传入顺序依次追加在其后。未指定脚本参数的 `vibe provision` 仅运行 `base`。执行 `vibe provision @rust @codex my.sh` 会依次运行 `base`、`rust`、`codex` 和 `my.sh`。传入 `@base` 会导致该脚本重复运行一次；第二次运行时会触发 `base.sh` 中的历史防护逻辑并直接返回。

当缺少 `default.raw` 时，单独运行 `vibe` 会通过执行所有内置脚本来创建它（`ensure_default_image`）。其中 `base` 最先执行。由于 `build.rs` 不对目录列表排序，其他脚本将按照目录遍历的顺序执行。如需保证顺序，请显式传入 `@name` 参数。`--image` 默认为 `default`。

在未指定 `--replace` 的情况下，针对已存在的镜像运行 `vibe provision --repl`，只会启动该镜像，不会运行任何脚本。

`--script` 是一项不同的功能，它用于常规 `vibe` 运行时的登录操作，`provision` 会拒绝该选项。

## 客体机内部的执行顺序

`provision_image`（`src/provision.rs`）会构建一个 `SessionPlan`（`build_provision_plan`）。`expand_session`（`src/session_plan.rs`）按以下顺序执行各个阶段。脚本运行期间，Shell 全程保持 root 权限。

1. 传统的 root 登录。当客体机输出 `Password:` 时，宿主机在关闭回显的情况下输入 `--root-password` 或 `VIBE_ROOT_PASSWORD`。密码绝不会写入脚本中。
2. 登录后，若提供了密码，则通过 `chpasswd` 应用该密码，以便发布的镜像保留此配置。若不存在非 root 的 `--vm-user`，则创建（使用 `useradd`、bash、独立的用户组）并进行校验。配置非 root 用户必须提供 root 密码。
3. 挂载 VirtioFS 共享目录。详见后文的[共享目录](#shares)部分。
4. 若镜像中缺少 `bindfs`，则进行安装，以便后续运行限制 apt 缓存。
5. 将 `src/bashrc.sh` 写入 `/root/.profile` 并执行 source。非 root 用户也会在其 `~/.profile` 中获得该文件（由 `runuser` 写入以确保归属权属于该用户）。配置流程始终执行此操作，而常规运行仅在指定 `--write-bashrc` 时才会写入。
6. 宿主机导出环境变量：`VIBE_PROVISION_IMAGE`、`VIBE_PROVISION_BASE`、`VIBE_GIT_SHA`、`VIBE_BUILD_DATE`、`VIBE_PROVISION_SCRIPTS` 以及 `VIBE_PROVISION_VM_USER`。
7. 执行脚本。此阶段中的失败会标记为 `[action N: ...]`，编号从 1 开始。
8. 随后，非 root 配置流程会以 root 身份运行 `check-vm-user-home`。若未指定 `--repl`，接下来的脚本将清空 `/etc/machine-id`。若指定了 `--repl`，该脚本会在 `/run` 下安装临时单元，以便在关机时清空该 ID。接着客体机输出 `VIBE_PROVISION_SUCCESS`。若未指定 `--repl`，宿主机输入 `systemctl poweroff`。这些步骤统一标记为 `[shutdown]`。

在非 root 环境下使用 `--repl` 时，会在出现成功标识后切换到 vm-user。输入 `exit` 返回 root，再次 `exit` 将关机并保存镜像。在交互式 Shell 打开期间，机器 ID 仍保持本次启动的值。

## 单个脚本的执行方式

`script_command`（`src/session.rs`）会将一个封装器输入到 root Shell 中。脚本主体经过 base64 编码，因此脚本中的引号不会破坏控制台命令行。控制台会显示该封装器。切勿在脚本中放置敏感信息，root 密码绝不会包含在文本中。

封装器会将脚本写入 `/tmp/vibe-scripts-<uid>/<id>.sh` 并执行。该目录的权限为 `700`，所有者为运行脚本的 UID。`<id>` 为 `script_` 加上其在完整登录操作列表中的索引。脚本应将状态保存在用户主目录或 `/etc` 下，而非 `/tmp` 中。

脚本进程的 stdin 绑定至 `/dev/null`。脚本启动的程序绝不能读取控制台。脚本内部的管道仍保留各自的 stdin。内置脚本统一采用 `curl | bash` 模式。

宿主机对单个脚本的等待时间最长为 24 小时（`SCRIPT_ACTION_TIMEOUT`）。封装器会输出 `VIBE_SCRIPT_STATUS_<id>` 及退出码。退出码为 0 则继续执行；非 0 则终止会话，且不会发布镜像。除非设置了 `VIBE_KEEP_FAILED_WORK=1`，否则临时工作副本 `<name>.tmp.<pid>` 会被删除。

每个脚本都在独立的新进程中运行。脚本内执行的 `export` 会在进程退出时失效。向后续脚本传递数据应通过文件实现。

执行每个脚本前，root Shell 会释放页面缓存（page cache）并整理压缩内存。脚本返回 0 退出后，若该 root Shell 中存在 `$HOME/.local/bin/mise`，则会运行 `mise activate bash`，该操作仅更新当前 root Shell。

root 脚本直接执行（`"$__vibe_script"`），它继承 root Shell 的环境（包括已 source 的 `/root/.profile`），不会再次读取 `.profile`。

用户脚本执行方式有所不同。root 用户切换至目标用户主目录，并运行 `runuser` 和 `bash --login -c 'exec ...'`。登录 Shell 会读取该用户的 `.profile`，然后替换为脚本进程。脚本继承导出的环境变量，不会再次读取 `.profile`。工作目录为该用户的主目录。此时会清除针对该进程的 root mise 变量（`MISE_SHELL`、`__MISE_DIFF`、`__MISE_ORIG_PATH`、`__MISE_SESSION`），并将 `HOME`、`USER`、`LOGNAME` 和 `SHELL` 设置为对应的 vm-user。

## `# vibe:run-as-user` 标记行

`RUN_AS_USER_MARKER` 指完全匹配的行：`# vibe:run-as-user`。行尾空格会被忽略，行首不能有任何空格。只以首次匹配作为唯一分割点；写在 `echo` 或其他注释中的相同文本不会触发分割。

仅当以下两项条件均满足时，宿主机才会对脚本进行拆分：

- `--vm-user` 不是 root
- 脚本为内置脚本：名称及字节内容与内置文件完全一致

路径脚本即便利用字节与内置脚本相同，也无法通过第二项检查。此时该标记仅作为普通注释，整个文件将以 root 身份运行。

针对非 root 用户的内置脚本，`split_run_as_user` 会将其拆分为两个脚本：

| 部分 | 内容 | 执行身份 |
| --- | --- | --- |
| Root 部分 | 标记行之前的字节内容。 | Root，且仅在该部分包含实际命令时执行（空行和注释不计入）。 |
| 用户部分 | 原始 `#!` 行，后接标记行之后的字节内容。标记行本身不包含在任一部分中。 | vm-user，通过 `runuser` 执行。 |

这两部分属于独立的进程，不共享函数或变量。因此，各内置脚本在标记行后都需要重新声明 `set -euxo pipefail` 和重试辅助函数。

若使用默认的 `--vm-user root`，则不进行拆分。文件作为一个整体以 root 身份运行一次（包含标记行），两部分共享同一进程，工具将安装在 `/root` 下。

拆分后的内置脚本会占用两个操作编号，标签分别为 `script 'name'` 和 `script 'name' as vm-user`。

`base` 和 `rust` 在标记前包含命令，因此非 root 配置会先后执行 root 部分和用户部分。其他内置脚本均将标记置于命令之前，因此会跳过 root 部分，仅运行用户部分。

脚本执行完毕后，非 root 配置会运行 `check-vm-user-home`。它会扫描该文件系统中 `$VIBE_VM_HOME` 下所有所有者为 UID 0 的路径（挂载点除外）。一旦发现此类路径，即终止发布，并在报错中最多输出 20 个相关路径。请务必在用户部分安装用户级工具，切勿以 root 身份向该主目录写入内容。

## 环境变量

在执行脚本之前，root Shell 中会导出以下变量，子进程将自动继承：

| 变量 | 设置时机 | 值 |
| --- | --- | --- |
| `VIBE_VM_USER` | 仅限非 root `--vm-user` | 账户名称 |
| `VIBE_VM_UID` | 仅限非 root `--vm-user` | 客体机中的 UID |
| `VIBE_VM_GID` | 仅限非 root `--vm-user` | 客体机中的 GID |
| `VIBE_VM_HOME` | 仅限非 root `--vm-user` | 从 `getent` 获取的主目录 |
| `VIBE_PROVISION_VM_USER` | 每次配置均设置 | `--vm-user`（包含 `root`） |
| `VIBE_PROVISION_IMAGE` | 每次配置均设置 | 镜像名称（文件主干名） |
| `VIBE_PROVISION_BASE` | 每次配置均设置 | 基础 raw 文件的宿主机路径 |
| `VIBE_GIT_SHA` | 每次配置均设置 | 编译进 `vibe` 的 Git SHA |
| `VIBE_BUILD_DATE` | 每次配置均设置 | 编译进 `vibe` 的 UTC 构建日期 |
| `VIBE_PROVISION_SCRIPTS` | 每次配置均设置 | 脚本名称列表，每行一个，`base` 居首 |

以 root 身份配置时不会设置 `VIBE_VM_USER` 和 `VIBE_VM_HOME`。`base.sh` 会通过 `${VIBE_VM_USER:-root}` 与 `${VIBE_VM_HOME:-/root}` 设置默认值，自定义脚本也应采取相同处理方式。

`src/bashrc.sh` 即为前文写入的 `.profile`。它负责导出 `PATH`（包含 cargo、bun、`~/.local/bin`），并在存在 `~/.local/bin/mise` 时激活 mise，同时配置 agent 变量（`IS_SANDBOX`、`GEMINI_SANDBOX`、遥测标记及该文件中的其他内容）。

## 共享目录

配置流程仅挂载缓存共享目录，不挂载项目目录，亦不挂载 agent 配置（`--mount-agents-conf` 仅限运行时使用）。

| 共享目录 | 客体机路径 | 宿主机目录 | 挂载条件（除非指定以下选项） |
| --- | --- | --- | --- |
| apt | `/var/cache/apt` | `<cache>/apt`（所有用户共享） | `--no-mount-sys-cache` |
| cargo, yarn, m2, go | `~/.cargo`, `~/.yarn`, `~/.m2`, `~/go` | `<cache>/user-<vm-user>/<name>` | `--no-mount-sys-cache` |
| mise | `~/.local/share/mise` | `<cache>/user-<vm-user>/mise-cache` | `--no-mount-mise` |

`~` 代表 `--vm-user` 的主目录。配置期间，apt 共享采用普通的 bind 挂载。后续以非 root 用户运行时，若镜像包含 `bindfs`，则会通过 `bindfs` 挂载。

使用 `--no-mount-sys-cache` 和 `--no-mount-mise` 会将这些工具直接写入镜像中。后续运行该镜像时需携带相同选项，否则挂载的共享目录会覆盖镜像内部的文件。

## 代理配置

在 `nat` 模式下，`vibe-usernet` 辅助程序代理客体机的 TCP 连接。此时 `guest_proxy` 为空，脚本不会接收到任何代理环境变量。

在 `vznat` 模式下，宿主机在拆分脚本前会填充 `base.sh` 中的 `INJECT_PROXY_CODE` 区域。替换内容会将 `Acquire::http::Proxy` 和 `Acquire::https::Proxy` 追加到 `/etc/apt/apt.conf.d/99timeout` 中，随后导出 `http_proxy`、`https_proxy`、`no_proxy` 及其大写形式。该区域位于 root 部分，导出的变量仅在当前进程中生效。apt 配置文件（包含 URL 中的凭证信息）会保存在镜像中。后续脚本以及 `base` 的用户部分不会继承这些环境变量。这些脚本中的 `curl` 和 `mise` 不会读取 apt 配置文件。

源文件中的该区域应保持为空，且必须保留在创建 `99timeout` 的代码行之后。`vznat` 配置会替换整段区域。

## 内置脚本规范

内置脚本需与 `provisioning/` 下的现有文件保持一致：

- 文件命名为 `provisioning/<name>.sh`，通过 `@<name>` 引用。
- 第 1 行必须是 bash shebang（`#!/bin/bash` 或 `#!/usr/bin/env bash`）。用户部分将以此行开头启动。
- 第 2 行格式为 `# 简短描述`。该文本将作为帮助信息展示。
- `# vibe:run-as-user` 必须独占一行。所有内置脚本均包含此行。
- 将 `apt` 及其他系统级设置置于标记行之前。
- 将 mise、rustup 及其他用户级工具置于标记行之后。
- 用户部分需以 `set -euxo pipefail` 及其独立的重试辅助函数开头。测试项 `builtin_scripts_install_user_tools_as_the_vm_user` 依赖该 `set` 行，且禁止在用户部分出现 `apt ` 文本。
- 重试逻辑：先执行命令，失败后最多重试 3 次，每次间隔 `sleep 10`，若仍失败则执行 `exit 1`。单条命令使用 `exec_retry`；管道命令使用 `while retryable "$?"; do ...; done`。确保已开启 `pipefail`。
- 以对应用户的身份写入文件，路径位于 `$HOME` 或当前工作目录下（用户部分默认在用户主目录中启动）。
- 禁止读取 stdin。
- 发生错误时以非 0 状态退出，以标记配置失败。
- 使用 2 个空格缩进。`scripts/format.sh` 会通过 `shfmt --indent 2` 格式化 `scripts/` 目录，`provisioning/` 应保持相同的缩进规范。`scripts/test.sh` 不会扫描 `provisioning/`。

路径脚本遵循相同的客体机封装机制，但有以下三点区别：标记行不会触发拆分；始终以 root 身份运行；如需以 vm-user 身份执行命令，请调用 `runuser -u "${VIBE_VM_USER:-root}"`。

## `base.sh` 的作用

`base` 先于所有其他脚本运行。后续脚本均默认其已完成镜像的初始化准备。

每次运行时，它都会向 `/root/vibe-image-history.txt` 追加一段记录（包括时间、镜像、基础路径、Git SHA、构建日期、vm-user、脚本名称列表）。

若该文件已非空，root 部分将在执行软件包安装前直接返回。若当前 vm-user 缺少 `~/.local/state/vibe/base-user.done` 且不存在可执行的 `~/.local/bin/mise`，则会打印警告。该步骤不会创建 `/run/vibe/base-first-run`。若 `--vm-user` 为 `root`，此处的 `exit` 会终止进程，标记行后面的内容不会执行；若为其他用户，用户部分作为第二个脚本，会因缺少标识文件而直接退出。先前配置保留的工具保持不变。在旧镜像上创建的新用户无法获得这些工具。如需为新用户配置环境，请基于干净的基础镜像重新创建：`vibe provision --image <new-name> --vm-user <name> --root-password <password>`。

首次运行时，root 部分会执行以下操作：

- 在 `/etc/apt/apt.conf.d/99timeout` 中设置较短的 apt 超时时间
- 添加 Debian Fast Forward 源并执行更新
- 安装软件包（`build-essential`、`curl`、`git`、`ripgrep`、`vim`、`tmux`、`openssh-server` 及文件中的其他依赖）
- 禁用 `ssh` 服务，允许 root 登录，并禁用密码登录
- 执行 `growpart /dev/vda 1` 与 `resize2fs /dev/vda1`（若复制的文件较小，宿主机已预先将其扩容至 12 GiB）
- 将主机名设置为 `vibe`
- 为本次启动创建 `/run/vibe/base-first-run`（`/run` 不会保留在发布的镜像中）

除非 `/run/vibe/base-first-run` 存在且 `~/.local/state/vibe/base-user.done` 不存在，否则用户部分将直接返回。在满足条件的情况下，它会安装 mise，生成 `~/.config/mise/config.toml`（包含 `uv` 0.11.3、`fzf`、`node` 24、`usage`），运行 `mise install`，并创建完成标记文件。

## 其他内置脚本

帮助信息为各文件的第二行内容。“用户部分”指非 root 配置下执行的内容。root 配置会将整个文件作为 root 运行。

| `@name` | Root 部分 | 用户部分 |
| --- | --- | --- |
| `rust` | `apt install sccache` | rustup（最小安装版，包含 `rustfmt` 和 `clippy`） |
| `golang` | 无 | `mise use -g go@latest` |
| `claude` | 无 | mise 工具 `claude` |
| `codex` | 无 | `mise use -g npm:@openai/codex@latest` |
| `copilot` | 无 | GitHub Copilot 安装脚本 |
| `gemini` | 无 | Antigravity 安装脚本 |
| `grok` | 无 | xAI CLI 安装脚本 |
| `pi` | 无 | `mise use -g npm:@earendil-works/pi-coding-agent@latest` |

这些用户部分均会调用 `mise` 或 `curl`，它们依赖于 `base` 在首次运行用户部分时所做的初始化工作。
