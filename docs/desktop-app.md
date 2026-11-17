# Desktop app

The Runberth app puts your server in the macOS menu bar or the Linux tray. It shows:

- the server's memory, swap, mode, and which repository's job holds the lock;
- every connected repository, with its latest workflow runs and the jobs of runs in progress, including the runner each job runs on;
- a notification when a run finishes, which opens the run on GitHub when clicked.

The icon is a boat in its berth, with a badge for the server's state: a turning spinner while jobs run, with the number of runs in progress beside the icon on macOS and Ubuntu, a check when every repository's latest run passed, an exclamation mark when one failed, and a cross while the server is unreachable. Without a badge, the app is connecting or has no results yet. On macOS the badges are coloured; on Linux they follow the panel's colour.

The app only reads. It changes nothing on the server or in GitHub.

## Requirements

- A server running Runberth 0.2.0 or later, installed with `--sudoers`. The app runs `sudo -n /opt/runberth/bin/runberth status --json` over SSH, which that rule allows without a password.
- SSH access from your computer to the server with a key, as for `scripts/vps.sh`.
- A GitHub token. To show runs and results it needs **Actions: Read** on the connected repositories. To connect and disconnect repositories from the app or its `runberth` command, it also needs **Administration: Read and write** and **Contents: Read**, as `connect` registers a runner and reads the repository's manifest. A fine-grained token with those permissions on your private repositories does both, as does a classic token with the `repo` scope. Without a token the app still shows the server and its repositories, but not runs or results.

## Install

