fn main() {
    #[cfg(feature = "tray")]
    std::process::exit(runberth_app::runberth_app_main());
    #[cfg(not(feature = "tray"))]
    std::process::exit(runberth_app::cli_main());
}
