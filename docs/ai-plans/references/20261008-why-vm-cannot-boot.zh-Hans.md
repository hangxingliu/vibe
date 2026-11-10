# Debian 新版 nocloud 镜像无法进入 Vibe 登录流程的调查报告

调查日期：2026-10-08。

计划修订日期：2026-10-09。原调查结论和实验记录保持不变；本次结合“可以修改现有虚拟机启动代码”的要求，重新选择后续启动与命令注入方案。以下明确区分已验证的故障修复与尚待实现、实测的设计，不将新增方案描述为已经可用的功能。

## 1. 结论

**问题不是新版 Debian 内核无法在 Apple Virtualization.framework 上运行，而是新版 nocloud 镜像改变了首次启动和 root 登录策略，破坏了 Vibe 原先依赖的“无需交互初始化、root 空密码登录”约定。**

两个独立障碍已经通过本地镜像检查和实际 VM 实验确认：

1. 新镜像的 `/etc/machine-id` 内容是 `uninitialized\n`，而旧镜像是零字节文件。systemd 因此将新镜像识别为首次启动，运行 `systemd-firstboot.service`。该服务启动交互式配置向导，并阻塞 `sysinit.target`；向导未完成时，`serial-getty@hvc0.service` 尚未启动。
2. 新镜像的 root 密码字段是 `!unprovisioned`，旧镜像则为空。即使跳过 firstboot，输入 `root` 后仍会出现 `Password:`，不能继续执行 Vibe 的自动登录动作。

Vibe 的 EFI 启动参数没有指定 `console=hvc0`。实际读取到的内核活动控制台是 `tty0`，但宿主终端连接的是 Virtio Console 的 `hvc0`。firstboot 向导使用 `/dev/console`，所以原始运行中连向导提示也看不到，最终只有等待 `login: ` 的 120 秒超时。

**在新镜像的隔离副本中，仅将 `/etc/machine-id` 清空并清除 root 密码字段，未经修改的 Vibe 就能通过原有 EFI 链路启动 `6.12.111+deb13-arm64`、自动登录并正常关机。** 同样的副本使用用户给出的 NAT、SOCKS5、UDP 和 DNS 参数也成功启动，后续重启成功。不需要降级内核、替换 GRUB/shim、增加内存或延长登录超时。

原调查只交付报告；本次交付更新后的实施计划，仍不修改运行时代码、原始缓存镜像或进行 Git 提交。用户已允许后续实现修改启动代码，因此计划不再把“必须保留现有登录注入链路”作为限制。重新分析后的首选是**受控维护 kernel/initramfs 准备镜像，正常运行保留目标 EFI 启动，受管理镜像使用独立执行通道与文件式脚本注入**，详见 6.2.5 和 6.5。

## 2. 调查范围与环境

| 项目 | 实际情况 |
| --- | --- |
| 宿主 | ARM64 Mac，macOS 27 |
| 当前源码 | Git `c661483` |
| 已有 `target/release/vibe` | `--version` 显示 Git `d5cf06f`，构建日期 2026-10-07 |
| 当前源码验证 | 另建隔离输出目录，离线构建 `c661483`，签署项目已有 virtualization entitlement 后重复验证 |
| 新镜像 | `debian-13-nocloud-arm64-20261001-2618.tar.xz` |
| 旧镜像 | `debian-13-nocloud-arm64-20260112-2355.tar.xz` |
| VM 资源 | 2 CPU、4096 MiB；与失败样例保持一致 |
| 初始隔离条件 | `--network vznat --no-default-mounts`，无用户网络 helper、无项目目录共享 |
| 最终验证条件 | 默认 `nat`，`--proxy socks5://192.168.1.88:3003 --proxy-udp --dns 1.1.1.1` |

已有二进制与当前源码版本不同，但启动配置和登录链路没有对应变化。为避免将旧二进制的结果误认为当前代码的结果，另外离线构建了当前源码，并完整复现 `provision @base ... --repl` 的相同超时，再确认当前构建也能登录修复后的副本。

已有工作区中的子模块改动和其他未跟踪文档保持原样。没有执行 `sudo`、宿主磁盘挂载或安装系统工具。诊断代码和 VM 镜像副本均放在会话目录；VM 内的 root 操作只针对自行创建的测试副本，不是宿主提权。

### 网络访问

外部资料通过 `curl` 读取，保留了环境中的大小写 `HTTP_PROXY`、`HTTPS_PROXY` 和 `NO_PROXY` 变量，使用现有 HTTP 代理 `http://192.168.1.88:3003`。首次 Debian 访问出现 SSL 连接超时，重试后成功取得官方校验和、镜像元数据及参考文档。

部分尝试的版本化文档或源码 URL 返回 HTTP 404，随后改用镜像自带的 systemd 257.13 手册、可访问的官方最新手册，以及 Debian 实际构建脚本路径；没有将 404 当成镜像缺陷或持续网络故障。未连接中国镜像站，也未上传项目代码。

## 3. 镜像完整性与离线差异

### 3.1 不是下载或解压损坏

两份压缩包内都只有名为 `disk.raw` 的磁盘文件，大小均为 `3221225472` 字节，即 3 GiB。两者 GPT 分区布局相同：

| 分区 | 起始 LBA | 结束 LBA | 用途 |
| --- | --- | --- | --- |
| 1 | 262144 | 6289407 | ext4 根文件系统 |
| 15 | 2048 | 262143 | EFI System Partition |

本地压缩包 SHA-512 均匹配各自 Debian 官方 `SHA512SUMS`：

```text
20261001-2618.tar.xz
c049582fe2d13d9e0c1a4767ade3a32bed217774bd6e39ac8fc60ca78534bfc258a8c52363ddfad639d5db7eacc2550688d5393f16aaf62bc5d19ecd4a7e6170

20260112-2355.tar.xz
6ab9be9e6834adc975268367f2f0235251671184345c34ee13031749fdfbf66fe4c3aafd949a2d98550426090e9ac645e79009c51eb0eefc984c15786570bb38
```

缓存中已有的新 `.raw` 文件也与官方 raw 校验和匹配：

```text
6c5b0669a135e12f8da279e2885bda3757fd60080cf23492d478c7563f025a8abf1d8cb17f7d8805341b6d2e1b67ea0b2b910eb366246a9ba0f653969bb6ce74
```

检查根文件系统时使用临时只读 ext4 读取脚本，不挂载、不修改原镜像。旧镜像的状态检查使用重新从压缩包提取的未启动副本，避免将启动后生成的 machine-id 错当成原始状态。

### 3.2 与问题直接相关的差异

| 项目 | 旧镜像 `20260112-2355` | 新镜像 `20261001-2618` |
| --- | --- | --- |
| Debian 版本 | 13.3 | 13.7 |
| 内核 | `6.12.63+deb13-arm64` | `6.12.111+deb13-arm64` |
| systemd | `257.9-1~deb13u1` | `257.13-1~deb13u1` |
| `/etc/machine-id` | 文件存在，长度 0 | `uninitialized\n` |
| `/etc/shadow` 的 root 密码字段 | 空字符串 | `!unprovisioned` |
| `/etc/localtime` | 存在 | 不存在 |
| `/etc/locale.conf` | `LANG=C.UTF-8` | `LANG=C.UTF-8` |
| `/etc/default/locale` | `LANG=C.UTF-8` | `LANG=C.UTF-8` |
| `/etc/hostname` | 空文件 | 空文件 |
| GRUB 内核命令行 | `root=PARTUUID=... ro` | `root=PARTUUID=... ro` |
| 静态 getty 配置 | `getty@tty1.service` | 相同 |
| `systemd-firstboot.service` 配置 | 包含首次启动向导 | 相同 |

两版 GRUB 配置中的根文件系统 UUID、PARTUUID 和内核路径各自与镜像匹配，未发现根分区引用错误。新镜像虽然同时更新了 GRUB、shim、initramfs-tools 和内核，但后续保留这些组件的成功 EFI 启动排除了“必须降级启动组件”的解释。

### 3.3 Debian 构建脚本解释了这些变化

调查时 Debian 官方 cloud-images 仓库的 `master` 中：

- `config_space/13/scripts/NOCLOUD/10-password` 将 root 设置为 `!unprovisioned`。
- `config_space/13/scripts/LAST/10-cleanup` 对 `NOCLOUD` 删除 `etc/localtime` 和 `etc/vconsole.conf`，并向 `etc/machine-id` 写入 `uninitialized`；非 `NOCLOUD` 镜像仍使用空 machine-id 文件。

这些公开构建规则与本地新镜像的实际内容一致，说明问题来自 nocloud 初始化策略变化，不是 Vibe 解压时写坏了磁盘。

