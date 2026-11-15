//! Fixed host paths. Cleanup removes exactly these, so they never vary.

pub const REGISTRY: &str = "/etc/runberth";
pub const SHARED_MARKER: &str = "/etc/runberth/shared-owned";
pub const OPT: &str = "/opt/runberth";
pub const BIN: &str = "/opt/runberth/bin/runberth";
pub const HOST_CONFIG: &str = "/opt/runberth/config/host.json";
pub const HOOK_STARTED: &str = "/opt/runberth/hooks/job-started.sh";
pub const HOOK_COMPLETED: &str = "/opt/runberth/hooks/job-completed.sh";
pub const STATE_ROOT: &str = "/var/lib/runberth";
pub const LOCK_DIR: &str = "/var/lock/runberth";
pub const LOCK_FILE: &str = "/var/lock/runberth/host.lock";
pub const UNIT_DIR: &str = "/etc/systemd/system";
pub const SWAP_FILE: &str = "/swapfile.runberth";
pub const FSTAB_LINE: &str = "/swapfile.runberth none swap sw 0 0";
pub const RUNNER_GROUP: &str = "runberth-runners";
pub const SAFE_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";
