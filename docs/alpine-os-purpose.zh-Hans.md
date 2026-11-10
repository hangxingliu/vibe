# Alpine 维护镜像

`src/alpine.rs` 固定版本、下载并校验一个官方 Alpine 云磁盘。该磁盘作为辅助工具使用。Vibe 绝不会发布它，绝不会将其作为用户虚拟机启动，也绝不会编辑其缓存副本。

该辅助镜像的存在是为了让 `vibe provision` 能够使当前的 Debian nocloud 镜像像旧版本镜像一样登录：拥有一个空的 `/etc/machine-id` 和一个空的 root 密码字段。交互式路径（`vibe`、`vibe path/to/disk.raw`、`.vibe/instance.raw`、`vibe --image`）既不会下载此镜像，也不会打开它。

`seed.iso`（构建方式、`cidata` 卷名以及三磁盘挂载配置）已在 [How a VM boots](how-vm-boot.md) 的“Maintenance VM”小节以及 `src/seed.rs` 中进行了说明。本文档仅说明 Alpine 客户机所依赖的约定规范。

## 选用此镜像的原因

2026 年底以后的 Debian nocloud 镜像（包括锁定的 `debian-13-nocloud-arm64-20261001-2618`）附带锁定的 root 密码字段，并在交互式首次启动向导中暂停。串口终端保持静默，因此传统的登录方式（等待 `login:`、发送 `root`、期望免密）永远无法进入 shell。

Vibe 不会驱动该向导，不会对目标执行 `chroot`，也不会运行目标磁盘中的任何程序。它需要第二个 Linux 系统，满足：

- 无需密码提示即可从自身的 EFI 固件启动
- 已内置 `e2fsck`、`sgdisk`、`mount` 和 `findfs`（busybox），因此运行过程无需网络和 `apk add`
- 启动脚本时无需 getty、无需 PAM，且无需 VirtioFS 共享

官方 aarch64 **tiny** 云镜像满足这些要求。它是 Alpine 加上 tiny-cloud 和 `linux-virt` 内核。同一个文件适用于所有云环境的 NoCloud 镜像：tiny-cloud 会寻找标签为 `cidata` 的卷，并在其最终阶段运行以 `#!` 开头的 `user-data` 文件。该文件即内嵌的 `src/guest/maint.sh`。

其他已发布的变体并不适用。`cloudinit` 引入了基于 Python 的 cloud-init，体积更大且速度更慢。`tiny-metal` 和 `cloudinit-metal` 使用 `linux-lts` 和硬件固件。`.qcow2` 和 `.vhd` 则无法直接被 Virtualization.framework 挂载。Vibe 不会自行构建或分发内核、initramfs 或维护镜像。

固定的版本配置（调研于 2026-10-14，仍为 `src/alpine.rs` 中的常量）：

| 常量 | 值 |
| --- | --- |
| `ALPINE_IMAGE_NAME` | `alpine-3.24.2-aarch64-tiny-r1` |
| `ALPINE_ARCHIVE_URL` | `https://dl-cdn.alpinelinux.org/alpine/v3.24/releases/cloud/alpine-3.24.2-aarch64-tiny-r1.raw.tar.gz` |
| `ALPINE_ARCHIVE_SHA512` | `c66badf3648147e0e06f710d600698ec35804af70b00667742b4dce9f28df5cb516190ea90d63792c3cf970a6541aa675ffe1ddddaed13df1abde1fe6f43698b` |
| 归档大小 | 66,222,607 字节。包含单一成员 `disk.raw`，1,073,741,824 字节（1 GiB，稀疏文件）。 |
| 固件 | limine（`EFI/BOOT/BOOTAA64.EFI`）。从 3.24 开始，文件名不再包含 `uefi`。 |
| 内核 | `6.18.54-0-virt`。`limine.conf` 包含 `timeout: 10` 和 `root=LABEL=/`。串口输入不会跳过菜单。内核日志行不会出现在 hvc0 上。 |
| tiny-cloud | 3.4.1-r2。`user-data` 在最终阶段运行，该阶段不等待网络连接。 |

