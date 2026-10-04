# 添加代理支持（Socks5/HTTP）开发计划

该实现目前位于 `helpers/gvisor-tap-vsock` 子模块中，基于当前的 upstream `main` 分支。对于下一次 upstream 更新，请参考 [docs/how-to-rebase-onto-gvisor-tap-vsock-latest-code.md](../docs/how-to-rebase-onto-gvisor-tap-vsock-latest-code.md)。本说明为原始计划，并非仍然适用的 patch。

本文档概述了如何在 `helpers/vibe-usernet` 中为 `virtualnetwork`（由 `gvisor-tap-vsock` 提供）添加代理支持，从而允许将虚拟机（VM）发出的所有 TCP/UDP 请求通过指定的 HTTP 或 Socks5 代理服务器进行转发。

## 1. 通过 Git Submodule 引入 `gvisor-tap-vsock`
目前，`helpers/vibe-usernet/go.mod` 直接引用远程的 `gvisor-tap-vsock` 包。为了修改其源码以支持自定义代理 Dialer，必须将其作为 submodule 引入项目中。

**步骤：**
1. 在项目根目录下执行以下命令：
   ```bash
   git submodule add https://github.com/containers/gvisor-tap-vsock.git helpers/gvisor-tap-vsock
   ```
2. 修改 `helpers/vibe-usernet/go.mod`，在末尾添加 `replace` 指令：
   ```go
   replace github.com/containers/gvisor-tap-vsock => ../gvisor-tap-vsock
   ```
这可确保 `vibe-usernet` 在编译期间使用本地的 `helpers/gvisor-tap-vsock` 代码，从而允许直接修改 `gvisor-tap-vsock` 内的逻辑。

## 2. 修改 `gvisor-tap-vsock` 以支持代理请求
我们需要将底层 TCP/UDP 转发器中的直接 `net.Dial` 替换为感知代理的 Dialer。

**步骤：**
1. **修改配置结构体**：
   在 `helpers/gvisor-tap-vsock/pkg/types/configuration.go` 中，向 `Configuration` 结构体添加两个字段：
   ```go
   Proxy    string
   ProxyUDP bool
   ```
2. **修改 TCP 转发逻辑**：
   在 `helpers/gvisor-tap-vsock/pkg/services/forwarder/tcp.go` 中，修改 `TCP` 函数（`Proxy` 的值必须通过 `services.go` 传入）：
   找到以下代码：
   ```go
   outbound, err := net.Dial("tcp", net.JoinHostPort(localAddress.String(), fmt.Sprint(r.ID().LocalPort)))
   ```
   将其修改为：如果配置了 `Proxy`，则使用代理 Dialer 代替原生的 `net.Dial`。
3. **修改 UDP 转发逻辑**：
   在 `helpers/gvisor-tap-vsock/pkg/services/forwarder/udp.go` 中，修改 `UDP` 函数。如果 `ProxyUDP` 为 `true` 且使用了 `socks5` 代理，则利用 Socks5 UDP Associate 协议将数据包转发到代理服务器，而不是使用通过 `net.DialUDP` 或 `net.ListenUDP` 建立的本地连接。

*(注意：必须在 `pkg/virtualnetwork/services.go` 的 `addServices` 函数中将 `configuration.Proxy` 和 `configuration.ProxyUDP` 向下传递给 `forwarder.TCP` 和 `forwarder.UDP`。)*

## 3. 为 `helpers/vibe-usernet/main.go` 添加 CLI 选项
我们需要在 `helpers/vibe-usernet/main.go` 中接收 `--proxy` 和 `--proxy-udp` 参数，并将它们传递给 `virtualnetwork.New()`。

**步骤：**
1. 在 `main.go` 的 `run` 函数中注册新 flags：
   ```go
   proxy := flag.String("proxy", "", "Proxy URL (e.g., http://127.0.0.1:1080 or socks5://127.0.0.1:1080)")
   proxyUDP := flag.Bool("proxy-udp", false, "Proxy UDP requests (only supported for socks5)")
   ```
