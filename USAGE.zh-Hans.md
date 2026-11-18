# 使用 Vibe

这是完整的使用指南。[README](readme.md)提供了简略版本。

## 命令

Vibe 仅执行两项操作：

1. 从原始磁盘镜像文件**运行**虚拟机
2. 通过启动基础原始磁盘镜像、在其中运行脚本，然后将生成的原始磁盘镜像保存为模板来**配置**虚拟机

```
vibe [OPTIONS] [LOGIN-ACTIONS ...] [path/to/disk.raw]
vibe provision [OPTIONS] [PROVISION-OPTIONS] [@built-in | path/to/script.sh ...]
```

## 运行虚拟机

在项目目录中运行 `vibe` 时，它会将默认模板（`~/.cache/vibe/default.raw`）复制到 `.vibe/instance.raw`，将其启动，并将你的终端附加到该虚拟机。

当你在该 Shell 中输入 `exit` 退出时，虚拟机将关闭。
磁盘状态会一直保留，直到你将其删除。

如果指定了 `path/to/disk.raw`，则会按原样启动。运行时不会检查或修改磁盘。未初始化的当前 Debian 镜像必须先使用 `vibe provision --base` 复制。

## 制作镜像

作为一名认真阅读自述文件的开发者，你可能会喜欢通过运行以下命令来创建自定义模板镜像：

    vibe provision --image my-template @rust @codex my-custom-script.sh
    vibe provision --proxy socks5://127.0.0.1:1080 @rust

（省略 `--image` 将配置默认模板。代理选项、`--dns`、`--network` 和 `-p`/`--publish` 可以放置在 `provision` 之前或之后。）
脚本按顺序运行，以 `@` 开头的脚本将解析为 Vibe 自带的内置脚本。

在 `my-custom-script.sh` 中，我可以设置自己的 tmux 快捷键绑定、偏好的 Shell 自定义配置等。

在项目目录中，你可以运行 `vibe --image my-template` 来使用此模板镜像（如果 `.vibe/instance.raw` 已存在，则忽略此标志）。

