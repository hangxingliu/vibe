# 将 gvisor-tap-vsock 分支变基到上游 main

Vibe 的 NAT 网络是 `helpers/gvisor-tap-vsock` 子模块。上游为 `origin`（`https://github.com/containers/gvisor-tap-vsock`）。该分支添加了 `helpers/vibe-usernet` 已经调用的出站代理和 DNS 行为。本篇文档记录了将该行为移植到上游 `338d9a0f`（2026-10-04）时行之有效的步骤。下次 `origin/main` 更新时请参考本流程。

除非有明确要求，否则在变基过程中不要进行提交（commit）或推送（push）。

## 分支新增的内容

`fork/main` 上共有四个提交，均在 `af3ea886`（截至 2026-05-27 的上游）之后：

| 提交 | 作用 |
| --- | --- |
| `b7b89bcb` | 出站 TCP 支持 HTTP CONNECT 和 SOCKS5。开启 UDP 代理时支持 SOCKS5 UDP associate。 |
| `5033bec8` | 为 `github.com/txthinking/socks5` 执行 `go mod vendor`。 |
| `ca5e2518` | 转发的 DNS 支持 SOCKS5，并支持显式指定的上游 DNS 服务器。 |
| `f5b13c80` | 内置对环回地址、链路本地地址和私有网络目标的直连绕过。 |

`docs/ai-chats/20261004-065953.patch` 是针对旧代码树的修改快照。一旦上游修改了相同的函数，`git apply` 和普通的 cherry-pick 都会失败。需要在新代码树上重新实现该行为。补丁仅作为规范参考。

`helpers/vibe-usernet` 已经将 `Proxy`、`ProxyUDP` 和 `DNSUpstreams` 传入 `virtualnetwork.New`。Rust CLI 也已具备 `--proxy`、`--proxy-udp` 和 `--dns`。仅修改子模块并完成变基就足以支持这些标志，随后在 `helpers/vibe-usernet` 中运行 `go mod tidy` 即可。

## 变基后必须保留的行为

当 `Proxy` 为空且 `DNSUpstreams` 为空时，转发和 DNS 行为与上游保持一致。切勿修改该路径。

- TCP：`http://` 和 `https://` 使用 HTTP CONNECT。`socks5://` 使用 SOCKS5 connect。`https://` 是通过代理的普通 TCP 连接进行 CONNECT。代理会话本身不是 TLS。
- UDP：仅当设置了 `ProxyUDP` 且 URL 为 `socks5://` 时才进行代理。HTTP 代理在处理 UDP 时会返回错误，而不是直接发送数据报，这样可以防止客户机意外泄漏流量。
- DNS：仅 `socks5://` 会改变 DNS 行为。HTTP 代理无法承载它。自定义上游会替换主机解析器。缺少端口时默认为 53。
- 绕过：环回地址、链路本地地址、RFC1918、IPv6 ULA 以及 `pkg/services/noproxy` 中的其他范围均直接连接，包括位于这些网络上的 DNS 上游。包含 `localhost`。
- 保留上游的 `TCPMaxInFlight` 和 `TCPConnectTimeout`。超时时间涵盖直接连接和代理握手。切勿将其变为已转发连接上的超时截止时间（deadline）。

## 旧补丁无法应用的原因

上游移动了该分支修改过的三个函数。

- `forwarder.TCP` 新增了 `maxInFlight` 和 `connectTimeout` 参数，并使用 `net.DialTimeout` 进行连接。在这些参数之后追加 `proxy string`。不要移除它们，也不要回退到 `net.Dial`。
- `forwarder.UDP` 的连接回调现为 `func(from net.Addr) (net.Conn, error)`。分支旧有的 `func() (net.Conn, error)` 无法通过编译。
- `dns.New` 仍会构建主机解析器，但处理程序还会转发 `net.Resolver` 无法查询的 SOA、PTR、AAAA、CAA 及其他类型。它是通过 `miekg/dns` 和 `hostNameservers()`（`/etc/resolv.conf`）完成的。SOCKS5 代理也必须覆盖该路径，否则这些查询会跳过代理。
- `types.Configuration` 新增了 `TCPMaxInFlight` 和 `TCPConnectTimeout`。在这些字段之后添加 `Proxy`、`ProxyUDP` 和 `DNSUpstreams`，以便减少下次上游修改时的冲突。

