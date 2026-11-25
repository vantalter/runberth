#!/bin/sh
# Install the `runberth` command on this computer, from a GitHub release:
#   curl -fsSL https://github.com/vantalter/runberth/releases/latest/download/install.sh | sh
#
# It picks the build for this system (macOS, or Linux on x86_64 or arm64),
# checks it against its published SHA-256, and installs it as
# $RUNBERTH_INSTALL_DIR/runberth (default /usr/local/bin), with sudo only when
# that folder is not writable. RUNBERTH_VERSION=v0.2.0 picks a release instead
# of the latest. The command then sets up and runs Runberth on a server:
#   runberth config --host 203.0.113.10 --user ubuntu --key ~/.ssh/vps_key
#   runberth install --owner YOUR_GITHUB_NAME --swap 3
set -eu

repo=vantalter/runberth
dir=${RUNBERTH_INSTALL_DIR:-/usr/local/bin}
version=${RUNBERTH_VERSION:-}

fail() {
  echo "install: $*" >&2
  exit 1
}

case "$(uname -s)" in
  Darwin) asset=runberth-cli-macos-universal ;;
  Linux)
    case "$(uname -m)" in
      x86_64 | amd64) asset=runberth-cli-x86_64-unknown-linux-musl ;;
      aarch64 | arm64) asset=runberth-cli-aarch64-unknown-linux-musl ;;
      *) fail "no runberth build for Linux on $(uname -m); build it from source" ;;
    esac
    ;;
  *) fail "no runberth build for $(uname -s); it runs on macOS and Linux" ;;
esac
case "$version" in
  "") base="https://github.com/$repo/releases/latest/download" ;;
  v[0-9]*) base="https://github.com/$repo/releases/download/$version" ;;
  *) fail "RUNBERTH_VERSION must be a release tag, such as v0.2.0" ;;
esac

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT INT TERM
echo "Downloading $asset ${version:-(latest)}"
for file in "$asset" "$asset.sha256"; do
  curl --proto '=https' --tlsv1.2 -fsSL "$base/$file" -o "$work/$file" \
    || fail "cannot download $base/$file"
done
expected=$(cut -d ' ' -f 1 "$work/$asset.sha256")
if command -v sha256sum >/dev/null 2>&1; then
  actual=$(sha256sum "$work/$asset" | cut -d ' ' -f 1)
else
  actual=$(shasum -a 256 "$work/$asset" | cut -d ' ' -f 1)
fi
if [ -z "$expected" ] || [ "$expected" != "$actual" ]; then
  fail "checksum mismatch for $asset"
fi
chmod 0755 "$work/$asset"

sudo=""
if [ ! -d "$dir" ] || [ ! -w "$dir" ]; then
  [ "$(id -u)" -eq 0 ] || sudo=sudo
fi
$sudo mkdir -p "$dir"
$sudo install -m 0755 "$work/$asset" "$dir/runberth"
echo "Installed $dir/runberth ($("$dir/runberth" --version))"
case ":$PATH:" in
  *":$dir:"*) ;;
  *) echo "Add $dir to your PATH to run runberth by name." ;;
esac
