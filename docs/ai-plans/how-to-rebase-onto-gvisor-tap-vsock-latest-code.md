# Rebase the gvisor-tap-vsock fork onto upstream main

Vibe's NAT networking is the `helpers/gvisor-tap-vsock` submodule. Upstream is `origin` (`https://github.com/containers/gvisor-tap-vsock`). The fork adds outbound proxy and DNS behavior that `helpers/vibe-usernet` already calls. This note is the procedure that worked when that behavior was brought forward onto upstream `338d9a0f` (2026-10-04). Use it the next time `origin/main` moves.

Do not commit or push as part of the rebase unless someone asked for that.

## What the fork adds

Four commits on `fork/main`, all after `af3ea886` (upstream as of 2026-05-27):

| Commit | What it does |
| --- | --- |
| `b7b89bcb` | HTTP CONNECT and SOCKS5 for outbound TCP. SOCKS5 UDP associate when UDP proxying is on. |
| `5033bec8` | `go mod vendor` for `github.com/txthinking/socks5`. |
| `ca5e2518` | SOCKS5 for forwarded DNS, plus explicit upstream DNS servers. |
| `f5b13c80` | Built-in bypass for loopback, link-local, and private destinations. |

`docs/ai-chats/20261004-065953.patch` is a snapshot of that work against the old tree. `git apply` and a plain cherry-pick both fail once upstream has edited the same functions. Reimplement the behavior on the new tree. The patch is only a spec.

`helpers/vibe-usernet` already passes `Proxy`, `ProxyUDP`, and `DNSUpstreams` into `virtualnetwork.New`. The Rust CLI already has `--proxy`, `--proxy-udp`, and `--dns`. A rebase that only touches the submodule is enough for those flags, followed by `go mod tidy` in `helpers/vibe-usernet`.

## Behavior that must survive the rebase

When `Proxy` is empty and `DNSUpstreams` is empty, forwarding and DNS match upstream. Do not change that path.

- TCP: `http://` and `https://` use HTTP CONNECT. `socks5://` uses SOCKS5 connect. `https://` is CONNECT on a plain TCP connection to the proxy. The proxy session is not TLS.
- UDP: proxied only when `ProxyUDP` is set and the URL is `socks5://`. An HTTP proxy returns an error for UDP rather than sending the datagram directly, so a guest cannot leak traffic by accident.
- DNS: only `socks5://` changes DNS. HTTP proxies cannot carry it. Custom upstreams replace the host resolver. A missing port means 53.
- Bypass: loopback, link-local, RFC1918, IPv6 ULA, and the other ranges in `pkg/services/noproxy` are dialed directly, including DNS upstreams on those networks. `localhost` is included.
- Keep upstream's `TCPMaxInFlight` and `TCPConnectTimeout`. The timeout covers the direct dial and the proxy handshake. It must not become a deadline on the forwarded connection.

## Why the old patch does not apply

Upstream moved the three functions the fork edited.

- `forwarder.TCP` gained `maxInFlight` and `connectTimeout`, and it dials with `net.DialTimeout`. Append `proxy string` after those arguments. Do not drop them and do not go back to `net.Dial`.
- `forwarder.UDP`'s dial callback is now `func(from net.Addr) (net.Conn, error)`. The fork's `func() (net.Conn, error)` does not compile.
- `dns.New` still builds a host resolver, but the handler also forwards SOA, PTR, AAAA, CAA, and other types that `net.Resolver` cannot look up. It does that with `miekg/dns` and `hostNameservers()` (`/etc/resolv.conf`). A SOCKS5 proxy has to cover that path too, or those queries skip the proxy.
- `types.Configuration` gained `TCPMaxInFlight` and `TCPConnectTimeout`. Add `Proxy`, `ProxyUDP`, and `DNSUpstreams` after those fields so the next upstream edit conflicts less.

`NewWithUpstreamResolver` is the seam tests use to inject a fake resolver. Leave its signature and its host-nameserver behavior alone.

## Where the code lives now

| Piece | File |
| --- | --- |
| Bypass policy | `pkg/services/noproxy` |
| HTTP CONNECT and SOCKS5 dialing | `pkg/services/netproxy` |
| TCP and UDP forwarders | `pkg/services/forwarder/tcp.go`, `udp.go` |
| DNS upstream selection | `pkg/services/dns/upstream.go` |
| Hooks in the DNS server | `dns.New` and `exchangeWithNameserver` in `pkg/services/dns/dns.go` |
| Config fields | `pkg/types/configuration.go` |
| Wiring | `pkg/virtualnetwork/services.go` |

`upstream.go` is separate so the next rebase is a small hook in `dns.go` plus this file, not a hand-merge of the whole DNS server.

## Steps

1. In `helpers/gvisor-tap-vsock`, fetch `origin` and check out `origin/main` on a new branch. Do not start from `fork/main`.
2. Read `git diff af3ea886...fork/main -- pkg/ ':!vendor'` (or the patch file) for the intended behavior. Ignore `vendor/`.
3. Read the current `tcp.go`, `udp.go`, `dns.go`, and `services.go` before editing. Port the behavior onto the new signatures.
4. Keep the empty-proxy path calling the same dial the current upstream uses (`DialContext` / the system resolver / `hostNameservers`).
5. Add `github.com/txthinking/socks5` at the version the fork used (`v0.0.0-20260601051520-339b044ab0eb`) unless a newer one is required. `golang.org/x/net/proxy` has no UDP associate, which is why this module is here.
6. From the submodule directory:

