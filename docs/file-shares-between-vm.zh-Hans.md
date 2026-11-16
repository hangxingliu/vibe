# Mac 与虚拟机之间的文件共享

本文档介绍了 Vibe 如何与虚拟机（VM）共享宿主机目录、权限由谁决定，以及哪些部分不构成安全边界。
文档描述了当前的行为。相关测试数据是在搭载 Debian 13 客户机（内核 `6.12.116+deb13-arm64`）的 ARM Mac 上测得的。

## 1. 共享的建立方式

### 宿主机端

- `run_main`（`src/main.rs`）收集 `DirectoryShare` 值：项目目录、`--mount` 条目、各缓存、`node_modules` 以及只读的 `.git`。
- 每个共享都会获得一个标签 `<basename>_<hash>`（`DirectoryShare::tag`，宿主机路径的 djb2 哈希值）。
- `default_interactive_spec`（`src/vm_config.rs`）将所有共享放入一个标签为 `shared` 的 VirtioFS 设备中。该设备包含一个 `VZMultipleDirectoryShare`。每个条目都是一个 `VZSharedDirectory`，其 `readOnly` 属性根据共享设置。
- 只读限制由宿主机强制执行。客户机无法关闭它，即使是 root 用户也不行。

### 客户机端

会话计划（`src/session_plan.rs`）在运行任何用户操作之前，以 root 身份挂载所有内容：

1. `mkdir -p /mnt/root/virtiofs-shared && chmod -R 700 /mnt/root && mount -t virtiofs shared /mnt/root/virtiofs-shared`。
   这是暂存目录。挂载前 `/mnt/root` 的权限为 `0700`，因此只有 root 用户可以访问。
2. 对于每个共享，执行 `mount --bind /mnt/root/virtiofs-shared/<tag> <target>`。
   客户机中的相对路径位于 `--vm-user` 的 HOME 目录下（root 为 `/root`，其他用户为 `/home/<name>`）。非 root 用户 HOME 目录下缺失的目录将通过 `install -d -o "$VIBE_VM_UID" -g "$VIBE_VM_GID"` 创建，确保归该用户所有。
3. 掩码：`mount -t tmpfs -o mode=0555,uid=0,gid=0 tmpfs <path>`。掩码是一个归 root 所有的空目录，其他任何人都无法写入。
4. 对于非 root 的 `--vm-user`，apt 缓存使用 `bindfs` 挂载，而不是普通的 bind 挂载（见下文）。
5. 只有在完成上述操作后，会话才会通过 `su --login --pty` 切换到目标用户。

### 共享内容

| 宿主机 | 客户机 | 模式 |
| --- | --- | --- |
| 当前目录 | `~/<project>` | 读写 |
| `--mount HOST:GUEST[:MODE]` | `GUEST`（相对于 `~`） | 按指定设置，默认读写 |
| `<project>/.vibe` | 使用 tmpfs 掩码遮盖 | 只读、空目录 |
| `<project>/.git` | `--git ro`（默认）：独立的只读共享；`--git rw`：作为项目共享的一部分；`--git no`：遮盖 | 如左所述 |
| `<cache>/guest_node_modules/<tag>` | `<project>/node_modules`，仅在宿主机存在 `node_modules/` 时 | 读写 |
| `<cache>/apt` | `/var/cache/apt`，由所有 `--vm-user` 共享 | root 为读写；非 root 用户通过 `bindfs` 设为只读 |
| `<cache>/user-<name>/{cargo,mise-cache,yarn,m2,go}` | `~/.cargo`、`~/.local/share/mise`、`~/.yarn`、`~/.m2`、`~/go` | 读写 |
| `<cache>/user-<name>/<agent>`（`--mount-agents-conf`） | 该 agent 在 `~` 下的配置目录 | 读写 |
| `~/.tmux`（`--mount-tmux-conf`） | `~/.tmux` | 只读 |