`expand_root` 在每次启动时都会扩展 Alpine 自身的根目录。因此，`run_maintenance` 会将缓存的 raw 文件克隆（`cache::clone_file`：优先使用 APFS 克隆，回退为普通复制）到 `maint/<pid>-<nonce>/alpine.raw` 并启动该克隆副本。`~/.cache/vibe/` 下的文件始终保持只读输入状态。维护校验要求运行后其 SHA-256 保持不变。

## 启动时机

`ensure_bootable`（`src/disk_prepare.rs`）是 provision 路径上的唯一调用方。`provision_image` 在私有工作副本 `<name>.tmp.<pid>` 保持原始大小时调用它，此时尚未通过 `set_len` 将文件扩容至 12 GiB，也尚未调用 `run_vm`。若提前扩容，维护清单中的 `disk_bytes` 将与分区表不符。

`probe_disk` 在宿主机上对副本进行分类。随后 `prepare_decision` 处理逻辑如下：

| 类别 | 执行操作 |
| --- | --- |
| `LegacyCompatible` | 打印 `Disk is already legacy compatible; skipping Alpine preparation.`。不执行下载。此为较旧的 nocloud 镜像，或 Vibe 已经发布过的镜像：Debian 系统，machine-id 缺失、为空或为有效的 32 位十六进制 ID，包含一行 `root:` shadow 记录，且密码字段为空。 |
| `Unsupported` | 打印 `[probe] Warning: ...` 并继续沿用旧版登录路径。不启动 Alpine 虚拟机。常见原因：缺少 GPT 根分区，或者 ext4 根分区的 `os-release` 非 Debian。 |
| `NeedsPreparation` | machine-id 为 `uninitialized` 或其他内容，或 root 密码字段非空。按需下载 Alpine，然后执行 `process_disk`。提示信息显示这大约需要 20 秒。 |
| `Unknown` | 有界读取器无法做出判定（例如 `needs recovery`、machine-id 为软链接、异常的 root 行等）。由 Alpine 自行检查该镜像。 |

在准备工作开始前会打印 `EMPTY_ROOT_NOTICE`：发布的镜像以及基于它创建的所有虚拟机都将具有空的 root 密码。

原始的基础文件和 `--base` 路径仅为只读。在已存在的镜像上执行 `provision --repl` 会调用 `run_vm` 而不会进行探测。失败的维护运行不会发布 `<name>.raw`。

### 缓存

`ensure_alpine_image` 获取 `alpine.lock`，随后执行 `ensure_locked`：

1. 如果缓存归档文件的 SHA-512 与 `ALPINE_ARCHIVE_SHA512` 不匹配，则将其删除并重新获取（`images::fetch_verified`：下载到 `.part`，校验摘要，重命名）。
2. 若缺少 `{name}.raw`，则使用 `extract_disk_raw` 进行解压（归档成员为 `disk.raw`）。
3. `alpine_root_uuid_matches` 要求分区 `ALPINE_ROOT_PART` 为 ext4 格式且 UUID 为 `ALPINE_ROOT_FS_UUID`。如果不匹配，则删除 raw 文件并返回 `[download] verify: ...`。

如果已存在合法的 raw 文件和归档文件，则不会发生网络请求。替换损坏的归档文件时，完好的 raw 文件仍会被保留。所有 `AlpineError` 均带有 `[download]` 前缀显示；获取失败时，会明确指出 URL、预期的 SHA-512 以及可手动放置归档文件的本地路径。

## 客户机执行的操作

