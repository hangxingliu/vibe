# Networking and Proxy

This document explains how `vibe` networking parameters and environment variables affect various network requests: in-VM TCP/UDP/DNS/ICMP, gateway to host, `-p` port forwarding, host-side image downloads, and `vznat` mode.

## Parameter Overview

| Parameter | Scope | Default |
| --- | --- | --- |
| `--proxy <URL>` | VM traffic and host downloads | Proxy from environment variables (see below); no proxy if none are set |
| `--proxy-bypass <LIST>` | VM traffic and host downloads | `no_proxy` > `NO_PROXY` > built-in list |
| `--[no-]proxy-udp` | VM traffic only (`nat`) | Enabled for SOCKS5, disabled for HTTP |
| `--[no-]proxy-dns` | VM gateway DNS only (`nat`) | Enabled for SOCKS5, disabled for HTTP |
| `--[no-]strict-proxy` | VM traffic only (`nat`) | Disabled |
| `--no-proxy-vm` | VM traffic uses no proxy configurations | Disabled |
| `--dns <ADDR>` | Upstream for VM gateway DNS | See "Gateway DNS" |
| `--network <nat\|vznat>` | Network mode | `nat` |

When the same flag appears multiple times, the last one takes effect (for example, `--proxy-udp --no-proxy-udp` is equivalent to `--no-proxy-udp`). Proxy-related parameters can be placed before or after `provision`.

## Where Proxies Come From

1. `--proxy`.
2. When `--proxy` is not set, it reads `https_proxy`, `HTTPS_PROXY`, `http_proxy`, and `HTTP_PROXY` in order, using the first non-empty value.
3. `ALL_PROXY`/`all_proxy` are not read.

URL rules:

- If the scheme is missing, `http://` is appended (`192.168.1.88:3003` is equivalent to `http://192.168.1.88:3003`).
- Supports `http://`, `https://`, `socks5://`, and `socks5h://`. On the VM side, `socks5h://` is treated as `socks5://` (the forwarding layer receives only IPs); for host downloads, `socks5h://` is preserved, and the proxy resolves domain names.
- `socks4://`, `socks://`, and other schemes will produce an error. An error is also raised when the proxy comes from environment variables (can be overridden with `--proxy`).
- Usernames and passwords in the output are always displayed as `***@`.

## Bypass List

Only one source is used without merging: `--proxy-bypass` > `no_proxy` > `NO_PROXY` > built-in list. A user-provided list **replaces** the built-in list. `--proxy-bypass ""` indicates an empty list (all traffic goes through the proxy except for the gateway).

Built-in list: `localhost`, `localhost.localdomain`, and `0.0.0.0/8`, `10.0.0.0/8`, `127.0.0.0/8`, `169.254.0.0/16`, `172.16.0.0/12`, `192.168.0.0/16`, `100.64.0.0/10`, `192.0.0.0/24`, `198.18.0.0/15`, `224.0.0.0/4`, `255.255.255.255/32`, `::1/128`, `fc00::/7`, `fe80::/10`, `ff00::/8`.

Formats (separated by `,`, ignoring surrounding whitespace, case-insensitive):

| Format | VM Traffic | Host Downloads |
| --- | --- | --- |
| IP, CIDR (`10.1.2.3`, `10.0.0.0/8`, `::1`) | Supported | Supported |
| IPv4 prefix (`192.168.*`, `10.*`) | Supported | Supported (converted to CIDR for curl) |
| `localhost`, `localhost.localdomain` | Equivalent to `127.0.0.0/8` and `::1/128` | Supported |
| Domain (`example.com`, `.example.com`, `*.example.com`) | Not supported, prints a warning | Supported |
| With port (`10.1.2.3:8080`, `[::1]:8080`) | Matches only when ports match | Not supported by curl, prints a warning and ignores |
| `*` | Direct for all, prints a warning | Direct for all, prints a warning |
| Unresolvable entries | Prints a warning (indicating source) and ignores | Same as left |

Regardless of the list, traffic to the gateway `192.168.5.2` (NAT rewritten to host `127.0.0.1`) is always direct.

## VM Traffic in `nat` Mode

In `nat` mode, the built-in `vibe-usernet` forwards VM traffic, allowing `vibe` to enforce proxy settings.

