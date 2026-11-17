# 抽离 provision 脚本的重试工具到 `include.sh`

本文是实现计划，不是已完成功能的说明。基于 2026-11-17 工作区（`HEAD` 为 `35a85c9`）。本文只新增计划文档，不改产品代码。

## 目标

`provisioning/*.sh` 每个文件都复制了一份 `retry_error`、`retryable`、`exec_retry`。`base.sh` 和 `rust.sh` 在 `# vibe:run-as-user` 前后各复制一份，共 11 份。

本计划把这组函数放进一个 `include.sh`。provision 时 host 把它写进会话目录。脚本用 `source "$VIBE_PROVISION_INCLUDE"` 引入。built-in 脚本和用户脚本都能用。

## 方向决定（Q&A）

用户对 Q1 到 Q4 都选了方案 A。实现时按下列决定执行。

### Q1. `VIBE_PROVISION_INCLUDE` 指向哪个文件？

答：指向 `/tmp/vibe-scripts-<uid>/include.sh`，即脚本副本的同一目录。

- host 把 `include.sh` 写进会话目录 `scripts/include.sh`。
- 包装器 `src/guest/script_wrapper.sh` 每次执行脚本前，把它复制到 `/tmp/vibe-scripts-<uid>/`。复制沿用现有防符号链接检查。
- 变量只传给该脚本进程。root 和 vm-user 都能读。

理由：会话目录在 `/mnt/root` 下，权限 0700，vm-user 读不到。脚本也不在会话目录中执行，而是执行 `/tmp` 中的副本。

### Q2. 文档中“同目录引入”怎样写？

答：写 `source "$(dirname "$0")/include.sh"`，即“脚本所在目录”语义。不支持字面的 `source ./include.sh`。

理由：`./` 相对 cwd。root 脚本的 cwd 是 `/root`。vm-user 脚本的 cwd 是 `$VIBE_VM_HOME`，`claude.sh` 依赖这一点，不能改。本地测试时，用户把一份 `include.sh` 放在脚本旁边即可。

### Q3. `vibe run --script` 是否也提供 `include.sh`？

答：不提供。只覆盖 `vibe provision`（built-in 脚本和用户传入的路径脚本）。

理由：变量名含 `PROVISION`，范围一致。`run` 不写 `scripts/include.sh`，包装器找不到文件就不设变量，因此不用改 `run` 路径。

### Q4. 是否顺带修复“重试从不生效”的缺陷？

答：修复。

- 现状：`set -e` 下循环体命令失败就结束脚本，`while retryable "$?"` 从不进入第二轮。本机 bash 3.2 和 bash 5 都已复现。
- `exec_retry` 改为在函数内捕获退出码（`"$@" || status=$?`），不依赖 `$?` 和全局 `retry_error`。
- 管道写法改为 `exec_retry bash -o pipefail -c 'curl ... | sh'`。
- 删除 `retryable` 和 `retry_error`。
- 重试次数（3 次）、间隔（10 秒）、提示文字和最终 `exit 1` 不变。

## 已定假设

下列条目是本文补的决定。用户可改。

### 源文件位置

`build.rs` 把 `provisioning/` 下每个 `*.sh` 嵌入为 built-in 脚本。若源文件放在 `provisioning/include.sh`，它会变成 `@include`，并出现在帮助文本中。

决定：源文件放在 `src/guest/provision_include.sh`。`src/provision.rs` 用 `include_str!` 嵌入。文件没有 `INJECT_*` 区块，Rust 原样写出。会话目录中的文件名固定为 `scripts/include.sh`。`stage_scripts` 的文件名都带 `NNN-` 前缀，不会冲突。

### 包装器怎样找到 `include.sh`

决定：包装器从 `__vibe_script_src` 推出路径：`${__vibe_script_src%/*}/include.sh`。文件存在才复制和导出变量。Rust 的 `LoginAction` 和 `script_command` 不加新参数。

理由：只有 provision 写这个文件（见 Q3），所以 run 自动不受影响。内联脚本（`__vibe_script_src` 为空）和维护 VM 也不受影响。备选：给 `Script`/`UserScript` 加 `include: Option<GuestFile>` 字段。要改的构造点较多，暂不采用。

