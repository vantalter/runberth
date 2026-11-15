#!/usr/bin/env bash
# Install Runberth on a VPS over SSH, from this checkout on your computer.
#
# Gets the static Linux binary (a published release, or --binary),
# uploads it with config/host.json, then runs check-host, preflight, and
# install-host on the server. Optionally adds swap, installs apt packages
# through Runberth's package tracking, and installs the passwordless sudoers
# rule that `connect` and `cleanup` need when streamed over SSH, so one run
# sets up the whole host. Re-running it updates Runberth.
set -euo pipefail

readonly ASSET=runberth-x86_64-unknown-linux-musl

usage() {
  cat <<'EOF'
Usage: scripts/install-vps.sh --host HOST --key PRIVATE_KEY [options]

Required:
  --host HOST               VPS IP address or host name (default: $RUNBERTH_HOST)
  --key PATH                private SSH key for the VPS account (default: $RUNBERTH_KEY)

Options:
  --user NAME               SSH account on the VPS (default: $RUNBERTH_USER, else root)
  --port PORT               SSH port (default: 22)
  --binary PATH             install this Linux x86_64 binary instead of downloading
                            a release; PATH.sha256 is checked if present
  --release-repo OWNER/REPO download releases from here (default: vantalter/runberth)
  --version TAG             install this release, such as v0.1.0 (default: the latest)
  --config PATH             host config to install (default: config/host.json)
  --swap 2|3|4              also add a Runberth-owned swap file of this many GiB
  --packages "NAME..."      also install these apt packages, tracked so cleanup
                            removes them; for this repository's CI on the runner:
                            --packages "gcc libc6-dev git shellcheck"
  --on-demand               also switch to on-demand runners: the dock starts a
                            repository's runner only when work arrives (experimental)
  --sudoers                 also install the sudoers rule that lets --user run `runberth
                            connect`, `disconnect`, `cleanup`, `status`, and `logs`
                            without a password
  --accept-new-host-key     trust the server's SSH host key on first connection
                            (default: the key must already be in known_hosts)
  --skip-preflight          install even if preflight fails (not recommended)
  -h, --help                show this help
EOF
}

fail() {
  echo "install-vps: $*" >&2
  exit 1
}

host="${RUNBERTH_HOST:-}" key="${RUNBERTH_KEY:-}" user="${RUNBERTH_USER:-root}" port="22"
binary="" swap="" packages="" sudoers=false on_demand=false accept_new=false skip_preflight=false
release_repo="vantalter/runberth" version=""
root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
config="$root_dir/config/host.json"

while (($#)); do
  case "$1" in
    --host) host="${2:-}"; shift 2 ;;
    --key) key="${2:-}"; shift 2 ;;
    --user) user="${2:-}"; shift 2 ;;
    --port) port="${2:-}"; shift 2 ;;
    --binary) binary="${2:-}"; shift 2 ;;
    --release-repo) release_repo="${2:-}"; shift 2 ;;
    --version) version="${2:-}"; shift 2 ;;
    --config) config="${2:-}"; shift 2 ;;
    --swap) swap="${2:-}"; shift 2 ;;
    --packages) packages="${2:-}"; shift 2 ;;
    --sudoers) sudoers=true; shift ;;
    --on-demand) on_demand=true; shift ;;
    --accept-new-host-key) accept_new=true; shift ;;
    --skip-preflight) skip_preflight=true; shift ;;
    -h | --help) usage; exit 0 ;;
    *) usage >&2; fail "unknown option: $1" ;;
  esac
done

# Everything below reaches a remote shell or a sudoers file, so accept only
# plain values.
[[ -n "$host" && -n "$key" ]] || { usage >&2; fail "--host and --key are required (or RUNBERTH_HOST and RUNBERTH_KEY)"; }
[[ "$host" =~ ^[A-Za-z0-9.:-]+$ ]] || fail "--host must be an IP address or host name"
[[ "$user" =~ ^[a-z_][a-z0-9_-]*$ ]] || fail "--user must be a plain Unix account name"
[[ "$port" =~ ^[0-9]+$ ]] || fail "--port must be a number"
[[ -z "$swap" || "$swap" =~ ^[234]$ ]] || fail "--swap must be 2, 3, or 4"
[[ "$release_repo" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]] || fail "--release-repo must be OWNER/REPO"
[[ -z "$version" || "$version" =~ ^[A-Za-z0-9._-]+$ ]] || fail "--version must be a release tag such as v0.1.0"
packages="${packages//,/ }"
for package in $packages; do
  [[ "$package" =~ ^[a-z0-9][a-z0-9+.:-]*$ ]] || fail "--packages has an invalid package name: $package"
done
[[ -f "$key" && -r "$key" ]] || fail "cannot read SSH key: $key"
[[ -f "$config" ]] || fail "cannot read host config: $config"
if [[ "$sudoers" == true && "$user" == root ]]; then
  fail "--sudoers is for a non-root --user; root needs no sudoers rule"
fi

workdir="$(mktemp -d)"
trap 'rm -rf "$workdir"' EXIT

sha256_check() { # DIR CHECKSUM_FILE
  if command -v sha256sum >/dev/null; then
    (cd "$1" && sha256sum -c "$2")
  else
    (cd "$1" && shasum -a 256 -c "$2")
  fi
}

