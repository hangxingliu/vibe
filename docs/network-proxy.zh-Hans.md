# 网络与代理

本文说明 `vibe` 的网络相关参数和环境变量如何影响各类网络请求：VM 内的 TCP/UDP/DNS/ICMP、网关到宿主机、`-p` 端口转发、宿主机侧的镜像下载，以及 `vznat` 模式。

## 参数一览

| 参数 | 作用范围 | 默认值 |
| --- | --- | --- |
| `--proxy <URL>` | VM 流量和宿主机下载 | 环境变量中的代理（见下文），都没有时不使用代理 |
| `--proxy-bypass <LIST>` | VM 流量和宿主机下载 | `no_proxy` > `NO_PROXY` > 内置列表 |
| `--[no-]proxy-udp` | 只管 VM 流量（`nat`） | SOCKS5 开启，HTTP 关闭 |
| `--[no-]proxy-dns` | 只管 VM 的网关 DNS（`nat`） | SOCKS5 开启，HTTP 关闭 |
| `--[no-]strict-proxy` | 只管 VM 流量（`nat`） | 关闭 |
| `--no-proxy-vm` | VM 流量不使用任何代理配置 | 关闭 |
| `--dns <ADDR>` | VM 网关 DNS 的上游 | 见“网关 DNS” |
| `--network <nat\|vznat>` | 网络模式 | `nat` |

同一个开关出现多次时，最后一次生效（例如 `--proxy-udp --no-proxy-udp` 等于 `--no-proxy-udp`）。代理相关参数可以放在 `provision` 之前或之后。

## 代理从哪里来

1. `--proxy`。
2. 没有 `--proxy` 时，依次读取 `https_proxy`、`HTTPS_PROXY`、`http_proxy`、`HTTP_PROXY`，取第一个非空值。
3. 不读取 `ALL_PROXY`/`all_proxy`。

URL 的规则：

- 没有 scheme 时补 `http://`（`192.168.1.88:3003` 等于 `http://192.168.1.88:3003`）。
- 支持 `http://`、`https://`、`socks5://`、`socks5h://`。`socks5h://` 在 VM 侧按 `socks5://` 处理（转发层拿到的都是 IP）；宿主机下载时保留 `socks5h://`，由代理解析域名。
- `socks4://`、`socks://` 和其他 scheme 报错。代理来自环境变量时同样报错（可以用 `--proxy` 覆盖）。
- 输出中的用户名和密码一律显示为 `***@`。

## bypass 列表

只取一个来源，不合并：`--proxy-bypass` > `no_proxy` > `NO_PROXY` > 内置列表。用户给出的列表**替换**内置列表。`--proxy-bypass ""` 表示空列表（除网关外全部经过代理）。

内置列表：`localhost`、`localhost.localdomain`，以及 `0.0.0.0/8`、`10.0.0.0/8`、`127.0.0.0/8`、`169.254.0.0/16`、`172.16.0.0/12`、`192.168.0.0/16`、`100.64.0.0/10`、`192.0.0.0/24`、`198.18.0.0/15`、`224.0.0.0/4`、`255.255.255.255/32`、`::1/128`、`fc00::/7`、`fe80::/10`、`ff00::/8`。

写法（以 `,` 分隔，忽略两侧空白，大小写不敏感）：

| 写法 | VM 流量 | 宿主机下载 |
| --- | --- | --- |
| IP、CIDR（`10.1.2.3`、`10.0.0.0/8`、`::1`） | 支持 | 支持 |
| IPv4 前缀（`192.168.*`、`10.*`） | 支持 | 支持（转换成 CIDR 交给 curl） |
| `localhost`、`localhost.localdomain` | 等于 `127.0.0.0/8` 和 `::1/128` | 支持 |
| 域名（`example.com`、`.example.com`、`*.example.com`） | 不支持，打印警告 | 支持 |
| 带端口（`10.1.2.3:8080`、`[::1]:8080`） | 只在端口相同时匹配 | curl 不支持，打印警告并忽略 |
| `*` | 全部直连，打印警告 | 全部直连，打印警告 |
| 无法解析的项 | 打印警告（指出来源）并忽略 | 同左 |

无论列表是什么，发往网关 `192.168.5.2` 的流量（NAT 改写为宿主机 `127.0.0.1`）始终直连。

## `nat` 模式下 VM 的流量

`nat` 模式由内置的 `vibe-usernet` 转发 VM 的流量，因此 `vibe` 可以强制执行代理设置。

