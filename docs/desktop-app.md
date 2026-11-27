# Desktop app

The Runberth app puts your server in the macOS menu bar or the Linux tray. It shows:

- the server's memory, swap, mode, and which repository's job holds the lock;
- every connected repository, with its latest workflow runs and the jobs of runs in progress, including the runner each job runs on;
- a notification when a run finishes, which opens the run on GitHub when clicked.

The icon is a boat in its berth, with a badge for the server's state: a turning spinner while jobs run, with the number of runs in progress beside the icon on macOS and Ubuntu, a check when every repository's latest run passed, an exclamation mark when one failed, a warning triangle when job results need a working GitHub token, and a cross while the server is unreachable. While it is, the menu keeps the repositories from the server's last answer, without the runner's state, and Show Logs, Disconnect, and Connect Repository wait until it answers again. Without a badge, the app is connecting or has no results yet. On macOS the badges are coloured; on Linux they follow the panel's colour.

The app only reads. It changes nothing on the server or in GitHub.

## Requirements

- A server running Runberth 0.2.0 or later, installed with `--sudoers`. The app runs `sudo -n /opt/runberth/bin/runberth status --json` over SSH, which that rule allows without a password.
- SSH access from your computer to the server with a key, as for `scripts/vps.sh`.
- A GitHub token. To show runs and results it needs **Actions: Read** on the connected repositories. To connect and disconnect repositories from the app or its `runberth` command, it also needs **Administration: Read and write** and **Contents: Read**, as `connect` registers a runner and reads the repository's manifest. A fine-grained token with those permissions on your private repositories does both, as does a classic token with the `repo` scope. Without a token the app still shows the server and its repositories, but not runs or results.

## Install