当任何文件系统标签为 `/` 或 `cidata`（不区分大小写），或者任何 ext4 UUID 等于 `ALPINE_ROOT_FS_UUID` 时，`process_disk`（`src/maintenance.rs`）会首先拒绝处理该工作副本。因为 Alpine 的 limine 配置行为 `root=LABEL=/`，且 tiny-cloud 通过 `findfs LABEL=cidata` 查找 seed。如果目标磁盘重用了上述任一名称，可能会被错误地挂载为辅助系统自身的根目录或 seed。若 UUID 校验失败，客户机内的 `self` 步骤也会在执行任何写入前中止。宿主机检查可防止此类磁盘被启动。

该无头虚拟机的配置为 2 个 CPU 和 1 GiB 内存，无网络设备、无共享文件夹、无键盘串口。磁盘按客户机识别顺序排列：

| 客户机设备 | 文件 | 模式 |
| --- | --- | --- |
| `vda` | Alpine raw 镜像的克隆副本 | 可写 |
| `vdb` | Debian 工作副本 | 可写 |
| `vdc` | `seed.iso` | 只读 |

`hvc0` 为控制台日志。`hvc1` 用于传输控制帧 `VIBE1 <nonce> ...`。EFI 变量存储在维护目录中，并随该目录一同销毁。`HeadlessDeadlines::maintenance` 设置等待 `PREP-READY` 的超时时间为 90 秒（实测启动耗时约 17 秒，其中 limine 菜单约占 10 秒），等待 `PREP-RESULT` 为 300 秒。

tiny-cloud 的最终阶段运行 `maint.sh`。该脚本将其自身输出重定向至 `hvc0`，在两个控制台上禁用 `ONLCR`，并且绝不打印 shadow 的字节内容或开启 xtrace。`mount_seed` 要求 `findfs LABEL=cidata` 必须对应 `/dev/vdc`，并将该 ISO 挂载至 `/run/vibe-seed`（挂载选项为 `ro,nodev,nosuid,noexec`）。若 seed 无法挂载，则不会发出任何帧。该 ISO 中的清单提供随机数（nonce）、`alpine_root_uuid`（见下文常量）、工作副本大小，以及目标的根分区与 ESP 标识信息。`policy` 为 `minimal`。

各步骤严格按此顺序执行。`observe_frame` 会拒绝任何其他顺序。若步骤失败，会发送 `PREP-RESULT fail <code> <step>` 并关机。若脚本在 `PREP-READY` 之后异常终止，exit trap 也会执行相同操作。

| 步骤 | 代码 | 工作内容 |
| --- | --- | --- |
| `seed` | 2 | 清单格式为 1，policy 为 `minimal`，各字段均为单一 token。 |
| `self` | 3 | 已挂载的根设备为 `/dev/vda2`，且 `findfs UUID=<alpine_root_uuid>` 对应同一设备。此步骤用于确认“启动的是辅助镜像而非 Debian 磁盘”。 |
| `target` | 4 | `/sys/class/block/vdb/size` 与 `disk_bytes` 相匹配。清单中根分区的 `sgdisk -i` 信息与类型 GUID 和 PARTUUID 一致。Busybox 的 `blkid` 不打印 PARTUUID。 |
| `fsck` | 5 | 在目标根分区上执行 `e2fsck -p`，返回值为 0 或 1。 |
| `inspect` | 6 | 以只读方式挂载根分区。确认 `os-release` 中 `ID=debian`。确认 `/etc`、`/etc/machine-id` 和 `/etc/shadow` 均为实体文件而非符号链接。`shadow` 中有且仅有一行格式规范的 `root` 记录。dbus machine-id 不存在、为普通文件，或是指向 `/etc/machine-id` 的符号链接。`sshd_config` 或 `sshd_config.d/*.conf` 中不存在 `PermitEmptyPasswords yes`。`/etc/passwd` 中有一行 `root` 记录且密码字段为 `x`。随后重新挂载为读写模式。 |
| `apply` | 7 | 截断清空 `/etc/machine-id` 及常规的 dbus machine-id。`apply_shadow` 仅将 root 密码字段重写为空，并保留文件的权限模式和所有者信息。 |
| `verify` | 8 | machine-id 是一个空的普通文件。Shadow 字节内容与重写后的渲染结果一致。Shadow 权限模式和所有者保持不变。 |
| `unmount` | 9 | 执行 `sync`，卸载，确认 `/dev/vdb` 上无残留挂载项，执行 `e2fsck -n`。 |

