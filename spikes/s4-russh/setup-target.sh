#!/usr/bin/env bash
# Spike S4 target (docs/decisions.md "S4 image"): a throwaway WSL distro with sshd.
# Built from Ubuntu's official WSL image, verified before import. Never copies the
# user's own distros. Run from Git Bash, non-admin:  ./setup-target.sh <work-dir>
# Writes <work-dir>/target.json (facts for FINDINGS.md) and the test key pair.
set -euo pipefail

WORK="${1:?usage: setup-target.sh <work-dir>}"
DISTRO="amiri-s4-sshd"
PORT=22422
BASE="https://cloud-images.ubuntu.com/wsl/releases/24.04/current"
IMAGE="ubuntu-noble-wsl-amd64-24.04lts.rootfs.tar.gz"
# Ubuntu cloud image signing key ("UEC Image Automatic Signing Key <cdimage@ubuntu.com>").
SIGNING_KEY="D2EB44626FDDC30B513D5BB71A5D6C4C7DB87C81"
GPG="/c/Program Files/Git/usr/bin/gpg.exe"

mkdir -p "$WORK/download" "$WORK/gnupg" "$WORK/distro"
chmod 700 "$WORK/gnupg"
cd "$WORK/download"

if wsl.exe -l -q 2>/dev/null | tr -d '\0\r' | grep -qx "$DISTRO"; then
  echo "distro $DISTRO already exists; run teardown-target.sh first" >&2
  exit 1
fi

echo "== download"
curl -fsSLO "$BASE/SHA256SUMS"
curl -fsSLO "$BASE/SHA256SUMS.gpg"
[ -f "$IMAGE" ] || curl -fSLO "$BASE/$IMAGE"

echo "== verify signature (temporary keyring; the user's keyring is untouched)"
export GNUPGHOME="$WORK/gnupg"
"$GPG" --batch --quiet --keyserver hkps://keyserver.ubuntu.com --recv-keys "$SIGNING_KEY"
"$GPG" --batch --status-fd 1 --verify SHA256SUMS.gpg SHA256SUMS > gpg-status.txt 2>/dev/null || true
VALIDSIG=$(grep -o '^\[GNUPG:\] VALIDSIG [0-9A-F]*' gpg-status.txt | awk '{print $3}' || true)
PRIMARY=$(grep '^\[GNUPG:\] VALIDSIG' gpg-status.txt | awk '{print $NF}' || true)
if [ "$PRIMARY" != "$SIGNING_KEY" ] && [ "$VALIDSIG" != "$SIGNING_KEY" ]; then
  echo "signature check FAILED (VALIDSIG=$VALIDSIG primary=$PRIMARY)" >&2
  exit 1
fi

echo "== verify checksum"
EXPECTED=$(grep " \*\?$IMAGE\$" SHA256SUMS | awk '{print $1}')
ACTUAL=$(sha256sum "$IMAGE" | awk '{print $1}')
if [ -z "$EXPECTED" ] || [ "$EXPECTED" != "$ACTUAL" ]; then
  echo "checksum MISMATCH (expected=$EXPECTED actual=$ACTUAL)" >&2
  exit 1
fi

echo "== import $DISTRO"
wsl.exe --import "$DISTRO" "$(cygpath -w "$WORK/distro")" "$(cygpath -w "$WORK/download/$IMAGE")" --version 2

echo "== configure sshd (key-only, 127.0.0.1:$PORT, user amiri)"
rm -f "$WORK/s4_key" "$WORK/s4_key.pub"
ssh-keygen -q -t ed25519 -N "" -C amiri-s4-test -f "$WORK/s4_key"
PUB=$(cat "$WORK/s4_key.pub")
wsl.exe -d "$DISTRO" -u root -- sh -euc "
  export DEBIAN_FRONTEND=noninteractive
  # No service starts during install (systemd is not ready on first boot); sshd is
  # started by hand with our config, and systemd's ssh units are masked.
  printf '#!/bin/sh\nexit 101\n' > /usr/sbin/policy-rc.d
  chmod 755 /usr/sbin/policy-rc.d
  apt-get -qq update
  apt-get -qq install -y openssh-server tmux >/dev/null || dpkg --configure -a
  systemctl mask --now ssh.socket ssh.service >/dev/null 2>&1 || true
  id amiri >/dev/null 2>&1 || useradd -m -s /bin/bash amiri
  install -d -m 700 -o amiri -g amiri /home/amiri/.ssh
  printf '%s\n' '$PUB' > /home/amiri/.ssh/authorized_keys
  chown amiri:amiri /home/amiri/.ssh/authorized_keys
  chmod 600 /home/amiri/.ssh/authorized_keys
  cat > /etc/ssh/sshd_config.d/amiri-s4.conf <<EOF
Port $PORT
ListenAddress 127.0.0.1
PasswordAuthentication no
KbdInteractiveAuthentication no
PubkeyAuthentication yes
PermitRootLogin no
AllowUsers amiri
EOF
  ssh-keygen -A >/dev/null
  mkdir -p /run/sshd
  sshd -t
"
HOSTKEY=$(MSYS_NO_PATHCONV=1 wsl.exe -d "$DISTRO" -u root -- cat /etc/ssh/ssh_host_ed25519_key.pub | tr -d '\r')
printf '[127.0.0.1]:%s %s\n' "$PORT" "$(echo "$HOSTKEY" | awk '{print $1, $2}')" > "$WORK/known_hosts"

FPR=$("$GPG" --batch --with-colons --fingerprint "$SIGNING_KEY" | awk -F: '/^fpr/{print $10; exit}')
UID_LINE=$("$GPG" --batch --with-colons --list-keys "$SIGNING_KEY" | awk -F: '/^uid/{print $10; exit}')
cat > "$WORK/target.json" <<EOF
{
  "distro": "$DISTRO",
  "image_url": "$BASE/$IMAGE",
  "sha256sums_url": "$BASE/SHA256SUMS",
  "image_sha256": "$ACTUAL",
  "sha256_matches_published": true,
  "signature_valid": true,
  "signing_key_fingerprint": "$FPR",
  "signing_key_uid": "$UID_LINE",
  "sshd_port": $PORT,
  "sshd_listen": "127.0.0.1",
  "host_key": "$(echo "$HOSTKEY" | awk '{print $1, $2}')",
  "test_key_fingerprint": "$(ssh-keygen -lf "$WORK/s4_key.pub" | awk '{print $2}')"
}
EOF
echo "== done"; cat "$WORK/target.json"
