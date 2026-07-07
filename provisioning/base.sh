#!/usr/bin/env bash
set -euxo pipefail

image_history=/root/vibe-image-history.txt
base_first_run=false
if [[ ! -s "${image_history}" ]]; then
  base_first_run=true
fi

{
  if ! "$base_first_run"; then
    echo
  fi
  echo '===== vibe provision ====='
  printf 'provisioned_at_utc: %s\n' "$(date -u '+%Y-%m-%dT%H:%M:%SZ')"
  printf 'image: %s\n' "${VIBE_PROVISION_IMAGE:-unknown}"
  printf 'base: %s\n' "${VIBE_PROVISION_BASE:-unknown}"
  printf 'vibe_git_sha: %s\n' "${VIBE_GIT_SHA:-unknown}"
  printf 'vibe_build_date: %s\n' "${VIBE_BUILD_DATE:-unknown}"
  echo 'scripts:'
  if [[ -n "${VIBE_PROVISION_SCRIPTS:-}" ]]; then
    while IFS= read -r script; do
      printf '  - %s\n' "${script}"
    done <<<"${VIBE_PROVISION_SCRIPTS}"
  fi
} >>"${image_history}"

if [[ "${base_first_run}" != true ]]; then
  exit 0
fi

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
exec_retry() {
  while retryable "$?"; do "${@}"; done
}


ip addr;
pwd;

exec_retry \
apt update;

##########################################################
# Use debian fast-forward so we get newer package versions
# https://wiki.debian.org/Fastforward
# https://fastforward.debian.net/doc/installation/
# region

exec_retry \
apt install --no-install-recommends --update --yes ca-certificates gnupg debian-keyring wget

mkdir -p /usr/share/debian-fastforward/pgp-keys

exec_retry \
wget https://deb.fastforward.debian.net/debian-fastforward/project/pgp/fastforward-debian-13-trixie-signing-key.pub -O /usr/share/debian-fastforward/pgp-keys/deb.fastforward.debian.net.gpg
exec_retry \
wget https://deb.fastforward.debian.net/debian-fastforward/project/pgp/fastforward-debian-13-trixie-signing-key.pub.sig -O /usr/share/debian-fastforward/pgp-keys/deb.fastforward.debian.net.gpg.sig

gpg --keyring /usr/share/keyrings/debian-keyring.gpg --keyring /usr/share/keyrings/debian-maintainers.gpg --verify /usr/share/debian-fastforward/pgp-keys/deb.fastforward.debian.net.gpg.sig
rm -f /usr/share/debian-fastforward/pgp-keys/deb.fastforward.debian.net.gpg.sig

gpg --import /usr/share/debian-fastforward/pgp-keys/deb.fastforward.debian.net.gpg
rm -f /usr/share/debian-fastforward/pgp-keys/deb.fastforward.debian.net.gpg
gpg -o /usr/share/debian-fastforward/pgp-keys/deb.fastforward.debian.net.gpg --export 2BDDB08FA13971B749E4A221F93CF7F4CEBEC933

sh -c 'cat > /etc/apt/sources.list.d/debian-fastforward.sources << EOF
# /etc/apt/sources.list.d/debian-fastforward.sources

Types: deb
URIs: https://deb.fastforward.debian.net/debian-fastforward
Suites: trixie-fastforward trixie-fastforward-security trixie-fastforward-updates trixie-fastforward-backports
Components: main contrib non-free non-free-firmware
PDiffs: no
Signed-By: /usr/share/debian-fastforward/pgp-keys/deb.fastforward.debian.net.gpg
EOF'

sh -c 'cat > /etc/apt/preferences.d/debian-fastforward.pref << EOF
# /etc/apt/preferences.d/debian-fastforward.pref

Package: *
Pin: release n=trixie-fastforward
Pin-Priority: 990

Package: *
Pin: release n=trixie-fastforward-security
Pin-Priority: 990

Package: *
Pin: release n=trixie-fastforward-updates
Pin-Priority: 990

Package: *
Pin: release n=trixie-fastforward-backports
Pin-Priority: 990
EOF'

exec_retry \
apt full-upgrade --update --yes

##########################################################
# End of debian fast forward
# endregion

apt_packages=(
  cloud-guest-utils
  build-essential
  pkg-config
  libssl-dev
  curl
  git
  ripgrep
  vim
  wget
  htop
  tmux
  openssh-server
  unzip
);

exec_retry \
apt install --no-install-recommends --yes "${apt_packages[@]}"


echo 'PasswordAuthentication no
PermitRootLogin  yes' | tee /etc/ssh/sshd_config.d/99_custom.conf

systemctl disable ssh


# Expand disk partition
growpart /dev/vda 1

# Expand filesystem
resize2fs /dev/vda1

# Set hostname to "vibe" so it's clear that you're inside the VM.
hostnamectl set-hostname vibe

cd /root
mkdir -p .ssh


export PATH="${HOME}/.cargo/bin:${HOME}/.local/bin:${PATH}";


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

# Trust everything by default, since we're already in a VM sandbox
trusted_config_paths = ["/root"]

# Trust everything by default, since we're already in a VM sandbox
experimental = true

# idiomatic_version_file_enable_tools = ["rust"]

[tools]
usage = "latest"
uv = "0.11.3"
fzf = "latest"
node = "24"
MISE
# "npm:@github/copilot" = "latest"
# "npm:@google/gemini-cli" = "latest"
# "npm:@openai/codex" = "latest"
# "npm:@anthropic-ai/claude-code" = "latest"
# "npm:@earendil-works/pi-coding-agent" = "latest"

touch .config/mise/mise.lock

exec_retry \
mise install

true;