这里引用的是调查时可访问的 `master`，不是新镜像构建时的不可变 commit。官方镜像 JSON 记录了 `cloud-admin-team-master` 和镜像版本，但未提供可以直接用于锁定上述改动的源码 commit；**本报告不推断策略首次引入的精确日期或提交号**。根因判断主要依据已校验的两份实际镜像及 VM 对照实验。

## 4. 项目启动链路为什么会失败

### 4.1 项目依赖旧版镜像的隐含约定

`readme.md:250` 说明选择 Debian nocloud 的理由是可以直接进入 root 登录流程。当前实际代码的依赖更具体：

| 位置 | 行为 |
| --- | --- |
| `src/main.rs:37-40` | 固定下载新版本 `20261001-2618` 及对应 SHA-512 |
| `ensure_base_image()`，约 1165-1232 行 | 校验压缩包并提取 `disk.raw`；不做首次启动状态兼容处理 |
| `provision_image()`，约 1314-1432 行 | 复制 base、扩展磁盘文件至 12 GiB，再调用 `run_vm()` |
| `create_vm_configuration()`，约 1653-1831 行 | 使用 `VZEFIBootLoader`、单个 Virtio Block 磁盘和两个 Virtio Console 串口；没有图形设备 |
| 串口配置，约 1776-1818 行 | 第一个串口供终端使用，第二个用于 resize 控制；guest 对应 `hvc0`、`hvc1` |
| `run_vm()`，约 1995-2005 行 | 等待 `login: `，发送 `root`，再等待 `Debian GNU/Linux`；没有发送密码的步骤 |
| `spawn_login_actions_thread()` | 顺序执行登录动作，之后才执行 provisioning 脚本 |

`VZEFIBootLoader` 不像 `VZLinuxBootLoader` 那样提供直接设置 Linux 命令行的入口。启动参数来自磁盘上的 GRUB。因此，单纯在宿主代码中“给 EFI loader 加一个 console 参数”不是现有 API 下可以直接完成的修复。

### 4.2 新版 machine-id 触发了阻塞向导

镜像自带的 `machine-id(5)` 明确区分：

- 文件存在但为空：生成 machine-id，但不认为这是首次启动。
- 文件包含 `uninitialized`：认为这是首次启动，运行满足 `ConditionFirstBoot=yes` 的服务。

两版镜像的 `systemd-firstboot.service` 都包含以下关键配置：

```ini
ConditionPathIsReadWrite=/etc
ConditionFirstBoot=yes
Before=first-boot-complete.target sysinit.target
Type=oneshot
ExecStart=systemd-firstboot --prompt-locale --prompt-keymap --prompt-timezone --prompt-root-password
StandardOutput=tty
StandardInput=tty
StandardError=tty
```

新镜像需要时区等首次配置，实际启动后停在：

```text
Welcome to your new installation of Debian GNU/Linux 13 (trixie)!
Please configure your system!
-- Press any key to proceed --
```

该向导不是后台的可选提示，而是阻塞早期系统初始化的 oneshot 服务。Vibe 尚未等到 getty 的 `login: `，自然也不会发送 `root` 或运行后续脚本。

### 4.3 为什么原始日志完全没有 guest 输出

原始 GRUB 命令行没有 `console=hvc0`。修复副本通过原始 EFI 启动后，读取到：

```text
BOOT_IMAGE=/boot/vmlinuz-6.12.111+deb13-arm64 root=PARTUUID=2dee7ac4-57d3-494b-9090-ddc9a1e78576 ro

/sys/class/tty/console/active:
tty0
```

firstboot 的 TTY 默认是 `/dev/console`；在这种配置下不是 Vibe 连接的 `hvc0`。Vibe 又没有配置可交互的显示设备，所以 firstboot 等待输入时用户看到的是空白，而不是可操作的向导。

正常完成早期启动时，systemd 可以为虚拟化控制台创建 `serial-getty@hvc0.service`；它不要求 hvc0 同时是内核活动控制台。因此旧镜像能出现 hvc0 登录提示，与活动控制台为 tty0 并不矛盾。

### 4.4 跳过 firstboot 仍有第二个障碍

在临时直接内核启动探针中加入 `systemd.firstboot=no` 后，新镜像到达了：

```text
[  OK  ] Started serial-getty@hvc0.service - Serial Getty on hvc0.
Debian GNU/Linux 13 localhost hvc0
localhost login: root
Password:
```

这说明 Virtio Console、getty、新内核和根文件系统能够正常工作，但 root 认证策略与 Vibe 不兼容。`!unprovisioned` 不是可供脚本输入的默认密码。

因此只处理 firstboot、只启动 getty、只修改 expect 字符串，都不足以恢复完整自动登录链路。

此外，第二次 expect 检查的是 `Debian GNU/Linux`，不是实际 shell 就绪标记。如果密码认证失败后 getty 再次打印登录 banner，该文本也可能被匹配，随后命令会错误地发送到登录提示而不是 shell。后续修复应验证真实命令完成，不能仅以 banner 判定认证成功。

## 5. 实验与证据

### 5.1 实验方法

实际启动只使用新建镜像副本；原始压缩包和缓存中的 base raw 未作为可写 VM 磁盘使用。初始实验禁用默认 mounts；复现当前 `provision` 时，把 `XDG_CACHE_HOME` 指向会话目录，使默认 cache shares 也只包含测试数据。

诊断工具包括只读 ext4 文件读取器和使用 Apple Virtualization.framework 的独立 Swift 启动探针。探针配置 2 CPU、4096 MiB、Virtio Block、NAT、entropy 及两个 Virtio Console 串口，不共享宿主目录。直接启动所需 kernel/initrd 从新镜像离线读取，不另外下载内核。

直接启动和普通 Vibe EFI 启动是不同实验：前者用于显示隐藏的 firstboot 状态及进入救援 shell，**最终修复验证仍使用原始 Vibe EFI 链路**。

### 5.2 对照结果

| 实验 | 条件 | 结果 |
| --- | --- | --- |
| 旧镜像原始启动 | 已有 Vibe，EFI，vznat，无默认 mounts | root 自动登录成功，执行标记、读取内核版本并关机，退出码 0 |
| 新镜像原始启动 | 与旧镜像同样的资源和隔离条件 | 120 秒等待 `login: ` 超时，退出码 1，无 guest 控制台输出 |
| 新镜像直接启动 | 新 kernel/initrd，`console=hvc0` | 新内核正常挂载根分区，显示 firstboot 向导；没有到达登录提示 |
| 新镜像跳过向导 | 直接启动，额外 `systemd.firstboot=no` | 到达 hvc0 登录提示，但 root 后要求密码，未进入 shell |
| 新镜像兼容性修复 | 清空 machine-id、清除 root 密码、设置 UTC | 原始 Vibe EFI 启动成功，运行新内核，hvc0 getty 为 active |
| 新镜像最小修复 | 全新副本，仅清空 machine-id、清除 root 密码；不设置时区 | 原始 Vibe EFI + 用户 NAT/代理参数成功登录并关机，退出码 0 |
| 最小修复后重启 | 同一副本，再次 EFI + 相同 NAT/代理参数 | 成功，退出码 0 |
| 当前源码复现 | 隔离构建 `c661483`，`provision @base ... --repl`，未经修复的新 base | 相同 120 秒超时，退出码 1 |
| 当前源码修复验证 | 隔离构建 `c661483`，EFI + 最小修复副本 + 用户 NAT/代理参数 | 登录成功、执行验证标记并正常关机，退出码 0 |

最小修复后的 guest 输出包含：

```text
VIBE_DIAG_MINIMAL_LOGIN_OK
Linux localhost 6.12.111+deb13-arm64 #1 SMP Debian 6.12.111-1 (2026-09-28) aarch64 GNU/Linux
BOOT_IMAGE=/boot/vmlinuz-6.12.111+deb13-arm64 root=PARTUUID=2dee7ac4-57d3-494b-9090-ddc9a1e78576 ro
tty0
active
inactive
ls: cannot access '/etc/localtime': No such file or directory
```

这里的 `active` 对应 `serial-getty@hvc0.service`，`inactive` 对应 `systemd-firstboot.service`。时区文件仍不存在但启动成功，说明**补齐时区不是最小修复的必要条件**，可以作为正常镜像准备工作单独考虑。

当前源码构建使用 `cargo build --offline --locked --release --target-dir <会话目录>/build`，成功；存在原有 `PASSWORD_EXPECT_TIMEOUT` 未使用警告，与本问题无关。

### 5.3 证据边界

