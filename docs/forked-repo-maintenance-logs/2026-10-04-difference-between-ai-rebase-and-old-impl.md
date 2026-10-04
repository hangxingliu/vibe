# Differences Between 2026-10-04 AI Rebase and the Old Implementation

``` bash
cd helpers/gvisor-tap-vsock/
git diff origin/main...HEAD > ../20261004-ai-rebase.diff
git diff af3ea886ffe298dd75c94980b40fea8cfc715ebe...f5b13c8065cc202f1dc7a726f1b2a7295606260b > ../20260800-old-impl.diff
```

---

Comparison of the two patches:

| | Old Implementation | AI Rebase |
| --- | --- | --- |
| File | `helpers/20260800-old-impl.diff` | `helpers/20261004-ai-rebase.diff` |
| Lines | 4445 | 5150 |
| Baseline | Upstream around 2026-05. `forwarder.TCP` uses `net.Dial`, without concurrency limits or connection timeouts in the signature | Upstream `338d9a0f` (2026-10-04). Current submodule commit `3e10df39` is this patch |

Both patches add the same set of capabilities to `helpers/gvisor-tap-vsock` without introducing any new user-facing toggles. Existing `Proxy`, `ProxyUDP`, and `DNSUpstreams` options (`--proxy`, `--proxy-udp`, `--dns`) in `helpers/vibe-usernet` and the Rust CLI remain consistent.

For rebase steps, see [how-to-rebase-onto-gvisor-tap-vsock-latest-code.zh-Hans.md](how-to-rebase-onto-gvisor-tap-vsock-latest-code.zh-Hans.md). This document only highlights the differences between the two implementations.

## Common Parts

Configuration fields and YAML keys are identical: `proxy`, `proxyUDP`, and `dnsUpstreams`. When both `Proxy` and `DNSUpstreams` are empty, forwarding and DNS follow the original upstream paths.

- TCP: `http://` and `https://` use HTTP CONNECT; `socks5://` uses SOCKS5 connect. The old code's `switch` already accepted `https`; the new comment clarifies that the proxy session itself is not TLS.
- UDP: Only routes through the proxy when `ProxyUDP` is enabled and the URL scheme is `socks5://`. HTTP proxies return an error, and datagrams do not fall back to direct connections.
- DNS: Only `socks5://` alters DNS routing. HTTP proxies cannot carry it and are ignored. Custom upstreams replace the host resolver, defaulting to port 53 if missing. DNS over SOCKS5 uses TCP instead of UDP associate.
- Bypass: CIDR and `localhost` rules in `pkg/services/noproxy` are identical between both versions, matching line by line. Loopback, link-local, RFC1918, IPv6 ULA, CGNAT, and other destinations connect directly, including DNS upstreams within these networks.
- Dependencies: Both use `github.com/txthinking/socks5` `v0.0.0-20260601051520-339b044ab0eb`, along with indirect dependencies `patrickmn/go-cache` and `txthinking/runnergroup`. The files for these three modules in `vendor/` are identical. Newly added lines in `go.sum` are also identical. The new patch neither replaces libraries nor rolls back remaining upstream dependencies to older versions.

## Code Organization

The old implementation wrote dialers directly inside the forwarder. The new implementation splits out the dialers and DNS upstream selection, avoiding the need to merge the entire `dns.go` during future rebases.

| Responsibility | Old Implementation | AI Rebase |
| --- | --- | --- |
| HTTP CONNECT, SOCKS5 | `pkg/services/forwarder/proxy.go`, and `dialTCP`, `makeSocks5UDPDialer` in `tcp.go` / `udp.go` | `DialTCP`, `DialUDP`, `DialSOCKS5` in `pkg/services/netproxy` |
| Bypass | `pkg/services/noproxy` | Same as left |
| DNS Upstream Selection | `buildUpstreamResolver`, `systemNameservers` in `dns.go` | `buildUpstreamSpec` in `pkg/services/dns/upstream.go` |
| Forwarder | Dials directly | Delegates to `netproxy` |
| Assembly | Added `proxy` and `dnsUpstreams` parameters to `dnsServer` | Keeps `dnsServer` signature unchanged, passing config fields into `dns.New` |
| Documentation | None | Submodule `README.md`, comments in `cmd/gvproxy/config.yaml` |

## Signature Changes to Track Upstream

The old patch was unaware of three upstream changes made in the summer of 2026. The new patch adapts to the new signatures without reverting those upstream changes.

- Since `afe8c1b2` (2026-07-15), the DNS handler forwards query types unsupported by `net.Resolver` (SOA, PTR, AAAA, CAA, etc.) as-is using `miekg/dns` and `hostNameservers()`.
- `f9306b96` (2026-08-19) changed the UDP dial callback to `func(from net.Addr) (net.Conn, error)`. Upstream itself does not use `from` to select targets. The new patch likewise ignores it; the target remains the address from the client request. `udp_proxy.go` was not modified, so source address forwarding is preserved.
- `e86bba2e` (2026-08-25) added `TCPMaxInFlight` (default 128) and `TCPConnectTimeout` (default 30 seconds) to TCP. The old forwarder hardcoded the concurrency limit to 10 and dialed with `net.Dial` without a timeout.