From the [latest release](https://github.com/vantalter/runberth/releases/latest):

- **macOS 12 or later:** download `Runberth-VERSION-macos-universal.dmg`, open it, and drag Runberth to Applications. The app updates itself with Sparkle; choose **Check Now…** in Settings > Updates to check now.
- **Linux:** install `runberth-app_VERSION_amd64.deb` (or `arm64`) with `sudo apt install ./runberth-app_*.deb`, or the `.rpm` with `sudo dnf install ./runberth-app-*.rpm`, then start **Runberth** from your applications. The menu shows when a newer release is available.

On GNOME, which has no tray of its own, install and enable the AppIndicator extension, for example `sudo apt install gnome-shell-extension-appindicator` on Ubuntu, then log out and back in.

To start the app with your session, turn on **Launch at Login** in Settings > General on macOS, or in the menu on Linux.

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

Set `host`, `user`, `port`, and `key` as for `scripts/install-vps.sh`; without `key`, ssh uses your agent and `~/.ssh/config`. When Runberth runs on the same Linux computer as the app, set `host` to `localhost`: on port 22 the app runs `sudo -n /opt/runberth/bin/runberth status --json` itself, without SSH, and the menu names the server **This computer**. That needs the same passwordless sudo rule for your account that `--sudoers` installs. On any other port, such as a virtual machine's forwarded SSH port, `localhost` still connects with ssh. `notifications` is `all` (Everything in Settings: each finished run, and the start and end of the app's own work, such as a connect), `failures` (Failures and fixes: failed runs, a workflow passing again after a failure, and the app's own work that failed), or `off`; the menu and the Activity window show the app's work either way. The app refreshes every `interval_seconds`, every 10 seconds while runs are in progress, and when you open the menu. It reads the file again on every refresh, so edits apply without a restart. The token is never in this file; on macOS you can also store it from a terminal with `security add-generic-password -s Runberth -a github -w`, which prompts for it.

## Connect, disconnect, and logs

**Connect Repository** in the menu lists your private repositories that are not connected yet. Choose one to connect it, as `runberth connect` does on the server: the repository needs `.github/runberth.json` on its default branch, as in [Connect a repository](../README.md#2-connect-a-repository). To disconnect one, open its submenu and choose **Disconnect**, then confirm with **Disconnect OWNER/REPO**. Change its jobs' `runs-on` first, as the [personal account guide](personal-account-setup.md) describes. The menu shows the work in progress, and a notification reports the result or the server's error.

**Show Logs** in a repository's submenu saves its latest 300 lines of lock helper notes and runner journal, as `runberth logs` prints them, to the app's cache folder and opens them in your text editor. To follow them live, run `runberth logs OWNER/REPO --follow`.

## The runberth command

The `runberth` command runs Runberth's commands on the server in the app's settings, over SSH or directly for localhost. It comes with the app, and also on its own, without the tray app, for macOS and Linux on x86_64 or arm64:

```sh
curl -fsSL https://github.com/vantalter/runberth/releases/latest/download/install.sh | sh
```

The installer picks the build for your system, checks its SHA-256, and installs it as `/usr/local/bin/runberth`, using sudo only when that folder is not writable; set `RUNBERTH_INSTALL_DIR` for another folder, or `RUNBERTH_VERSION=v0.2.0` for a release other than the latest. In the macOS app, **Install Command Line Tool…** in Settings > General links the app's own binary there instead, so the command updates with the app; once it is installed, the same button removes that link. The Linux packages install it as `/usr/bin/runberth`, and removing the package removes it. To remove a copy the installer put there, delete the file: `sudo rm /usr/local/bin/runberth`, or the `runberth` file in your `RUNBERTH_INSTALL_DIR`. Your settings and stored token stay; the app's settings and the Keychain or Secret Service item hold them.

Without the app, `runberth config` writes the settings the app would use, and shows them when run alone:

```sh
runberth config --host 203.0.113.10 --user ubuntu --key ~/.ssh/vps_ed25519
gh auth token | runberth config --token-stdin
runberth config
```

Then:

```sh
runberth init                    # in a repository's checkout: writes .github/runberth.json
runberth connect OWNER/REPO
runberth disconnect OWNER/REPO
runberth status --watch 5
runberth logs OWNER/REPO --follow
runberth sync OWNER/REPO         # after changing its .github/runberth.json
runberth packages                # what Runberth installed, and who needs each
runberth install --swap 3 --on-demand
runberth update
runberth cleanup
```

`init` writes the repository's opt-in file with the values of the server in the settings, so they match its host config; commit it to the default branch before `connect`. It writes at the top of the Git repository around the current folder, and when the file is there with other values, shows how they differ and asks before replacing it, keeping it unless you answer yes. Without a terminal, as in a script, it replaces the file only with `--force`. `--print` prints the file instead. `--runberth` and `--label` stand in for a server that does not report its values yet, and are refused when they differ from what the server reports, since `connect` would refuse them too: to use another label, install the server with `runberth install --label`. `runberth init --help` lists the options.

`install` sets up Runberth on the server in the settings, as `scripts/install-vps.sh` does from a checkout: it installs the release that matches the app's version, checked against its published SHA-256, with the host config for your GitHub token's account (or `--owner NAME`) and the label `runberth-ci` (or `--label`). It also installs the sudoers rule the app needs; `--swap`, `--packages`, and `--on-demand` add the same extras as the script. `update` installs the app's version over an existing installation and keeps its host config, restarting the on-demand dock if it runs. Both allocate a terminal, so `sudo` can ask for a password; `runberth install --help` lists the options. When the server's Runberth is older than the app, or lacks the sudoers rule, the menu offers **Update Server…**, which runs `runberth update` in the app and shows its steps, as any of its work; only when sudo on the server asks for a password does it continue in a terminal window.

When the server answers but has no Runberth, as a new server or one just cleaned up, the menu says so. On macOS, **Install Runberth…** in Settings > Server, beside Test Connection, asks for the swap size and on-demand runners; it installs only what every runner needs, `git`, `curl`, `unzip`, and `ca-certificates`, as each repository lists its own build packages in its `.github/runberth.json`. **Install** then runs `runberth install` with those options. If sudo on the server asks for a password, which needs a terminal, the install continues in Terminal, and the app sees it finish once the server has Runberth. On Linux, run `runberth install` yourself. Clean Up… is disabled while the server has no Runberth, and Install Runberth… while it has one.

`runberth cleanup` shows what the [cleanup](cleanup.md) would remove from the server; `runberth cleanup --execute` removes it, deleting each runner's GitHub registration with the piped or stored token. In the macOS app, **Clean Up…** in Settings > Server shows the same plan and removes everything only after you choose **Remove Everything**; Return does not choose it, and Escape cancels. Change the repositories' `runs-on` first.

`connect` and `disconnect` read a GitHub token from stdin when one is piped, as in `gh auth token | runberth connect OWNER/REPO`, and otherwise use the token stored for the app. Run `runberth --help` for the options.

## What the app is doing

Work that takes a while, such as installing, cleaning up, connecting, disconnecting, syncing a repository's packages, or testing the connection, shows the same way everywhere. The tray icon spins and its tooltip names it; the top of the menu shows it with a running clock and the server's latest step, as in `Connecting aerofoil… 0:42 · Installing runner dependencies`. Work made of steps, such as installing, updating, cleaning up, connecting, disconnecting, and syncing, says which step it is on, as `Updating Runberth on the server · 3 of 7`; the server prints them as `[3/7] Installing Runberth`, and the rest of what it prints goes to the output. A server older than these steps shows its latest line instead. When it ends, the menu shows the result for a few minutes, or until the next one starts: `✓ Connected aerofoil · took 1:12` in green, or `✗ Couldn't connect aerofoil: the reason` in red. A notification says when work you started begins and ends, and when automatic work, such as a sync, ends. On macOS, **Show Details…** under it in the menu opens the **Runberth Activity** window, which also opens by itself when you install, clean up, or test the connection from Settings: an entry for each piece of work, with the server it works on and its time. Work that numbers its steps also shows the current one, as `Installing Runberth…` and `Step 5 of 7`, above a bar that fills with the steps finished, so it is full only when the work reports it is done; other work shows only its title. **Operation details** shows what the work printed, while it runs or after: step lines as headings with what each printed indented beneath, advice such as `Recommendation:` in amber, and, once the work ends, its result. Below the log, **Receiving output** says lines still arrive, or how long the work has been quiet, which a long package install often is and which is no sign of failure; the log follows the newest line until you scroll up to read, and scrolling back to the end follows again. The log is in the system's monospaced font, so aligned columns and tables keep their shape; ⌘+ and ⌘− change its size and ⌘0 restores it, and resizing the window gives the log more room. **Copy logs** copies the selected text, or all of it, and a counter redrawn in place, as apt's `Reading database ... 45%`, keeps one line. Quitting from the menu while work runs asks first, as quitting partway can leave the server half changed; **Keep Running** is the default. On Linux, **Show Output** in the menu opens the output.

## Troubleshooting

- **"too old for this app"** or **"needs Runberth's sudoers rule":** choose **Update Server…** in the menu, or run `runberth update`.
- **"Runberth not installed":** choose **Install Runberth…** in Settings > Server on macOS, or run `runberth install`.
- **A build fails with "command not found" or a missing linker:** the server lacks a package the workflow needs. The failing step on GitHub names it; **Show Logs** shows only the runner's side. Add it to the repository's `.github/runberth.json`, as `"packages": ["gcc"]`, on its default branch. The app syncs the change within a few minutes and notifies you ("Synced app: Installed for vantalter/app: gcc"); without the app, run `runberth sync OWNER/REPO`. Then run the job again. `runberth packages` lists what Runberth installed and who needs each. For a package the host needs rather than a repository, `runberth packages add NAME` installs it, kept until cleanup.
- **No notifications on macOS:** allow them for Runberth in System Settings > Notifications.
- **No icon on Linux:** the desktop needs a StatusNotifier tray, such as KDE's, or GNOME's AppIndicator extension. The app prints a message when it finds none.

To see what the app shows on each refresh, run it from a terminal with `RUNBERTH_APP_TRACE=1`: it prints each menu as JSON. On macOS, run `/Applications/Runberth.app/Contents/MacOS/runberth`.

To report a problem, choose **Report a Problem…** in Settings > Support on macOS or in the menu on Linux, or run `runberth report`. It opens a new GitHub issue with the app's version, your system and processor, and the server's Runberth version and mode filled in. It never includes the host, user, key, repositories, or token. Nothing is sent until you submit the issue; add the steps and any logs first.

## Build from source

The app lives in `app/`, a member of the Cargo workspace.

- **Linux:** install `libglib2.0-dev`, `libjson-glib-dev`, and `pkg-config`, then `cargo run -p runberth-app`. The release builds `.deb` and `.rpm` packages from `app/packaging/linux/nfpm.yaml`.
- **macOS:** `cargo run -p runberth-app` runs the menu bar item without notifications, Sparkle, or Launch at Login, which need the app bundle. `dist/Runberth.app/Contents/MacOS/runberth --settings` starts with the Settings window open. `app/scripts/build-macos-app.sh` builds `dist/Runberth.app` with Xcode and signs it with your Apple Development certificate when you have one. `app/scripts/check-macos-app.sh` checks the result.

## Releasing the macOS app

Each release builds the app with the server binaries; see [Releasing](../README.md#releasing). The macOS app is signed and notarized when the repository has these Actions secrets, and ad-hoc signed otherwise:

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