`NewWithUpstreamResolver` 是测试用于注入假解析器的接合点。保持其签名和主机名称服务器行为不变。

## 当前代码位置

| 组件 | 文件 |
| --- | --- |
| 绕过策略 | `pkg/services/noproxy` |
| HTTP CONNECT 和 SOCKS5 连接 | `pkg/services/netproxy` |
| TCP 和 UDP 转发器 | `pkg/services/forwarder/tcp.go`, `udp.go` |
| DNS 上游选择 | `pkg/services/dns/upstream.go` |
| DNS 服务器中的钩子 | `pkg/services/dns/dns.go` 中的 `dns.New` 和 `exchangeWithNameserver` |
| 配置字段 | `pkg/types/configuration.go` |
| 组装逻辑 | `pkg/virtualnetwork/services.go` |

`upstream.go` 是独立的文件，因此下次变基只需在 `dns.go` 中保留一个很小的钩子并保留此文件即可，无需手动合并整个 DNS 服务器。

## 操作步骤

1. 在 `helpers/gvisor-tap-vsock` 中，拉取 `origin` 并在新分支上签出 `origin/main`。不要从 `fork/main` 开始。
2. 阅读 `git diff af3ea886...fork/main -- pkg/ ':!vendor'`（或补丁文件）以了解预期的行为。忽略 `vendor/`。
3. 在编辑之前阅读当前的 `tcp.go`、`udp.go`、`dns.go` 和 `services.go`。将相关行为移植到新的签名上。
4. 保持代理为空时的路径调用当前上游使用的相同连接方式（`DialContext` / 系统解析器 / `hostNameservers`）。
5. 添加分支之前使用的版本（`v0.0.0-20260601051520-339b044ab0eb`）的 `github.com/txthinking/socks5`，除非需要更新的版本。`golang.org/x/net/proxy` 不支持 UDP associate，因此需要引入此模块。
6. 在子模块目录下执行：

```bash
go get github.com/txthinking/socks5@v0.0.0-20260601051520-339b044ab0eb
go mod tidy
go mod vendor
```

该模块使用 vendor 目录构建。修改 `go.mod` 但不更新 `vendor/` 会导致构建失败。不要将旧的 `vendor/` 树直接复制过来。上游更新了其他依赖项，旧的 vendor 提交会导致它们降级。

7. 在 `helpers/vibe-usernet` 中，运行 `go mod tidy`。其 `replace` 指向子模块，因此其 `go.sum` 必须获取子模块当前的依赖要求。预计 `go` 版本行会与子模块保持一致（在本次移植时为 `go 1.26.0`），且间接依赖版本会随上游更新。这对于在该目录下执行 `go test` 和 `go build` 是必需的，并非无关紧要的顺手升级。
8. 更新 `dns.New` 的调用点。`pkg/services/dns/dns_test.go` 中的 ginkgo 测试调用了 `New`，需要为新参数传入 `"", nil`。
9. 如果绕过规则发生更改，请更新子模块的 README、`cmd/gvproxy/config.yaml` 注释，以及 `src/main.rs` 和 `readme.md` 中的 `--proxy` / `--dns` 帮助说明。
10. 运行下列测试。除非有要求，否则不要提交。

## 本次遇到的常见陷阱

**Go 解析器不会连接你传递给 `Dial` 的地址。** 在设置了 `PreferGo: true` 的情况下，它仍然会读取 `/etc/resolv.conf`，然后调用 `Dial(ctx, network, systemNameserver)`。要遵守 `DNSUpstreams`，请忽略该地址并自行连接配置的列表。如果列表为空且设置了 SOCKS5 代理，则使用 `hostNameservers()`；若主机没有名称服务器，则使用 `8.8.8.8:53`。

