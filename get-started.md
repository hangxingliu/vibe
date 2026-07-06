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



## Disk Image Files Explanation

- **Base Image**: A raw Debian 13 "trixie" (testing) nocloud image downloaded from the official Debian cloud repository. It is decompressed and stored in the global cache directory (`~/.cache/vibe/`).
- **Default Image**: A copy of the base image that is resized to 10 GB and provisioned using `provisioning/base.sh`. The provisioning includes:
  - Expanding the disk partition and filesystem.
  - Installing system packages (`build-essential`, `curl`, `git`, `ripgrep`, `vim`, `htop`, `tmux`, etc.).
  - Setting up development environments (Rust via `rustup`, `mise`, `node`, `uv`).
  - Configuring optional proxy settings.
  It is also stored in the global cache directory and acts as a reusable template for all projects.
- **Instance Image (`.vibe/instance.raw`)**: A local copy of the **Default Image** created within the project's directory. This is the actual disk used by the VM, ensuring that each project has its own isolated, persistent state.


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