`<cache>` 为 `~/.cache/vibe`。`~` 为 `--vm-user` 的 HOME 目录。
`.env` 和其他文件不予遮盖（见第 4 节）。

### 非 root 用户的 apt 缓存

root 用户的 apt 会写入共享的 apt 缓存。普通用户绝不能修改它，否则可能会替换掉 root 后续安装的 `.deb` 包。
因此，非 root 会话通过以下命令挂载它：

    bindfs -o nonempty -u "$(id -u _apt)" -g 0 -p 0755,af-x <staging>/<apt tag> /var/cache/apt/

- 文件所有者显示为 `_apt:root`，目录权限为 `0755`，文件权限为 `0644`。用户可读，但无法写入、删除、`chmod` 或卸载。
- `_apt` 可以写入 `archives/partial`，且 root 的 `apt-get` 能够正常工作。下载的文件会保存在宿主机缓存中。
- 需要 `-o nonempty` 参数，因为镜像中的 `/var/cache/apt` 不是空的。
- 初始化配置会安装 `bindfs`（同时也会引入 `fuse3` 和 `libfuse2t64`）。
- 当缺少 `bindfs` 或 `_apt` 时，Vibe 会回退到可写的 bind 挂载并打印警告。请重新配置新镜像，或使用 `--no-mount-sys-cache` 运行。
- 当 `bindfs` 进程终止时，`/var/cache/apt` 会变成 `Transport endpoint is not connected`（传输端点尚未连接）。apt 会失败，而不会回退到可写共享。

## 2. 权限模型

当 root 以外的客户机用户向共享写入时，可能出现三种结果：

- R1：任何客户机 UID 都可以写入可写共享。
- R2：只有与宿主机 UID 相等的客户机 UID 才能写入。
- R3：通过 ID 映射挂载（`X-mount.idmap`）将客户机 UID 映射到宿主机 UID。

测试测得的结果为：R1。R2 不成立，且 R3 不可用。

### 访问权限的决定机制

- 访问权限由宿主机决定。Apple 的 VirtioFS 服务以运行 `vibe` 的用户身份打开宿主机文件。
  - 宿主机上权限为 `0555` 的目录会对所有客户机 UID（包括 root）返回 `EACCES`。
  - 宿主机上权限为 `0700` 的目录或 `0600` 的文件，对所有客户机 UID 都是可读写的。
  - 新创建的文件属于你在 Mac 上的用户。
- 只读共享会对所有客户机 UID（包括 root）返回 `EPERM`（而非 `EROFS`）。
- 客户机内核也会检查权限，但在实践中不会对任何人施加限制：
  - `virtio_fs_fill_super` 始终设置 `default_permissions` 和 `allow_other`。`/proc/self/mountinfo` 中不会显示它们；`fuse_show_options` 仅针对普通 FUSE 挂载打印它们。
  - `fuse_permission` 会调用 `generic_permission`。当遇到未及时刷新的属性导致 `EACCES` 时，它会以调用者身份重新获取属性并再次检查。
  - 服务端会将调用者的 UID 和 GID 报告为所有者，因此第二次检查会通过。

### 客户机视角下的所有权

- 共享文件的所有者显示为最后查询该文件的用户。属性会被缓存约 1 秒。
  - root 运行 `stat`，文件显示为 `0:0`。一秒后，`dev` 运行 `stat`，文件显示为 `dev:dev`。此时 root 看到的也是 `dev:dev`。
- 在客户机中运行 `chmod` 会更改宿主机文件的权限位。
- `chown` 会返回成功，但实际上没有任何改变。
- 挂载选项无法改变这一行为：
  - `mount -t virtiofs -o uid=1000,gid=1000` 会失败：提示 `virtiofs: Unknown parameter 'uid'`。virtiofs 仅接受 `dax`。
  - `mount --bind -o uid=...` 可以成功执行，但会忽略该选项。
  - `X-mount.idmap` 失败：提示 `cannot set ID-mapping: Invalid argument`。在 tmpfs 上执行相同的挂载则可以生效。