本次验证覆盖了镜像完整性、原始故障复现、隐藏阻塞点、root 认证障碍、新内核可运行性、最小修复后的 EFI 登录及重复启动。没有运行完整 `@base` 的 apt 升级和所有工具安装，也没有将其作为“完整 provisioning 成功”的依据；这些操作的网络和软件包问题不在本次启动故障结论之内。

没有二分 Debian 内核、追踪策略改变的首个 upstream commit，亦未测试其他 macOS 版本。由于在同一宿主、同一新版内核和 EFI 启动组件上已有修复前后对照，本次根因判断不依赖这些额外调查。

## 6. 可行解决方案

### 6.1 立即绕行：明确使用已知可工作的旧 base

在实现启动前兼容处理前，最直接的办法是使用旧镜像 `20260112-2355`。项目中的 `--base NAME` 解析为 `<cache>/NAME.raw`，**不会自动解压同名 `.tar.xz`**。如果只有压缩包，必须先自行提取 raw。

以下示例避免覆盖已有 raw；采用临时文件完成解压后再重命名。应在没有并发操作同名镜像的情况下执行：

```bash
set -e
cache="${XDG_CACHE_HOME:-$HOME/.cache}/vibe"
old_raw="$cache/debian-13-nocloud-arm64-20260112-2355.raw"

if [ ! -e "$old_raw" ]; then
  tmp_raw="$(mktemp "$cache/old-base.raw.tmp.XXXXXX")"
  if tar -xOf "$cache/debian-13-nocloud-arm64-20260112-2355.tar.xz" disk.raw > "$tmp_raw"; then
    mv -n "$tmp_raw" "$old_raw"
  else
    rm -f "$tmp_raw"
    exit 1
  fi
fi

./target/release/vibe \
  --proxy socks5://192.168.1.88:3003 --proxy-udp --dns 1.1.1.1 \
  provision @base \
  --base "$old_raw" --image test-legacy --repl
```

通用网络参数放在 `provision` 前面，兼容本机已有旧二进制的参数解析方式。使用新的目标名字避免覆盖现有 `test.raw`；若目标镜像已经存在，当前 `--repl` 行为是直接启动该镜像而不重新运行 provisioning，应选择未使用的目标名字。

调查验证的是该旧 base 的启动能力，不保证之后完整 apt/tool provisioning 的网络可用性。后续 provisioning 更新内核和 systemd，并不必然再次触发本问题：关键在于最终磁盘是否仍保留兼容的 machine-id/登录状态，而不是单纯内核版本。

如果选择在源码中临时回退默认下载，必须同步更新 URL 和 SHA-512，不能只改日期。本报告不建议把永久停留旧镜像当作最终方案。

### 6.2 已验证的最小修复：在首次正常启动前准备镜像

在**新镜像的可写工作副本**中完成：

```bash
# 以下命令是在目标 guest 根文件系统内、以该 guest 的 root 身份执行，
# 不是在 macOS 宿主执行；本次通过隔离 VM 的 init=/bin/bash 救援 shell 验证。
mount -o remount,rw /
printf '' > /etc/machine-id
passwd -d root
sync
/sbin/poweroff -f
```

这两项状态修改与旧镜像的约定一致。machine-id 必须是“存在的空文件”，不能删除文件，否则 systemd 仍会判断为首次启动。不要将救援启动或测试 VM 生成的固定 machine-id 直接分发给所有后续实例；制成可复制模板时，应另行处理 machine-id 和 `/var/lib/dbus/machine-id`，保证实例身份不会重复。

macOS 不能直接修改 raw 内的 ext4 文件。可行准备路径有：

| 路径 | 优点 | 代价或限制 |
| --- | --- | --- |
| 制作并分发 Vibe 专用已准备镜像 | 运行时最简单，保留现有 EFI 链路 | 需要镜像发布、校验和及更新流程 |
| 用已知可启动的 Linux helper VM 作为准备环境，将新磁盘作为第二块盘挂载并处理 | 不需宿主提权或宿主 ext4 写工具 | 当前 Vibe 只配置一块盘，需要实现 helper/多盘准备链路；此路径未在本次实现 |
| 从新镜像提取 kernel/initrd，临时直接内核启动进入救援 shell | 本次已实际验证，不依赖新镜像的 EFI 登录 | 需要提取 ext4 文件、配置临时 `VZLinuxBootLoader` 和受控命令执行，现有 CLI 尚无此能力 |
| Linux 上离线挂载并修改镜像副本 | 使用标准文件系统工具 | 通常涉及 loop/mount 的 root 权限；本次未执行，若采用须由用户 review 后在其自行授权的环境操作 |

**不能只把这些命令加到 `provisioning/base.sh`**：该脚本位于首次 root 登录之后，而故障恰恰发生在到达脚本之前。

root 空密码是恢复项目既有行为的兼容办法，不应不加区分地用于通用 Debian 服务器。需确保 SSH 禁止密码/空密码登录、限制对外端口，且不改变用户提供的自定义镜像认证策略。更长期可以考虑“root 继续锁定，仅 hvc0 显式 autologin”的控制台专用策略，但需要另行验证 Debian PAM、agetty 和 SSH 行为；本次没有验证该替代策略。

以下 A、B 两条路径是原计划中的**后续实现设计，不是本次新增的运行时功能**，保留用于对照；本次新增的首选路径 C 见 6.2.5。helper VM 路径尚未做 VM 实验；直接内核路径已经验证了“提取新镜像 kernel/initrd → 救援修改 → EFI 启动”，但自动提取、调度、错误处理和发布流程仍待实现。文中的函数和类型名除明确引用已有代码者外均为伪代码，不代表已有 API。

#### 6.2.1 两条路径共有的准备与发布边界

准备阶段应放在 `ensure_base_image()` 取得已校验的官方 raw 之后、`provision_image()` 使用 base 之前。官方 raw 继续只作为不可变输入，准备结果使用独立缓存名；不要将官方 raw 的校验和当作修改后模板的校验和。缓存键至少包含上游镜像校验和及准备策略版本，避免更新镜像后误用旧模板。下图及伪代码是 A/B 的原设计；路径 C 也必须遵守相同的副本、身份、停止、验证及发布边界，但替换其中的准备后端，不要求同时实现三个后端。

```text
下载、校验、提取官方 raw（保留不变）
                |
       加锁并创建独占工作副本
                |
       +--------+---------+
       |                  |
  helper VM 修改     直接内核救援修改
       |                  |
       +--------+---------+
                |
       VM 停止，释放磁盘 attachment
                |
      检查修改结果及模板身份状态
                |
   从模板另建验证副本，以原有 EFI 启动
                |
    验证 shell、SSH 策略、关机及重复启动
                |
      原子发布准备模板及完成元数据
                |
      继续原有 provisioning 流程
```

关键是**EFI 验证必须使用另外的副本**。正常启动会生成 machine-id；若把验证后启动过的磁盘直接发布成模板，会把同一实例身份复制给所有用户。准备后的模板保留空 `/etc/machine-id`，验证实例应生成非空身份；两个独立验证副本的身份必须不同，同一副本重启后身份应保持不变。

```rust
fn ensure_prepared_base(official_raw, upstream_checksum, strategy) -> Result<PathBuf> {
    let key = cache_key(upstream_checksum, PREPARATION_POLICY_VERSION);
    let lock = lock_prepared_base(key)?;
    if let Some(template) = find_valid_completed_template(key)? {
        return Ok(template);
    }

    let work = create_exclusive_copy_next_to_destination(official_raw)?;
    match strategy {
        HelperVm => prepare_with_helper(&work)?,
        DirectKernel => prepare_with_rescue_kernel(&work)?,
    }
    // 两个准备函数返回前，都必须确认 VM 已停止并释放磁盘。
    verify_template_offline(&work)?;
    verify_efi_on_disposable_copies(&work)?;
    publish_without_overwriting_existing_template(work, key)?;
    Ok(prepared_path(key))
}
```

工作文件与发布目标应在同一文件系统，便于原子重命名；并发创建需使用锁和独占文件创建，而不是仅依赖 PID 文件名。完成元数据包含输入校验和、策略版本及输出校验和；缺少元数据或校验失败的模板不得复用。发布磁盘后才发布完成记录，中途崩溃的孤立文件不视为成功缓存。任何阶段失败都返回带阶段名的错误，不回退成“准备成功”；保留失败日志，工作副本按明确的调试保留策略处理。不得在 VM 仍运行或持有 attachment 时清理、再次挂载或发布磁盘。

#### 6.2.2 路径 A：已知可启动的 Linux helper VM，第二块盘离线处理

**原理：**启动旧版、已知能登录的 Linux 作为工具环境。新镜像只作为第二块数据盘接入，完全不启动新镜像的 systemd、firstboot 或 getty。Linux 在自己的 guest 权限边界内挂载这块盘的 ext4 根分区，修改后卸载，再关机。

