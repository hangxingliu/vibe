#!/usr/bin/env bash
set -euxo pipefail

# Don't wait too long for slow mirrors.
echo 'Acquire::http::Timeout "2";' | tee /etc/apt/apt.conf.d/99timeout
echo 'Acquire::https::Timeout "2";' | tee -a /etc/apt/apt.conf.d/99timeout
echo 'Acquire::Retries "3";' | tee -a /etc/apt/apt.conf.d/99timeout

# region INJECT_PROXY_CODE
# endregion INJECT_PROXY_CODE
retry_error=
retryable() {
  if [ -z "$retry_error" ]; then retry_error=0; return 0; fi
  if [ "$1" == "0" ]; then retry_error=; return 1; fi

  retry_error="$((retry_error+1))";
  if [[ "$retry_error" -le 3 ]]; then
    echo "Warn:  waiting 10s and try again (error=${retry_error}) ..." >&2;
    sleep 10;
    return 0;
  fi
  echo "Error: too many errors and retry attempts, exiting ..." >&2;
  exit 1;
}


ip addr;
pwd;

while retryable "$?"; do
apt update &&
apt install -y --no-install-recommends   \
    cloud-guest-utils                    \
    build-essential                      \
    pkg-config                           \
    libssl-dev                           \
    curl                                 \
    git                                  \
    ripgrep                              \
    vim                                  \
    htop                                 \
    tmux
done

# Expand disk partition
growpart /dev/vda 1

# Expand filesystem
resize2fs /dev/vda1

# Set hostname to vibe" so it's clear that you're inside the VM.
hostnamectl set-hostname vibe

cd /root/

# Shutdown the VM when you logout
cat > .bash_logout <<EOF
history -w # Write bash history. Otherwise bash would be killed by poweroff without having written history
systemctl poweroff
sleep 100 # sleep here so that we don't see the login screen flash up before the shutdown.
EOF

export PATH="${HOME}/.cargo/bin:${HOME}/.local/bin:${PATH}";

# Install Rust
while retryable "$?"; do
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --component "rustfmt,clippy"
done

# Install Mise
while retryable "$?"; do
curl https://mise.run | sh
done

eval "$(mise activate bash)"

mkdir -p .config/mise/

cat > .config/mise/config.toml <<MISE
[settings]
# Always use the venv created by uv, if available in directory
python.uv_venv_auto = true
experimental = true
idiomatic_version_file_enable_tools = ["rust"]

[tools]
uv = "0.11.3"
node = "24"
fzf = "latest"
"npm:@google/gemini-cli" = "latest"
"npm:@github/copilot" = "latest"
MISE

touch .config/mise/mise.lock

while retryable "$?"; do
mise install
done

# Done provisioning, power off the VM
systemctl poweroff
