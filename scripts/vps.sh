#!/usr/bin/env bash
# Run a Runberth command on the VPS from your computer, over SSH.
#
# Examples:
#   scripts/vps.sh status --watch 5
#   scripts/vps.sh logs vantalter/REPO --follow
#   gh auth token | scripts/vps.sh connect vantalter/REPO
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/vps.sh [--host HOST] [--key PATH] [--user NAME] [--port PORT] COMMAND [ARGS...]

Runs `/opt/runberth/bin/runberth COMMAND ARGS...` on the VPS with sudo, which
the passwordless rule from `install-vps.sh --sudoers` allows for status, logs,
connect, disconnect, and cleanup. Other commands ask for the sudo password.

  --host HOST    VPS IP address or host name (default: $RUNBERTH_HOST)
  --key PATH     private SSH key (default: $RUNBERTH_KEY)
  --user NAME    SSH account (default: $RUNBERTH_USER, else root)
  --port PORT    SSH port (default: 22)

Common commands:
  status [--watch [SECONDS]]              memory, swap, load, lock holder, runners
  logs OWNER/REPO [--follow] [--lines N]  lock helper notes and runner service journal
  connect OWNER/REPO                      pipe a GitHub token in: gh auth token | ...
  disconnect OWNER/REPO                   pipe a GitHub token in, as for connect
  cleanup                                 dry run of a full removal
EOF
}

fail() {
  echo "vps: $*" >&2
  exit 1
}

host="${RUNBERTH_HOST:-}" key="${RUNBERTH_KEY:-}" user="${RUNBERTH_USER:-root}" port="22"
while (($#)); do
  case "$1" in
    --host) host="${2:-}"; shift 2 ;;
    --key) key="${2:-}"; shift 2 ;;
    --user) user="${2:-}"; shift 2 ;;
    --port) port="${2:-}"; shift 2 ;;
    -h | --help) usage; exit 0 ;;
    *) break ;;
  esac
done
(($#)) || { usage >&2; fail "a Runberth command is required"; }

# The command line reaches a remote shell, so accept only plain values.
[[ -n "$host" && -n "$key" ]] || { usage >&2; fail "--host and --key are required (or RUNBERTH_HOST and RUNBERTH_KEY)"; }
[[ "$host" =~ ^[A-Za-z0-9.:-]+$ ]] || fail "--host must be an IP address or host name"
[[ "$user" =~ ^[a-z_][a-z0-9_-]*$ ]] || fail "--user must be a plain Unix account name"
[[ "$port" =~ ^[0-9]+$ ]] || fail "--port must be a number"
[[ -f "$key" && -r "$key" ]] || fail "cannot read SSH key: $key"
for arg in "$@"; do
  [[ "$arg" =~ ^[A-Za-z0-9._/:=@+-]+$ ]] || fail "unexpected characters in argument: $arg"
done

remote="/opt/runberth/bin/runberth"
for arg in "$@"; do
  remote+=" $(printf '%q' "$arg")"
done
ssh_opts=(-i "$key" -o IdentitiesOnly=yes -p "$port")
if [[ -t 0 ]]; then
  # A terminal: allocate one remotely so Ctrl-C ends --watch and --follow,
  # and so sudo can ask for a password when no rule covers the command.
  [[ "$user" == root ]] || remote="sudo $remote"
  exec ssh -t "${ssh_opts[@]}" "$user@$host" "$remote"
fi
# Input is piped in, such as a token for connect: pass it through.
[[ "$user" == root ]] || remote="sudo -n $remote"
exec ssh "${ssh_opts[@]}" -o BatchMode=yes "$user@$host" "$remote"
