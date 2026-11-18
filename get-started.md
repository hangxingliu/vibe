# Get Started

``` bash
#
# git submodule init
# git submodule update

pushd helpers/vibe-usernet
go test -v ./...

#
popd
cargo build --release;
./target/release/vibe provision --image dev-image-2026 @golang @rust @copilot @gemini --repl
./target/release/vibe # --proxy http://192.168.105.1:1080
```

## Build and test the Go helpers

`nat` networking is two Go modules. Build and test each one with `go test` and `go build` from its own directory.

- `helpers/gvisor-tap-vsock` is the forked [gvisor-tap-vsock](https://github.com/containers/gvisor-tap-vsock) submodule. It is the user-mode network stack.
- `helpers/vibe-usernet` is the helper process that `vibe` embeds. Its `go.mod` replaces `github.com/containers/gvisor-tap-vsock` with `../gvisor-tap-vsock`.

Both modules declare `go 1.26.0`. Initialize the submodule before building either one:

```bash
git submodule update --init helpers/gvisor-tap-vsock
```

### helpers/gvisor-tap-vsock

The submodule vendors its dependencies, so these commands use that tree. From that directory:

```bash
cd helpers/gvisor-tap-vsock

# Unit tests for the packages this fork changes.
go test -count=1 -timeout 180s -skip TestSuite \
  ./pkg/services/noproxy/ \
  ./pkg/services/netproxy/ \
  ./pkg/services/dns/ \
  ./pkg/services/forwarder/

# Upstream's gvproxy CLI. The binary vibe embeds is vibe-usernet.
go build -o bin/gvproxy ./cmd/gvproxy
```

`-skip TestSuite` leaves out the port-forwarding specs in `pkg/services/forwarder`. Those listen on a host address and dial it. Run them with `go test -count=1 -timeout 180s -run TestSuite ./pkg/services/forwarder/` when you want that check.

`go test ./...` also compiles the QEMU and vfkit suites under `test-qemu/` and `test-vfkit/`. Those need a hypervisor and a guest image. `make test` builds `gvproxy` and runs every package, including those suites.

The DNS suite looks up `redhat.com` through the host resolver, so run it with outbound DNS.

### helpers/vibe-usernet

This module has no `vendor/` directory. `go test` and `go build` use the module cache, and the `replace` line takes `gvisor-tap-vsock` from the submodule.

```bash
cd helpers/vibe-usernet
go test -count=1 ./...
go build -o bin/vibe-usernet .
```

The binary above is what you run when you want the helper by itself. On an Apple Silicon Mac, `cargo build` compiles this package again (`CGO_ENABLED=1`, `GOOS=darwin`, `GOARCH=arm64`) and embeds that result. Other hosts embed an empty stub.

## How to Update OS Image

<https://cdimage.debian.org/cdimage/>

## Init Guest VM

``` bash
apt update
apt upgrade -y

apt install unzip -y
mise use -g node@24 go@latest java@latest

curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --component "rustfmt,clippy"

# https://bun.com/docs/installation
curl -fsSL https://bun.com/install | bash

# https://antigravity.google/download#antigravity-cli
curl -fsSL https://antigravity.google/cli/install.sh | bash

# https://x.ai/cli
curl -fsSL https://x.ai/cli/install.sh | bash

# https://docs.github.com/zh/copilot/how-tos/copilot-cli/set-up-copilot-cli/install-copilot-cli
curl -fsSL https://gh.io/copilot-install | bash
```

Disk images, how a VM boots, and how a new Debian base is prepared are described in [docs/how-vm-boot.md](docs/how-vm-boot.md).

## \[DEPRECATED\] Configure Gemini Cli

<https://geminicli.com/docs/cli/gemini-md/>

`~/.gemini/settings.json`:

``` json
{
  "context": {
    "fileName": [
      "AGENTS.md",
      "CONTEXT.md",
      "GEMINI.md"
    ]
  },
  "security": {
    "auth": {
      "selectedType": "oauth-personal"
    }
  },
  "ui": {
    "showMemoryUsage": true,
    "errorVerbosity": "low",
    "loadingPhrases": "witty"
  },
  "general": {
    "plan": {
      "directory": "docs/ai-plans"
    }
  }
}
```