| 流量 | 没有代理 | 有代理、非严格模式（默认） | 有代理、严格模式（`--strict-proxy`） |
| --- | --- | --- | --- |
| 网关 `192.168.5.2` 到宿主机 | 直连 | 直连 | 直连 |
| TCP，目标在 bypass 列表内 | 直连 | 直连 | 直连 |
| TCP，其他目标 | 直连 | 经过代理，失败时不回退直连 | 同左 |
| UDP，目标在 bypass 列表内 | 直连 | 直连 | 直连 |
| UDP，其他目标，UDP 代理开启 | 直连 | 经过 SOCKS5，失败时丢弃 | 同左 |
| UDP，其他目标，UDP 代理关闭 | 直连 | 直连，启动时警告 | 丢弃 |
| ICMP Echo（ping），目标在 bypass 列表内 | 直连 | 直连 | 直连 |
| ICMP Echo，其他目标 | 直连 | 直连 | 丢弃 |
| 链路本地（`169.254.0.0/16`）的 TCP/UDP | 丢弃 | 丢弃 | 丢弃 |

UDP 代理关闭指 HTTP 代理（只能承载 TCP），或 SOCKS5 代理加 `--no-proxy-udp`。

### 网关 DNS

`--[no-]proxy-dns` 只管 guest 发给网关 `192.168.5.2:53` 的 DNS。guest 直接发往其他服务器的 DNS（例如 `dig @8.8.8.8`）是普通的 UDP/TCP 流量，按上表处理：SOCKS5 加 UDP 代理时，`dig @8.8.8.8` 也会经过代理。

上游列表：`--dns` 指定的地址；没有 `--dns` 时，取宿主机 `/etc/resolv.conf` 中的 nameserver。

| 情况 | 网关 DNS 的处理 |
| --- | --- |
| 没有代理 | `--dns` 时直连这些上游；否则使用宿主机系统解析器 |
| DNS 代理开启（SOCKS5 的默认值） | 逐个判断上游：在 bypass 列表内的直连，其余经过代理（DNS over TCP）。`--dns` 指定的上游同样按此判断 |
| DNS 代理关闭，非严格模式 | 不经过代理：`--dns` 时直连这些上游，否则使用宿主机系统解析器。启动时警告 |
| DNS 代理关闭，严格模式 | 只使用 bypass 列表内的上游；一个都没有时，所有查询返回 `REFUSED`，启动时警告 |

DNS 代理关闭指 HTTP 代理，或 SOCKS5 代理加 `--no-proxy-dns`。

经过代理的上游在运行时失败（代理连不上、代理拒绝、上游超时）时返回 `SERVFAIL`，不回退直连，也不回退宿主机系统解析器。对配置的上游，每次查询最多等待 4 秒，确保 guest 的解析器（glibc 默认 5 秒超时）能收到 `SERVFAIL`，而不是自己超时。本地区域（例如 `host.vibe.internal`）的查询不受影响。

启动时会逐个列出上游以及它们是直连还是经过代理，例如：

    VM network DNS: upstreams from /etc/resolv.conf: 192.168.1.88:53 (direct, in bypass list), 100.100.100.100:53 (via proxy)

注意：由于用户给出的 bypass 列表替换内置列表，宿主机上的局域网或链路本地 DNS（例如 `fe80::...%en0`）如果不在列表内，会经过代理，而代理通常连不到它们。这种情况下可以用 `--dns` 指定一个代理能访问的上游，或把它们加入 bypass 列表。

### `-p` 端口转发

`-p`/`--publish` 是从宿主机进入 VM 的连接，不受代理设置影响。

## `vznat` 模式

`vznat` 使用 Apple 的 `VZNATNetworkDeviceAttachment`，`vibe` 看不到 VM 的流量，无法强制执行代理。

- 代理（`--proxy` 或环境变量）在登录时导出为 guest 的 `http_proxy`、`https_proxy`、`HTTP_PROXY`、`HTTPS_PROXY`；bypass 列表原样导出为 `no_proxy`、`NO_PROXY`（没有任何来源时导出内置列表的文本形式）。启动时警告：代理全靠 guest 内的程序遵守这些变量，请自行确保 guest 内能访问到代理服务器。`127.0.0.1:xxx` 这样的回环地址也允许。
- `vibe provision` 把代理写进新镜像的 `/etc/apt/apt.conf.d/99timeout`（`Acquire::http::Proxy`、`Acquire::https::Proxy`），不区分代理来自 `--proxy` 还是环境变量。SOCKS5 代理写成 `socks5h://`（apt 只支持这种写法）。写入的是包含用户名和密码的原值；启动摘要中的 `Image:` 一行会说明这一点。
- `--proxy-udp`、`--proxy-dns`、`--strict-proxy` 在 `vznat` 下报错；对应的否定形式允许。