在 `unmount` 之后发送 `PREP-RESULT ok 0 done`，随后执行 `poweroff`。在宿主机上，`verify_processed` 会重新读取工作副本：相同的 GPT 布局、空的 `/etc/machine-id`、空的 root 密码字段、dbus machine-id 缺失/为空/为指向 `/etc/machine-id` 的符号链接，且 `classify` == `LegacyCompatible`。若不匹配则报错 `[verify] host-verify`。软件包安装属于后续步骤，由 Debian 客户机内部通过 `provisioning/base.sh` 执行。辅助系统不会安装软件包，也不会编辑 GRUB、ESP、PAM、sshd 或时区。

在 M2 Mac 上，整个辅助流程运行耗时约 19 秒。面向用户的提示为约 20 秒，该时间也涵盖了在高速网络下的首次下载耗时。

## `ALPINE_ROOT_PART` 和 `ALPINE_ROOT_FS_UUID`

```rust
pub(crate) const ALPINE_ROOT_FS_UUID: &str = "d51a6213-c765-4b85-9105-1b438c523993";
pub(crate) const ALPINE_ROOT_PART: u32 = 2;
```

这两个值均直接读取自解压后的 `alpine-3.24.2-aarch64-tiny-r1` 的 `disk.raw`，并非人为指定。`gpt::tests::real_disk_layouts`（除非设置了 `VIBE_TEST_ALPINE_RAW`，否则默认忽略）会在真实文件上断言这些值：根分区索引为 2、ext4 标签为 `/`、UUID 与常量一致。

### 分区编号

`ALPINE_ROOT_PART` 是 GPT 分区表项数组中从 1 开始计数的插槽编号。空插槽同样占用编号，这也是 Debian ESP 位于插槽 15 时索引即为 15 的原因（`src/gpt.rs` 中的 `Partition::index`）。

在该 raw 镜像上，主 GPT 结构如下：

| 索引 | 角色 | 类型 | 文件系统 | 范围 |
| --- | --- | --- | --- | --- |
| 1 | ESP | EFI 系统分区 | vfat，标签 `EFI` | LBA 2048–380927 |
| 2 | root | Linux 文件系统 `0fc63daf-8483-4772-8e79-3d69d8477de4` | ext4，标签 `/` | LBA 380928–917470 |

这两个分区共占用 448 MiB。`disk.raw` 用零填充扩展至 1 GiB。`select_layout` 将分区 2 识别为唯一的 Linux 根分区，因为它是唯一的 `LINUX_FILESYSTEM_GUID` 项（此镜像没有 ARM64 根分区类型的 GUID）。

Virtio 会根据同一索引为设备节点编号。克隆镜像为第一块磁盘，因此分区 2 表现为 `/dev/vda2`。`src/guest/maint.sh` 中的 `step_self` 会与该字面路径进行比对。客户机脚本内部维护了该数字的副本。宿主机常量用于 `alpine_root_uuid_matches` 中的解压校验。若修改插槽索引，必须在两处同时更新。

### 文件系统 UUID

`ALPINE_ROOT_FS_UUID` 是分区 2 的 ext4 文件系统 UUID（`s_uuid`），其字节顺序与 `blkid` 打印为 `UUID` 的格式一致。请直接复制该字段。`sgdisk -i` 打印的是分区唯一 GUID，清单已将其作为目标的 `root_partuuid` 独立传递。GPT 类型和唯一 GUID 采用混合字节序；ext4 则以显示顺序存储 `s_uuid`（`src/gpt.rs` 中的 `format_ext4_uuid`）。