From the [latest release](https://github.com/vantalter/runberth/releases/latest):

- **macOS 12 or later:** download `Runberth-VERSION-macos-universal.dmg`, open it, and drag Runberth to Applications. The app updates itself with Sparkle; choose **Check for Updates…** in its menu to check now.
- **Linux:** install `runberth-app_VERSION_amd64.deb` (or `arm64`) with `sudo apt install ./runberth-app_*.deb`, or the `.rpm` with `sudo dnf install ./runberth-app-*.rpm`, then start **Runberth** from your applications. The menu shows when a newer release is available.

On GNOME, which has no tray of its own, install and enable the AppIndicator extension, for example `sudo apt install gnome-shell-extension-appindicator` on Ubuntu, then log out and back in.

Choose **Launch at Login** in the menu to start the app with your session.

## Set it up

### macOS: the Settings window

Choose **Settings…** in the menu. Opening Runberth again while it runs also shows Settings.

1. **Server:** enter the host, the SSH user, and the port, and choose the private key, as for `scripts/install-vps.sh`. Without a key, ssh uses your agent and `~/.ssh/config`. The server's host key must already be in `~/.ssh/known_hosts`: connect once with `ssh` first. Choose **Test Connection** to check: it shows the server's Runberth version and how many repositories are connected, or what went wrong.
2. **GitHub:** paste a token and choose **Save**. The app keeps it in your login keychain, never in a file. **Create a Token…** opens GitHub's token page. Instead of a stored token, the app can run a **token command** that prints one, such as `gh auth token --user NAME`.
3. **General:** how often the app refreshes, which finished runs it announces (all, failed only, or none), and Launch at Login.
4. **Updates:** automatic update checks and how often they run, when the app last checked, and **Check Now…**. **Donate** links to Ko-fi.

The window edits the settings file below, which stays the single source of truth; **Open settings.json** opens it.

### Linux: the settings file

Choose **Edit Settings…** in the menu. It opens the settings file in your default editor, creating it the first time. Then store the GitHub token in the Secret Service, such as GNOME Keyring or KWallet, so it never sits in a file:

```sh
secret-tool store --label="Runberth GitHub token" service runberth account github
```

`secret-tool` is in the `libsecret-tools` package. Instead of a stored token, set `github_token_command` to a command that prints one, such as `"gh auth token --user NAME"`. Choose **Refresh Now** to check: the first line of the menu names the server and its Runberth version, or says what went wrong.

### The settings file

- macOS: `~/Library/Application Support/Runberth/settings.json`
- Linux: `~/.config/runberth/settings.json`

```json
{
  "server": {
    "host": "203.0.113.10",
    "user": "ubuntu",
    "port": 22,
    "key": "~/.ssh/runberth-vps"
  },
  "github_token_command": null,
  "interval_seconds": 20,
  "notifications": "all",
  "check_updates": true
}
```

Set `host`, `user`, `port`, and `key` as for `scripts/install-vps.sh`; without `key`, ssh uses your agent and `~/.ssh/config`. When Runberth runs on the same Linux computer as the app, set `host` to `localhost`: on port 22 the app runs `sudo -n /opt/runberth/bin/runberth status --json` itself, without SSH, and the menu names the server **This computer**. That needs the same passwordless sudo rule for your account that `--sudoers` installs. On any other port, such as a virtual machine's forwarded SSH port, `localhost` still connects with ssh. `notifications` is `all`, `failures`, or `off`. The app refreshes every `interval_seconds`, every 10 seconds while runs are in progress, and when you open the menu. It reads the file again on every refresh, so edits apply without a restart. The token is never in this file; on macOS you can also store it from a terminal with `security add-generic-password -s Runberth -a github -w`, which prompts for it.

## Connect and disconnect repositories

**Connect Repository** in the menu lists your private repositories that are not connected yet. Choose one to connect it, as `runberth connect` does on the server: the repository needs `.github/runberth.json` on its default branch, as in [Connect a repository](../README.md#2-connect-a-repository). To disconnect one, open its submenu and choose **Disconnect**, then confirm with **Disconnect OWNER/REPO**. Change its jobs' `runs-on` first, as the [personal account guide](personal-account-setup.md) describes. The menu shows the work in progress, and a notification reports the result or the server's error.

## The runberth command

The app's binary is also a `runberth` command for your computer. It runs Runberth's commands on the server in the app's settings, over SSH or directly for localhost:

```sh
runberth connect OWNER/REPO
runberth disconnect OWNER/REPO
runberth status --watch 5
runberth logs OWNER/REPO --follow
```

`connect` and `disconnect` read a GitHub token from stdin when one is piped, as in `gh auth token | runberth connect OWNER/REPO`, and otherwise use the token stored for the app. Run `runberth --help` for the options.

The Linux packages install it as `/usr/bin/runberth`. On macOS, link it from the app once:

```sh
sudo mkdir -p /usr/local/bin
sudo ln -sf /Applications/Runberth.app/Contents/MacOS/runberth /usr/local/bin/runberth
```

## Troubleshooting

- **"too old for this app":** update the server with `scripts/install-vps.sh`, keeping the options you installed with.
- **"needs Runberth's sudoers rule":** reinstall with `--sudoers`.
- **No notifications on macOS:** allow them for Runberth in System Settings > Notifications.
- **No icon on Linux:** the desktop needs a StatusNotifier tray, such as KDE's, or GNOME's AppIndicator extension. The app prints a message when it finds none.

To see what the app shows on each refresh, run it from a terminal with `RUNBERTH_APP_TRACE=1`: it prints each menu as JSON. On macOS, run `/Applications/Runberth.app/Contents/MacOS/runberth`.

## Build from source

The app lives in `app/`, a member of the Cargo workspace.

- **Linux:** install `libglib2.0-dev`, `libjson-glib-dev`, and `pkg-config`, then `cargo run -p runberth-app`. The release builds `.deb` and `.rpm` packages from `app/packaging/linux/nfpm.yaml`.
- **macOS:** `cargo run -p runberth-app` runs the menu bar item without notifications, Sparkle, or Launch at Login, which need the app bundle. `dist/Runberth.app/Contents/MacOS/runberth --settings` starts with the Settings window open. `app/scripts/build-macos-app.sh` builds `dist/Runberth.app` with Xcode and signs it with your Apple Development certificate when you have one. `app/scripts/check-macos-app.sh` checks the result.

## Releasing the macOS app

A `v*` tag builds the app with the server binary. The macOS app is signed and notarized when the repository has these Actions secrets, and ad-hoc signed otherwise:

- `MACOS_CERTIFICATE` and `MACOS_CERTIFICATE_PASSWORD`: a Developer ID Application certificate as a base64 `.p12` file and its password;
- `APPLE_API_KEY`, `APPLE_API_KEY_ID`, and `APPLE_API_ISSUER`: an App Store Connect API key for notarization;
- `SPARKLE_PRIVATE_KEY`: the key that signs updates.

Sparkle installs only updates signed with the private half of `app/macos/sparkle-public-key`. Create the pair once with Sparkle's `generate_keys`, which keeps the private key in your login keychain:

```sh
curl -sSfL https://github.com/sparkle-project/Sparkle/releases/download/2.10.1/Sparkle-2.10.1.tar.xz | tar -xJf - ./bin/generate_keys
./bin/generate_keys --account runberth
./bin/generate_keys --account runberth -p > app/macos/sparkle-public-key
./bin/generate_keys --account runberth -x sparkle-private-key
gh secret set SPARKLE_PRIVATE_KEY --repo vantalter/runberth < sparkle-private-key
rm sparkle-private-key
```

Keep the key: an app signed with one key cannot update itself to a release signed with another.