helper 输入可以是已校验的旧版 `20260112-2355` 镜像或单独发布的受管理 helper 镜像，不能依赖“用户恰好已经有可启动的 `default.raw`”。两者都需要明确来源及校验和。helper 启动盘也必须创建临时可写副本，以免每次准备都改写共享 helper 模板。选择这一路径会增加一份 helper 下载/缓存，并要维护其可启动性和所需工具；不要为了取得 helper 而递归调用依赖本次准备流程的 `ensure_default_image()`。

**宿主侧接入点：**

1. 将 `create_vm_configuration()` 当前的单个 `disk_path` 组织成有序磁盘列表。第 1 块是 helper 启动盘，第 2 块是新镜像工作副本，均通过 `VZDiskImageStorageDeviceAttachment` 与 `VZVirtioBlockDeviceConfiguration` 接入。普通运行仍传一块盘，保持既有行为。
2. helper 仍使用 `VZEFIBootLoader` 和独立临时 EFI variable store。准备阶段配置空目录共享、空网络设备列表，不启动用户网络 helper，不暴露宿主项目、缓存、SSH key 或代理凭据；所需命令预装在 helper 内。
3. 复用串口管道、`OutputMonitor` 和脚本状态标记机制，但使用内部准备模式，不执行 `run_vm()` 中写入 profile、resize 后台任务等普通用户登录动作。helper 登录后先用带随机标记的命令确认真实 root shell，再发送准备脚本。
4. 准备脚本返回退出码 0、完成标记且目标成功卸载后，宿主请求 helper 正常关机；确认停止并释放两个磁盘 attachment 后，才进入离线检查和 EFI 验证。

```rust
fn prepare_with_helper(target_work) -> Result<()> {
    let helper_work = exclusive_copy(ensure_verified_helper_image()?)?;
    let target = read_expected_gpt_identity(target_work)?;
    let config = create_internal_vm_configuration(
        Boot::Efi,
        [
            Disk::read_write(&helper_work), // 第 1 块，预期 /dev/vda
            Disk::read_write(target_work),  // 第 2 块，预期 /dev/vdb
        ],
        /* network = */ [],
        /* directory_shares = */ [],
    )?;
    let vm = start_and_wait(config)?;
    vm.login_and_verify_root_shell(fresh_nonce())?;
    vm.run_checked_script(helper_prepare_script(target))?;
    vm.request_poweroff_and_wait_stopped()?;
    vm.release_disk_attachments()?;
    Ok(())
}
```

**guest 侧处理示意：**下例只针对本报告中已经确认的 GPT 布局。分区 1 是 ext4 根分区，分区 15 是 ESP；并非“任意镜像的第二块盘第一个分区都可以修改”。宿主先从目标 GPT 读取根分区 PARTUUID，guest 用 `lsblk`/`blkid` 验证设备、分区类型、PARTUUID 和挂载状态。即使预期名字是 `/dev/vdb1`，也不能只凭设备名动手；不匹配时失败退出，不猜测其他分区。

```bash
# 在 helper guest 的 root shell 内运行；不是 macOS 命令。
set -eu
target=/dev/vdb1
root=/mnt/vibe-target

# 以下校验由生成的脚本实现，不是现成 shell 命令：
# assert helper 的 / 所在磁盘是 /dev/vda，而非目标盘
# assert /dev/vdb1 属于第 2 块盘、类型 ext4、PARTUUID 等于宿主传入值
# assert 目标盘所有分区未被挂载，且目标文件系统没有待处理的损坏
mkdir -p "$root"
mount -t ext4 -o rw "$target" "$root"
# 安装清理 trap：失败时尝试卸载；卸载失败须上报，不能掩盖原始错误。

test -f "$root/etc/debian_version"
test -f "$root/etc/shadow"
test -f "$root/etc/machine-id"
test ! -L "$root/etc/machine-id"
printf '' > "$root/etc/machine-id"
chroot "$root" /usr/bin/passwd -d root

# 按 6.2.4 的规则处理 dbus machine-id、核验 SSH 与最终文件状态。
# chroot 仅执行目标镜像自带的 passwd；不运行 systemd 或用户 provisioning。
sync
umount "$root"
# 返回准备脚本状态；宿主收到成功结果后另发关机命令。
```

`chroot` 在这里可行的前提是 helper 与目标同为 ARM64 Linux，且目标的 `passwd` 及动态链接依赖完整。不能用 helper 自己的 `passwd -d root`，否则修改的是 helper 账号。优先使用目标工具，避免直接用字符串替换破坏 `/etc/shadow` 的其他字段、权限或属主；`passwd` 非零退出即准备失败。

这一阶段不需要扩容 GPT、根文件系统或修改 ESP/GRUB；新磁盘仍可保持原始 3 GiB，后续沿用已有 provisioning 的扩容处理。不要绑定挂载宿主目录，也不要将 helper 的 `/etc/machine-id`、`/etc/shadow` 或整套 `/etc` 复制到目标。

**路径特点：**修改 ext4 交给标准 Linux 工具，宿主无须实现 ext4 写入或提取 kernel；代价是维护可启动 helper、多盘配置和额外一次 VM 启动。它消除了对新镜像登录能力的依赖，但没有消除对 helper 自身登录能力的依赖。

#### 6.2.3 路径 B：提取目标 kernel/initrd，直接内核启动救援 shell

**原理：**绕过目标盘的 EFI/GRUB，以 `VZLinuxBootLoader` 加载目标自己的 Linux kernel/initrd，并通过内核命令行指定 `console=hvc0` 和 `init=/bin/bash`。initramfs 挂载真实根分区后直接执行 bash，而不是 systemd；因此无需经过 firstboot、getty 或 root 密码认证。

**提取阶段必须独立于目标的正常启动：**

1. 从已经校验的官方 raw 读取 GPT，定位 ext4 根分区；只读解析文件系统。
2. 查找实际 `/boot/vmlinuz-<版本>` 与对应 `/boot/initrd.img-<相同版本>`。解析 `/vmlinuz`、`/initrd.img` 等符号链接时仅在镜像内解析，不能把镜像中的绝对链接解释成宿主路径；多个版本时依据有效 GRUB 配置或明确选择规则匹配，不能混用。
3. 将两个文件提取到宿主独占临时目录，并读取该镜像根分区的 PARTUUID。不得把本报告的 PARTUUID 或内核版本写死在运行时代码中。
4. 校验读出的文件完整性、架构及 framework 所需内核格式。GRUB 能启动某个文件不自动代表 `VZLinuxBootLoader` 接受它；遇到不支持的压缩/封装、ext4 特性或损坏应明确报错。

本次临时只读 ext4 读取器是诊断工具，不应未经审查直接变成生产解析器。正式实现需界定 GPT/ext4 支持范围，检查偏移、长度、extent 和符号链接边界，并为本次新旧镜像及异常输入补测试；不支持的镜像不能静默读取错误数据。另一种提取方式是借助 Linux helper 只读挂载后导出文件，但那实际上依赖路径 A，不再是“无需 helper 的独立路径”。这条路径只需要**读** ext4，所有文件系统写入仍由救援 guest 完成。

**宿主侧接入点：**

```rust
fn prepare_with_rescue_kernel(target_work) -> Result<()> {
    let artifacts = extract_matching_kernel_initrd_and_root_partuuid(target_work)?;
    let loader = unsafe {
        let loader = VZLinuxBootLoader::initWithKernelURL(
            VZLinuxBootLoader::alloc(),
            &nsurl_from_path(&artifacts.kernel)?,
        );
        loader.setInitialRamdiskURL(Some(&nsurl_from_path(&artifacts.initrd)?));
        loader.setCommandLine(&NSString::from_str(&format!(
            "root=PARTUUID={} ro console=hvc0 init=/bin/bash",
            artifacts.root_partuuid
        )));
        loader
    };
    let config = create_internal_vm_configuration(
        Boot::Linux(loader),
        [Disk::read_write(target_work)],
        /* network = */ [],
        /* directory_shares = */ [],
    )?;
    config.validateWithError()?;
    let vm = start_and_wait(config)?;
    vm.verify_rescue_shell(fresh_nonce())?;
    vm.run_checked_rescue_script(rescue_prepare_script())?;
    vm.send("/sbin/poweroff -f")?;
    vm.wait_stopped_and_release_disk_attachments()?;
    Ok(())
}
```

