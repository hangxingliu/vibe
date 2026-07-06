# Development Plan for Adding Proxy Support (Socks5/HTTP)

This document outlines how to add proxy support to `virtualnetwork` (provided by `gvisor-tap-vsock`) in `helpers/vibe-usernet`, allowing all TCP/UDP requests from the virtual machine (VM) to be forwarded through a specified HTTP or Socks5 proxy server.

## 1. Introduce `gvisor-tap-vsock` via Git Submodule
Currently, `helpers/vibe-usernet/go.mod` directly references the remote `gvisor-tap-vsock` package. To modify its source code to support a custom proxy Dialer, it must be introduced into the project as a submodule.

**Steps:**
1. Execute the following in the project root:
   ```bash
   git submodule add https://github.com/containers/gvisor-tap-vsock.git helpers/gvisor-tap-vsock
   ```
2. Modify `helpers/vibe-usernet/go.mod` by adding a `replace` directive at the end:
   ```go
   replace github.com/containers/gvisor-tap-vsock => ../gvisor-tap-vsock
   ```
This ensures `vibe-usernet` uses the local `helpers/gvisor-tap-vsock` code during compilation, allowing direct modification of the logic within `gvisor-tap-vsock`.

## 2. Modify `gvisor-tap-vsock` to Support Proxy Requests
We need to replace the direct `net.Dial` in the underlying TCP/UDP forwarder with a proxy-aware Dialer.

**Steps:**
1. **Modify the configuration struct**:
   In `helpers/gvisor-tap-vsock/pkg/types/configuration.go`, add two fields to the `Configuration` struct:
   ```go
   Proxy    string
   ProxyUDP bool
   ```
2. **Modify TCP forwarding logic**:
   In `helpers/gvisor-tap-vsock/pkg/services/forwarder/tcp.go`, modify the `TCP` function (the `Proxy` value must be passed in via `services.go`):
   Locate the following code:
   ```go
   outbound, err := net.Dial("tcp", net.JoinHostPort(localAddress.String(), fmt.Sprint(r.ID().LocalPort)))
   ```
   Change it so that if `Proxy` is configured, a proxy Dialer is used instead of the native `net.Dial`.
3. **Modify UDP forwarding logic**:
   In `helpers/gvisor-tap-vsock/pkg/services/forwarder/udp.go`, modify the `UDP` function. If `ProxyUDP` is `true` and a `socks5` proxy is used, utilize the Socks5 UDP Associate protocol to forward packets to the proxy server instead of using a local connection established via `net.DialUDP` or `net.ListenUDP`.

*(Note: You must pass `configuration.Proxy` and `configuration.ProxyUDP` down to `forwarder.TCP` and `forwarder.UDP` within the `addServices` function in `pkg/virtualnetwork/services.go`.)*

## 3. Add CLI Options to `helpers/vibe-usernet/main.go`
We need to receive the `--proxy` and `--proxy-udp` arguments in `helpers/vibe-usernet/main.go` and pass them to `virtualnetwork.New()`.

**Steps:**
1. Register new flags in the `run` function of `main.go`:
   ```go
   proxy := flag.String("proxy", "", "Proxy URL (e.g., http://127.0.0.1:1080 or socks5://127.0.0.1:1080)")
   proxyUDP := flag.Bool("proxy-udp", false, "Proxy UDP requests (only supported for socks5)")
   ```
2. Pass these configurations into `virtualnetwork.New(&types.Configuration{ ... })`:
   ```go
   Proxy:    *proxy,
   ProxyUDP: *proxyUDP,
   ```

## 4. Add CLI Options to `src/main.rs`
Vibe is written in Rust, and users provide proxy parameters via `vibe run` or `vibe provision`. We need to parse these parameters on the Rust side and pass them to the Go process.

**Steps:**
1. Modify `CliCommand::Run` and related parameter structures in `src/main.rs` (and potentially `CliCommand::Provision`) to include the `--proxy <PROXY_URL>` option (extending the existing `--proxy` logic used for guest configuration) and the new `--proxy-udp` flag.
2. Modify the `NetworkMode::prepare` method in `src/networking.rs` to accept `proxy` and `proxy_udp`.
3. When building the `Command::new(...)` arguments to start `usernet_helper_path` (i.e., `vibe-usernet`), append the following if a proxy is set:
   ```rust
   command.arg("--proxy").arg(proxy_url);
   if proxy_udp {
       command.arg("--proxy-udp");
   }
   ```

## 5. Protocol Handling for `PROXY_URL` (HTTP / SOCKS)
- **HTTP Proxy (`http://`)**:
  - HTTP proxies only support proxying TCP traffic via the `CONNECT` method; they do not support UDP.
  - If a user provides an `http://` proxy and enables `--proxy-udp`, the program should validate this and throw an error, or print a warning and fall back to a direct connection for UDP (throwing an error is recommended to prevent unexpected IP leaks).
- **SOCKS5 Proxy (`socks5://`)**:
  - SOCKS5 natively supports both TCP and UDP proxying.
  - When the protocol is `socks5://` and `--proxy-udp` is disabled, only TCP is proxied, and UDP traffic uses a direct local connection. When enabled, both use the proxy.

## 6. Evaluation of Go Dependencies
To "correctly use HTTP/SOCKS proxies," we need to introduce new Go dependencies (or implement the Dialer protocol ourselves).

1. **HTTP Proxy (CONNECT)**:
   The Go standard library `net/http` provides global HTTP client proxy support by default, but it does not expose the underlying raw HTTP `CONNECT` Dialer for standard TCP Sockets.
   **Conclusion**: Introduce a library like `github.com/magisterquis/connectproxy`, or implement a simple Dial logic (write `CONNECT host:port HTTP/1.1\r\n\r\n` to the TCP connection and read until the `200 Connection established` response).
2. **Socks5 TCP Proxy**:
   `golang.org/x/net/proxy` provides a robust `proxy.SOCKS5` client implementation that can replace `net.Dial` for TCP forwarding. This can be resolved by importing `golang.org/x/net`.
3. **Socks5 UDP Proxy (UDP Associate)**:
   `golang.org/x/net/proxy` does **not** support the Socks5 UDP proxy feature; its interface is limited to stream-based `Dial`. To support `--proxy-udp`, we must implement the Socks5 UDP Associate protocol (connect to the proxy server via TCP to obtain a relay IP:Port, then send UDP packets with Socks5 headers to that relay address).
   **Conclusion**: It is highly recommended to use a library that implements full Socks5 TCP/UDP support, such as `github.com/txthinking/socks5`, rather than the official `x/net/proxy`.

**Dependency Summary**:
During development, run `go get github.com/txthinking/socks5` (or another UDP-capable Socks5 library) and a Dialer library for HTTP `CONNECT` in `helpers/gvisor-tap-vsock/go.mod`.
