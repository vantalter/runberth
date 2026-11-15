# Remove Runberth from a VPS

Runberth records its runner users and paths in `/etc/runberth`, plus any swap or apt packages installed through its setup helpers. Cleanup removes those recorded resources, runner services, runner homes and caches, systemd drop-ins, shared hooks and lock files, and the registry itself. It does not touch unrelated services, files, or packages. Source repository workflows and `.github/runberth.json` manifests remain in their repositories and must be changed separately if you no longer want them to request the retired runner.

## Review first

On the VPS, run the installed cleanup command:

```sh
sudo /opt/runberth/bin/runberth cleanup
```

This is a dry run. Check every repository, GitHub runner ID, service unit, user, swap file, and package in its output. Cleanup refuses unexpected paths, unit names, or users. Stop submitting new CI jobs while removing the server.

## Execute

The complete path uses a temporary GitHub token with repository Administration permission for every recorded private repository. The command reads the token from stdin, stops all recorded services, and deletes their exact runner IDs using the [GitHub REST API](https://docs.github.com/en/rest/actions/self-hosted-runners#delete-a-self-hosted-runner-from-a-repository). A recorded runner with no local ID, for example after an interrupted registration, is looked up by its name `runberth-REPO` and deleted if found. It then removes the recorded server resources. The token is not saved to disk. For example, an operator can stream `gh auth token` from their own computer over an authenticated SSH connection to the cleanup command, provided the admin account has the passwordless `sudoers` rule from [setup step 6](personal-account-setup.md#install-runberth-once):

```sh
gh auth token | ssh ADMIN@VPS 'sudo -n /opt/runberth/bin/runberth cleanup --execute --github-token-stdin'
```

If GitHub runner registrations were **already removed** from your computer with `gh api -X DELETE repos/OWNER/REPO/actions/runners/RUNNER_ID`, run `sudo /opt/runberth/bin/runberth cleanup --execute --github-removed` on the VPS instead. Stop the runner services before deleting registrations in this two-step route. GitHub's [runner removal guide](https://docs.github.com/en/actions/how-tos/manage-runners/self-hosted-runners/remove-runners) explains the remote lifecycle.

If swap is still needed by another workload, do not run cleanup until it can be turned off safely. Cleanup stops if `swapoff` fails. If apt's simulated purge would remove an untracked package, cleanup stops for review.

After cleanup, use the provider's OS rebuild if you need a guarantee that no job-generated files or system changes remain. A persistent self-hosted runner executes repository code, so no uninstall script can prove that such code made no changes outside Runberth's recorded paths.

Cleanup also removes the passwordless sudo rule, `/etc/sudoers.d/runberth`, when it is root-owned and grants only Runberth's own commands, as `scripts/install-vps.sh --sudoers` writes it. A file there with any other rule is left in place and reported. The systemd journal's entries for Runberth's services stay, as the journal owns them.