### 结果影响

- `--vm-user` 账户使用默认的 Debian UID（通过 `useradd` 创建，`UID_MIN` 为 `1000`）。宿主机 UID 不会传递给客户机，也没有任何机制来映射 UID。
- 虚拟机中任何能访问到挂载点的进程，都可以写入所有可写共享，并读取所有只读共享。
- 客户机用户之间的隔离仅依赖于：
  - 客户机本地目录：暂存目录（`/mnt/root`，`0700`）和用户的 HOME 目录（`0700`）；
  - 只读共享；
  - 掩码；
  - 针对 apt 缓存的 `bindfs`。

## 3. 常见问题

**为什么 `ls -l` 每次显示的所有者都不一样？**
显示的所有者是最后访问该文件的调用者，该信息会被缓存约 1 秒。这并不影响实际的访问权限。

**为什么 git 会提示 `detected dubious ownership`（检测到可疑的所有权）？**
Git 会将仓库所有者与当前用户进行对比。如果在不到一秒前 root 访问过该仓库，`dev` 就会看到所有者是 root。一秒后重试即可恢复正常。
Vibe 不会修改 git 的配置。如果这造成困扰，可以在虚拟机中设置 `safe.directory`。
其他会检查所有权的工具（如 ssh 的 `StrictModes`、gpg 的家目录检查）在共享目录中的文件上也可能遇到同样的问题。

**为什么 `chown` 没有任何效果？**
服务端保持宿主机文件始终归你的 Mac 用户所有。客户机只能将调用者视为所有者。

**为什么 root 不能写入只读共享？**
宿主机会拒绝写入操作（返回 `EPERM`）。客户机中的 root 对宿主机没有任何控制权。

**为什么我的用户对 `/var/cache/apt` 只有只读权限？**
它是与所有镜像和运行共享的，并且 root 会从中安装软件包。请参见“非 root 用户的 apt 缓存”。
若出现警告 `bindfs is not available; the shared apt cache is writable by every user in this VM`，意味着镜像中没有安装 `bindfs`。请重新配置新镜像。

**为什么 apt 会报错 `Could not open file /var/cache/apt/archives/partial/... (13: Permission denied)`？**
该现象出现在使用空 apt 缓存的基础镜像进行配置时。两次运行中，大约 180 个下载任务各丢失了一个。第三次运行时大部分软件包已被缓存，运行成功。
apt 以 `_apt` 身份下载到 `archives/partial`（`0700`），而 root 进程同时也在查看同一目录。可能的原因是：客户机看到的所有者在不同调用者之间发生切换，导致权限检查遭遇竞态条件而失败。
重新运行配置即可。每次运行都会保留已下载的软件包，从而减少参与竞态的下载数量。

## 4. 掩码及其局限性

在当前布局下使用非 root 用户（UID `1000`）测得：

- 无法访问暂存目录：`ls`、`stat`、`/proc/1/root/...`、指向暂存目录的软链接以及 `unshare -Urm`（列出、新挂载 virtiofs、卸载）均会失败。`/proc/self/mountinfo` 仍然会显示这些路径和标签。
- 掩码无法被移除：`umount` 提示 `must be superuser to unmount`（必须是超级用户才能卸载）。`rm` 和 `mv` 会返回 `EBUSY`。
- 在 `unshare -Urm` 内部，卸载掩码会失败，对项目的非递归绑定会失败（`EINVAL`），而 `--rbind` 会连同掩码一起复制。
- 只读的 `.git` 无法被写入或卸载。

### 大小写不敏感的宿主机卷

默认的 APFS 卷是大小写不敏感的，VirtioFS 服务会遵循宿主机的规则来查找名称。

