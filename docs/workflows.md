# Moving workflows to Runberth

These are changes you make in each connected repository's workflows; Runberth does not edit them. Connect the repository first, then move one workflow at a time. Keep existing `pull_request`, `push`, and manual triggers and existing check names, especially any that branch protection requires.

## Route jobs to the runner

Point eligible Linux jobs at the runner with the label from your host config:

```yaml
runs-on: [self-hosted, linux, x64, runberth-ci]
```

Each runner executes one job at a time, and Runberth's job hooks serialize workflow steps **across** the separate runners. The runner can prepare or download actions before the job-start hook takes the lock, so that preparation can overlap on the host, and each queued job keeps its runner worker process in memory while it waits. Measure that alongside the running build when sizing the host. A workflow's `concurrency` setting can still cancel stale runs, but it is not a host-wide queue.

Keep independent jobs independent, so a failure in one does not skip another's check; Runberth runs them one after another.

## Timeouts include the queue

A job waits for the host-wide lock inside its own "Set up runner" step, so that wait counts against the job's `timeout-minutes`. Set each job's timeout to its own cold duration **plus** the longest job that can hold the lock ahead of it. For example, a 15-minute job queued behind a 12-minute build times out before its first step. Measure cold runs first, then set timeouts from the slowest job; until those numbers exist, start with 45 minutes.

## Memory on a small host

Serialization limits concurrent jobs, not the memory one job uses. Common settings for a 1 GB host:

- **Rust:** set `CARGO_BUILD_JOBS: "1"` in the job environment. If cold builds still exhaust memory or disk, add `CARGO_PROFILE_DEV_DEBUG: "0"`, `CARGO_PROFILE_TEST_DEBUG: "0"`, and `CARGO_INCREMENTAL: "0"`. Keep `CARGO_TARGET_DIR` outside the checkout, which `actions/checkout` cleans on every run, so later builds reuse the cache.
- **Vitest:** run one worker, for example `npm test -- --no-file-parallelism`.
- **Node's test runner:** it may start parallel test processes; measure a cold run before deciding whether to cap them.
- **Playwright:** run one worker, for example `npm test -- --workers=1`, and run a cold suite before relying on 1 GB of RAM. Swap adds capacity but can make browser tests much slower.

Install the system packages your builds need on the host through `runberth track-packages`, or with `scripts/install-vps.sh --packages`, so cleanup can remove them. For Playwright, preinstall the browsers' system dependencies there, then replace `npx playwright install --with-deps ...` in the workflow with `npx playwright install ...`, so job code never needs `sudo` on the persistent runner.

## Jobs that need care

- **Write-capable tokens:** a job that publishes releases or tags, or deploys, holds credentials that a persistent runner's later jobs could reach through files left behind. Keep such jobs off the shared host unless every repository on it is equally trusted.
- **Deployments to production:** keep production deployment scripts out of CI on a shared runner.
- **Android:** a Gradle heap larger than the host's RAM, such as the common `-Xmx2g`, fails on a 1 GB host; test a low-memory configuration separately.
- **iOS:** app builds need Xcode on macOS and cannot run on a Linux host.

## Validate each moved workflow

Check that its PR check still appears and reports failure correctly, then check its normal push or tag path. Start jobs in two repositories at once to confirm that the second waits in "Set up runner" until the first completes. Watch peak memory, swap use, disk, and duration on a cold run with `scripts/vps.sh status --watch 5`. Every runner on the host must use Runberth's job hooks; a runner without them bypasses serialization.