### 变量未设置时

决定：built-in 脚本写 `source "${VIBE_PROVISION_INCLUDE:?run this script with vibe provision}"`。在 vibe 之外误跑时，`set -u` 之前就给出明确错误。用户脚本的写法由用户决定，文档给同样的示例。

### root 模式下重复 `source`

`--vm-user root` 时 built-in 不拆分，整个文件在一个进程中运行。marker 前后各有一次 `source`。重复 `source` 只是重新定义函数，无副作用。保留两次，因为非 root 时两部分是两个进程。

## 任务

下列任务按上文 Q&A 的决定编写。

### T1. 新增 `src/guest/provision_include.sh` ✅ 已完成 (Completed)

- 首行 `#!/usr/bin/env bash` 和 `# shellcheck shell=bash`。注释说明：本文件被 `source`，不要 `set -e`，不要在顶层 `exit`。
- 按 Q4 的决定定义 `exec_retry`：

  ```bash
  exec_retry() {
    local attempt=0 status
    while :; do
      status=0
      "$@" || status=$?
      [ "$status" -eq 0 ] && return 0
      attempt=$((attempt + 1))
      if [ "$attempt" -gt 3 ]; then
        echo "Error: too many errors and retry attempts, exiting ..." >&2
        exit 1
      fi
      echo "Warn:  waiting 10s and try again (error=${attempt}) ..." >&2
      sleep 10
    done
  }
  ```

- 注意：`"$@" || ...` 会让被调用的 shell 函数内部失去 `errexit`。只用于外部命令。文档写明这一点。

### T2. provision 把 `include.sh` 写进会话目录 ✅ 已完成 (Completed)

- `src/provision.rs`：新增 `const PROVISION_INCLUDE_SCRIPT: &str = include_str!("guest/provision_include.sh");`。
- `build_provision_plan` 在写 `provision-env.sh` 旁边调用 `session.write("scripts/include.sh", PROVISION_INCLUDE_SCRIPT)`。错误前缀 `[prepare] cannot write scripts/include.sh into the session directory: ...`。
- 更新该函数的文档注释，加上 `scripts/include.sh`。
- `provision_image` 总把 `base` 放进脚本列表，所以每次 provision 都会写这个文件。

### T3. 包装器复制 `include.sh` 并导出变量 ✅ 已完成 (Completed)

文件 `src/guest/script_wrapper.sh`：

- 在复制脚本之后，计算 `__vibe_include_src="${__vibe_script_src%/*}/include.sh"`。仅当 `__vibe_script_src` 非空且该文件存在时继续。
- root 分支：`cat -- "$__vibe_include_src" >"$__vibe_script_dir/include.sh"`。
- vm-user 分支：root 读源文件，经 `runuser -u "$VIBE_VM_USER" -- sh -c 'cat >"$1"' sh "$__vibe_script_dir/include.sh"` 写入。与写脚本副本用同一套目录检查。
- 复制失败时报 `Vibe: cannot write include.sh into $__vibe_script_dir`，状态为 1，不执行脚本。
- 执行脚本时只给该进程设置 `VIBE_PROVISION_INCLUDE="$__vibe_script_dir/include.sh"`。root 分支用前缀赋值。vm-user 分支加进现有 `env ... HOME=... USER=...` 参数列表。root shell 本身不留这个变量。
- 末尾 `unset` 加上新变量。`assert_clean_shell` 会检查残留的 `__vibe_*`。
- 更新文件头注释。

`src/session/shell.rs` 的 `script_command` 不变（见“已定假设”）。

### T4. 改写 built-in 脚本 ✅ 已完成 (Completed)

文件：`provisioning/{base,rust,claude,codex,copilot,gemini,golang,grok,pi}.sh`。