```bash
go get github.com/txthinking/socks5@v0.0.0-20260601051520-339b044ab0eb
go mod tidy
go mod vendor
```

The module builds with the vendor directory. A `go.mod` change without `vendor/` fails that build. Do not copy the old `vendor/` tree forward. Upstream has moved other dependencies, and the old vendor commit will downgrade them.

7. In `helpers/vibe-usernet`, run `go mod tidy`. Its `replace` points at the submodule, so its `go.sum` must learn the submodule's current requirements. Expect the `go` line to follow the submodule (`go 1.26.0` at the time of this port) and indirect versions to move with upstream. That is required for `go test` and `go build` there. It is not a drive-by upgrade.
8. Update `dns.New` call sites. The ginkgo tests in `pkg/services/dns/dns_test.go` call `New` and need `"", nil` for the new arguments.
9. Update the submodule README, `cmd/gvproxy/config.yaml` comments, and the `--proxy` / `--dns` help in `src/main.rs` and `readme.md` if the bypass rules changed.
10. Run the tests below. Do not commit unless asked.

## Traps that showed up this time

**The Go resolver does not dial the address you pass to `Dial`.** With `PreferGo: true` it still reads `/etc/resolv.conf`, then calls `Dial(ctx, network, systemNameserver)`. To honor `DNSUpstreams`, ignore that address and dial the configured list yourself. If the list is empty and a SOCKS5 proxy is set, use `hostNameservers()`, then `8.8.8.8:53` when the host has none.

**TCP versus UDP DNS framing follows the connection type, not the network string.** `net.Resolver` and `miekg/dns` use a length prefix when the conn is not a `net.PacketConn`. SOCKS5 DNS therefore dials TCP and returns that stream conn, even if the resolver asked for `"udp"`. A `*net.TCPConn` is not a `PacketConn` (`ReadFrom` there is `io.ReaderFrom`). Do not wrap it in something that implements `PacketConn`, or the length prefix will be omitted and the query will fail.

**`socks5.NewClient`'s timeout arguments are deadlines on the finished connection.** They are in whole seconds. Set them so a proxy that accepts TCP and then stalls cannot hang the dial, then clear the deadline on the returned conn (and on `TCPConn` for UDP associate) before returning it. Otherwise every proxied flow dies when `TCPConnectTimeout` elapses.

**`http.ReadResponse` uses a `bufio.Reader`.** Bytes the proxy sends immediately after the CONNECT response sit in that buffer. Return a conn whose `Read` drains the same reader. Also send both `Authorization` and `Proxy-Authorization` when the URL has userinfo. Proxies look at `Proxy-Authorization`.

**Default the port with `net.JoinHostPort`.** `host + ":1080"` is wrong for `[::1]`. Strip brackets, then join. HTTP defaults to 8080, SOCKS5 to 1080, matching the old fork.

**Tests that listen on 127.0.0.1 do not prove the proxy was used.** Loopback is bypassed, so a "successful" dial never touched the proxy. Point the client at a public address such as `93.184.216.34:80` or `8.8.8.8:53`, and make the fake proxy relay that CONNECT to a local server. Assert that a dead or counting proxy was actually contacted. The same trap applies to DNS: a `127.0.0.1` upstream is supposed to bypass SOCKS5.

**Do not mutate the caller's `DNSUpstreams` slice.** The old code appended `:53` in place. Copy the slice.

## Tests

From `helpers/gvisor-tap-vsock`:

```bash
go test -count=1 -timeout 180s -race \
  ./pkg/services/noproxy/ \
  ./pkg/services/netproxy/ \
  ./pkg/services/dns/ \
  ./pkg/services/forwarder/
go build -o /tmp/gvproxy ./cmd/gvproxy
```

From `helpers/vibe-usernet`:

```bash
go test -count=1 ./...
go build -o /tmp/vibe-usernet .
```

The DNS suite contains an existing lookup of `redhat.com`. It needs outbound DNS. The new tests do not: they use local UDP and TCP servers and a tiny SOCKS5 CONNECT stand-in.

`go test ./...` for the whole submodule also builds QEMU and vfkit integration tests. Those need a hypervisor and were not part of this port. Unit tests plus `gvproxy` and `vibe-usernet` builds are the check that the rebase compiled and that the new paths work.

On 2026-10-04 that set passed: `noproxy`, `netproxy`, `dns` (39 ginkgo specs plus the new upstream tests), `forwarder`, `go build ./cmd/gvproxy`, and `vibe-usernet`'s tests and build.

## After the code compiles

Search for the new fields so a future signature change cannot leave a caller behind:

```bash
git grep -n 'dns.New(\|forwarder.TCP(\|forwarder.UDP(' -- '*.go' ':!vendor' ':!tools'
```

`services.go` should pass `configuration.Proxy`, `configuration.ProxyUDP`, and `configuration.DNSUpstreams`. `vibe-usernet` should still set those fields. No other production caller existed at this port.