这是 `objc2` 调用形状的示意，实际实现以所用 crate 的方法签名和返回类型为准。`create_vm_configuration()` 需要内部启动方式参数，在 EFI 分支保留现有 variable store，在 Linux 分支设置上述 loader；不能试图向 `VZEFIBootLoader` 填入 Linux command line。两个分支继续复用 Virtio Block、entropy、hvc0 串口及 framework 生命周期管理。kernel/initrd 临时文件至少保留到 VM 完全停止和配置释放之后。

救援 VM **不能直接沿用 `run_vm()` 的登录前缀**：这里不会出现 `login: `，也不应发送 `root`。应增加内部 `RescueShell` 控制流程，主动发送探测命令，验证 `id -u` 为 0、shell PID 为 1、真实根分区身份符合预期后才修改。`VM start completion` 只表示虚拟机启动请求成功，不等于 Linux 已进入救援 shell。

**guest 侧处理示意：**

```bash
# 内核通过 init=/bin/bash 启动的 guest PID 1；不是普通 SSH shell。
# 宿主发送的修改脚本在子 bash 内运行，PID 1 自身不能因 set -e 而退出。
bash -eu <<'VIBE_RESCUE'
# 按需挂载 /proc 和 /sys；确认 /dev 可用，而非盲目覆盖现有 devtmpfs。
# 用 /proc/self/mountinfo、blkid 等验证 / 对应预期目标根分区。
mount -o remount,rw /
test -f /etc/shadow
test -f /etc/machine-id
test ! -L /etc/machine-id
printf '' > /etc/machine-id
/usr/bin/passwd -d root
# 按 6.2.4 处理 dbus machine-id、核验 SSH 与最终文件状态。
sync
mount -o remount,ro /
VIBE_RESCUE
status=$?
# 用独立完整行输出本次随机标记及 status，宿主检查后才发送关机命令。
```

`/proc`、`/sys`、设备节点和外部命令的可用性取决于 initramfs 的最终交接状态，不能假设已经完成普通 systemd 启动；缺失时挂载所需伪文件系统，失败则报告具体原因。挂载真实根失败而落入 initramfs emergency shell 时，禁止继续修改：此时 `/etc` 可能是 initramfs 的临时目录，而不是目标磁盘。

`init=/bin/bash` 的 PID 1 不要 `exit`，否则可能引起 kernel panic。子脚本失败后保持救援 shell 供宿主记录错误或保留副本；成功后先同步、将根重新挂为只读，再由宿主发送已在本次实验使用的 `/sbin/poweroff -f`。此处无 systemd，不能使用普通流程中的 `systemctl poweroff`。关机超时或强制停止不能替代成功状态，不发布模板。

**路径特点：**不需要旧 helper 镜像，准备时仍只有一块盘；本次目标 kernel/initrd 的直接启动已有实验依据。代价是实现安全可靠的只读提取器、Linux boot loader 分支及无登录的救援控制流程。它解决的是准备入口，最终正常启动仍回到 EFI，不永久更换用户运行时的启动方式。

#### 6.2.4 共享检查、失败处理与方向 Q&A

A/B 的最小修复共用以下兼容策略和验收逻辑。路径 C 复用身份、发布和错误处理规则，但首选保留 root 的原认证状态，以受控控制台服务代替清空密码，不混用两种认证策略：

1. `/etc/machine-id` 必须是存在的零字节普通文件。`/var/lib/dbus/machine-id` 若是指向 `/etc/machine-id` 的预期符号链接则保留；若是独立普通文件则清空；其他链接或异常文件类型报告错误，避免残留固定身份或误写其他文件。
2. 仅将 `/etc/shadow` 中 root 的密码字段置空，保留其他账号、字段及安全权限。修改后重新读取核验，不能只看命令发出成功。
3. 不把补齐时区、屏蔽全部 firstboot 服务或更换 GRUB 纳入最小修复。SSH 配置需要单独检查；若存在 sshd，应确保有效配置禁止空密码登录，并核验 root 的 SSH 登录限制。存在 `Include`/`Match` 时不能只 grep 单个文件；验证副本中使用有效配置检查和认证行为测试。不满足策略的受管理模板必须修正并重新验证或拒绝发布。
4. 内部串口执行可复用 `script_command_and_status_marker()` / `OutputMonitor::wait_for_line_after()` 的思路，但不能直接复用包含内存整理和 mise 激活的普通脚本包装。随机标记应由 guest 输出、按完整行解析并绑定本次执行及输出游标；构造发送命令时将标记拆分，避免终端回显包含完整成功行。不以 banner、提示符、命令回显或关机单独判断成功。
5. 分别设置启动、shell 就绪、脚本执行及停止超时；报错说明阶段、磁盘路径与退出状态。失败日志不记录 shadow 内容或凭据，不将不支持的镜像自动当作已兼容。所有停止/清理路径均需关闭 I/O、等待 worker 并释放 VM 资源，防止残留进程继续写盘。

**Q：本次是否选择并实现其中一条路径？**

A：本次选择路径 C 作为后续实现的首选设计，但不实现运行时代码。用户明确允许改启动代码，并未指定某个后端；这是重新分析后的工程决策，不是用户已经逐项批准了维护镜像、认证策略或控制协议。A/B 保留为技术对照，不要求一起交付，也不作为静默失败回退。

**Q：准备操作针对所有 `--base` 镜像吗？**

A：不是。自动处理针对 Vibe 管理、来源和支持配置已确认的 base；显式自定义镜像保留原策略。若后续需要适配自定义 Debian，提供明确的适配入口，仅创建并修改副本，检查支持范围后才准备；不靠看到 `!unprovisioned` 就擅自改写用户磁盘。对应 CLI 名称在实现时确定，本计划不声称已有该参数。

**Q：是否直接修复用户缓存里的原始新镜像，或长期固定旧镜像？**

A：都不是。保留官方输入，发布独立准备模板；旧镜像用于短期绕行或 helper 工具环境，不作为永久冻结默认镜像版本的理由。

**Q：为什么不把修复放进 `base.sh`，或给 EFI 加参数就结束？**

A：`base.sh` 必须登录后才能运行，而本次故障发生在登录前。EFI 参数来自磁盘内 GRUB；直接 Linux command line 仅属于路径 B 的临时救援启动。即便跳过 firstboot，仍需处理 root 认证状态。

**Q：恢复 root 空密码是否意味着放开 SSH？**

A：不是。A/B 清空密码仅是已验证的旧行为兼容修复。路径 C 首选不改 root 密码字段，通过 Vibe 专用 hvc0 root shell 服务提供本地控制台；该服务有意绕过本地密码认证，但不使用网络监听、不改 SSH 认证。此方向必须单独验收，不能将最小修复实验当作其成功证据。

**Q：哪些结果才算准备完成？**

A：修改状态可核验、guest 退出码成功、磁盘同步且 VM 停止；另外的副本通过原有 EFI 链路进入真实 shell、重复启动正常、独立实例身份不同且 SSH 策略符合要求，最后原子发布未正常启动过的模板。完整 apt/tool provisioning 是后续独立验收，不由准备成功推导。

本次文档任务的范围和安全边界已明确，无需新增“待理清的需求点”章节。技术验证任务不等于待用户回答的需求问题；未验证的能力列入下面的实施门槛与验收标准，不伪装成已经解决。

#### 6.2.5 路径 C（本次首选）：受控维护 initramfs，无登录准备；文件式注入与独立控制通道

**核心判断：不要把自动化建立在某个 Debian 镜像碰巧允许 root 登录上。** A 把依赖转移给旧 helper 的 getty，B 把依赖转移给目标 kernel/initrd 以及宿主 ext4 解析器；两者都能解决当前问题，但准备入口仍受外部镜像状态影响。更稳定的入口是 Vibe 自己管理的 ARM64 维护 kernel/initramfs：`VZLinuxBootLoader` 启动内存中的维护系统，由其 `/init` 主动运行固定准备程序，不启动目标 systemd、不经过 PAM/getty，也不需要目标密码。

正常使用则继续启动**目标镜像自己的 EFI/GRUB、内核和 initramfs**。维护内核只挂载数据盘，不作为 Debian 日常运行内核，也不要求它与目标 `/lib/modules` 匹配。这样既绕过准备时的认证和 firstboot，又保留 Debian 自己更新内核的能力，避免永久直接内核启动的提取、版本配对及缓存失效问题。

##### 适用范围与承诺

“给定 Debian 镜像都可以被使用”按以下可验收范围落实，而不是声称任何文件都能运行：

