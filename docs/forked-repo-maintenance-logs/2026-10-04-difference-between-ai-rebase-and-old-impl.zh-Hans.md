# 2026-10-04 AI rebase 与旧实现的差异

``` bash
cd helpers/gvisor-tap-vsock/
git diff origin/main...HEAD > ../20261004-ai-rebase.diff
git diff af3ea886ffe298dd75c94980b40fea8cfc715ebe...f5b13c8065cc202f1dc7a726f1b2a7295606260b > ../20260800-old-impl.diff
```

---

对比的两份补丁：

| | 旧实现 | AI rebase |
| --- | --- | --- |
| 文件 | `helpers/20260800-old-impl.diff` | `helpers/20261004-ai-rebase.diff` |
| 行数 | 4445 | 5150 |
| 基线 | 2026-05 前后的上游。`forwarder.TCP` 使用 `net.Dial`，签名里没有并发上限和连接超时 | 上游 `338d9a0f`（2026-10-04）。当前子模块提交 `3e10df39` 就是这份补丁 |

两份补丁给 `helpers/gvisor-tap-vsock` 加的是同一组能力，没有新的用户可见开关。`helpers/vibe-usernet` 和 Rust CLI 里已有的 `Proxy`、`ProxyUDP`、`DNSUpstreams`（`--proxy`、`--proxy-udp`、`--dns`）仍然对得上。

变基步骤见 [how-to-rebase-onto-gvisor-tap-vsock-latest-code.zh-Hans.md](how-to-rebase-onto-gvisor-tap-vsock-latest-code.zh-Hans.md)。本文只说明两份实现差在哪里。

## 两边相同的部分

配置字段和 YAML 名字相同：`proxy`、`proxyUDP`、`dnsUpstreams`。空 `Proxy` 且空 `DNSUpstreams` 时，转发和 DNS 走上游原来的路径。

- TCP：`http://` 和 `https://` 走 HTTP CONNECT，`socks5://` 走 SOCKS5 connect。旧代码的 `switch` 已经接受 `https`；新注释写明代理会话本身不是 TLS。
- UDP：只有 `ProxyUDP` 打开且 URL 是 `socks5://` 才走代理。HTTP 代理返回错误，数据报不会因此改成直连。
- DNS：只有 `socks5://` 会改变 DNS。HTTP 代理承载不了它，被忽略。自定义上游替换主机解析器，缺端口时用 53。SOCKS5 上的 DNS 走 TCP，不用 UDP associate。
- 绕过：`pkg/services/noproxy` 的 CIDR 和 `localhost` 规则两边一致，逻辑逐行相同。环回、链路本地、RFC1918、IPv6 ULA、CGNAT 等目标直连，包括落在这些网络上的 DNS 上游。
- 依赖：都是 `github.com/txthinking/socks5` `v0.0.0-20260601051520-339b044ab0eb`，外加间接依赖 `patrickmn/go-cache` 和 `txthinking/runnergroup`。`vendor/` 里这三个模块的文件内容相同。`go.sum` 新增的行也相同。新补丁没有换库，也没有把上游其余依赖退回旧版本。

## 代码放在哪里

旧实现把拨号写在转发器里面。新实现把拨号和 DNS 上游选择拆出去，下次变基时不用再合并整个 `dns.go`。

| 职责 | 旧实现 | AI rebase |
| --- | --- | --- |
| HTTP CONNECT、SOCKS5 | `pkg/services/forwarder/proxy.go`，以及 `tcp.go` / `udp.go` 里的 `dialTCP`、`makeSocks5UDPDialer` | `pkg/services/netproxy` 的 `DialTCP`、`DialUDP`、`DialSOCKS5` |
| 绕过 | `pkg/services/noproxy` | 同左 |
| DNS 上游选择 | `dns.go` 里的 `buildUpstreamResolver`、`systemNameservers` | `pkg/services/dns/upstream.go` 的 `buildUpstreamSpec` |
| 转发器 | 直接拨号 | 转调 `netproxy` |
| 组装 | `dnsServer` 增加 `proxy`、`dnsUpstreams` 参数 | `dnsServer` 签名不动，在 `dns.New` 处传入配置字段 |
| 说明 | 无 | 子模块 `README.md`、`cmd/gvproxy/config.yaml` 注释 |

## 为了跟上上游改的签名

旧补丁没见过 2026 年夏天上游的三处改动。新补丁按新签名接上，没有把这些改动退回去。

- `afe8c1b2`（2026-07-15）之后，DNS 处理程序会把 `net.Resolver` 无法查询的类型（SOA、PTR、AAAA、CAA 等）用 `miekg/dns` 和 `hostNameservers()` 原样转发。
- `f9306b96`（2026-08-19）把 UDP 的拨号回调改成 `func(from net.Addr) (net.Conn, error)`。上游自己也不用 `from` 选目标。新补丁同样忽略它，目标仍是客户机请求里的地址。`udp_proxy.go` 没改，源地址传递还在。
- `e86bba2e`（2026-08-25）给 TCP 增加 `TCPMaxInFlight`（默认 128）和 `TCPConnectTimeout`（默认 30 秒）。旧转发器把并发上限写死为 10，拨号用没有超时的 `net.Dial`。