| Traffic | No Proxy | Proxy, Non-strict Mode (Default) | Proxy, Strict Mode (`--strict-proxy`) |
| --- | --- | --- | --- |
| Gateway `192.168.5.2` to host | Direct | Direct | Direct |
| TCP, destination in bypass list | Direct | Direct | Direct |
| TCP, other destinations | Direct | Proxied, no direct fallback on failure | Same as left |
| UDP, destination in bypass list | Direct | Direct | Direct |
| UDP, other destinations, UDP proxy enabled | Direct | SOCKS5 proxied, dropped on failure | Same as left |
| UDP, other destinations, UDP proxy disabled | Direct | Direct, warning at startup | Dropped |
| ICMP Echo (ping), destination in bypass list | Direct | Direct | Direct |
| ICMP Echo, other destinations | Direct | Direct | Dropped |
| Link-local (`169.254.0.0/16`) TCP/UDP | Dropped | Dropped | Dropped |

UDP proxy disabled refers to an HTTP proxy (which can only carry TCP) or a SOCKS5 proxy with `--no-proxy-udp`.

### Gateway DNS

`--[no-]proxy-dns` only controls DNS queries sent by the guest to the gateway `192.168.5.2:53`. DNS queries sent directly by the guest to other servers (such as `dig @8.8.8.8`) are standard UDP/TCP traffic handled according to the table above: with SOCKS5 and UDP proxy enabled, `dig @8.8.8.8` is also proxied.

Upstream list: Addresses specified by `--dns`; if `--dns` is not set, nameservers in the host `/etc/resolv.conf` are used.

| Scenario | Gateway DNS Handling |
| --- | --- |
| No proxy | Direct to upstreams if `--dns` is set; otherwise uses host system resolver |
| DNS proxy enabled (default for SOCKS5) | Evaluated per upstream: direct if in bypass list, otherwise proxied (DNS over TCP). Upstreams specified by `--dns` are evaluated the same way |
| DNS proxy disabled, non-strict mode | Not proxied: direct to upstreams if `--dns` is set, otherwise uses host system resolver. Warning at startup |
| DNS proxy disabled, strict mode | Only uses upstreams in the bypass list; if none exist, all queries return `REFUSED`, with a warning at startup |

DNS proxy disabled refers to an HTTP proxy or a SOCKS5 proxy with `--no-proxy-dns`.

When a proxied upstream fails at runtime (proxy unreachable, proxy rejected, upstream timeout), it returns `SERVFAIL` without falling back to direct connection or the host system resolver. For configured upstreams, each query waits for up to 4 seconds to ensure the guest resolver (glibc default timeout is 5 seconds) receives `SERVFAIL` instead of timing out on its own. Queries for local zones (such as `host.vibe.internal`) are unaffected.

At startup, upstreams and their connection methods (direct or proxied) are listed individually, for example:

    VM network DNS: upstreams from /etc/resolv.conf: 192.168.1.88:53 (direct, in bypass list), 100.100.100.100:53 (via proxy)

Note: Since user-provided bypass lists replace the built-in list, LAN or link-local DNS on the host (such as `fe80::...%en0`) will be proxied if not included in the list, and proxies usually cannot reach them. In this case, use `--dns` to specify a proxy-accessible upstream or add them to the bypass list.

### `-p` Port Forwarding

`-p`/`--publish` connections enter the VM from the host and are unaffected by proxy settings.

## `vznat` Mode

`vznat` uses Apple's `VZNATNetworkDeviceAttachment`. Since `vibe` cannot inspect VM traffic, it cannot enforce proxies.

- Proxies (`--proxy` or environment variables) are exported upon login to the guest as `http_proxy`, `https_proxy`, `HTTP_PROXY`, and `HTTPS_PROXY`; the bypass list is exported as-is as `no_proxy` and `NO_PROXY` (the text representation of the built-in list is exported if no source is provided). Warning at startup: Proxying relies entirely on guest applications respecting these variables; ensure the proxy server is reachable from within the guest. Loopback addresses like `127.0.0.1:xxx` are also allowed.
- `vibe provision` writes the proxy into the new image's `/etc/apt/apt.conf.d/99timeout` (`Acquire::http::Proxy`, `Acquire::https::Proxy`), regardless of whether the proxy comes from `--proxy` or environment variables. SOCKS5 proxies are written as `socks5h://` (the only format apt supports). The raw value containing username and password is written; the `Image:` line in the startup summary indicates this.
- `--proxy-udp`, `--proxy-dns`, and `--strict-proxy` cause an error under `vznat`; their negative counterparts are allowed.