| 输入 | 计划行为 |
| --- | --- |
| 本报告两版官方 Debian 13 nocloud ARM64 raw | 必须通过准备、EFI 首启、重启和命令执行验收 |
| 后续官方同类 nocloud ARM64 raw | 通过来源校验及能力检查后准备；新版本纳入镜像升级集成验收，不能只换 URL/checksum |
| 未登记的自定义 Debian ARM64 raw | 默认保留原运行行为；用户显式选择适配时，只处理副本并执行同样的检查 |
| cloud/generic 镜像、LVM、多根分区、加密根、非 ext4、其他架构或损坏镜像 | 不属于首版自动适配范围；给出具体不支持原因，不猜分区、不清密码、不自动屏蔽 cloud-init |
| 能离线准备，但目标 EFI 或内核无法在 framework 上启动的镜像 | 报告 EFI/内核兼容失败；准备成功不代表可运行，不自动回退到另一套内核 |

首版支持配置为 GPT、一个可唯一识别的未加密 ext4 Debian 根分区、可用的 ARM64 EFI 启动链，以及 guest 中的 systemd、bash 和所需 Virtio 驱动。根分区与 ESP 通过类型、文件内容和实际启动配置确认，不写死分区号、PARTUUID 或 `/dev/vda1`。已有任意副作用的 firstboot 服务、自定义 PAM 和 SSH 配置不是“看见 Debian 标记就全盘兼容”的理由。

##### 维护资产和准备入口

1. 发布可重现构建的维护 kernel/initramfs，固定版本、来源、SHA-512、许可证及构建配置；构建在受控 Linux 环境完成，macOS 仅下载和校验。资产独立缓存，不嵌入 Rust 二进制，不引入宿主 root 权限或 macOS ext4 写工具。额外下载量、缓存占用和维护成本必须如实记录，不把该方案称为没有额外依赖。
2. initramfs 内包含固定 `/init`、准备程序及最小工具集，显式支持 GPT、ext4、Virtio Block、Virtio Console、VirtioFS、devtmpfs 和正常同步/卸载/关机。驱动内建或打包匹配的维护内核模块；启动不访问 apt 或网络。需要文件系统检查时使用打包的 e2fsprogs，正确区分“已修复”和致命退出码，不能把任何非零值统一当成功。
3. 维护 VM 使用 `VZLinuxBootLoader`、`console=hvc0`、**唯一一块目标工作盘**，无 EFI variable store、无网络设备、无用户共享、无 resize 或宿主 stdin 转发。维护根在 initramfs 中，因此不需要 helper 启动盘；目标盘只作为数据盘挂载。复用 framework 的设备与生命周期代码，但不复用普通 `run_vm()` 的登录前缀或终端 raw-mode 逻辑。
4. 宿主生成仅含本次 manifest 和固定策略输入的私有目录，以独立 VirtioFS tag 只读共享。guest 验证协议版本、请求 nonce、目标尺寸、GPT 身份、唯一根分区及 Debian 支持配置；不从 manifest 直接执行任意路径或字符串。目录只包含本次文件，不共享整个项目、cache 或会话目录；guest 中按只读、`nodev,nosuid,noexec` 挂载，脚本由明确的解释器读取。
5. `/init` 主动准备 `/proc`、`/sys`、`/dev`，挂载该共享并发送就绪帧；宿主无需先敲命令取得 shell。只读能力检查成功后，才对目标执行必要检查并读写挂载。写入受管理策略后重新读取核验，同步、卸载目标和共享，发送结果帧，再关机。PID 1 负责子进程回收及关机，不能简单执行完脚本就 `exit`。

不把目标 `passwd` 或安装脚本放进维护阶段执行，也不启动目标 systemd。该方案通常只需清空模板身份文件、写入 Vibe 自有文件和配置；避免 `chroot` 执行不受控目标程序。维护工具对文件路径、符号链接、权限和根目录边界作检查，不能顺着目标中的异常链接写到维护系统或错误分区。维护系统本身也在 VM 边界内，宿主不解析不受信任 ext4。

##### 受管理模板的明确策略

- 保留 root 原密码字段，不运行 `passwd -d root`。空 `/etc/machine-id` 及 dbus 身份规则沿用 6.2.4；以模板状态避免本次 `ConditionFirstBoot` 向导，不额外屏蔽所有首次启动服务。
- 安装 Vibe 专用 runner、控制台服务和只读启动共享的挂载配置。它们只在当前 VM 提供的 tag、版本及请求校验符合预期时工作；没有共享时不得打开无认证控制台或等待共享至无限超时。版本不匹配应输出明确诊断，不能当作普通镜像成功运行。
- 对受管理镜像用专用 hvc0 root shell 服务替代 `serial-getty@hvc0`，不走 `/bin/login`、不依赖 root 空密码或 banner。仅处理 hvc0 对应 getty，保留其他终端策略。服务以 root 运行 bash，明确连接 `/dev/hvc0`、配置 controlling TTY、终端 sane 状态、profile 和退出/关机行为。原 root 锁定状态不会阻止 systemd 启动 root 服务，但完整终端行为仍需实测。
- 控制台认证绕过是 Vibe 已拥有磁盘和串口访问权下的**显式产品策略**，不是通用 Debian 安全加固。不得产生 SSH、TCP 或对外可达的无认证执行入口；核验 sshd 的有效配置及实际认证行为。正常运行不改写 GRUB，firstboot 绕过也不依赖 `console=hvc0`；没有内核日志时仍应报告可观测性限制。

该策略与 A/B 的“恢复 root 空密码”不同，后续实现不能在控制台服务失败时悄悄退回清空密码。如果原型不能保持现有交互行为，则修正服务实现或明确报告阻塞，不以未经验证的 agetty/PAM 组合替代。

##### 命令传输与执行结果

**大脚本作为文件传输，交互串口只承担交互，执行结果走独立协议。** 不再依赖每 256 字节 sleep 10ms 去避免 TTY 溢出，也不让脚本正文、登录 banner、终端回显与成功标记竞争同一个缓冲区。

| 通道 | 用途与边界 |
| --- | --- |
| hvc0 | 用户可见控制台、交互 shell、兼容 `--send` / `--expect` 的终端语义 |
| hvc1 | 保留既有窗口 resize，不拿它兼任双向控制通道 |
| hvc2（新增） | guest runner 的双向控制协议：就绪、动作编号、执行请求、退出码和会话结束；不接宿主 stdin、不启动 getty |
| 独立只读 VirtioFS tag | 每次运行的脚本、环境文件、manifest；不复用用户可写的 `shared` tag |

维护 VM 可将专用维护串口用于同样的帧格式，不需要创建三个串口；协议代码复用，设备角色显式指定。普通 VM 的控制协议须设置无回显 raw 模式，采用有上限的长度帧或严格完整行协议，处理分片、粘包、EOF、未知版本、重复动作和非法退出码。控制帧绑定 nonce、会话和单调动作编号；nonce 用于关联与防误匹配，**不意味着 guest 已被安全认证，也不构成恶意 guest 的隔离边界**。

宿主在启动前原子生成并封存脚本和 manifest，使用安全权限，不在命令行或日志中暴露环境凭据。guest 先复制到私有 `/run/vibe` 目录并验证长度/校验和，再向固定解释器提交已登记动作；不能将“收到路径”实现为任意路径读取或 shell 拼接。只读 guest share 不阻止宿主修改文件，因此启动后宿主不修改已发布内容，guest 使用已验证的私有副本。

执行面保持现有顺序和 shell 语义：内部设置、环境导出、用户动作依次执行；`Send` 在交互 shell 中执行，`Script` 在子进程中执行，脚本 stdin 默认 `/dev/null`，成功后的 mise 激活仍回到父 shell。不能简单让 hvc2 runner 另起一个 shell 执行全部动作，否则 `cd`、`export`、`--expect` 和后续交互的状态会断裂。首版采用 runner 管理 hvc0 shell 的执行包装：内部文件式动作经短调用交给该 shell，包装执行后将本次编号和退出码原子写入私有 `/run/vibe` 结果文件，runner 有界等待并通过 hvc2 转发。hvc2 FD 只由 runner 持有，不传给交互 shell 或子脚本，避免后台进程拖住控制通道 EOF；普通用户 `--send` 保留终端发送语义，不自动升级成必须成功的脚本。

具体 FD/IPC 接线需在 6.5 的原型中证明：交互 shell 就绪确认、父 shell 状态保留、脚本不读控制输入、后台子进程不持有控制 FD。结果文件必须绑定当前会话、仅接收预期编号、验证完整内容并在消费后移除，不能读取残留的旧成功状态；runner 也要监测 shell 退出。若需要 runtime helper，应先用 guest 内的现有工具完成小实现，不新增常驻网络 agent 或大型 Rust 依赖。该执行包装是实施工作，不能把当前 `script_command_and_status_marker()` 原样搬过去就算独立通道已实现。

