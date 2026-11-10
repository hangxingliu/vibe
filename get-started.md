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
