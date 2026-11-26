//! Runberth's desktop app and its `runberth` command. The command line
//! (cli, install, and the modules they use) builds on its own; the `tray`
//! feature, on by default, adds the menu bar and tray app with its native code.
// The CLI-only build leaves the tray's uses of shared code unused.
#![cfg_attr(not(feature = "tray"), allow(dead_code))]

mod cli;
mod config;
mod github;
mod init;
mod install;
mod packages;
mod report;
mod server;
mod settings;
mod token;

#[cfg(feature = "tray")]
mod login;
#[cfg(feature = "tray")]
mod menu;
#[cfg(feature = "tray")]
mod native;
#[cfg(feature = "tray")]
mod runs;
#[cfg(feature = "tray")]
mod tray;
#[cfg(feature = "tray")]
mod updates;

#[cfg(feature = "tray")]
pub use tray::runberth_app_main;

/// The CLI-only build's entry point: a command, or the usage.
pub fn cli_main() -> i32 {
    let args: Vec<String> = std::env::args().skip(1).collect();
    cli::run(&args).unwrap_or_else(|| cli::usage(2))
}