- 文件掩码可以被绕过。Vibe 过去曾尝试用空文件遮盖 `.env`。但随后无论是 root 还是其他用户，运行 `cat ~/proj/.ENV` 依然能打印出宿主机的 `.env` 内容。运行 `ln .ENV leak` 会在宿主机上创建一个硬链接。
- 客户机无法弥补这一漏洞：
  - 一个由 n 个字母组成的名称具有 2^n 个大小写变体，此外还有 Unicode 规范化变体；
  - `VZSharedDirectory` 无法排除特定路径；
  - Linux 的大小写折叠功能仅在 ext4 和 f2fs 上可用。
- 因此，Vibe 不再遮盖 `.env` 或任何其他文件。**请将机密数据保存在项目目录之外。** 虚拟机中的 agent 可以读取共享中的所有文件。
- 目录掩码未被绕过：`.GIT` 和 `.Git` 会访问到只读的 `.git` 共享；`.VIBE` 和 `NODE_MODULES` 会返回 `ENOENT`。
  可能的原因是：Linux dcache 允许每个目录只有一个别名（`d_splice_alias`），但每个文件可以有多个（硬链接）。这是内核的实现细节，而不是一种规范约定。下方的 `vm-user` 集成检查对此进行了测试。

## 5. 复现与回归测试

### 集成检查

`tests/vm_user.rs` 通过 `vibe` 二进制文件运行 `src/vm_checks/vm_user.rs`。该检查会从 `VIBE_VM_CHECK_DISK` 的克隆中配置一个 `dev` 镜像并运行它。覆盖的测试项包括：

- 向项目共享、可写 `--mount` 和用户缓存写入数据（R1）；
- 向只读共享写入失败；
- 暂存目录不可访问；
- `.git` 及其大小写变体不可写入；
- `.vibe` 是一个 root 所有的只读空目录；
- `.env` 读取的内容与宿主机一致；
- apt 缓存为 `fuse.bindfs` 且对 `dev` 只读，同时 root 的 apt 能够正常工作。

运行命令：

    VIBE_VM_CHECK_DISK=~/.cache/vibe/default.raw \
    VIBE_VM_CHECK_ARGS="--proxy <proxy> --dns 1.1.1.1" \
    cargo test --release --test vm_user -- --ignored --test-threads=1

当 macOS 更新对 VirtioFS 产生改动时，R1 写入检查和只读检查通常会最先失败。此时可重新运行下方的探测脚本以查看具体变化。

### 探测脚本

`docs/ai-plans/references/` 包含三组宿主机/客户机脚本。每个宿主机脚本（`*-run.sh`）接受 `[vibe binary] [image]` 参数，在 APFS 克隆上运行，并在结束时删除其临时目录。虚拟机的代理配置来自 `VIBE_PROBE_PROXY`。

| 脚本 | 检查项 |
| --- | --- |
| `20261112-vm-user-virtiofs-probe{,-run}.sh` | 所有者和模式，UID 0、1000、宿主机 UID、1001 的写入情况；只读共享；`chmod`/`chown`；`X-mount.idmap`；`unshare` |
| `20261114-vm-user-virtiofs-uid-probe{,-run}.sh` | `uid=`/`gid=` 和 FUSE 挂载选项，bind 挂载选项，属性缓存时效，git 所有权 |
| `20261116-vm-user-q4-layout-probe{,-run}.sh` | 当前布局：暂存、bind 挂载、掩码、大小写变体，apt 的 `bindfs` |

其他 UID 使用 `setpriv --reuid --regid --clear-groups` 切换进入，因此无需额外创建账户。

## 6. 参考资料

- Linux v6.12：`fs/fuse/virtio_fs.c`（`virtio_fs_parameters`、`virtio_fs_parse_param`、`virtio_fs_fill_super`），`fs/fuse/inode.c`（`fuse_show_options`），`fs/fuse/dir.c`（`fuse_permission`）。
- Vibe：`src/main.rs`（`DirectoryShare`、`CacheShare`、`project_masks_and_shares`），`src/vm_config.rs`（VirtioFS 设备），`src/session_plan.rs`（暂存、bind 挂载、掩码、`bindfs`）。
- 启动顺序和缓存目录布局：[how-vm-boot.md](how-vm-boot.md)。