hvc0 日志仅供显示、诊断及用户 `--expect`。控制帧单独解析，不进入 `OutputMonitor` 的文本匹配缓冲；状态来自实际 guest 执行结果，不以“命令已发送”“看见提示符”或“VM 已停止”判断成功。受管理镜像启动须收到 runner 和真实 root shell 的就绪确认，旧/自定义镜像的 legacy 路径则至少用带随机完整行的真实命令响应替代 `Debian GNU/Linux` banner 认证。

##### 全链路与失败边界

```text
已校验的官方 raw + 已校验的维护 kernel/initramfs
             |
    独占目标副本 + 只读准备 manifest
             |
    维护 /init 自动挂载、准备、核验、卸载
             |
     结果成功 + VM 停止 + attachment 释放
             |
   独立副本正常 EFI 启动，验证控制台与 runner
             |
   身份/重启/SSH 验收后，原子发布未启动模板
             |
    provision 或普通运行：只读文件式注入
             |
       hvc2 就绪与执行结果；hvc0 用户交互
```

路径 C 新增维护资产和 runtime runner 版本到模板缓存键。能力检查失败、协议错误、脚本非零、停止失败、卸载失败或 EFI 验证失败均不发布。发生写入后失败，不在同一脏副本上换后端重试；保留脱敏日志，停止并释放 VM 后按明确策略清理或保留副本。

现有 `run_vm()` 在 VM 不再 Running 时可直接返回成功，而 `provision_image()` 随后就重命名目标；新实现必须显式汇总“全部必需动作成功”和“VM 正常结束”。VM 提前关机、通道 EOF 或 worker 断开均不得发布 provisioning 结果。VM 停止后要唤醒所有等待者，再 join worker；当前长脚本等待最长 24 小时，不能依赖它自然超时来完成清理。

**本方案的证据等级：**架构针对当前代码中的启动和注入弱点提出；维护资产、自动准备、专用控制台、hvc2 和文件式运行均未在本次实测。已验证的是 5 节记录的新旧镜像与最小修复，不能据此宣称路径 C 的性能、完整 provisioning 或所有 Debian 版本已通过。

### 6.3 firstboot 的其他处理方式

`systemd.firstboot=no` 已实验证明能够绕过初始化向导，但**它不会解除 root 登录障碍**。

还可以在准备阶段为模板屏蔽 `systemd-firstboot.service`，或者非交互设置 locale、keymap、timezone 和 root 凭据，让首次初始化不再等待输入。这些方案与清空 machine-id 的生命周期语义不同，应按产品需求选择，不应同时无条件屏蔽所有首次启动服务。

若通过 GRUB 加入 `systemd.firstboot=no` 或 `console=hvc0`，必须在启动前修改磁盘内有效的配置，并验证最终 kernel command line。`console=hvc0` 本身只是让提示可见，不会消除交互等待或解锁 root。

### 6.4 建议的项目修复顺序

1. 短期使用经过校验的旧 base 或单独发布已准备的新 base，恢复使用；不改变用户已有镜像。
2. 按路径 C 为受管理 base 增加“校验输入与维护资产 → 独占工作副本 → 无登录准备 → EFI/控制台/runner 验证 → 原子发布”。允许修改现有启动、设备配置和 I/O 代码，不将 getty 文本匹配视为不可改变的接口；自定义镜像适配仍需明确选择且只修改副本。
3. 将首次启动状态作为镜像升级验收项目，检查 machine-id、root 登录、活动控制台和 firstboot 依赖；URL、checksum 与兼容性验证一起更新。
4. 改善登录失败诊断，区分无串口输出、firstboot 交互、`Password:` 和登录动作失败；提供保留失败工作副本的调试选项，避免超时清理后证据丢失。
5. 修正 `--repl` 提前打印的 `Provisioning complete`，并更新 README 对 nocloud 直接登录的条件描述；完成提示应在实际 provisioning 成功后输出。

其中第 5 项是本次日志中的可观测性问题：`provision_image()` 在调用 `run_vm()` 之前就打印“Provisioning complete”。它不能证明 guest 脚本已经执行，用户提供的失败样例中实际尚未完成第一次登录。

### 6.5 可执行实施计划

按以下依赖推进；只交付路径 C 为正式后端，A/B 不列为必做并行实现。

| 阶段 | 具体工作和主要接入点 | 完成门槛 |
| --- | --- | --- |
| P0：先证明入口和终端模型 | 制作固定维护 kernel/initramfs；在两版一次性副本上挂载、识别根分区；安装最小 hvc0 服务和 runner；正常 EFI 启动后验证 root 保持原认证状态仍能进入交互 shell | 无 guest 登录、无网络、无宿主提权即可准备；hvc0 controlling TTY、Ctrl-C、resize、profile、退出关机均实测。维护产物格式被 `VZLinuxBootLoader` 接受，不能仅以构建成功代替 |
| P1：配置与生命周期 | `create_vm_configuration()` 增加内部 EFI/Linux 启动配置、磁盘/串口角色和可选网络；普通调用默认仍为原 EFI。拆开 headless 维护 I/O、hvc0 终端和 hvc2 控制读写，启动前建立读端；所有等待共享取消/EOF 通知，内部初始化及自动动作完成前暂停宿主交互输入 | VM 启动失败、停止、脚本执行中退出和取消均可有界释放线程、管道、资产及 attachment；维护 VM 不接用户共享和 stdin，用户按键不能与初始化短调用交错 |
| P2：准备缓存与发布 | `ensure_base_image()` 保留官方输入；新增受管理 prepared base 入口，调用 P1 维护模式；`provision_image()` 使用准备模板。实现锁、独占副本、身份核验、策略/资产版本元数据和验证副本 | 并发只有一份有效产物；中断与损坏缓存不会被复用；验证生成的 machine-id 不进入发布模板 |
| P3：运行与文件式动作 | `run_vm()` 按模板元数据选择 managed runner 或 legacy 登录；重构内部动作执行和 `script_command_and_status_marker()` 的传输面；保留 shell 单会话语义、`--send` / `--expect`、脚本顺序、mise 和环境行为 | 大脚本不经过 TTY 全量输入；每个必需动作有真实结果；`cd` / `export` / Script 子进程语义和后续交互不变；`--no-default-mounts` 不禁用独立内部只读注入 share |
| P4：错误与成功状态 | `OutputMonitor` 与控制协议分离；`run_vm()` 返回必需动作完成状态；`provision_image()` 只在全部脚本成功后发布。修正 `--repl` 成功提示与提前关机路径，增加脱敏阶段日志 | EOF、非法帧、脚本失败、关机超时、worker 断开都失败；`--repl` 在真正完成时提示，定制期间未正常关机不发布 |
| P5：回归与文档 | `src/tests.rs` 增加策略选择、manifest/帧解析和状态汇总测试；Mac VM 集成覆盖第 8 节；更新 README 中 nocloud 登录前提、维护资产、支持范围、自定义适配及缓存语义 | CLI 原参数兼容；支持矩阵与失败矩阵都有实际记录；发布的 kernel/initramfs 可追溯且有固定校验和 |

P0 是主要技术风险的前置验证，不先投入宿主 ext4 解析器或通用多后端框架。P2 依赖 P1；P3 依赖 P0/P1；P4 集成 P2/P3；P5 在整条链路完成后验收。P0 若发现 framework、维护内核能力或终端模型不满足条件，应调整并记录工程方案，不能假报成功或静默改用 A/B。

**Q：是否必须保留现在“等待 login、输入 root、发送 base64”的实现？**

A：不必。用户明确允许修改启动代码；保留的是现有 CLI、用户动作顺序与交互体验，不是这些脆弱的内部步骤。受管理镜像用 runner 入口，自定义和旧模板保留可诊断的 legacy 路径，不因缺少 metadata 自动改写磁盘。

**Q：是否让每个 Debian 都永久使用维护内核，或为绕过密码开放 SSH？**

A：都不是。维护内核只准备数据盘；正常运行使用目标 EFI/内核。root shell 和控制协议只连接宿主已拥有的 VM 串口，SSH 策略保持独立。

**Q：新方案会增加文件共享和下载，这是否违背最小实现？**

A：这是消除宿主 ext4 解析和旧镜像登录依赖的明确成本。维护资产独立缓存，注入目录仅含本次文件且 guest 只读，不扩大为通用 guest agent。若采用此方案，必须将资产大小与冷/热启动开销写入实测结果，不未经测量承诺“仍然 10 秒启动”。

**Q：没有更多用户回答，为什么不保留需求待确认章节？**

A：当前任务是完成可实施计划；支持范围、原镜像不变、正常 EFI、无登录维护入口及显式自定义适配已作合理工程决策。剩余问题是 P0 的技术实测门槛，不是阻止写计划的重大需求歧义。文中的工程选型与原实验明确分开，不冒充用户已给出的选择。

