//! The removal plan reads only Runberth's recorded resources.

use std::fs::{self, DirBuilder};
use std::os::unix::fs::DirBuilderExt;
use std::path::Path;
use std::process::{Command, Output};

use runberth::registry::Registry;

const RUNBERTH: &str = env!("CARGO_BIN_EXE_runberth");

fn cleanup(registry: &Path, args: &[&str]) -> Output {
    Command::new(RUNBERTH)
        .arg("cleanup")
        .args(["--registry-root", &registry.display().to_string()])
        .args(args)
        .output()
        .unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
fn dry_run_lists_only_recorded_resources() {
    let temp = tempfile::tempdir().unwrap();
    let registry = Registry::new(temp.path().join("registry"));
    registry.register_runner("vantalter/example").unwrap();
    registry.register_swap(3).unwrap();
    registry.add_packages(&["git".into(), "curl".into()]).unwrap();

    let result = cleanup(&registry.root, &[]);
    let stdout = text(&result.stdout);
    assert!(result.status.success(), "{}", text(&result.stderr));
    assert!(stdout.contains("runner vantalter/example"), "{stdout}");
    assert!(stdout.contains("will look up runberth-example"), "{stdout}");
    assert!(stdout.contains("/swapfile.runberth"));
    assert!(stdout.contains("curl, git"));
    assert!(stdout.contains("Dry run only"));
    assert!(registry.runners_dir().join("vantalter-example.json").exists());
}

#[test]
fn symlinked_record_is_rejected() {
    let temp = tempfile::tempdir().unwrap();
    let registry = Registry::new(temp.path().join("registry"));
    registry.secure_root().unwrap();
    // Private like a real registry, so only the symlink can fail the check,
    // including when the tests run as root.
    DirBuilder::new()
        .mode(0o700)
        .create(registry.runners_dir())
        .unwrap();
    let outside = temp.path().join("outside.json");
    fs::write(&outside, "{}").unwrap();
    std::os::unix::fs::symlink(&outside, registry.runners_dir().join("vantalter-example.json")).unwrap();

    let result = cleanup(&registry.root, &[]);
    assert!(!result.status.success());
    assert!(
        text(&result.stderr).contains("Refusing symlink"),
        "{}",
        text(&result.stderr)
    );
}

#[test]
fn missing_registry_is_an_empty_plan() {
    let temp = tempfile::tempdir().unwrap();
    let result = cleanup(&temp.path().join("absent"), &[]);
    assert!(result.status.success());
    assert!(text(&result.stdout).contains("swap: none recorded"));
}

#[test]
fn execution_is_refused_outside_the_host_registry() {
    let temp = tempfile::tempdir().unwrap();
    let registry = Registry::new(temp.path().join("registry"));
    registry.register_runner("vantalter/example").unwrap();
    let result = cleanup(&registry.root, &["--execute", "--github-removed"]);
    assert!(!result.status.success());
    let stderr = text(&result.stderr);
    assert!(
        stderr.contains("requires root") || stderr.contains("default /etc/runberth"),
        "{stderr}"
    );
    assert!(registry.runners_dir().join("vantalter-example.json").exists());

    let both = cleanup(
        &registry.root,
        &["--execute", "--github-removed", "--github-token-stdin"],
    );
    assert!(text(&both.stderr).contains("Choose one GitHub removal mode"));
}