The three new fields in `Configuration` are added after `TCPConnectTimeout` to minimize future merge conflicts when upstream modifies this struct.

## Substantive Behavioral Differences

These are the differences between the new and old implementations. External fields remain the same, but the old code had issues handling real-world proxy and DNS edge cases.

**Connection timeout covers the handshake, then gets cleared.** The old SOCKS5 client used a timeout of 0, which could cause handshakes to hang indefinitely. `txthinking/socks5` sets this timeout as the deadline on the established connection. The new code retrieves `TCPConnectTimeout` from `context`, passes it to the client during dialing, and clears the deadline before returning the connection. HTTP CONNECT similarly sets a deadline only during the handshake phase. Timeouts do not become the lifetime of the forwarded connection. UDP dialing still uses `context.Background()` with no separate timeout, matching the old implementation.

**Bytes following the HTTP CONNECT response are preserved.** The old `dialHTTPConnect` used `bufio.Reader` to read the response, then returned the raw `net.Conn`. If the proxy sent subsequent data along with the `200` response in a single transmission, those bytes remained in the buffer and were never read by the caller. The new code returns a connection that continues reading from that `Reader`.

**Proxy authentication includes both headers.** The old code only called `SetBasicAuth`, sending `Authorization`. The new code also sets `Proxy-Authorization`. Proxies that only accept the latter failed authentication under the old implementation.

**Addresses missing ports use `net.JoinHostPort`.** The old code directly concatenated `":1080"` or `":8080"` onto the host. While `[::1]` worked by chance, a bare `::1` became `::1:1080`. For DNS upstreams, calling `net.JoinHostPort` on addresses already containing square brackets turned `[2001:db8::1]` into `[[2001:db8::1]]:53`. The new code strips square brackets before joining. The default port remains 8080 for HTTP and 1080 for SOCKS5.

**Custom DNS lists no longer mutate the caller's slice.** The old code appended `:53` in place to the provided `dnsUpstreams`, mutating values in the configuration and turning empty entries into `:53`. The new code makes a copy, removes empty items, and then appends the port.

**SOCKS5 and custom upstreams cover both DNS paths simultaneously.** The old `buildUpstreamResolver` only replaced `net.Resolver`. A, CNAME, MX, NS, SRV, and TXT queries followed this path. Raw queries added upstream in 2026-07 were still populated with the host's `hostNameservers()` via `NewWithUpstreamResolver` and sent directly via `Exchange`, ignoring both `DNSUpstreams` and SOCKS5. The new `dns.New` assembles `dnsHandler` directly when customization is needed: replacing `nameservers` with selected upstreams and attaching `dialNameserver` when using SOCKS5. `exchangeWithNameserver` uses this dial function to send raw queries.

**Host DNS list falls back to upstream's existing parser when no custom upstreams are set.** The old code scanned `/etc/resolv.conf` manually, only recognizing the IP after `nameserver` with a fixed port 53, and discarded the entire list if scanning failed. The new code calls the existing `hostNameservers()` (`ClientConfigFromFile` from `miekg`), respecting the `port` specified in `resolv.conf`. When the list remains empty, both fall back to `8.8.8.8:53`.

**Direct DNS preserves the transport requested by the caller.** The old resolver dialer always used UDP when bypassing SOCKS5. The new code uses TCP if requested by the caller, and UDP otherwise. When routing through SOCKS5, both force TCP: since the returned connection is not a `PacketConn`, `net.Resolver` and `miekg/dns` will write length prefixes. Raw queries always request TCP once `dialNameserver` is attached, even for bypassed local upstreams. `net.Resolver` can still use UDP when querying local upstreams.

**Closes dialed connections when TCP endpoint creation fails.** The new patch adds connection teardown in the `CreateEndpoint` failure path. The old patch lacked this.

## Testing

The old patch only included `forwarder/tcp_noproxy_test.go`: loopback addresses could still connect when the proxy was unreachable, and dialing public IP `93.184.216.34:80` failed. The latter assertion did not prove that traffic routed through the proxy, as connection failure could stem from other causes.

The new patch removes that test and provides coverage at the dialer layer instead:

- `pkg/services/netproxy/dial_test.go`: Bypassing, genuine HTTP CONNECT echo, early-arriving bytes after CONNECT response, `Authorization` and `Proxy-Authorization`, UDP bypassing, HTTP proxy rejecting UDP, and IPv6 default ports.
- `pkg/services/dns/upstream_test.go`: Ensuring empty and HTTP proxies do not install a DNS dial function; upstream address normalization (including blank items and `[2001:db8::1]`); verifying custom upstreams can answer A and SOA queries; verifying SOCKS5 increments query counts for `8.8.8.8` while loopback upstreams never touch the proxy.

Calls to `dns.New` in `dns_test.go` were updated with `"", nil`. The old patch modified two locations; the new patch modified four, as the new baseline introduced test cases related to protected zones.

Using public IP addresses paired with local mock proxies is intentional. If tests only listened on `127.0.0.1`, bypass rules would make connections appear successful even if the proxy was never used.