配置时始终会运行[基础配置脚本](provisioning/base.sh)，以安装基础工具，如 gcc、[mise-en-place](https://mise.jdx.dev/)、ripgrep 等。
如果你不需要这些，可以制作自己的 `.raw` 磁盘镜像并将其复制到 `~/.cache/vibe/`（或任何额外的镜像目录，请参阅[将镜像保存在其他磁盘上](#将镜像保存在其他磁盘上)）中作为模板使用。

## 默认挂载项和文件

在虚拟机启动之前，`vibe` 和 `vibe provision` 会输出共享到其中的目录以及 Vibe 写入其中的文件。

运行虚拟机时：

| 项目 | 默认状态 | 选项 |
| --- | --- | --- |
| 挂载当前目录至 `~/<project>` 并 `cd` 进入 | 开启 | `--no-mount-cwd` |
| `.vibe` 屏蔽（空只读目录），项目目录（当前目录和 `--mount`）的 `node_modules`、`.git` 处理 | 总是生效 | - |
| 共享缓存 `apt`、`cargo`、`yarn`、`m2`、`go` | 开启 | `--no-mount-sys-cache` |
| 共享 mise 数据（位于 `~/.local/share/mise`） | 开启 | `--no-mount-mise` |
| 宿主机 `~/.tmux`（只读）和 `~/.tmux.conf` | 关闭 | `--mount-tmux-conf` |
| 缓存目录中的 Agent 配置（codex、copilot、claude、gemini、pi、grok、opencode） | 关闭 | `--mount-agents-conf` |
| 写入 Vibe 的 `.profile` 并加载它 | 关闭 | `--write-bashrc` |
| 写入 `.bash_logout`（在 `exit` 时关机） | 总是生效 | - |

每个选项都有其反向形式（`--mount-mise`、`--no-write-bashrc` 等）；以最后指定的选项为准。
`~` 表示 `--vm-user` 的 HOME 目录（默认为 `/root`）；apt 缓存始终位于 `/var/cache/apt`。

共享缓存保存在缓存目录（`~/.cache/vibe`）中。所有用户共享 `apt/`。其他缓存按 `--vm-user` 保存在 `user-<NAME>/` 下，例如 `user-root/cargo` 或 `user-dev/mise-cache`。
在此布局之前，缓存直接存放在缓存目录中（`cargo/`、`mise-cache/` 等）。Vibe 不再挂载这些旧目录，因此 `default.raw` 安装到其中的工具将会缺失。
如果旧目录存在而新目录不存在，Vibe 会输出提示，但绝不会移动或删除它。可以在没有虚拟机运行时手动将其移动到 `user-root/` 下，或者重新配置镜像。
配置命令接受 `--[no-]mount-sys-cache` 和 `--[no-]mount-mise`，始终写入并加载 `.profile`，且从不挂载当前目录、tmux 或 Agent 配置。

共享缓存是配置过程中安装许多工具的地方：`@rust` 将 Rust 安装到 `~/.cargo` 中，mise 将其工具安装到 `~/.local/share/mise` 中。
因此：

- 使用 `--no-mount-sys-cache` 或 `--no-mount-mise` 运行会隐藏配置过程放入共享目录中的工具（Vibe 会打印提示）。
- 使用 `--no-mount-sys-cache` 或 `--no-mount-mise` 进行配置会将这些工具直接安装到镜像本身中。运行该镜像时需要使用相同的选项才能使用它们；否则共享目录会挂载覆盖并隐藏它们。
- 默认镜像由所有项目共享，因此 `vibe` 仅在挂载了共享目录的情况下创建它。如果 `default.raw` 尚不存在，`vibe --no-mount-mise` 将失败并要求你先运行 `vibe provision`（带有或不带有这些选项）。

## 以非 root 用户身份运行

默认情况下，虚拟机中的所有内容都以 root 身份运行。若要为 Agent 分配普通账户，请为其配置一个镜像，并使用相同的 `--vm-user` 运行：

    vibe provision --image dev --vm-user dev --root-password 'something-secret' @codex
    vibe --image dev --vm-user dev --root-password 'something-secret'

配置过程会创建该账户（`useradd`、专属用户组、bash、`/home/dev`），设置 root 密码，并以 `dev` 身份安装内置脚本的用户级工具（mise、rustup、代码 Agent）。你自己的配置脚本仍然以 root 身份运行；它们可以使用 `runuser -u "$VIBE_VM_USER" -- ...`。自定义脚本可以 `source "$VIBE_PROVISION_INCLUDE"` 并调用 `exec_retry`。
可以使用 `VIBE_ROOT_PASSWORD` 代替 `--root-password`。

运行时，Vibe 以 root 身份登录，将共享挂载到 `/home/dev` 下，然后使用 `su --login` 切换到 `dev`。登录操作（`--send`、`--expect`、`--script`）以 `dev` 身份运行。
`exit` 返回到 root shell，在 root shell 中再次 `exit` 会关闭虚拟机。
在登录操作中，在 `--send exit` 之后应使用 `--expect 'back in the root shell'`，然后再发送更多内容；在用户 Shell 关闭期间发送的文本将会丢失。
如果没有 root 密码，`dev` 可以通过 `su -` 提升为 root，因此 Vibe 会对 root 账户没有密码的镜像发出警告。

需要了解的事项：

- 共享目录不按客户机用户隔离：虚拟机中的每个用户都可以写入可写共享，并且那里的新文件属于 Mac 上的当前用户。
  隔离来自虚拟机内部的目录（暂存目录和 `dev` 的 HOME 目录是私有的）以及只读共享和屏蔽。
  [docs/file-shares-between-vm.md](docs/file-shares-between-vm.md) 解释了权限模型及其限制。
- 共享 apt 缓存使用 `bindfs` 为 `dev` 挂载为只读，因此只有 root 的 apt 才能更改它。没有 `bindfs` 的镜像（配置时会安装）将获得可写缓存并收到警告。
- 共享存储库中的 Git 可能会报告 `dubious ownership`，因为共享文件似乎属于最后查看它们的人。如果对此介意，请在虚拟机中设置 `safe.directory`。
- 为一个用户配置的镜像不包含另一个用户的用户级工具。请为每个用户基于全新基础镜像配置新镜像。

## 代理

如果你的 Shell 设置了 `https_proxy`/`http_proxy`（或其大写形式），Vibe 将自动使用该代理：用于虚拟机流量以及在宿主机上下载基础镜像。
`no_proxy`/`NO_PROXY` 成为直接访问的目标列表。
`--proxy` 和 `--proxy-bypass` 会覆盖环境变量，而 `--no-proxy-vm` 可使虚拟机不走代理（下载仍会使用它）。

每次运行都会首先简要总结哪些流量走代理，哪些不走，例如：

    VM network (nat):
      Proxy:  socks5://***@192.168.1.88:3003 (from --proxy)
      Strict: off
      TCP:    proxied
      UDP:    proxied
      DNS:    proxied (DNS-over-TCP); upstreams in the bypass list are queried directly
      ICMP:   direct (not proxied)
      Bypass: 127.0.0.1,localhost,192.168.* (from no_proxy); gateway 192.168.5.2 -> host is always direct

使用 SOCKS5 代理时，UDP 和虚拟机的 DNS 也会走代理。HTTP 代理仅支持 TCP，因此 Vibe 会警告 UDP 和 DNS 直接连接；添加 `--strict-proxy` 可改为丢弃该流量。
关于每个选项如何影响各类流量，请参阅 [docs/network-proxy.md](docs/network-proxy.md)。

## 将镜像保存在其他磁盘上

磁盘镜像体积较大。如果你想将它们存放在外置驱动器上，可以使用两个可选的环境变量指定额外的镜像目录：

    export VIBE_EXTRA_IMAGE_DIR=/Volumes/ExternalDisk/VMImages:/Volumes/ExternalDisk2/VMImages
    export VIBE_PREFERRED_IMAGE_DIR=/Volumes/ExternalDisk2/VMImages

- `VIBE_EXTRA_IMAGE_DIR` 是以 `:` 分隔的绝对路径列表。不存在的目录（例如未挂载的驱动器）将被跳过。
- `VIBE_PREFERRED_IMAGE_DIR` 必须是 `~/.cache/vibe`（遵循 `$XDG_CACHE_HOME` 的默认目录）或额外目录之一，且必须存在。未设置时，首选默认目录。

镜像名称（`--image NAME`、`--base NAME` 和 `default` 镜像）、Debian 基础镜像以及 Alpine 维护镜像按以下顺序查找：首选目录，接着是默认目录，然后是其余额外目录。优先使用首个匹配项。
类似 `vibe path/to/disk.raw`、`--base path/to/x.raw` 和 `.vibe/instance.raw` 的路径将按原样使用。

下载内容和新镜像将写入首选目录。配置已存在的镜像（带有 `--replace`）会在其被发现的目录中进行替换。
非首选目录中的 Debian 和 Alpine 镜像仅作读取用途：绝不会锁定、修复或重新下载。其中损坏的 Alpine 镜像将被跳过并发出警告。
准备新的 Debian 基础镜像时，会在正在使用的 Alpine 镜像旁创建临时的 `maint/` 文件夹（如果该位置不可写，则在首选目录中创建）。
共享缓存（`apt`、`cargo` 等）、Agent 配置、`vibe-usernet` 和维护日志（`logs/`）始终保留在默认目录中。

为了避免遗忘此设置，`vibe` 和 `vibe provision` 启动时会打印查找顺序，`vibe --help` 结尾也会显示：

    Image directories (lookup order):
      1. /Volumes/ExternalDisk2/VMImages (preferred)
      2. /Users/me/.cache/vibe (default)
      3. /Volumes/ExternalDisk/VMImages (missing, skipped)

当按名称找到镜像时，Vibe 会输出其完整路径。

## 选项参考

选项可以出现在 `provision` 之前或之后。
`--base`、`--replace` 和 `--repl` 仅对 `provision` 有效。
登录操作、`--[no-]mount-cwd`、`--[no-]mount-tmux-conf`、`--[no-]mount-agents-conf`、`--[no-]write-bashrc`、`--env`、`--mount`、`--git` 和 `--ssh-key` 仅在运行虚拟机时有效。

如果指定了 `path/to/disk.raw`，则会按原样启动。运行时不会检查或修改磁盘。未初始化的当前 Debian 镜像必须先使用 `vibe provision --base` 复制。

### 通用选项（运行与配置）

- `-h`, `--help`：打印帮助信息。
- `--version`：打印版本（commit SHA 和构建日期）以及代码仓库 URL。
- `--image NAME`：运行：模板镜像（如果 `.vibe/instance.raw` 已存在则忽略）。
  运行时不会检查或修改磁盘。未初始化的当前 Debian 镜像必须先使用 `vibe provision --base` 复制。
  配置：要创建的镜像名称（默认为 `default`）。
- `--cpus COUNT`：虚拟 CPU 数量（默认为 2）。
- `--ram MB`：内存大小，单位为 MB（运行时默认为 2048，配置时默认为 4096）。
- `--vm-user NAME`：会话所属的客户机账户（默认为 `root`）。
  Vibe 以 root 身份登录，挂载共享，然后使用 `su --login` 切换到 NAME：
  登录操作以 NAME 身份运行，`exit` 返回到 root shell，在那里再次 `exit` 会关机。
  相对客户机路径（项目目录、除 apt 之外的缓存、`--mount`、`.tmux`）均位于 NAME 的 HOME 目录下。
  运行操作会检查该账户是否存在并使用 bash；`provision` 会创建它（`useradd`，无额外附加组）。
  为非 root 的 NAME 进行配置需要提供 root 密码。如果 root 没有密码，运行操作会发出警告
  （因为 NAME 随后可以通过 `su -` 提升为 root）。
- `--root-password PW`：用于登录的 root 密码（默认值：`VIBE_ROOT_PASSWORD` 环境变量）。
  运行脚本的配置操作会将其设置为新镜像的 root 密码。
  该值在宿主机进程列表中可见。
- `--[no-]mount-sys-cache`：挂载共享的 apt、cargo、yarn、m2 和 go 缓存（默认开启）。
  使用 `--no-mount-sys-cache` 时，配置期间安装到其中的工具（例如 Rust）将不可用。
- `--[no-]mount-mise`：将共享的 mise 数据目录挂载到 `~/.local/share/mise`（默认开启）。
  使用 `--no-mount-mise` 时，配置期间 mise 安装到其中的工具将不可用。

### 运行选项

- `--[no-]mount-cwd`：将当前目录共享到 `~/<project>` 并 cd 进入（默认开启）。
- `--[no-]mount-tmux-conf`：共享宿主机 `~/.tmux`（只读）并将宿主机 `~/.tmux.conf` 复制到虚拟机中（默认关闭）。
- `--[no-]mount-agents-conf`：从缓存目录挂载共享的代码 Agent 配置目录
  （codex、copilot、claude、gemini、pi、grok、opencode）（默认关闭）。
- `--[no-]write-bashrc`：使用 Vibe 的内置 bash 配置覆盖 vm-user 的 `~/.profile` 并加载它（默认关闭）。
  配置操作始终执行此操作。
- `--env NAME`：在虚拟机内部导出宿主机环境变量 NAME。
  如果 NAME 未设置或为空，则报错。
- `--mount HOST:GUEST[:MODE]`：将宿主机的 HOST 挂载到虚拟机的 GUEST。MODE 为 `read-write`（默认）或 `read-only`。
  相对路径 GUEST 位于 vm-user 的 HOME 目录下。
  如果 HOST 不存在，则报错。
- `--git MODE`：`.git` 目录的处理方式（默认为 `ro`）。
  `rw`：将宿主机的 `.git` 共享为读写。
  `ro`：将宿主机的 `.git` 共享为只读。
  `no`：使用 tmpfs 屏蔽 `.git`。

上述开关的规则：

- 每个 `--[no-]NAME` 开关有两种形式（`--mount-mise`、`--no-write-bashrc` 等）；以最后指定的为准。
- 运行时，`--no-mount-sys-cache` 和 `--no-mount-mise` 需要已存在默认镜像（请先通过 `vibe provision` 创建）。
- 项目目录（当前目录和 `--mount` 共享目录）始终屏蔽 `.vibe`。
- `.env` 等文件在虚拟机中的表现与共享目录中的任何其他文件一样；请将敏感信息保存在项目目录之外。
- 上述 `~` 指 `--vm-user` 的 HOME 目录（默认为 `/root`）。
- 除 apt 之外的缓存按 `--vm-user` 保存在缓存目录中的 `user-NAME/` 下（例如 `user-root/cargo`）。
- 虚拟机中的每个用户都可以写入可写共享；那里的新文件属于运行 vibe 的宿主机用户。
- Git 可能会在共享存储库中报告 `dubious ownership`；如果需要，请在虚拟机中设置 `safe.directory`。
- 对于非 root 的 `--vm-user`，共享 apt 缓存使用 bindfs 为该用户挂载为只读（只有 root 的 apt
  才能修改）；在没有 bindfs 的镜像上，它将挂载为可写并打印警告。

### 网络与代理选项（运行与配置）

- `--network MODE`：客户机网络模式（默认为 `nat`）。
  `nat` 使用 Vibe 内置的用户态网络栈。
  `vznat` 使用 Apple 的 VZNATNetworkDeviceAttachment。
- `-p SPEC`, `--publish SPEC`：将客户机端口转发到宿主机（可重复指定）。
  SPEC 格式为 `[udp:][HOST_ADDR:]HOST_PORT:GUEST_PORT`。
  示例：`-p 127.0.0.1:8022:22 -p udp:8080:80`
- `--proxy URL`：虚拟机流量和宿主机端镜像下载的代理（`http://`、`https://`、`socks5://`、`socks5h://`；
  不带协议前缀的 URL 默认使用 `http://`）。如果不指定 `--proxy`，将依次使用 `https_proxy`、`HTTPS_PROXY`、
  `http_proxy` 和 `HTTP_PROXY`（不读取 `ALL_PROXY`）。
  `nat`：虚拟机的出站 TCP 流量走代理，绝不回退到直连。
  `vznat`：代理仅作为 `http_proxy`/`https_proxy` 导出给客户机（vibe 无法
  强制执行），且 `provision` 会将其保存到新镜像的 apt 配置中。
  启动摘要会显示使用了哪个代理以及其来源。
- `--proxy-bypass LIST`：直连目标（以逗号分隔；默认值：`no_proxy`，然后是 `NO_PROXY`，再然后是
  内置的环回地址、链路本地地址和私有网段列表）。会替换内置列表。
  接受 IP、CIDR、IPv4 前缀（`192.168.*`）、`localhost`、`*` 以及 `host:port`。
  域名仅适用于宿主机下载。通过网关 192.168.5.2 发往宿主机的流量始终直连。
- `--[no-]proxy-udp`：将虚拟机的出站 UDP 流量通过代理路由（仅限 `nat`）。默认值：SOCKS5 开启，
  HTTP 代理关闭（HTTP 代理不支持 UDP；此时指定 `--proxy-udp` 会报错）。
- `--[no-]proxy-dns`：将网关 DNS（向 192.168.5.2 发起的查询）通过代理以 TCP 协议发送至其上游（仅限 `nat`）。
  默认值：SOCKS5 开启，HTTP 代理关闭。绕过列表中的上游会直接查询。
  直接发送到其他服务器的 DNS（如 `dig @8.8.8.8`）属于普通 UDP/TCP 流量，
  遵循 `--proxy-udp` 和 `--strict-proxy` 规则。
- `--[no-]strict-proxy`：丢弃无法通过代理的虚拟机流量（仅限 `nat`；默认关闭）。未走代理的 UDP 和
  网关 DNS 仅能访问绕过列表；ICMP（ping）仅能访问绕过列表。
  如果不开启此选项，此类 UDP、DNS 和 ICMP 流量将直连。
- `--no-proxy-vm`：不代理虚拟机流量。`--proxy` 和代理环境变量仍适用于下载。
- `--dns ADDR`：虚拟机网关 DNS 的上游 DNS 服务器（可重复指定；例如 `--dns 8.8.8.8 --dns 1.1.1.1`）。
  默认值：DNS 被代理或启用严格模式时使用宿主机的 `/etc/resolv.conf` 域名服务器，
  否则使用宿主机系统解析器。代理 DNS 也会将这些上游服务器通过代理发送。

### 登录操作（仅限运行）

登录操作在 vm-user 的 Shell 中按顺序执行。每个操作都可以重复指定。

- `--script PATH`：在虚拟机中运行脚本；如果退出状态码非零则终止。
- `--send TEXT`：向虚拟机中输入 TEXT 并附加换行符。
- `--expect TEXT [SECS]`：等待控制台输出中出现 TEXT，然后再执行下一个登录操作。
  如果 TEXT 未在 SECS 秒内出现（默认为 30），则报错并关闭虚拟机。
- `--ssh-key FILE`：为 vm-user 安装 SSH 公钥 FILE 并启动 SSH 服务器。

### 配置选项

配置过程通过运行（内置）脚本来创建新的指定名称镜像。

- `--base NAME|PATH`：使用此现有镜像或 `path/to/image.raw` 作为新镜像的基础（默认为 Debian Stable）。
- `--image NAME`：新镜像的名称（默认为 `default`）。
- `--replace`：如果存在同名镜像，则替换现有的 NAME 镜像。
- `--repl`：配置完成后（或镜像已存在时），进入交互式 Shell。
  如果镜像不存在或指定了 `--replace`：运行所有脚本，然后保持
  Shell 打开，以便在保存镜像之前进一步自定义。
  如果镜像已存在（且未指定 `--replace`）：直接启动现有镜像
  供交互使用，无需重新运行任何脚本。
  如果 `--vm-user` 为非 root 用户，该 Shell 为 vm-user 的 Shell；`exit` 返回到
  root shell，在 root shell 中再次 `exit` 会关机（并保存新镜像）。
- `--cpus COUNT`：用于配置虚拟机的虚拟 CPU 数量（默认为 2）。
- `--ram MB`：用于配置虚拟机的内存大小，单位为 MB（默认为 4096）。

内置脚本以 root 身份安装系统软件包，并以 `--vm-user` 身份安装用户级工具（mise、rustup、代码 Agent）。
你自己的脚本在设置了 `VIBE_VM_USER` 和 `VIBE_VM_HOME` 的情况下以 root 身份运行；使用 `runuser -u "$VIBE_VM_USER" -- ...`
以该用户身份运行命令。
新的 Debian 基础镜像首先在无头 Alpine 维护虚拟机中准备一次；较旧的基础镜像和 Vibe 镜像按原样使用。

### 环境变量

- `VIBE_ROOT_PASSWORD`：`--root-password` 的默认值。
- `VIBE_EXTRA_IMAGE_DIR=DIR[:DIR ...]`：额外的镜像目录（绝对路径，以 `:` 分隔），在默认目录之后搜索。
  不存在的目录将被标记为跳过。
- `VIBE_PREFERRED_IMAGE_DIR=DIR`：首选目录：默认目录或 `VIBE_EXTRA_IMAGE_DIR` 中的条目。
  优先搜索该目录且该目录必须存在。下载内容和新镜像均写入此处；
  现有镜像会在其原本所在的目录中被替换。
- `VIBE_KEEP_FAILED_WORK=1`：保留配置失败的工作副本和维护文件以供调试。

镜像目录：

镜像名称（`--image NAME`、`--base NAME`、`default` 镜像）以及 Debian 和 Alpine 基础镜像会依次在各个镜像目录中查找；
使用首个匹配项。包含 `/` 的路径将按原样使用。
默认镜像目录是缓存目录。`VIBE_EXTRA_IMAGE_DIR` 和 `VIBE_PREFERRED_IMAGE_DIR` 可用于添加更多目录。
共享缓存（apt、cargo 等）、Agent 配置和维护日志始终保留在缓存目录中。
运行或配置虚拟机时会打印查找顺序。
请参阅[将镜像保存在其他磁盘上](#将镜像保存在其他磁盘上)。

## 帮助输出

`vibe --help` 会打印以下内容，后接缓存目录和镜像目录信息：

```
Vibe spins up a Linux VM on Mac to sandbox LLM agents.

Usage:
  vibe [OPTIONS] [LOGIN-ACTIONS ...] [path/to/disk.raw]
  vibe provision [OPTIONS] [PROVISION-OPTIONS] [@built-in | path/to/script.sh ...]

Options may appear before or after `provision`.
Full reference: USAGE.md (https://github.com/hangxingliu/vibe/blob/main/USAGE.md)

Options (run and provision):
  -h, --help                 Print this help.
  --version                  Print the commit SHA, build date, and repository URL.
  --image NAME               Run: template image (ignored once .vibe/instance.raw exists).
                             Provision: image to create (default `default`).
  --cpus COUNT               Virtual CPUs (default 2).
  --ram MB                   RAM in MB (default 2048; 4096 when provisioning).
  --vm-user NAME             Guest account for the session (default root).
  --root-password PW         Root password to log in (default $VIBE_ROOT_PASSWORD).
  --[no-]mount-sys-cache     Mount the shared apt, cargo, yarn, m2, and go caches (on).
  --[no-]mount-mise          Mount the shared mise data at ~/.local/share/mise (on).

Run options:
  --[no-]mount-cwd           Share the current directory at ~/<project> (on).
  --[no-]mount-tmux-conf     Share host ~/.tmux and copy ~/.tmux.conf (off).
  --[no-]mount-agents-conf   Mount the shared coding agent configuration (off).
  --[no-]write-bashrc        Write Vibe's ~/.profile and load it (off).
  --env NAME                 Export host environment variable NAME in the VM.
  --mount HOST:GUEST[:MODE]  Share HOST at GUEST. MODE: read-write (default)
                             or read-only. A relative GUEST is below the vm-user's HOME.
  --git MODE                 How to share .git. MODE: ro (default), rw, or no
                             (mask .git with tmpfs).

Network options (run and provision):
  --network MODE             MODE: nat (default, bundled user-mode stack) or vznat.
  -p, --publish SPEC         Forward a guest port to the host (repeatable).
                             SPEC: [udp:][HOST_ADDR:]HOST_PORT:GUEST_PORT
                             Example: -p 127.0.0.1:8022:22 -p udp:8080:80
  --proxy URL                Proxy for VM traffic and image downloads
                             (http, https, socks5, socks5h; default $https_proxy etc.).
  --proxy-bypass LIST        Destinations reached directly (comma separated).
  --[no-]proxy-udp           Proxy the VM's UDP (nat only; on for SOCKS5).
  --[no-]proxy-dns           Proxy gateway DNS over TCP (nat only; on for SOCKS5).
  --[no-]strict-proxy        Drop VM traffic the proxy cannot carry (nat only; off).
  --no-proxy-vm              Do not proxy VM traffic; downloads still use the proxy.
  --dns ADDR                 Upstream DNS server for the gateway DNS (repeatable).

Login actions (run only; in order, in the vm-user's shell, repeatable):
  --script PATH              Run a script in the VM; stop if it fails.
  --send TEXT                Type TEXT and a newline.
  --expect TEXT [SECS]       Wait for TEXT; shut down with an error after
                             SECS (default 30).
  --ssh-key FILE             Install a public key for the vm-user and start sshd.

Provision options:
  --base NAME|PATH           Base image (default Debian stable).
  --replace                  Replace an existing image with the same name.
  --repl                     Open a shell after the scripts run,
                             or boot an existing image.

Environment:
  VIBE_ROOT_PASSWORD         Default for --root-password.
  VIBE_EXTRA_IMAGE_DIR       Extra image directories, `:` separated.
  VIBE_PREFERRED_IMAGE_DIR   Directory searched first; downloads and new images go here.
  VIBE_KEEP_FAILED_WORK=1    Keep the work files of a failed provision.

Built-in provisioning scripts:

  @claude     Install Anthropic's Claude.
  @codex      Install OpenAI's Codex.
  @copilot    Install Github Copilot.
  @gemini     Install Google's Gemini (antigravity).
  @golang     Install Go language.
  @grok       Install Grok Build
  @pi         Install Earendil's Pi.
  @rust       Install Rust with sccache.
```