`Configuration` 里三个新字段加在 `TCPConnectTimeout` 后面，减少下次上游改这个结构体时的冲突。

## 行为上的实质差异

这些是新实现和旧实现不一样的地方。对外字段没变，出错的是旧代码在真实代理和 DNS 上的细节。

**连接超时覆盖握手，然后被清掉。** 旧 SOCKS5 客户端用超时 0，握手可以一直挂着。`txthinking/socks5` 把这个超时写成已建立连接上的截止时间。新代码从 `context` 取出 `TCPConnectTimeout`，拨号时交给客户端，返回连接前再把截止时间清掉。HTTP CONNECT 同样只在握手期间设置截止时间。超时不会变成已转发连接的寿命。UDP 拨号仍使用 `context.Background()`，和旧实现一样没有单独的超时。

**HTTP CONNECT 响应之后的字节会留下来。** 旧的 `dialHTTPConnect` 用 `bufio.Reader` 读响应，然后把原始 `net.Conn` 交回去。代理把后续数据跟 `200` 写在同一次发送里时，这些字节留在缓冲区里，调用方永远读不到。新代码返回一个从该 `Reader` 继续读的连接。

**代理认证同时带两个头。** 旧代码只调用 `SetBasicAuth`，发出 `Authorization`。新代码再设一份 `Proxy-Authorization`。只认后者的代理，旧实现认证失败。

**缺端口的地址用 `net.JoinHostPort`。** 旧代码把 `":1080"` 或 `":8080"` 直接拼到主机后面。`[::1]` 碰巧还能用，裸的 `::1` 会拼成 `::1:1080`。DNS 上游对已经带方括号的地址调用 `net.JoinHostPort`，`[2001:db8::1]` 会变成 `[[2001:db8::1]]:53`。新代码先去掉方括号再拼接。HTTP 默认端口仍是 8080，SOCKS5 仍是 1080。

**自定义 DNS 列表不再改调用方的切片。** 旧代码在传入的 `dnsUpstreams` 上原地补 `:53`，配置里的值会被改掉，空白项会变成 `:53`。新代码复制一份，去掉空白，再补端口。

**SOCKS5 和自定义上游同时覆盖两条 DNS 路径。** 旧的 `buildUpstreamResolver` 只替换 `net.Resolver`。A、CNAME、MX、NS、SRV、TXT 走这条路径。上游在 2026-07 增加的原始查询仍由 `NewWithUpstreamResolver` 填上主机的 `hostNameservers()`，直接 `Exchange`，既不看 `DNSUpstreams`，也不进 SOCKS5。新的 `dns.New` 在需要定制时自己组装 `dnsHandler`：`nameservers` 换成选好的上游，SOCKS5 时再装上 `dialNameserver`。`exchangeWithNameserver` 用这个拨号函数发原始查询。

**没有自定义上游时，主机 DNS 列表改用上游已有的解析。** 旧代码自己扫 `/etc/resolv.conf`，只认 `nameserver` 后的 IP，端口固定 53，扫描出错就整表丢弃。新代码调用已有的 `hostNameservers()`（`miekg` 的 `ClientConfigFromFile`），端口跟 `resolv.conf` 里的 `port` 走。列表仍为空时，两边都退到 `8.8.8.8:53`。

**直连 DNS 保留调用方要的传输。** 旧的解析器拨号在不走 SOCKS5 时一律用 UDP。新代码在调用方要求 TCP 时用 TCP，否则用 UDP。走 SOCKS5 时两边都强制 TCP：返回的连接不是 `PacketConn`，`net.Resolver` 和 `miekg/dns` 才会写长度前缀。原始查询在装了 `dialNameserver` 之后固定请求 TCP，被绕过的本地上游也是 TCP。`net.Resolver` 查到本地上游时仍可以走 UDP。

**TCP 端点创建失败时关掉已经拨出的连接。** 这是新补丁在 `CreateEndpoint` 失败路径上补的关闭。旧补丁没有。

## 测试

旧补丁只有 `forwarder/tcp_noproxy_test.go`：环回地址在代理不可达时仍能连上，公网地址 `93.184.216.34:80` 拨号失败。后一个断言证明不了流量进过代理，连接失败的原因可以是别的。

新补丁删掉这个测试，改在拨号层覆盖：

- `pkg/services/netproxy/dial_test.go`：绕过、真正的 HTTP CONNECT 回显、CONNECT 响应后的提前到达字节、`Authorization` 与 `Proxy-Authorization`、UDP 绕过、HTTP 代理拒绝 UDP、IPv6 默认端口。
- `pkg/services/dns/upstream_test.go`：空代理和 HTTP 代理不安装 DNS 拨号函数；上游地址规范化（含空白项和 `[2001:db8::1]`）；自定义上游能回答 A 和 SOA；SOCKS5 对 `8.8.8.8` 会计数，环回上游则一次都不进代理。

`dns_test.go` 里对 `dns.New` 的调用都补了 `"", nil`。旧补丁改了两处，新补丁改了四处，因为新基线上多了 Protected zone 相关用例。

公网地址加本地假代理是故意的。测试若只监听 `127.0.0.1`，绕过规则会让这次连接看起来成功，代理却没被用到。