2. 将这些配置传递到 `virtualnetwork.New(&types.Configuration{ ... })` 中：
   ```go
   Proxy:    *proxy,
   ProxyUDP: *proxyUDP,
   ```

## 4. 为 `src/main.rs` 添加 CLI 选项
Vibe 使用 Rust 编写，用户通过 `vibe run` 或 `vibe provision` 提供代理参数。我们需要在 Rust 端解析这些参数并传递给 Go 进程。

**步骤：**
1. 修改 `src/main.rs` 中的 `CliCommand::Run` 及相关参数结构体（可能还包括 `CliCommand::Provision`），以包含 `--proxy <PROXY_URL>` 选项（扩展用于 guest 配置的现有 `--proxy` 逻辑）和新的 `--proxy-udp` flag。
2. 修改 `src/networking.rs` 中的 `NetworkMode::prepare` 方法以接收 `proxy` 和 `proxy_udp`。
3. 在构建启动 `usernet_helper_path`（即 `vibe-usernet`）的 `Command::new(...)` 参数时，若设置了代理，则追加以下内容：
   ```rust
   command.arg("--proxy").arg(proxy_url);
   if proxy_udp {
       command.arg("--proxy-udp");
   }
   ```

## 5. `PROXY_URL` 的协议处理（HTTP / SOCKS）
- **HTTP 代理（`http://`）**：
  - HTTP 代理仅支持通过 `CONNECT` 方法代理 TCP 流量；不支持 UDP。
  - 如果用户提供了 `http://` 代理并启用了 `--proxy-udp`，程序应当对此进行校验并抛出错误，或者打印警告并在 UDP 上回退到直接连接（建议抛出错误，以防止意外的 IP 泄露）。
- **SOCKS5 代理（`socks5://`）**：
  - SOCKS5 原生支持 TCP 和 UDP 代理。
  - 当协议为 `socks5://` 且禁用 `--proxy-udp` 时，仅代理 TCP，UDP 流量使用本地直接连接。当启用时，两者均走代理。

## 6. Go 依赖项评估
为了“正确使用 HTTP/SOCKS 代理”，我们需要引入新的 Go 依赖（或者自行实现 Dialer 协议）。

1. **HTTP 代理（CONNECT）**：
   Go 标准库 `net/http` 默认提供全局 HTTP 客户端代理支持，但它没有为标准 TCP Sockets 暴露底层的原始 HTTP `CONNECT` Dialer。
   **结论**：引入类似于 `github.com/magisterquis/connectproxy` 的库，或者实现一个简单的 Dial 逻辑（向 TCP 连接写入 `CONNECT host:port HTTP/1.1\r\n\r\n` 并读取直至收到 `200 Connection established` 响应）。
2. **Socks5 TCP 代理**：
   `golang.org/x/net/proxy` 提供了完善的 `proxy.SOCKS5` 客户端实现，可以替换用于 TCP 转发的 `net.Dial`。可以通过导入 `golang.org/x/net` 来解决。
3. **Socks5 UDP 代理（UDP Associate）**：
   `golang.org/x/net/proxy` **不**支持 Socks5 UDP 代理特性；其接口仅限于基于流的 `Dial`。为了支持 `--proxy-udp`，我们必须实现 Socks5 UDP Associate 协议（通过 TCP 连接到代理服务器以获取中继 IP:Port，然后向该中继地址发送带有 Socks5 头的 UDP 数据包）。
   **结论**：强烈建议使用完整实现了 Socks5 TCP/UDP 支持的库，例如 `github.com/txthinking/socks5`，而不是官方的 `x/net/proxy`。

**依赖项总结**：
在开发过程中，需在 `helpers/gvisor-tap-vsock/go.mod` 中运行 `go get github.com/txthinking/socks5`（或其他支持 UDP 的 Socks5 库）以及一个用于 HTTP `CONNECT` 的 Dialer 库。