`read_ext4_label` 按如下方式读取该信息：

1. 分区字节偏移量为 `first_lba * 512`。针对此镜像为 `380928 * 512`。
2. 超级块始于 1024 字节之后。
3. 魔数 `0xEF53` 位于超级块偏移量 `0x38` 处的双字节小端序 `u16`。
4. `s_uuid` 为超级块偏移量 `0x68` 处的 16 个字节。
5. 卷标是位于超级块偏移量 `0x78` 处以 NUL 结尾的字符串（16 字节）。在此镜像中它为 `/`。

这 16 个字节为 `d5 1a 62 13 c7 65 4b 85 91 05 1b 43 8c 52 39 93`，`format_ext4_uuid` 将其呈现为 `d51a6213-c765-4b85-9105-1b438c523993`。`src/alpine.rs` 中的 `synthetic_raw` 写入相同的字节，以便在没有真实镜像的情况下运行缓存测试。

该 UUID 应用于三个位置：

- 解压后，`alpine_root_uuid_matches` 会拒绝分区 2 非该 ext4 格式的 raw 镜像。这可以在归档校验通过的情况下，依然捕获未完整解压或被替换的 `disk.raw`。
- `process_disk` 将其作为 `alpine_root_uuid` 复制到 seed 清单中。`step_self` 要求 `findfs UUID=<that value>` 在操作 `vdb` 之前必须为 `/dev/vda2`。
- `label_block_reason` 会拒绝已包含该 ext4 UUID 的工作副本，以防客户机将 Debian 根分区误判为自身的根分区。

Alpine 自身通过标签 `/` 寻找根分区（`limine.conf` 中的 `root=LABEL=/`）。UUID 则是 Vibe 用来确认当前启动的根目录是否为该锁定镜像的校验手段。

### 从新的 raw 镜像中读取这两个字段

在 Mac 上，代码库的解析器是最权威的读取工具。`gpt::tests::real_disk_layouts` 打开所有三条路径，然后检查 Alpine raw 的索引 2、标签 `/` 以及该 UUID：

```bash
VIBE_TEST_DEBIAN_NEW_RAW=/path/to/debian-new.raw \
VIBE_TEST_DEBIAN_OLD_RAW=/path/to/debian-old.raw \
VIBE_TEST_ALPINE_RAW=/path/to/alpine.raw \
  cargo test --lib gpt::tests::real_disk_layouts -- --ignored --nocapture
```

在测试用例和常量同步更新前，新的 Alpine 镜像将无法通过该断言。在修改测试前若要打印 UUID，可以读取 `scan_labels` 所读取的相同超级块。当前镜像的分区 2：

```bash
# 超级块 = first_lba * 512 + 1024；s_uuid 位于其后偏移 0x68 字节处
dd if=disk.raw bs=1 skip=$((380928 * 512 + 1024 + 0x68)) count=16 2>/dev/null | xxd
```

当布局发生变动时，请从 GPT 重新计算 `first_lba`。`sgdisk -p disk.raw` 列出表项编号；`sgdisk -i 2` 打印分区唯一 GUID，切勿将其复制到 `ALPINE_ROOT_FS_UUID`。在 Linux 上，对第二分区执行 `blkid -o value -s UUID` 和 `blkid -o value -s LABEL` 会得到相同的两个字符串（`/` 和 UUID）。

`.raw.tar.gz` 的 SHA-512 会根据文件同级的官方 `.sha512` 以及发布版本 `.yaml` 中的 `outputs.raw.tar.gz.sha512` 进行校验。代码存储该摘要，不调用 GPG。

## 更新锁定的版本

在修改 URL 的同一次变更中重新测定磁盘参数。Alpine 3.24 将 GRUB 替换为 limine，并从文件名中移除了 `uefi`。后续版本可能会移动根分区位置、生成新的文件系统 UUID、修改标签、改变 tiny-cloud 的 NoCloud 探测逻辑，或修改 limine 超时设置。

