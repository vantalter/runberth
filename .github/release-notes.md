## Which file do I need?

| You want | Get |
|---|---|
| The macOS menu bar app, with the `runberth` command | [Runberth-{{VERSION}}-macos-universal.dmg](https://github.com/vantalter/runberth/releases/download/{{TAG}}/Runberth-{{VERSION}}-macos-universal.dmg) |
| The Linux tray app, with the `runberth` command | `runberth-app_{{VERSION}}_amd64.deb` or `_arm64.deb`; the `.rpm` files for Fedora and similar |
| Only the `runberth` command, on macOS or Linux | `curl -fsSL https://github.com/vantalter/runberth/releases/download/{{TAG}}/install.sh \| RUNBERTH_VERSION={{TAG}} sh` |
| Runberth on the server | Nothing to download: `runberth install`, or `runberth update` for an existing server, installs `runberth-<processor>-unknown-linux-musl` for you |

The `runberth-cli-*` files are the command that `install.sh` installs, and `runberth-*-unknown-linux-musl` without `cli` is the server. `SHA256SUMS` lists every file's checksum, and each binary also has its own `.sha256`.
