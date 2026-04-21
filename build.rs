// Copyright (c) 2026-2027 Resonator LLC. Licensed under MIT.

use std::path::{Path, PathBuf};

/// Locate QuickJS install directory.
///
/// QuickJS does not ship a .pc file, so we check (in order):
///   1. `QUICKJS_DIR` env var
///   2. Homebrew prefix on macOS
///   3. Common system paths
fn find_quickjs() -> PathBuf {
    if let Ok(val) = std::env::var("QUICKJS_DIR") {
        return PathBuf::from(val);
    }

    // Homebrew: quickjs installs under <prefix>/lib/quickjs and <prefix>/include/quickjs
    if let Ok(output) = std::process::Command::new("brew")
        .args(["--prefix", "quickjs"])
        .output()
    {
        if output.status.success() {
            let prefix = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let p = PathBuf::from(&prefix);
            if p.join("include/quickjs/quickjs.h").exists() {
                return p;
            }
        }
    }

    // Fallback: system paths
    for base in &["/usr/local", "/usr"] {
        let p = PathBuf::from(base);
        if p.join("include/quickjs/quickjs.h").exists() {
            return p;
        }
    }

    panic!(
        "Could not find QuickJS. Install it (e.g. `brew install quickjs`) \
         or set QUICKJS_DIR to its prefix."
    );
}

fn main() {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let manifest_dir = Path::new(&manifest);

    // --- Serd (system library via pkg-config) ---
    let serd = pkg_config::Config::new()
        .atleast_version("0.30")
        .statik(true)
        .probe("serd-0")
        .expect("Could not find serd. Install it (e.g. `brew install serd`) or set PKG_CONFIG_PATH.");

    // --- Toxcore (system library via pkg-config) ---
    let toxcore = pkg_config::Config::new()
        .statik(true)
        .probe("toxcore")
        .expect("Could not find toxcore. Install it (e.g. `brew install toxcore`) or set PKG_CONFIG_PATH.");

    // --- QuickJS (no pkg-config, manual lookup) ---
    let quickjs_prefix = find_quickjs();
    let quickjs_inc = quickjs_prefix.join("include").join("quickjs");
    let quickjs_lib = quickjs_prefix.join("lib").join("quickjs");

    println!("cargo:rustc-link-search=native={}", quickjs_lib.display());
    println!("cargo:rustc-link-lib=static=quickjs");

    // Compile the shim that wraps static-inline QuickJS functions for FFI
    let src_dir = manifest_dir.join("src");
    cc::Build::new()
        .file(src_dir.join("quickjs_shim.c"))
        .include(&quickjs_inc)
        .warnings(false)
        .compile("quickjs_shim");

    // --- Carrier (compiled from submodule) ---
    let carrier_dir = if let Ok(val) = std::env::var("CARRIER_DIR") {
        PathBuf::from(val)
    } else {
        manifest_dir.join("third_party").join("carrier")
    };
    let carrier_src = carrier_dir.join("src");
    let carrier_inc = carrier_dir.join("include");

    cc::Build::new()
        .files(&[
            carrier_src.join("carrier.c"),
            carrier_src.join("carrier_events.c"),
            carrier_src.join("carrier_log.c"),
        ])
        .include(&carrier_inc)
        .include(&carrier_src)
        // Carrier needs serd and toxcore headers
        .includes(serd.include_paths.iter())
        .includes(toxcore.include_paths.iter())
        .define("SERD_STATIC", None)
        .std("c11")
        .warnings(false)
        .compile("carrier");

    // pthread (always needed by toxcore)
    println!("cargo:rustc-link-lib=pthread");

    // Link math lib on non-macOS
    #[cfg(not(target_os = "macos"))]
    println!("cargo:rustc-link-lib=m");

    // Rebuild triggers
    println!("cargo:rerun-if-changed={}", carrier_src.display());
    println!("cargo:rerun-if-changed={}", carrier_inc.display());
    println!(
        "cargo:rerun-if-changed={}",
        src_dir.join("quickjs_shim.c").display()
    );
}
