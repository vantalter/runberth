# Runberth

**Superlight GitHub Actions runner management. Your workflows. Your server. Your terminal.**

Runberth brings your existing GitHub Actions workflows to a small Linux server. Install it once, connect your private repositories, and serialize their workflow steps across the host. Keep your workflow files, PR checks, and push triggers in GitHub.

Runberth is a single static Rust binary with no runtime dependencies, and by default it adds no persistent background service; the experimental [on-demand mode](#5-on-demand-runners-experimental) adds one small dock process. GitHub's official runners handle communication with GitHub; systemd manages their lifecycle. A small lock helper runs while a job is active. There is no Runberth database, webhook endpoint, or Docker or Kubernetes requirement; an optional [desktop app](docs/desktop-app.md) shows the server from your Mac or Linux desktop.

> **Early preview.** Local tests cover setup logic, job coordination, and cleanup. Installation, connecting repositories, and on-demand mode run on a real 1 GB VPS. Shutdown and out-of-memory recovery, and arm64 on real hardware, still need validation. Host capacity and build performance have not been measured yet.

## What you get

- **One host, multiple projects.** A shared lock serializes workflow execution after each runner's job-start hook.
- **Configuration in each repository.** Projects opt in with a small `.github/runberth.json` file.
- **Automated connection.** Runberth verifies private ownership, installs runtime dependencies, checks the official runner archive's checksum, registers the runner, and starts its service.
- **Separate runner accounts.** Each repository gets its own Unix user, work directory, and caches.
- **Tracked setup and removal.** Review a cleanup plan before removing registrations, services, accounts, and the packages and swap created through Runberth's helpers.

## Get started

The current target is **Ubuntu 24.04 or 26.04, x64 or arm64, with systemd and cgroup v2**, for trusted private repositories owned by one personal GitHub account. The host commands checked by preflight must be available; no compiler or language runtime is needed. The server needs outbound access to GitHub and your build dependencies; no inbound CI port is required.

### 1. Set up the host

The quickest way is the `runberth` command for your computer, on macOS or Linux. Install it, point it at the VPS, and set the server up, all without a checkout:

```sh
curl -fsSL https://github.com/vantalter/runberth/releases/latest/download/install.sh | sh
runberth config --host 203.0.113.10 --user ubuntu --key ~/.ssh/vps_ed25519
runberth install --owner YOUR_GITHUB_ACCOUNT --swap 3
```

The installer picks the build for your system, checks its SHA-256, and installs it into `/usr/local/bin`. `runberth install` downloads the matching server release for the VPS's processor and runs the steps below; see the [command's guide](docs/desktop-app.md#the-runberth-command). The [desktop app](docs/desktop-app.md) includes the same command.

From a checkout instead, clone Runberth, then write your host config. Copy [config/host.json](config/host.json) to a file of your own, such as `~/runberth-host.json`, and set:

- `owner`: your personal GitHub account, which owns the private repositories you will connect;
- `runberth`: a name for this installation, `YOUR_ACCOUNT/runberth`, which each repository's opt-in file repeats; it does not need to be a real repository;
- `runner_label`: the label your jobs will request; `runberth-ci` is fine.

Then install over SSH with the VPS's address and your private key. The script downloads the latest [release](https://github.com/vantalter/runberth/releases) and checks its SHA-256, so you need neither a fork nor a GitHub login for this:

```sh
export RUNBERTH_HOST=203.0.113.10 RUNBERTH_KEY=~/.ssh/vps_ed25519 RUNBERTH_USER=ubuntu
scripts/install-vps.sh --config ~/runberth-host.json --swap 3 --sudoers
```

The script uploads the binary with your host config and runs `preflight` and `install-host` on the server. `--swap` adds a Runberth-owned swap file. `install-host` installs what every runner needs; each repository lists its own build dependencies in its `.github/runberth.json` (see step 2), and `--packages` adds any the host itself needs, recorded so cleanup removes them. `--sudoers` installs the passwordless sudo rule for `connect`, `sync`, and `cleanup`; it is not needed when connecting as root. `--version v0.1.0` pins a release; `--binary PATH` installs a binary you [built yourself](#development) instead, checking a `PATH.sha256` beside it. The server's SSH host key must already be in your `known_hosts`, or pass `--accept-new-host-key` to trust it on first connection. The `RUNBERTH_*` variables stand in for `--host`, `--key`, and `--user` here and in `scripts/vps.sh`. Re-run the script to update Runberth to the latest release. Run `scripts/install-vps.sh --help` for every option.

To install by hand instead, download the binary and its `.sha256` from the [releases](https://github.com/vantalter/runberth/releases), copy them and your host config to the VPS as `host.json`, check the binary with `sha256sum -c`, then run `./runberth preflight` and `sudo ./runberth install-host --config host.json`.

Follow the [setup guide](docs/personal-account-setup.md) to install your workflows' build dependencies through the package tracker.

### 2. Connect a repository

Commit this file as `.github/runberth.json` on the project's default branch. Its values must match the host config:

```json
{
  "version": 1,
  "runberth": "YOUR_ACCOUNT/runberth",
  "runner_label": "runberth-ci"
}
```

List the apt packages the project's workflows need on the server, such as a C toolchain, as `"packages": ["gcc", "libc6-dev"]`. Connecting installs them; Runberth itself installs only what every runner needs: `git`, `curl`, `unzip`, and `ca-certificates`. Packages are shared: one that two projects list stays until neither does, and one the server had before Runberth always stays. Only the default branch's file counts, so a pull request cannot make the server install anything. When the file changes there, the desktop app notices within a few minutes and runs `runberth sync OWNER/REPO`, which installs what was added and removes what nothing needs any more; without the app, run it yourself. A server older than 0.2.3 refuses a file with `packages`: update it first.

`runberth init`, run in the project's checkout, writes it with the server's own values, and prints the `runs-on` line for its jobs. A server installed before `runberth init` existed does not report them: pass `--runberth YOUR_ACCOUNT/runberth --label runberth-ci`, or update it first.

From your computer, with GitHub CLI authenticated:

```sh
gh auth token | runberth connect YOUR_ACCOUNT/PROJECT
```

In the desktop app, choose **Connect Repository** in the menu instead. Without the `runberth` command, run it over SSH with the [passwordless sudo rule](docs/personal-account-setup.md#install-runberth-once) that `runberth install` and `install-vps.sh --sudoers` set up: `gh auth token | ssh ADMIN@VPS 'sudo -n /opt/runberth/bin/runberth connect YOUR_ACCOUNT/PROJECT'`.

The token needs access to read the private repository's manifest and manage its runners. Runberth reads it from stdin without saving it. The connection command fetches the committed manifest and prepares the repository's runner. Repeat for each project; there is no central repository list. Removing a manifest later does not unregister its runner; `runberth disconnect OWNER/REPO` does, as described in the [setup guide](docs/personal-account-setup.md#removal).

### 3. Route your existing jobs

Set eligible Linux jobs to use the configured label:

```yaml
runs-on: [self-hosted, linux, x64, runberth-ci]
```

On an arm64 server, such as an Ampere or Graviton instance, use `arm64` in place of `x64`: GitHub labels each runner with its processor. Keep existing triggers and check names. Independent jobs remain independent checks; the shared lock makes their workflow steps execute sequentially on the server. GitHub's runner may prepare or download actions before it runs the job-start hook, so that preparation can overlap across runners. A waiting job's runner worker also stays in memory while it waits, so count one idle worker per queued job alongside the running build. Waiting for the lock happens inside the job, so allow time for both the queue and the build in `timeout-minutes`.

### 4. Watch the host

From your computer:

```sh
runberth status --watch 5                  # memory, swap, load, lock holder, runner services
runberth logs OWNER/REPO --follow          # lock helper notes and the runner's service journal
```

`status` and `logs` only read, and the sudoers rule allows them without a password. From a checkout, `scripts/vps.sh` runs the same commands over SSH with the `RUNBERTH_*` variables, as in `scripts/vps.sh status --watch 5`.

### 5. On-demand runners (experimental)

Each connected runner normally keeps GitHub's runner listener running, about 70 MB each on a small host. In on-demand mode, one small `runberth dock` process waits for work for every connected repository instead, and starts a repository's runner only when a job arrives:

```sh
runberth install --on-demand                # or on an existing server: scripts/vps.sh on-demand on
runberth status                             # runners show "waiting (on demand)"
scripts/vps.sh on-demand off                # back to always-running runners
```

`runberth update` keeps the mode and restarts the dock. `install-vps.sh --on-demand` turns it on from a checkout.

The dock waits on each runner's behalf with that runner's own credentials. When a job arrives it closes its connection without taking the job, and the runner it starts receives the job from GitHub. After 10 idle minutes (`on-demand on --idle-minutes N`) the dock stops the runner again. A job therefore waits for the runner to start, typically 5 to 15 seconds. This relies on the part of GitHub's runner protocol that waits for work, which GitHub does not document; a GitHub change could break it, and `on-demand off` returns to the standard setup.

### 6. Desktop app

The Runberth app for macOS and Linux shows the server in the menu bar or tray: memory and the lock holder, each connected repository's runs and the jobs in progress, and a notification when a run finishes. From the menu you can also connect and disconnect repositories, open a runner's logs, and update the server; on macOS, Settings can clean the server up. It reads the server with `status --json` over SSH and the runs from GitHub. See the [desktop app guide](docs/desktop-app.md) to install and set it up.

## Small footprint, realistic limits

Runberth is designed to add little machinery around GitHub's runners. Its only process during a job is the lock helper, which holds about 1.5 MB of resident memory (measured on Ubuntu 24.04; the earlier Python helper used about 16 MB). Between jobs, Runberth runs nothing. Each connected personal repository still needs its own running listener, and each account keeps its own toolchains and caches. Idle RAM and disk use grow with the number of connected projects.

A **1 GB VPS with 2–4 GB swap and around 30 GB free disk** is an experimental starting point for a small set of lightweight Linux workflows. It is not a tested capacity guarantee. Measure idle memory, peak build memory, swap use, and cold-build duration before adding projects. Swap adds capacity at a performance cost.

Serialization limits concurrent jobs, not the memory used inside one job. Start Rust builds with `CARGO_BUILD_JOBS=1`, disable Vitest file parallelism, and use one Playwright worker where applicable. Heavier builds may need a larger host. macOS and Windows jobs need their own compatible runners.

Use a dedicated CI host for trusted code. Separate Unix accounts and the shared lock do not sandbox untrusted workflow code; public repositories are rejected during connection.

## Remove the installation

Preview the recorded resources from your computer, or on the VPS with `sudo /opt/runberth/bin/runberth cleanup`:

```sh
runberth cleanup
```

`runberth cleanup --execute` removes them, and in the macOS app **Clean Up Server…** in Settings > Server does the same after you confirm. Follow the [cleanup guide](docs/cleanup.md) to unregister runners and remove tracked server resources. Workflow files and repository manifests remain in their repositories. Cleanup covers recorded installation resources; it cannot guarantee removal of arbitrary changes made by workflow code.

## Development

Runberth needs Rust 1.87 or later. Run the checks with:

```sh
cargo test                  # the server
cargo test --workspace      # the server and the desktop app
cargo clippy --workspace --all-targets
```

The desktop app needs native libraries on Linux; see [Build from source](docs/desktop-app.md#build-from-source). The job lock and cleanup tests drive the real binary, and the Linux-only cgroup test runs on Linux. To build the static release binary on a Linux x86_64 machine (on arm64, use `aarch64-unknown-linux-musl`):

```sh
sudo apt-get install musl-tools
rustup target add x86_64-unknown-linux-musl
cargo build --release --target x86_64-unknown-linux-musl
```

The binary is `target/x86_64-unknown-linux-musl/release/runberth`. Run `runberth` without arguments to list its commands.

### Releasing

Releases are prepared by [release-please](https://github.com/googleapis/release-please). Pull requests are squash-merged under their title, which CI checks is a [Conventional Commit](https://www.conventionalcommits.org): `fix: …` for a bug fix, `feat: …` for a new feature, and `docs:`, `ci:`, `test:`, `refactor:`, `build:`, or `chore:` for changes users do not notice; add `!`, as in `feat!: …`, for a change that breaks existing setups. The title becomes a line in the changelog and in the macOS app's update window, so after the type, say what changed for users. Do not bump the version or edit `CHANGELOG.md` by hand.

After each merge to `main`, release-please keeps a release pull request open, titled like `chore(main): release 0.3.0`, that sets the next version under `[workspace.package]` in `Cargo.toml` and in `Cargo.lock`, and adds the changes to `CHANGELOG.md`. While Runberth is below 1.0, `feat:` and breaking changes raise the minor version (0.2.5 to 0.3.0), and `fix:` and `perf:` the patch version (0.2.5 to 0.2.6). Other types start no release.

To release, merge the release pull request. release-please tags the merge and creates a draft release with the changelog; the release workflow then tests and builds the server binaries for x86_64 and arm64, the `runberth` command and its installer, the macOS app, and the Linux tray app's packages, attaches them with their checksums, puts [.github/release-notes.md](.github/release-notes.md), a guide to which file to download, above the changelog, and publishes the release. Until then it stays a draft, so the app's updates and `install.sh` never find a release without its files. If the build fails, fix the cause and re-run the failed jobs from the Actions tab; the draft waits.

The release pull request is opened with the workflow's own token, so CI does not run on it; it changes only the version and the changelog. Two repository settings are needed: **Settings → Actions → General → Allow GitHub Actions to create and approve pull requests**, and, under **Settings → General → Pull Requests**, squash merging with **Default to pull request title**.

release-please makes no pre-releases. To test a build first, tag the release pull request's branch, which already has the new version, with a release candidate tag and push it:

```sh
git fetch origin
git tag v0.3.0-rc.1 origin/release-please--branches--main && git push origin v0.3.0-rc.1
```

It is published as a pre-release, which the `latest` downloads, `install.sh`, and the app's updates all skip, and `runberth update --version v0.3.0-rc.1` installs it on a server. Any tag pushed by hand runs the release workflow, if it matches the version in `Cargo.toml`. See [Releasing the macOS app](docs/desktop-app.md#releasing-the-macos-app) for its signing secrets.

The desktop app is the workspace member `app/`; `cargo test -p runberth-app` tests it alone, and `cargo build -p runberth-app --no-default-features` builds the `runberth` command without the tray. See [Build from source](docs/desktop-app.md#build-from-source) for the macOS app bundle.

Reports from real installations are especially useful during this preview. Choose **Report a Problem…** in the desktop app, run `runberth report`, or [open an issue](https://github.com/vantalter/runberth/issues/new?template=bug_report.yml): the form fills in the versions, OS, and processor, and asks for the rest. Connected runner count, workload, and memory observations help too. Remove tokens and repository secrets from logs before sharing them.

## License

Runberth is released under the [MIT License](LICENSE). The macOS app also includes [Sparkle](https://sparkle-project.org) and GitHub's mark from [Primer Octicons](https://github.com/primer/octicons), each under its own MIT License.