## 7. 不应作为本次修复的做法

| 做法 | 为什么不足或不适合 |
| --- | --- |
| 把 120 秒改成更长 | firstboot 在等待交互，不是单纯慢启动 |
| 更换代理、DNS 或关闭 UDP 代理 | vznat 无代理也复现；修复后原 NAT/代理参数能登录 |
| 增加 RAM/CPU | 同样 2 CPU、4096 MiB 下修复前失败、修复后成功 |
| 修改 `login: ` 匹配文本 | 原始状态没有启动 hvc0 getty，串口根本没有登录提示 |
| 只加入 `console=hvc0` | 暴露向导，但仍需初始化及 root 登录策略处理 |
| 只跳过 firstboot | 已验证随后停在 root 的 `Password:` |
| 只给 root 清除密码 | machine-id 仍可能触发 firstboot 的时区/键盘配置等待 |
| 只在 `base.sh` 中处理 | 脚本根本无法在当前登录前故障中开始执行 |
| 直接原地修改缓存 base | 混淆官方原始镜像与准备后的模板，影响校验和、复现与已有使用者 |
| 随意换成 cloud/generic 镜像 | 会引入 cloud-init、用户和凭据约定差异，不能保证满足现有登录动作 |

## 8. 后续修复的验收标准

实际实现修复时，至少验证：

1. 两版原始官方 base 经准备后的副本、已准备模板均可在默认资源下进入真实 root shell；未准备的新 base 保留原失败作为负对照，不要求它凭空正常登录。正常 EFI 的 shell/runner 就绪仍在原有 120 秒窗口内完成；维护准备、冷资产下载和完整 provisioning 分别计时，不混入该指标。
2. 默认 `nat`、`vznat` 以及用户提供的代理参数下，启动和首次登录行为一致；完整 provisioning 的网络测试另行执行。
3. 模板复制出的独立实例具有各自的 machine-id，第一次及第二次启动均不再出现 firstboot 等待。
4. 路径 C 准备前后 root 密码字段一致；锁定 root 时 hvc0 专用 shell 与 hvc2 runner 均可用。无注入共享或协议版本错误时不开放该控制台入口。SSH 不能因适配而接受不期望的空密码或绕过认证登录，不将 shadow 内容写入日志。
5. 初始化失败不发布损坏或半准备模板，不覆盖现有目标；日志明确标识失败阶段。
6. 自定义镜像不被隐式改写认证策略，CLI 参数解析测试保持通过；VM 集成测试另行覆盖真实启动，不能用 CLI 单元测试替代。
7. 维护 VM 在无网络、无默认共享、无 host stdin 条件下准备成功；唯一目标盘及根分区识别正确。非支持架构、文件系统、加密根、分区歧义、异常 symlink 和损坏输入在写入前明确拒绝；不在初始化临时根上执行目标修改。
8. 文件式执行覆盖空脚本、Unicode、多行、超过 1 MiB 的脚本、非零退出及 stdout 中伪造旧成功标记；校验 guest 私有副本与宿主原文件一致，证明没有 TTY 截断。混合 `Send` / `Expect` / `Script` 验证顺序、父 shell 的 `cd` / `export`、mise 激活和脚本 stdin 隔离。
9. hvc2 协议覆盖跨 read 分片、多个帧、重复编号、错误 nonce、超长帧、非法退出码与 EOF。脚本执行过程中停止 VM 后，所有 worker 在集成测试设定的 10 秒清理窗口内退出；取消不等到 24 小时脚本超时。
10. 全部 provisioning 动作成功且正常关机才发布目标。专门复现“脚本未完成就 poweroff”“成功标记仅出现在回显”“网络 helper 退出”“磁盘卸载失败”和“发布期间中断”，均不输出成功、不发布半成品。
11. 控制台验收覆盖 Ctrl-C、窗口 resize、TTY sane、交互工具、退出关机与 `--repl`；legacy 路径对 `Password:`、无输出及 shell 未就绪分别诊断，banner 或历史匹配不得当作成功。
12. 完整 `@base` 与一个附加脚本在新旧支持镜像上实际成功，验证第一次工具安装和后续重复运行。启动验收不替代 apt/proxy/tool 验收；离线准备可成功，网络 provisioning 失败则单独报错且不发布。
13. 记录维护资产下载大小、缓存空间、首次准备和缓存命中时间，以及普通 EFI 启动耗时；在相同宿主/资源下与旧版对照。无改动的普通实例不能每次重新启动维护 VM，已有 `.vibe/instance.raw` 不被自动替换。

本次未把运行时修复提交到项目，因此以上是后续实现的验收要求，不是已经完成的全套功能声明。

## 9. 参考资料与诊断记录

### 官方镜像与元数据

- [新镜像 SHA512SUMS](https://cdimage.debian.org/cdimage/cloud/trixie/20261001-2618/SHA512SUMS)
- [旧镜像 SHA512SUMS](https://cdimage.debian.org/cdimage/cloud/trixie/20260112-2355/SHA512SUMS)
- [新镜像 JSON 元数据](https://cdimage.debian.org/cdimage/cloud/trixie/20261001-2618/debian-13-nocloud-arm64-20261001-2618.json)

### 首次启动与构建策略

- [systemd machine-id：First Boot Semantics](https://www.freedesktop.org/software/systemd/man/latest/machine-id.html#First%20Boot%20Semantics)
- [systemd-firstboot：内核命令行配置](https://www.freedesktop.org/software/systemd/man/latest/systemd-firstboot.html)
- 新镜像内 `/usr/share/man/man5/machine-id.5.gz`、`/usr/share/man/man1/systemd-firstboot.1.gz` 和 `/usr/lib/systemd/system/systemd-firstboot.service`；这些是与实际 systemd 257.13 版本对应的依据，优先于最新在线手册中的新增选项。
- [Debian nocloud root 策略脚本](https://salsa.debian.org/cloud-team/debian-cloud-images/-/blob/master/config_space/13/scripts/NOCLOUD/10-password)
- [Debian 镜像清理及 machine-id 策略脚本](https://salsa.debian.org/cloud-team/debian-cloud-images/-/blob/master/config_space/13/scripts/LAST/10-cleanup)

### 新方案的接口依据与核验边界

- [Apple VZLinuxBootLoader](https://developer.apple.com/documentation/virtualization/vzlinuxbootloader)：维护模式直接加载 kernel/initramfs，区别于正常 EFI 模式。本次未新增 framework 集成实验，实际参数和内核格式接受性是 P0 门槛。
- [Linux initramfs 与 `/init`](https://www.kernel.org/doc/html/latest/filesystems/ramfs-rootfs-initramfs.html)：维护入口设计所依据的内核机制；本次在线请求超时，未将该次访问列为成功核验。
- [systemd.exec：标准输入输出与 TTYPath](https://www.freedesktop.org/software/systemd/man/latest/systemd.exec.html)：本次读取在线手册确认 TTY 输入输出及 controlling terminal 配置语义。最新手册不替代 Debian 实际版本的 unit 和终端集成验收。
- VirtioFS 只读共享与 Virtio Console 配置已有 `create_vm_configuration()` 中的使用依据；独立 tag、维护驱动、第三串口、guest runner 及协议是新增设计，不是已有代码能力。

### 本地实验记录

小型诊断脚本、配置摘要和日志保存在本次会话目录的 `files/boot-investigation/`，用于核对实验过程，不是仓库依赖或正式工具。主要日志：

| 文件 | 内容 |
| --- | --- |
| `old-boot.log` / `new-boot.log` | 相同隔离条件下两版镜像启动对照 |
| `new-direct-boot.log` | 直接启动显示 firstboot 阻塞 |
| `new-direct-no-firstboot.log` | 跳过 firstboot 后显示 root 密码提示 |
| `new-minimal-rescue.log` | 最小两项状态修改及关机成功 |
| `new-minimal-efi.log` / `new-minimal-reboot.log` | 原始 Vibe + 最小修复 + NAT/代理的首次及重复启动 |
| `current-build.log` | 当前源码隔离离线构建 |
| `current-provision-new.log` | 当前源码重现用户 provisioning 超时 |
| `current-repaired-boot.log` | 当前源码成功登录修复副本 |

测试用大型 raw 副本、提取的 kernel/initrd、编译输出及可明确归属的临时文件在调查结束后清理；用户原始压缩包、原始缓存 raw 和项目已有 `target/release/vibe` 均保留不变。对 Vibe 原有流程在宿主临时目录产生、但无法可靠确认归属的文件，不做批量删除。