`nat` 模式下 provision 不把代理写进镜像。

## 宿主机侧的镜像下载

下载 Debian 基础镜像和 Alpine 维护镜像时，`vibe` 调用 curl：

- 代理：`--proxy` > 环境变量（顺序同上）。没有代理时显式传 `--proxy ""`，curl 不会再读取 `ALL_PROXY` 等变量。
- bypass：`--proxy-bypass` > `no_proxy` > `NO_PROXY`，作为 curl 的 `--noproxy`。域名有效；带端口的项被忽略并警告。没有任何来源时不传 `--noproxy`。
- `--no-proxy-vm` 不影响下载。
- 下载前打印 `Host downloads proxy: ...` 一行。

## `--no-proxy-vm`

VM 内部流量不使用任何代理配置（`nat` 下所有流量直连，`vznat` 下不导出代理变量，provision 不写入代理）。`--proxy`、`--proxy-bypass` 和环境变量仍然用于宿主机下载。

## 报错的组合

- `--no-proxy-vm` 加 `--proxy-udp`、`--proxy-dns` 或 `--strict-proxy`。
- VM 没有代理（没有 `--proxy`，也没有环境变量）时使用 `--proxy-udp`、`--proxy-dns` 或 `--strict-proxy`。此时 `--proxy-bypass` 只打印警告。
- HTTP 代理加 `--proxy-udp` 或 `--proxy-dns`（代理来自环境变量时同样报错）。
- `--network vznat` 加 `--proxy-udp`、`--proxy-dns` 或 `--strict-proxy`。
- 不支持的代理 scheme（`socks4://` 等）。

## 启动摘要

每次运行（包括 `provision`）在启动 VM 之前打印摘要，警告写到 stderr，以 `warning: ` 开头。例如本机环境变量是 HTTP 代理时：

    VM network (nat):
      Proxy:  http://192.168.1.88:8132 (from https_proxy)
      Strict: off
      TCP:    proxied
      UDP:    direct (not proxied)
      DNS:    direct (not proxied)
      ICMP:   direct (not proxied)
      Bypass: 127.0.0.1,localhost,192.168.* (from no_proxy); gateway 192.168.5.2 -> host is always direct
    warning: UDP from the VM is sent directly, not through the proxy (HTTP proxies cannot carry UDP)
    warning: gateway DNS queries are not sent through the proxy; ...

## 常见组合

| 命令 | 效果 |
| --- | --- |
| `vibe`（环境变量为 HTTP 代理，`no_proxy=127.0.0.1,localhost,192.168.*`） | TCP 经过 HTTP 代理；UDP、网关 DNS、ICMP 直连，并有警告；`192.168.*` 直连 |
| `vibe --proxy socks5://PROXY:1080` | TCP、UDP、网关 DNS 都经过代理；ICMP 直连 |
| `vibe --proxy socks5://PROXY:1080 --strict-proxy --dns LAN_DNS` | 同上；ICMP 只发往 bypass 列表；LAN_DNS 在列表内时直连，其他上游经过代理 |
| `vibe --strict-proxy`（环境变量为 HTTP 代理） | TCP 经过代理；bypass 列表以外的 UDP、ICMP 被丢弃；网关 DNS 只使用列表内的上游，没有时返回 `REFUSED` |
| `vibe --proxy socks5://PROXY:1080 --proxy-bypass ''` | 除网关外所有目标都经过代理，包括局域网和回环地址 |
| `vibe --no-proxy-vm` | VM 流量不经过代理；镜像下载仍使用环境变量中的代理 |
| `vibe --network vznat` | 环境变量中的代理导出到 guest；`vibe` 不强制执行 |

## 排查

- 先看启动摘要：代理从哪里来、bypass 列表从哪里来、每类流量如何处理，以及 `VM network DNS:` 一行中每个上游是直连、经过代理还是被跳过。
- 网关 DNS 失败时：
  - 如果上游显示 `via proxy`，确认代理能访问到它。局域网或链路本地的 DNS 往往不能通过代理访问，可以把它加入 bypass 列表，或用 `--dns` 指定其他上游。
  - 查询返回 `SERVFAIL`，通常是代理或上游不可用。
  - 查询返回 `REFUSED`，说明严格模式下没有可用上游。
- `vibe-usernet` 的错误输出写在实例目录的 `.vibe/vibe-usernet.log` 中（使用 `.vibe/instance.raw` 时）。
- 在宿主机上用 `lsof -nP -a -p <vibe-usernet 的 pid> -i` 查看 helper 的外部连接：除了代理和 bypass 列表内的地址，不应有其他连接。