- 删除每份 `retry_error=`、`retryable()`、`exec_retry()` 定义。
- 在原位置写 `source "${VIBE_PROVISION_INCLUDE:?run this script with vibe provision}"`。`base.sh`、`rust.sh` 在 marker 前后各写一次。加 `# shellcheck source=/dev/null`，或沿用 `rust.sh` 顶部已有的 `disable=SC1091`。
- 按 Q4 的决定改写管道调用：
  - `base.sh`：`curl https://mise.run | sh`
  - `rust.sh`：rustup 安装命令
  - `grok.sh`、`copilot.sh`、`gemini.sh`：`curl -fsSL ... | bash`

  统一为 `exec_retry bash -o pipefail -c '<原管道>'`。
- 保持 `set -euxo pipefail` 在 user 部分开头。测试 `builtin_scripts_install_user_tools_as_the_vm_user` 依赖它。
- 第 2 行描述不变（它是帮助文本）。`USAGE.md` 的 help 块无需更新。

### T5. 测试 ✅ 已完成 (Completed)

只加能在缺少本改动时失败的用例。

- `src/session/shell.rs` 的包装器 host 测试（`script_wrapper_root_branch_runs_a_file_and_reports_its_status` 附近）：
  - 会话目录中有 `scripts/include.sh` 时，脚本能 `source "$VIBE_PROVISION_INCLUDE"` 并调用其中的函数。变量值是 `<dir_base>-<uid>/include.sh`。root shell 中没有残留的 `VIBE_PROVISION_INCLUDE` 和 `__vibe_*`。
  - 没有 `include.sh`（内联脚本）时，脚本中 `VIBE_PROVISION_INCLUDE` 未设置。
- `exec_retry` 行为（Q4）：一个 host 测试，用 bash 执行 `set -euo pipefail; source <include>; exec_retry <前两次失败的命令>`。把 `sleep` 用 PATH 中的假 `sleep` 替换，避免等待。断言第三次成功、脚本继续。另测 4 次失败后退出码为 1。放在 `src/provision.rs` 的测试模块。
- `src/provision.rs`：在现有 `builtin_scripts_install_user_tools_as_the_vm_user` 循环中加一条断言：每个 user 部分含 `source "${VIBE_PROVISION_INCLUDE`，且不含 `retryable()`。不新建测试。
- 不测试常量和 `include_str!`。

### T6. 文档和变更记录 ✅ 已完成 (Completed)

- `docs/how-provisioning-scripts-work.md` 和 `.zh-Hans.md`：
  - “How one script is executed”：写明包装器把 `include.sh` 复制到脚本副本同目录。
  - “Environment” 表：加一行 `VIBE_PROVISION_INCLUDE`，“Every provision script”，值为 `/tmp/vibe-scripts-<uid>/include.sh`。说明它只存在于脚本进程。
  - “The `# vibe:run-as-user` line”：把“Each built-in therefore repeats ... the retry helpers”改为“sources `include.sh` again”。
  - “Rules for a built-in script”：改写两条重试规则。给出 `exec_retry cmd` 和 `exec_retry bash -o pipefail -c '...'`。说明本地测试可用 `source "$(dirname "$0")/include.sh"`。
- `USAGE.md` 和 `.zh-Hans.md` 第 82 行附近（自定义脚本以 root 运行）：加一句，自定义脚本可 `source "$VIBE_PROVISION_INCLUDE"` 使用 `exec_retry`。
- `CHANGELOG.md` 和 `.zh-Hans.md`，`[1.0.0] - Unreleased`：
  - `Added`：provision 脚本可 `source "$VIBE_PROVISION_INCLUDE"` 获得 `exec_retry`。
  - `Fixed`（Q4）：built-in 脚本的网络重试在 `set -e` 下从不生效，现已生效。
- `AGENTS.md` 和 `.zh-Hans.md` 模块表：`src/provision.rs` 一行加上 `build_provision_plan` 写 `scripts/include.sh`（源 `src/guest/provision_include.sh`）。
- `readme.md` 不改。

### T7. 验证 ✅ 已完成 (Completed)

host 上：

- `cargo build --locked`、`cargo test`、`cargo fmt --check`、`cargo clippy`。
- `shellcheck` 和 `shfmt -d` 检查 `src/guest/provision_include.sh`、`src/guest/script_wrapper.sh` 和改过的 `provisioning/*.sh`。版本见 `mise.toml`。

