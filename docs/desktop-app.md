# Desktop app

The Runberth app puts your server in the macOS menu bar or the Linux tray. It shows:

- the server's memory, swap, mode, and which repository's job holds the lock;
- every connected repository, with its latest workflow runs and the jobs of runs in progress, including the runner each job runs on;
- a notification when a run finishes, which opens the run on GitHub when clicked.

The icon is a boat in its berth. A dot appears in the berth's entrance while jobs run, with the number of runs in progress beside the icon on macOS and Ubuntu, and an exclamation mark when a repository's latest run failed. While the server is unreachable, the menu says so.

The app only reads. It changes nothing on the server or in GitHub.

## Requirements

- A server running Runberth 0.2.0 or later, installed with `--sudoers`. The app runs `sudo -n /opt/runberth/bin/runberth status --json` over SSH, which that rule allows without a password.
- SSH access from your computer to the server with a key, as for `scripts/vps.sh`.
- A GitHub token that can read Actions on the connected repositories: a fine-grained token with **Actions: Read** on them, or a classic token with the `repo` scope. Without a token the app still shows the server and its repositories, but not runs or results.

## Install

From the [latest release](https://github.com/vantalter/runberth/releases/latest):

- **macOS 12 or later:** download `Runberth-VERSION-macos-universal.dmg`, open it, and drag Runberth to Applications. The app updates itself with Sparkle; choose **Check for Updates…** in its menu to check now.
- **Linux:** install `runberth-app_VERSION_amd64.deb` (or `arm64`) with `sudo apt install ./runberth-app_*.deb`, or the `.rpm` with `sudo dnf install ./runberth-app-*.rpm`, then start **Runberth** from your applications. The menu shows when a newer release is available.

On GNOME, which has no tray of its own, install and enable the AppIndicator extension, for example `sudo apt install gnome-shell-extension-appindicator` on Ubuntu, then log out and back in.

Choose **Launch at Login** in the menu to start the app with your session.

## Set it up

1. Choose **Settings…** in the menu. It opens the settings file in a text editor, creating it the first time:
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
     "interval_seconds": 20
   }
   ```

   Set `host`, `user`, and `key` as for `scripts/install-vps.sh`. Without `key`, ssh uses your agent and `~/.ssh/config`. The server's host key must already be in `~/.ssh/known_hosts`: connect once with `ssh` first. The app refreshes every `interval_seconds`, every 10 seconds while runs are in progress, and when you open the menu. It reads the settings again on every refresh, so edits apply without a restart.

2. Store the GitHub token where the app looks for it, so it never sits in a file:

   ```sh
   # macOS: the login keychain (prompts for the token)
   security add-generic-password -s Runberth -a github -w
   # Linux: the Secret Service, such as GNOME Keyring or KWallet
   secret-tool store --label="Runberth GitHub token" service runberth account github
   ```

   Or set `github_token_command` to a command that prints a token, such as `"gh auth token --user NAME"`.

3. Choose **Refresh Now**. The first line of the menu names the server and its Runberth version. Any problem with the server, the settings, or the token appears in the menu below it.

## Troubleshooting

- **"too old for this app":** update the server with `scripts/install-vps.sh`, keeping the options you installed with.
- **"needs Runberth's sudoers rule":** reinstall with `--sudoers`.
- **No notifications on macOS:** allow them for Runberth in System Settings > Notifications.
- **No icon on Linux:** the desktop needs a StatusNotifier tray, such as KDE's, or GNOME's AppIndicator extension. The app prints a message when it finds none.

To see what the app shows on each refresh, run it from a terminal with `RUNBERTH_APP_TRACE=1`: it prints each menu as JSON. On macOS, run `/Applications/Runberth.app/Contents/MacOS/runberth`.

## Build from source

The app lives in `app/`, a member of the Cargo workspace.

- **Linux:** install `libglib2.0-dev`, `libjson-glib-dev`, and `pkg-config`, then `cargo run -p runberth-app`. The release builds `.deb` and `.rpm` packages from `app/packaging/linux/nfpm.yaml`.
- **macOS:** `cargo run -p runberth-app` runs the menu bar item without notifications, Sparkle, or Launch at Login, which need the app bundle. `app/scripts/build-macos-app.sh` builds `dist/Runberth.app` with Xcode and signs it with your Apple Development certificate when you have one. `app/scripts/check-macos-app.sh` checks the result.

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