# 1. The binary, verified against its published checksum.
if [[ -n "$binary" ]]; then
  [[ -f "$binary" ]] || fail "cannot read binary: $binary"
  cp "$binary" "$workdir/$ASSET"
  if [[ -f "$binary.sha256" ]]; then
    awk -v name="$ASSET" '{ print $1 "  " name }' "$binary.sha256" > "$workdir/$ASSET.sha256"
    sha256_check "$workdir" "$ASSET.sha256" || fail "checksum mismatch for $binary"
  else
    echo "No $binary.sha256 found; installing the binary unchecked."
  fi
else
  base="https://github.com/$release_repo/releases/latest/download"
  [[ -z "$version" ]] || base="https://github.com/$release_repo/releases/download/$version"
  echo "Downloading $ASSET ${version:-(latest)} from $release_repo"
  for file in "$ASSET" "$ASSET.sha256"; do
    curl --proto '=https' --tlsv1.2 -fsSL "$base/$file" -o "$workdir/$file" \
      || fail "cannot download $base/$file; check --release-repo and --version, or pass --binary"
  done
  sha256_check "$workdir" "$ASSET.sha256" || fail "checksum mismatch for the downloaded release"
fi
cp "$config" "$workdir/host.json"

# 2. The steps that run on the server, in one SSH session so sudo asks for a
#    password at most once.
sudo_cmd=""
[[ "$user" == root ]] || sudo_cmd="sudo"
# `cleanup *` needs at least one argument, so the dry run is listed too.
# status and logs only read; status with no arguments is listed separately.
sudoers_rule="$user ALL=(root) NOPASSWD: /opt/runberth/bin/runberth connect *, \
/opt/runberth/bin/runberth disconnect *, \
/opt/runberth/bin/runberth cleanup, /opt/runberth/bin/runberth cleanup *, \
/opt/runberth/bin/runberth status, /opt/runberth/bin/runberth status *, \
/opt/runberth/bin/runberth logs *"
{
  echo 'set -euo pipefail'
  printf 'sudo_cmd=%q swap=%q packages=%q sudoers=%q on_demand=%q skip_preflight=%q sudoers_rule=%q\n' \
    "$sudo_cmd" "$swap" "$packages" "$sudoers" "$on_demand" "$skip_preflight" "$sudoers_rule"
  cat <<'REMOTE'
dir="$(cd "$(dirname "$0")" && pwd)"
trap 'rm -rf "$dir"' EXIT
cd "$dir"
chmod 0755 runberth
./runberth check-host --config host.json
if [[ "$skip_preflight" == true ]]; then
  ./runberth preflight || echo "Preflight failed; continuing because of --skip-preflight."
else
  ./runberth preflight
fi
$sudo_cmd ./runberth install-host --config host.json
if [[ -n "$swap" ]]; then
  $sudo_cmd /opt/runberth/bin/runberth add-swap "$swap"
fi
if [[ -n "$packages" ]]; then
  read -ra package_list <<<"$packages"
  $sudo_cmd apt-get update -qq
  $sudo_cmd /opt/runberth/bin/runberth track-packages \
    apt-get install -y --no-install-recommends "${package_list[@]}"
fi
if [[ "$on_demand" == true ]]; then
  $sudo_cmd /opt/runberth/bin/runberth on-demand on
fi
if [[ "$sudoers" == true ]]; then
  printf '%s\n' "$sudoers_rule" > sudoers.runberth
  $sudo_cmd visudo -cf sudoers.runberth
  $sudo_cmd install -o root -g root -m 0440 sudoers.runberth /etc/sudoers.d/runberth
  echo "Installed /etc/sudoers.d/runberth"
fi
REMOTE
} > "$workdir/install.sh"

# 3. Upload and run.
known_hosts=(-o StrictHostKeyChecking=yes)
[[ "$accept_new" == true ]] && known_hosts=(-o StrictHostKeyChecking=accept-new)
ssh_opts=(-i "$key" -o IdentitiesOnly=yes -o BatchMode=yes "${known_hosts[@]}")
target="$user@$host"
scp_target="$user@$host"
[[ "$host" == *:* ]] && scp_target="$user@[$host]"

remote_dir="$(ssh "${ssh_opts[@]}" -p "$port" "$target" 'mktemp -d /tmp/runberth-install.XXXXXX')" \
  || fail "cannot connect to $target; check the key, and the host key (see --accept-new-host-key)"
[[ "$remote_dir" =~ ^/tmp/runberth-install\.[A-Za-z0-9]+$ ]] || fail "unexpected remote directory: $remote_dir"
scp -q "${ssh_opts[@]}" -P "$port" "$workdir/$ASSET" "$scp_target:$remote_dir/runberth"
scp -q "${ssh_opts[@]}" -P "$port" "$workdir/host.json" "$workdir/install.sh" "$scp_target:$remote_dir/"
ssh -t "${ssh_opts[@]}" -p "$port" "$target" "bash $remote_dir/install.sh"

cat <<EOF

Runberth is installed on $host. Connect a private repository that has
committed .github/runberth.json:

  gh auth token | scripts/vps.sh --host $host --key $key --user $user connect OWNER/REPO

Check memory, the lock, and runners with: scripts/vps.sh ... status --watch 5
EOF
if [[ "$user" != root && "$sudoers" != true ]]; then
  echo
  echo "That needs the passwordless sudoers rule; re-run with --sudoers to install it."
fi