VM 中（需要 ARM Mac host）：

- 使用 `~/.cache/vibe` 中已有的镜像，不重新下载。
- guest 网络必须走 `socks5://192.168.1.88:3003`：加 `--proxy socks5://192.168.1.88:3003 --proxy-udp`。host 侧的 cargo 等请求仍按系统代理变量。
- 场景 1（root）：`vibe provision --image t-inc-root --proxy ... --proxy-udp @golang ./probe.sh`。`probe.sh` 内 `source "$VIBE_PROVISION_INCLUDE"`，然后 `exec_retry true`。预期成功。
- 场景 2（非 root）：`vibe provision --image t-inc-dev --vm-user dev --root-password ... --proxy ... --proxy-udp @rust @grok`。预期 `base`、`rust` 的 user 部分都能 `source`。
- 场景 3（重试）：`probe.sh` 中 `exec_retry` 一个前两次失败的命令（用计数文件）。日志中应出现两次 `Warn:  waiting 10s`，然后成功。
- 结束后删除 `t-inc-*` 镜像。

#### 验收结果（2026-11-18）

全部通过。未重新下载镜像。

- host：`cargo build --locked` 通过。`cargo test` 198 项通过。`cargo fmt --check`、`cargo clippy --all-targets` 无输出。
- host：`shellcheck` 0.11.0 和 `shfmt` 最新版检查上述脚本，无问题。
- guest 网络：加 `--dns 1.1.1.1 --dns 8.8.8.8`。不加时，guest DNS 用 host 的链路本地解析器 `fe80::…%bridge0`，经代理无法解析 `deb.debian.org`。这两个上游不在 bypass 列表，DNS 仍走 `socks5://192.168.1.88:3003`。
- 场景 1 和 3（同一 VM）：`vibe provision --image t-inc-root --proxy socks5://192.168.1.88:3003 --proxy-udp --dns 1.1.1.1 --dns 8.8.8.8 @golang ./probe.sh`，退出码 0。
  - `base.sh`、`golang.sh` 都 `source /tmp/vibe-scripts-0/include.sh`。`exec_retry bash -o pipefail -c 'curl https://mise.run | sh'` 成功。
  - probe 输出 `VIBE_PROVISION_INCLUDE=/tmp/vibe-scripts-0/include.sh`。`exec_retry true` 成功。
  - 计数命令：两行 `Warn:  waiting 10s and try again (error=1|2) ...`，第 3 次成功，脚本继续。
- 场景 2：`vibe provision --image t-inc-dev --vm-user dev --root-password ... --proxy ... --proxy-udp --dns ... @rust @grok ./probe.sh`，退出码 0。
  - `base.sh`、`rust.sh` 的 root 部分 `source /tmp/vibe-scripts-0/include.sh`。user 部分 `source /tmp/vibe-scripts-1000/include.sh`。
  - rustup 和 grok 的 `exec_retry bash -o pipefail -c '...'` 都成功。
- 两个 `t-inc-*` 镜像和上次中断留下的 `t-inc-root.tmp.*` 已删除。

probe 脚本：

```bash
#!/usr/bin/env bash
set -euo pipefail
echo "PROBE: VIBE_PROVISION_INCLUDE=${VIBE_PROVISION_INCLUDE:-<unset>}"
source "$VIBE_PROVISION_INCLUDE"
exec_retry true
counter=$(mktemp)
echo 0 >"$counter"
exec_retry bash -c 'n=$(($(cat "$1") + 1)); echo "$n" >"$1"; echo "PROBE: attempt $n"; [ "$n" -ge 3 ]' _ "$counter"
echo "PROBE: done"
```

## 验收后的清理 (Cleanup after Acceptance)

无需清理。T5 新增的测试守护包装器和 `exec_retry` 的核心行为，保留。

## 不在范围内

- 不把 `include.sh` 留在已发布的镜像中。
- 不加可配置的重试次数或间隔。
- 维护 VM（Alpine）和 `src/vm_checks/` 不用 `include.sh`。
