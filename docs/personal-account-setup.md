# Personal-account runner setup

This procedure is for a dedicated Ubuntu 24.04 or 26.04 VPS, x64 or arm64. Do not run it on a server that hosts anything else you care about: builds use all of its memory, and runner jobs execute repository code on it.

## Install Runberth once

1. Write your host config from [`config/host.json`](../config/host.json): set `owner` to your personal GitHub account, `runberth` to a name for this installation such as `YOUR_ACCOUNT/runberth` (each repository's opt-in file repeats it; it need not be a real repository), and choose one runner label.
Steps 2 to 6 are what `scripts/install-vps.sh` does from your computer over SSH; see the README. To do them by hand:

2. Get the `runberth` binary for the server's processor. Runberth is a single static Rust binary, so the VPS needs no compiler or language runtime. Download `runberth-x86_64-unknown-linux-musl`, or `runberth-aarch64-unknown-linux-musl` for arm64, and its `.sha256` file from the [releases](https://github.com/vantalter/runberth/releases), or build it yourself as described in the README. Copy the binary and your host config to a temporary setup directory on the VPS, check the binary with `sha256sum -c` and its `.sha256` file, rename it to `runberth`, and make it executable.
3. From that directory, run `./runberth preflight`; it only reads local system information.
4. Run `sudo ./runberth install-host --config host.json`. It installs the binary itself to `/opt/runberth/bin/runberth`, the host config, root-owned job hook scripts that call the binary, a group-writable lock file, and an installation registry in `/etc/runberth`. Re-running it with a different host config is refused. To update Runberth later, run the same command with the new binary.
5. Run `sudo /opt/runberth/bin/runberth add-swap 3` for a Runberth-owned 3 GB swap file if needed. `install-host` installs what every runner needs, `git`, `curl`, `unzip`, and `ca-certificates`; each repository lists its own build dependencies in its manifest, and connecting installs them. For anything the host needs beyond that, use `sudo /opt/runberth/bin/runberth add-packages NAME...`, which records the packages so cleanup removes them. Both commands refuse to run before step 4. Do not let PR workflow code run `sudo`.
6. The commands below stream a GitHub token over SSH into `sudo -n`, which cannot prompt for a password. Give the admin account a `sudoers` rule, edited with `visudo`, that allows only the two token-reading commands without a password:

   ```
   ADMIN ALL=(root) NOPASSWD: /opt/runberth/bin/runberth connect *, /opt/runberth/bin/runberth sync *, /opt/runberth/bin/runberth disconnect *, /opt/runberth/bin/runberth cleanup, /opt/runberth/bin/runberth cleanup *, /opt/runberth/bin/runberth status, /opt/runberth/bin/runberth status *, /opt/runberth/bin/runberth logs *
   ```

   `cleanup *` and `status *` match only with arguments, so the bare forms are listed separately. `status` and `logs` only read. Naming the subcommands keeps the rule from granting the binary's other root commands, or general root access.

Runberth has no inbound network port. Runner services poll GitHub over outbound HTTPS; builds also need access to their package and toolchain sources.

## Opt in each private repository

1. Commit `.github/runberth.json` to the repository's default branch, using the [example manifest](../examples/repository/.github/runberth.json). Its `runberth` and `runner_label` must match the host config. List the apt packages its workflows need, such as build tools or Playwright's Linux browser system dependencies, as `"packages": [...]`; connecting installs them, and `runberth sync OWNER/REPO` applies a later change to the default branch's file. Add the manifest only to private projects that have suitable Linux Actions jobs. The manifest contains no token, VPS address, or deployment secret.
2. Use an admin-capable GitHub token for that private repository. From a trusted computer with `gh` authenticated, send it once to the VPS without saving it there:

   ```sh
   gh auth token | ssh ADMIN@VPS 'sudo -n /opt/runberth/bin/runberth connect vantalter/REPO'
   ```

   The command reads the committed manifest through the [GitHub repository contents API](https://docs.github.com/en/rest/repos/contents#get-repository-content) and confirms private personal ownership and admin access. A missing, public, or mismatched repository is rejected before server changes. It then creates the repository's dedicated runner account and state directory, and installs its runner:

   - installs any missing runner runtime libraries (ICU, Kerberos, LTTng, OpenSSL, zlib) through package tracking, choosing each release's package names the way the runner's own dependency script does,, so cleanup can remove them;
   - downloads the latest official Linux [runner release](https://github.com/actions/runner/releases) for the server's processor, x64 or arm64, and checks it against the SHA-256 GitHub publishes for it;
   - registers it as `runberth-REPO` with the host config's label, using a one-hour [registration token](https://docs.github.com/en/rest/actions/self-hosted-runners#create-a-registration-token-for-a-repository) that is passed to `config.sh` through its environment and never saved;
   - adds both job hooks, its private state directory, and the shared lock file to the runner's `.env`, running as the runner's own account;
   - writes and starts its systemd unit, `actions.runner.vantalter-REPO.runberth-REPO.service`.

   Use `/opt/runberth/bin/runberth user-for vantalter/REPO` to display its deterministic Unix username. Repeat for each participating private repository. No central repository list is needed. The manifest is checked at connection time; removing it later does not automatically unregister an existing runner.

Re-running `connect` for a connected repository keeps the existing registration and repairs its hooks and service; it does not restart a running service, so it never aborts an active job. If registration fails before the runner's `.runner` file is written, Runberth removes the partial runner directory so you can fix the cause and run it again; `--replace` lets the retry take over a registration GitHub may already have recorded. Once `.runner` exists the directory is kept, because it holds the GitHub runner ID that cleanup uses; re-run `connect` to finish setup. Pressing Ctrl-C or losing the SSH session during `connect` stops it at the next step, so this rollback still runs.

The unit is written by Runberth rather than by the runner's `svc.sh`, so root never runs a script the runner user can modify. It is based on GitHub's [runner service template](https://github.com/actions/runner/blob/main/src/Misc/layoutbin/actions.runner.service.template), which uses `KillMode=process` and can leave the lock helper or a build process running when the service stops. Runberth's unit uses `KillMode=control-group` so a service stop terminates all processes in its cgroup. On a stop, the lock helper keeps the host lock until every other process in that cgroup has exited, so a build that is slow to handle SIGTERM cannot overlap the next repository's job; if something ignores SIGTERM, systemd kills the helper and the remaining processes together after `TimeoutStopSec` (5 minutes). The unit also uses `OOMPolicy=stop` so an out-of-memory kill anywhere in the service stops the whole cgroup. The kernel may kill `Runner.Worker` rather than the build; the worker then never runs the completion hook, and only stopping the cgroup ends the lock helper and releases the host lock. The affected job is reported as a lost runner rather than a failed step. `Restart=on-failure` restarts the service 30 seconds later; the next job's start hook removes the stale lease. After the first deliberate out-of-memory test on the VPS, confirm with `systemctl status` that the unit came back. Do not stop a service during an active job except when intentionally aborting it.

The job-completed hook runs before the runner's own "Cleaning up orphan processes" step, so a process a job left in the background, such as a server started with `&`, would otherwise outlive the lock. When the completed hook runs, the lock helper acknowledges at once and then holds the lock until every process of that job has exited, in the runner's cgroup or any cgroup below it. A process belongs to the job when it started during the job, runs under the runner's account, and carries the job's `RUNNER_TRACKING_ID`: the same processes the runner's orphan cleanup ends, normally within seconds. Processes without that ID never count, because the runner's next job worker appears without one. If the job's processes are still running after `RUNBERTH_DRAIN_SECONDS` (30 seconds by default; set it in the runner's `.env`), the helper sends them SIGTERM, then SIGKILL 10 seconds later, and records their process IDs in `/var/lib/runberth/RUNNER_ID/RUNNER_ID.log`. If the hooks receive no `RUNNER_TRACKING_ID`, the helper releases the lock at completion and records that in the same log; check it after the first job on a new host.

Two kinds of process escape both the runner's orphan cleanup and this check: a process a job starts without its `RUNNER_TRACKING_ID`, and Docker containers, which run in the Docker daemon's cgroup rather than the runner's. Runberth does not install Docker; if a workflow uses containers, make sure each job stops its own before finishing.

## Update workflows and verify

Keep each repository's existing PR, `main`, manual, and tag triggers and check names. Route eligible Linux jobs to `runs-on: [self-hosted, linux, x64, runberth-ci]`, or `arm64` in place of `x64` on an arm64 server; use the label in its manifest. See [moving workflows](workflows.md) for timeouts, memory settings, and jobs that need separate handling.

Run `cargo test` in Runberth. After VPS setup, queue jobs in two private repositories and confirm that the second waits in `Set up runner` until the first finishes. Check service logs, memory, swap, disk, and cold-build duration. Every runner on this VPS must have both hooks configured; a runner without them bypasses serialization.

Do not use this persistent host for public repositories or untrusted PR authors. If a helper or runner crashes and the lock remains held, stop all runner services, confirm no build is still active, terminate any remaining `runberth job hold` process, then start the services.

## Removal

To disconnect one repository, change its jobs' `runs-on` back first, then stream a token as for `connect`:

```sh
gh auth token | ssh ADMIN@VPS 'sudo -n /opt/runberth/bin/runberth disconnect vantalter/REPO'
```

It refuses while a job is running on that repository's runner. Otherwise it stops the runner's service, deletes its GitHub registration, and removes its service, account, home (with the runner and its caches), and state. Runberth itself, other repositories' runners, swap, and tracked packages stay. If GitHub registration was already removed, pass `--github-removed` instead of a token. Remove `.github/runberth.json` from the repository too, so it is not reconnected by mistake.

To remove everything, use the [cleanup guide](cleanup.md) and `runberth cleanup`. Cleanup shows a dry run by default. It removes only recorded host resources; repository manifests and workflows remain in their source repositories.