In `nat` mode, provisioning does not write proxies into the image.

## Host-side Image Downloads

When downloading the Debian base image and the Alpine maintenance image, `vibe` invokes curl:

- Proxy: `--proxy` > environment variables (in the same order as above). When no proxy is set, `--proxy ""` is explicitly passed so curl does not read variables like `ALL_PROXY`.
- Bypass: `--proxy-bypass` > `no_proxy` > `NO_PROXY`, passed to curl as `--noproxy`. Domains are valid; entries with ports are ignored with a warning. If no sources exist, `--noproxy` is omitted.
- `--no-proxy-vm` does not affect downloads.
- Prints a `Host downloads proxy: ...` line before downloading.

## `--no-proxy-vm`

Internal VM traffic uses no proxy configurations (all traffic is direct under `nat`, proxy variables are not exported under `vznat`, and provisioning does not write proxies). `--proxy`, `--proxy-bypass`, and environment variables are still used for host downloads.

## Invalid Combinations

- `--no-proxy-vm` with `--proxy-udp`, `--proxy-dns`, or `--strict-proxy`.
- Using `--proxy-udp`, `--proxy-dns`, or `--strict-proxy` when the VM has no proxy (neither `--proxy` nor environment variables). In this case, `--proxy-bypass` only prints a warning.
- HTTP proxy with `--proxy-udp` or `--proxy-dns` (errors out even if the proxy originates from environment variables).
- `--network vznat` with `--proxy-udp`, `--proxy-dns`, or `--strict-proxy`.
- Unsupported proxy schemes (`socks4://`, etc.).

## Startup Summary

A summary is printed before starting the VM on every run (including `provision`), and warnings are written to stderr starting with `warning: `. For example, when local environment variables specify an HTTP proxy:

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

## Common Combinations

| Command | Effect |
| --- | --- |
| `vibe` (env var is HTTP proxy, `no_proxy=127.0.0.1,localhost,192.168.*`) | TCP is proxied via HTTP; UDP, gateway DNS, and ICMP are direct with warnings; `192.168.*` is direct |
| `vibe --proxy socks5://PROXY:1080` | TCP, UDP, and gateway DNS are proxied; ICMP is direct |
| `vibe --proxy socks5://PROXY:1080 --strict-proxy --dns LAN_DNS` | Same as above; ICMP is only sent to the bypass list; LAN_DNS is direct if in the list, while other upstreams are proxied |
| `vibe --strict-proxy` (env var is HTTP proxy) | TCP is proxied; UDP and ICMP outside the bypass list are dropped; gateway DNS only uses upstreams in the list, returning `REFUSED` if none exist |
| `vibe --proxy socks5://PROXY:1080 --proxy-bypass ''` | All destinations go through the proxy except the gateway, including LAN and loopback addresses |
| `vibe --no-proxy-vm` | VM traffic is not proxied; image downloads still use the proxy from environment variables |
| `vibe --network vznat` | Environment variable proxies are exported to the guest; `vibe` does not enforce them |

## Troubleshooting

- Check the startup summary first: where the proxy and bypass list originate, how each traffic type is handled, and whether each upstream in the `VM network DNS:` line is direct, proxied, or skipped.
- When gateway DNS fails:
  - If the upstream shows `via proxy`, verify that the proxy can reach it. LAN or link-local DNS often cannot be reached via proxies; add them to the bypass list or specify alternative upstreams using `--dns`.
  - Queries returning `SERVFAIL` usually indicate an unreachable proxy or upstream.
  - Queries returning `REFUSED` indicate that no upstreams are available in strict mode.
- `vibe-usernet` error logs are written to `.vibe/vibe-usernet.log` in the instance directory (when using `.vibe/instance.raw`).
- Run `lsof -nP -a -p <vibe-usernet pid> -i` on the host to inspect external helper connections: no connections should exist other than those to the proxy and addresses in the bypass list.
