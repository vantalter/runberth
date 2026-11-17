fn main() {
    println!("cargo:rerun-if-changed=native");
    let target = std::env::var("CARGO_CFG_TARGET_OS").unwrap();
    let mut build = cc::Build::new();
    build.include("native").warnings(true);
    let mut libraries = Vec::new();
    match target.as_str() {
        "macos" => {
            // The menu bar icon's layers, from scripts/generate-tray-icons.swift.
            let mut header = String::new();
            for (name, file) in [
                ("tray_glyph", "assets/tray/glyph.png"),
                ("tray_glyph_cut", "assets/tray/glyph-cut.png"),
                ("tray_success", "assets/tray/badge-success.png"),
                ("tray_failed", "assets/tray/badge-failed.png"),
                ("tray_offline", "assets/tray/badge-offline.png"),
                ("tray_running", "assets/tray/badge-running.png"),
            ] {
                println!("cargo:rerun-if-changed={file}");
                let bytes = std::fs::read(file).unwrap_or_else(|e| panic!("Missing {file}: {e}"));
                let bytes: Vec<String> = bytes.iter().map(u8::to_string).collect();
                header += &format!("static const unsigned char {name}[] = {{{}}};\n", bytes.join(","));
            }
            let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
            std::fs::write(out.join("tray_icons.h"), header).expect("Cannot embed the tray icons");
            build.include(out);
            build
                .flag("-mmacosx-version-min=12.0")
                .file("native/macos.m")
                .flag("-fobjc-arc");
            for framework in ["AppKit", "ServiceManagement", "UserNotifications"] {
                println!("cargo:rustc-link-lib=framework={framework}");
            }
        }
        "linux" => {
            for package in ["gio-unix-2.0", "json-glib-1.0"] {
                let lib = pkg_config::Config::new()
                    .cargo_metadata(false)
                    .probe(package)
                    .unwrap_or_else(|e| panic!("Missing {package} development files: {e}"));
                for path in &lib.include_paths {
                    build.include(path);
                }
                libraries.push(lib);
            }
            build.file("native/linux.c").flag("-std=c11");
        }
        _ => panic!("runberth-app supports macOS and Linux"),
    }
    build.compile("runberth_native");
    // GNU ld resolves left to right: system libraries must follow the static
    // native library that references them.
    for lib in libraries {
        for path in lib.link_paths {
            println!("cargo:rustc-link-search=native={}", path.display());
        }
        for name in lib.libs {
            println!("cargo:rustc-link-lib={name}");
        }
    }
}