**TCP 与 UDP 的 DNS 帧格式遵循连接类型，而非网络字符串。** 当连接不是 `net.PacketConn` 时，`net.Resolver` 和 `miekg/dns` 会使用长度前缀。因此，即使解析器请求的是 `"udp"`，SOCKS5 DNS 也会连接 TCP 并返回该流式连接。`*net.TCPConn` 并不是 `PacketConn`（其中的 `ReadFrom` 是 `io.ReaderFrom`）。不要将其包装在实现 `PacketConn` 的对象中，否则长度前缀会被省略，导致查询失败。

**`socks5.NewClient` 的超时参数是已建立连接的截止时间（deadline）。** 它们以整秒为单位。设置这些参数，以防止代理在接受 TCP 连接后挂起导致连接过程卡住；在返回该连接前，请清除返回连接（以及 UDP associate 的 `TCPConn`）上的截止时间。否则，一旦超过 `TCPConnectTimeout`，每个被代理的连接流都会断开。

**`http.ReadResponse` 使用了 `bufio.Reader`。** 代理在 CONNECT 响应之后立即发送的字节会停留在该缓冲区中。返回一个其 `Read` 方法会清空该 Reader 的连接。此外，当 URL 包含用户信息时，同时发送 `Authorization` 和 `Proxy-Authorization`。代理通常会检查 `Proxy-Authorization`。

**使用 `net.JoinHostPort` 设置默认端口。** 对于 `[::1]`，`host + ":1080"` 是错误的。应去除括号后再进行拼接。HTTP 默认为 8080，SOCKS5 默认为 1080，与旧分支保持一致。

**监听在 127.0.0.1 的测试无法证明代理被实际使用。** 环回地址会被绕过，因此一次“成功”的连接根本没有经过代理。应将客户端指向公共地址（如 `93.184.216.34:80` 或 `8.8.8.8:53`），并让假代理将该 CONNECT 请求中继到本地服务器。断言实际上连接了挂起或带有计数器的代理。同样的陷阱也适用于 DNS：`127.0.0.1` 上游本就应该绕过 SOCKS5。

**不要修改调用方的 `DNSUpstreams` 切片。** 旧代码直接在原切片上追加 `:53`。请复制该切片。

## 测试

在 `helpers/gvisor-tap-vsock` 目录下执行：

```bash
go test -count=1 -timeout 180s -race \
  ./pkg/services/noproxy/ \
  ./pkg/services/netproxy/ \
  ./pkg/services/dns/ \
  ./pkg/services/forwarder/
go build -o /tmp/gvproxy ./cmd/gvproxy
```

在 `helpers/vibe-usernet` 目录下执行：

```bash
go test -count=1 ./...
go build -o /tmp/vibe-usernet .
```

DNS 测试套件中包含针对 `redhat.com` 的既有查询。这需要出站 DNS。新测试则不需要：它们使用本地 UDP 和 TCP 服务器以及极小的 SOCKS5 CONNECT 替代实现。

针对整个子模块的 `go test ./...` 还会构建 QEMU 和 vfkit 集成测试。这些测试需要虚拟机监控程序（hypervisor），不属于本次移植的一部分。单元测试加上 `gvproxy` 和 `vibe-usernet` 的构建足以验证变基是否编译成功以及新路径是否正常工作。

在 2026-10-04，以下测试均已通过：`noproxy`、`netproxy`、`dns`（39 个 ginkgo 规范加上新的上游测试）、`forwarder`、`go build ./cmd/gvproxy`，以及 `vibe-usernet` 的测试和构建。

## 代码编译通过后

搜索新字段，以防将来的签名更改遗漏调用方：

```bash
git grep -n 'dns.New(\|forwarder.TCP(\|forwarder.UDP(' -- '*.go' ':!vendor' ':!tools'
```

`services.go` 应当传递 `configuration.Proxy`、`configuration.ProxyUDP` 和 `configuration.DNSUpstreams`。`vibe-usernet` 仍应设置这些字段。在本次移植时不存在其他生产环境调用方。