1. 从 <https://alpinelinux.org/cloud/> 获取当前的 aarch64 **tiny** `.raw.tar.gz`。忽略 `.qcow2`、`.vhd`、`cloudinit` 和 `metal`。
2. 对照发布的 `.sha512` 和镜像 yaml 校验 SHA-512。记录归档大小。
3. 解压 `disk.raw`（`tar -xOf archive.raw.tar.gz disk.raw`），并按前文步骤重新读取分区索引、ext4 标签和 `s_uuid`。Vibe 所依赖的标签为 `/`。
4. 确认镜像仍然包含 `e2fsprogs`、`sgdisk` 和 busybox 的 `findfs` / `mount`；确认 tiny-cloud 仍可在无网络环境下于最终阶段运行带有 `#!` 的 `user-data`；确认 getty 没有绑定到 `hvc0` 或 `hvc1`；确认 `expand_root` 依然会重写启动磁盘（缓存文件必须保持作为克隆源）。
5. 更新 `src/alpine.rs` 中的 `ALPINE_IMAGE_NAME`、`ALPINE_ARCHIVE_URL`、`ALPINE_ARCHIVE_SHA512`、`ALPINE_ROOT_FS_UUID` 和 `ALPINE_ROOT_PART`。
6. 若根分区插槽不再是 2，需修改 `step_self` 中的 `/dev/vda2` 字面量。仅当 Alpine 克隆副本在 `run_maintenance` 中保持为第一块磁盘时，设备节点才等于 `vda` 拼接 GPT 索引。
7. 使测试桩数据同步支持新的 UUID 和新索引：
   - `src/alpine.rs` 中的 `synthetic_raw`（超级块 `0x68` 处的 16 字节，以及 `TestPart.index`）
   - `src/seed.rs` 测试中的示例 `alpine_root_uuid`
   - `gpt::tests::real_disk_layouts`（索引、标签 `/`、UUID）
   - 当文件名改变时，修改 `tests/disk_prepare.rs` 中的缓存主体名称 `alpine-3.24.2-aarch64-tiny-r1`
   - `docs/how-vm-boot.md` 和 `docs/how-vm-boot.zh-Hans.md` 中的镜像名称
8. 从缓存（`$XDG_CACHE_HOME/vibe` 或 `~/.cache/vibe`）中删除旧的 `<name>.raw` 和 `<name>.raw.tar.gz`。仍符合旧 UUID 的 raw 文件会被持续使用且不会重新下载。`ensure_alpine_image` 根据 `ALPINE_IMAGE_NAME` 建立文件索引，因此重命名的锁定版本代表一组新文件；请在同一次提交中清理旧文件。
9. 运行 `cargo test`。在 Mac 上，运行 `VIBE_VM_CHECK=maintenance-all`（`tests/alpine_maintenance.rs`）和 `VIBE_VM_CHECK=prepare`（`tests/disk_prepare.rs`），同时将 `VIBE_TEST_ALPINE_RAW` 指向新的 raw 文件，并将归档文件作为 `<raw>.tar.gz` 放置在同级目录下。针对新的 Debian 工作副本启动一次辅助镜像，验证 `PREP-RESULT ok 0 done`、空 machine-id、空的 root 密码字段，且缓存 raw 文件的 SHA-256 保持不变。

若 limine 的 `timeout` 变长，请将实测启动时间与 `MAINT_FIRST_OUTPUT_TIMEOUT`（90 秒）进行对比。当前菜单等待耗时约 10 秒，在预算范围内。内核直接启动功能（`VZLinuxBootLoader`，在最初的调研中约为 8.5 秒）目前未实现；维护虚拟机仍然使用 EFI，以便宿主机无需解包 FAT ESP 或 zboot 内核。
