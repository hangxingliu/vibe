# 更新日志

此 Vibe 分支的所有显著变更均记录在本文档中。

格式基于 [Keep a Changelog](https://keepachangelog.com/en/1.1.0/)，
并且本项目遵循[语义化版本](https://semver.org/spec/v2.0.0.html)。

原始仓库为 <https://github.com/lynaghk/vibe>。
1.0.0 版本列出了此分支相较于原项目的所有更改（基于上游版本 0.1.0，截至提交 `0858d69`）。

## [1.0.0] - 2026.11.18

### 新增

- `-p` / `--publish [udp:][HOST_ADDR:]HOST_PORT:GUEST_PORT` 将客户机 TCP 或 UDP 端口转发到宿主机。
  多次使用该参数以转发其他端口。
  示例：`-p 127.0.0.1:8080:80 -p udp:127.0.0.1:9002:9002`。
- `--proxy <URL>` 通过 HTTP 或 SOCKS5 代理发送虚拟机流量和宿主机镜像下载流量。
  支持的协议为 `http://`、`https://`、`socks5://` 和 `socks5h://`。
  未指定协议的 URL 默认使用 `http://`。
- 在 `nat` 模式下，`vibe-usernet` 代理出站 TCP。该路径绝不会回退到直接连接。
- 在 `vznat` 模式下，客户机接收 `http_proxy` 和 `https_proxy`。
  `vibe provision` 会将代理保存到新镜像的 apt 配置中。 SOCKS5 代理会保存为 apt 支持的 `socks5h://`。
- `--proxy-bypass <LIST>` 列出直接访问的目标地址。
  该列表会替换内置的回环、链路本地和私有地址范围。
  条目可以是 IP、CIDR、`192.168.*` 前缀、`localhost`、`*` 或 `host:port`。
  域名仅适用于宿主机下载。
  通过网关到宿主机的流量保持直连。
- `--[no-]proxy-udp` 通过 SOCKS5 代理路由虚拟机的出站 UDP。SOCKS5 代理默认开启此项。
- `--[no-]proxy-dns` 通过 TCP 将网关 DNS 经由 SOCKS5 代理发送到其上游。 SOCKS5 代理默认开启此项。
  绕过列表中的上游会直接查询。
  通过代理失败的上游返回 `SERVFAIL`。
- `--[no-]strict-proxy` 丢弃无法使用代理的虚拟机流量。
  未代理的 UDP、网关 DNS 和 ICMP 只能访问绕过列表。
  当绕过列表中没有网关 DNS 上游时，返回 `REFUSED`。
- `--no-proxy-vm` 使虚拟机流量不经过代理。宿主机下载仍使用代理。
- 启动时会打印虚拟机代理及其来源。还会打印通过类型连接协议的处理方式。
  详见 [docs/network-proxy.md](docs/network-proxy.md)。
- `--dns <ADDR>` 为虚拟机设置上游 DNS 服务器。多次使用该参数以设置其他服务器。
- `--ssh-key <PUBLIC_KEY_FILE>` 为 vm-user 安装公钥并启动 SSH 服务。
  安装脚本： [src/guest/install_ssh_key.sh](src/guest/install_ssh_key.sh)。
  root 使用同一脚本。`authorized_keys` 以换行结尾。
- `--git <rw | ro | no>` 控制项目的 `.git` 目录。
  默认值 `ro` 以只读方式共享。
  `rw` 以读写方式共享。
  `no` 使用属主为 root、权限为 0555 的空 tmpfs 隐藏 `.git`。
- 项目目录包括当前目录和 `--mount` 共享目录。
  每个 `.vibe` 目录都会被属主为 root、权限为 0555 的空 tmpfs 隐藏。
  每个 `node_modules` 目录都保存在客户机缓存中。
  客户机在该缓存中读写 `node_modules`。
- 挂载参数：`--[no-]mount-cwd`、`--[no-]mount-sys-cache`、`--[no-]mount-mise`、`--[no-]mount-tmux-conf` 和 `--[no-]mount-agents-conf`。
  以最后指定的参数为准。
- `--mount-tmux-conf` 以只读方式共享宿主机的 `~/.tmux`。它会将 `~/.tmux.conf` 复制到 vm-user 的 HOME 目录中。
- `--write-bashrc` 使用 Vibe 的 bash 配置重写 vm-user 的 `~/.profile` 并加载它。配置准备过程始终会写入该文件。
- `--vm-user <NAME>` 选择会话（运行和配置准备）使用的客户机账户。默认值为 `root` (`--vm-user root` 为同一个账户)。
- Vibe 以 root 身份登录，挂载共享，然后使用 `su --login` 切换用户。
  共享路径位于该用户的 HOME 目录下（例如 `/home/dev/proj` 和 `/home/dev/.cargo`）。
  apt 缓存保留在 `/var/cache/apt`。
  缺失的共享目录会被创建且属主归该用户所有。
- 登录操作（`--send`、`--expect`、`--script`）在该用户的 shell 及项目目录中运行。
  `--env` 会在该 shell 中导出。
  执行 `exit` 返回 root shell。
  在 root shell 中执行 `exit` 会关闭虚拟机电源。
- 相对的 `--mount` 客户机路径使用该用户的 HOME 目录。
  `--write-bashrc`、`--mount-tmux-conf` 和 `--ssh-key` 也使用该 HOME 目录。
- 使用非 root 的 `--vm-user` 时，内置的配置脚本会以 root 身份安装系统软件包。
  用户级别的部分会安装 mise、mise 安装的工具、rustup 和编程代理。该部分以 vm-user 身份运行。
  您自己的脚本以 root 身份运行。 这些脚本可以访问 `VIBE_VM_USER`、`VIBE_VM_UID`、`VIBE_VM_GID` 和 `VIBE_VM_HOME`。
- 配置准备过程会为 root 和 vm-user 写入 Vibe 的 `.profile`。
  如果该用户的 HOME 目录中包含属主为 root 的文件（挂载点除外），Vibe 将拒绝保存镜像。
- `vibe provision --repl` 在脚本执行后打开一个 shell。
  在保存镜像之前可以在那里修改镜像。
  对于非 root 的 `--vm-user`，该 shell 为该用户的 shell。
  执行 `exit` 返回 root。在 root 中执行 `exit` 会保存镜像。
  对于现有镜像且未使用 `--replace`，`provision --repl` 会启动该镜像并跳过脚本。
- 为已有其他用户 Vibe 工具的镜像进行配置准备时会打印警告。用户级工具不会为新的 vm-user 安装。
- 会话启动前会检查非 root 的 `--vm-user`。
  对现有镜像执行 `provision --repl` 也会进行此检查。
  当账户不存在、UID 为 0 或其登录 shell 不是 bash 时，Vibe 会停止运行。
  当 HOME 目录不存在、不是绝对路径目录、不归该账户所有或为 `/root` 时，Vibe 会停止运行。
  Vibe 不会更改现有账户。
- 属于 `root`、`sudo`、`adm`、`wheel` 或 `admin` 组会打印警告。
  无密码的 root 账户也会打印警告。因为 vm-user 之后可以通过 `su -` 提权为 root。
- `vibe provision --vm-user <NAME>` 在账户不存在时自动创建该账户。
  `useradd` 会为其分配默认 UID、同名用户组、bash 和 `/home/<NAME>`。
  该账户不会加入附加组。
  非 root 的 `--vm-user` 需要指定 `--root-password` 或设置 `VIBE_ROOT_PASSWORD`。
  镜像中的配置历史会记录 `vm_user:`。
- 每次运行脚本的配置准备都会安装 `bindfs`。
  使用非 root 的 `--vm-user` 运行会将共享的 apt 缓存通过 `bindfs` 挂载。
  只有 root 的 apt 才能修改该缓存。
  若没有 `bindfs`，缓存将以可写方式挂载 (此时 Vibe 会打印警告)。
  安装脚本： [src/guest/install_bindfs.sh](src/guest/install_bindfs.sh)。
- `--root-password <PASSWORD>` 是用于登录的 root 密码。
  省略该参数时，Vibe 会读取 `VIBE_ROOT_PASSWORD`。
  该参数优先级更高，即使其值被拒绝。
- 空密码属于错误。控制字符（U+0000 到 U+001F，或 U+007F）属于错误。
  错误信息会指明 `--root-password` 或 `VIBE_ROOT_PASSWORD`。
  错误信息不会打印密码明文。诊断输出打印 `***`。
  与其他参数一样，该值在宿主机进程列表中可见。
- 执行脚本的配置准备在登录后且在加载 `.profile` 之前，通过 `chpasswd` 设置该密码。
  这涵盖了新镜像、`--repl` 和首次隐式创建的 `default.raw`。密码不会出现在客户机命令行中。
  `vibe` 以及对现有镜像执行的 `provision --repl` 仅使用该密码进行登录。
- 内置配置脚本：`@copilot`、`@golang` 和 `@grok`。
- `VIBE_EXTRA_IMAGE_DIR` 和 `VIBE_PREFERRED_IMAGE_DIR` 可用于添加更多镜像目录。
  常见用途之一是使用外接驱动器。
  参见[将镜像保存在其他磁盘上](USAGE.md#keeping-images-on-another-disk)。
  `vibe` 和 `vibe provision` 会打印共享到虚拟机中的目录。
  它们还会打印写入虚拟机的文件以及镜像目录查找顺序。这些操作均在虚拟机启动前发生。
- 登录操作仍在运行时键入的按键会被丢弃。在此期间连按两次 Ctrl-C 可停止虚拟机。
- 无法进入 root shell 的启动过程会报告失败的阶段。当镜像可能需要准备时，错误信息会建议运行 `vibe provision`。
- provision 脚本可以 `source "$VIBE_PROVISION_INCLUDE"` 来调用 `exec_retry`。

### 变更

- `vibe --help` 更短，宽度不超过 90 列。完整的选项参考移到了 `USAGE.md`。
- `vibe --version` 打印的仓库地址改为 `https://github.com/hangxingliu/vibe/`。
- 支持最新的 Debian nocloud 基础镜像。这些镜像会要求设置 root 密码并运行首次开机向导。
  在执行脚本之前，`vibe provision` 会在一个无界面的 Alpine 维护虚拟机中对这种基础镜像进行一次性准备。
  运行现有磁盘绝不会检查或修改该磁盘。详见 [docs/alpine-os-purpose.md](docs/alpine-os-purpose.md)。
- 代理选项、`--dns`、`--network` 和 `-p` / `--publish` 现可与 `vibe provision` 配合使用。
  可以放在 `provision` 之前或之后。
- **破坏性变更：** 未指定 `--proxy` 时，Vibe 会依次优先使用 `https_proxy`、`HTTPS_PROXY`、`http_proxy` 或 `HTTP_PROXY`。
  该代理覆盖虚拟机流量和宿主机下载。
  在 `vznat` 模式下，`vibe provision` 还会将其保存到新镜像中。
  `no_proxy` 或 `NO_PROXY` 会成为绕过列表。传入 `--no-proxy-vm` 可使虚拟机不走代理。
- **破坏性变更：** `no_proxy`、`NO_PROXY` 或 `--proxy-bypass` 会替换内置的绕过列表。
  像 `10.0.0.0/8` 这样的范围若未列出，将通过代理路由。
  将其添加到 `no_proxy` 或 `--proxy-bypass` 中以保持直连。
- **破坏性变更：** 使用 SOCKS5 代理时，UDP 默认走代理。传入 `--no-proxy-udp` 可直接发送 UDP。
- **破坏性变更：** 宿主机下载始终向 curl 传递 `--proxy`。不再读取 `ALL_PROXY`。请改用 `--proxy` 或 `https_proxy`。
- 在 `vznat` 模式下，客户机的 `no_proxy` 和 `NO_PROXY` 即为绕过列表。`vibe provision` 也会保存来自环境变量的代理。
- 配置准备虚拟机默认使用 4096 MB 内存。之前的默认值为 2048 MB。
- **破坏性变更：** 用户缓存保存在缓存目录下的 `user-<NAME>/` 中，root 亦是如此。
  名称包括 `mise-cache`、`cargo`、`yarn`、`m2`、`go` 和 `--mount-agents-conf` 目录。
  示例：`~/.cache/vibe/user-root/cargo`。apt 缓存仍位于 `apt/`。
- 旧的用户缓存目录不再被挂载。现有的 `default.raw` 安装在其中的工具将不可用。
  当旧目录存在而新目录不存在时，Vibe 会打印提示。Vibe 不会移动或删除旧目录。
  请在没有虚拟机运行的情况下手动将旧目录移动到 `user-root/` 中。
  重新配置镜像以将工具安装到新缓存中。
- **破坏性变更：** 代理配置目录（codex、copilot、claude、gemini、pi、grok、opencode）默认不挂载。
  tmux 配置默认不挂载。需显式传入 `--mount-agents-conf` 和 `--mount-tmux-conf`。
- 每次启动都会写入 `.bash_logout`。
  从控制台执行 `exit` 会关闭虚拟机电源。当客户机关机时，`vibe` 会立即退出。
  tmux 服务端会阻止该关机行为，包括其他用户启动的服务端。SSH 会话也会阻止该关机。
- **破坏性变更：** 运行虚拟机不会重写或加载 `~/.profile`。传入 `--write-bashrc` 可重写 vm-user 的 `~/.profile` 并加载它。
  配置准备过程仍会为 root 和非 root 的 vm-user 写入 `.profile`。
- **破坏性变更：** 当 `default.raw` 不存在时，`vibe` 会拒绝 `--no-mount-sys-cache` 和 `--no-mount-mise`。
  请先使用 `vibe provision` 创建默认镜像。
- 使用非 root 用户的 `vibe --vm-user <NAME>` 在未设置 root 密码时会拒绝创建缺失的 `default.raw`。
  错误信息会提示可使用的 `vibe provision` 命令。
- 出现 `Password:` 提示且无密码时属于错误。密码被拒绝属于错误。
  如果客户机未及时接受或拒绝密码，Vibe 会报告超时。这些密码错误不会建议运行 `vibe provision`。
  对于不要求密码的镜像，Vibe 不会输入所提供的密码。
- VirtioFS 暂存目录为 `/mnt/root/virtiofs-shared`。该目录已从 `/mnt/shared` 迁出。`/mnt/root` 仅 root 可访问。
- 客户机包装器把每个脚本复制到 `/tmp/vibe-scripts-<uid>`（权限 0700）。
- 运行和配置准备会创建 `<cache>/sessions/<pid>-<nonce>/`（权限 0700），并以只读方式放到 VirtioFS 暂存挂载上。
  该共享不做 bind 挂载。宿主机文件日志和客户机共享表都不列出它。
  虚拟机正常停止后删除该目录。不正常停止时目录保留。
  下次 `SessionDir::create` 在该 pid 已不存在时删除它。
- 非 root 运行在挂载 apt 共享时执行 `src/guest/mount_apt_cache.sh`。
  `bindfs` 的标准输出转到标准错误。只有 `mount --bind` 成功后才打印回退提示词。
- 配置准备脚本和 `--script` 文件经只读会话共享进入虚拟机，不再在控制台键入。
  每个脚本是会话目录中的 `scripts/NNN-<name>.sh`，所以保留的目录能显示实际运行的内容。
  非 root `--vm-user` 的 run `--script` 仍内嵌传入，因为该用户读不到会话目录。
- 控制台不再显示脚本包装器，每个脚本只显示一行 `eval`。
- 配置准备从会话目录 source `provision-env.sh`。它导出 `DEBIAN_FRONTEND=noninteractive` 以及 `VIBE_PROVISION_*`、`VIBE_GIT_SHA`、`VIBE_BUILD_DATE` 变量。

### 移除

- **破坏性变更：** 项目目录中的 `.env` 文件不再被屏蔽。因为存在被绕过的可能性。
  详见 [docs/file-shares-between-vm.md](docs/file-shares-between-vm.md)。
- **破坏性变更：** 移除了 `--no-default-mounts`。
  请使用 `--no-mount-cwd`、`--no-mount-sys-cache` 和 `--no-mount-mise`。
  `.vibe` 屏蔽和 `node_modules` 共享无法被关闭。`--git` 仍可控制 `.git`。

### 修复

- 内置 provision 脚本在 `set -e` 下的网络重试从未进入第二次。`exec_retry` 现在会重试。
