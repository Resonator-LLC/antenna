// Copyright (c) 2026-2027 Resonator LLC. Licensed under MIT.

use std::path::{Path, PathBuf};

fn find_quickjs() -> PathBuf {
    if let Ok(val) = std::env::var("QUICKJS_DIR") {
        return PathBuf::from(val);
    }

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

fn host_triple() -> (String, String) {
    let uname_s = std::process::Command::new("uname")
        .arg("-s")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    let uname_m = std::process::Command::new("uname")
        .arg("-m")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    let uname_r = std::process::Command::new("uname")
        .arg("-r")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();

    if uname_s == "Darwin" {
        // contrib's host-triple dir uses the Apple arch name (arm64); pjsip's
        // libraries inside that dir use the GNU arch name (aarch64).
        let host = format!("{uname_m}-apple-darwin{uname_r}");
        let pj_arch = uname_m.replace("arm64", "aarch64");
        let pj = format!("{pj_arch}-apple-darwin{uname_r}");
        (host, pj)
    } else {
        let host = format!("{uname_m}-linux-gnu");
        (host.clone(), host)
    }
}

fn main() {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let manifest_dir = Path::new(&manifest);

    let quickjs_prefix = find_quickjs();
    let quickjs_inc = quickjs_prefix.join("include").join("quickjs");
    let quickjs_lib = quickjs_prefix.join("lib").join("quickjs");

    println!("cargo:rustc-link-search=native={}", quickjs_lib.display());
    println!("cargo:rustc-link-lib=static=quickjs");

    let src_dir = manifest_dir.join("src");
    cc::Build::new()
        .file(src_dir.join("quickjs_shim.c"))
        .include(&quickjs_inc)
        .warnings(false)
        .compile("quickjs_shim");

    // ------------------------------------------------------------------
    // Carrier (Jami-backed). The compiled libcarrier.a + libjami + contrib
    // libs are produced by `make` in CARRIER_DIR. We do not rebuild them
    // here — antenna links against the artifacts in place.
    // ------------------------------------------------------------------
    let carrier_dir = if let Ok(val) = std::env::var("CARRIER_DIR") {
        PathBuf::from(val)
    } else {
        manifest_dir.join("third_party").join("carrier")
    };
    let carrier_lib = carrier_dir.join("build").join("libcarrier.a");
    let carrier_inc = carrier_dir.join("include");

    if !carrier_lib.exists() {
        panic!(
            "libcarrier.a not found at {}.\n\
             Build it first:\n  cd {} && make libjami-build && make\n\
             Or point CARRIER_DIR at a tree with build/libcarrier.a.",
            carrier_lib.display(),
            carrier_dir.display(),
        );
    }
    if !carrier_inc.join("carrier.h").exists() {
        panic!(
            "carrier.h not found at {}.\n\
             Set CARRIER_DIR to the canonical carrier checkout.",
            carrier_inc.display(),
        );
    }

    println!(
        "cargo:rustc-link-search=native={}",
        carrier_lib.parent().unwrap().display()
    );
    println!("cargo:rustc-link-lib=static=carrier");

    // ------------------------------------------------------------------
    // libjami + contrib (mirror of carrier/Makefile §Link flags)
    //
    // Resolved to a pre-built static prefix at $JAMI_PREFIX, defaulting to
    // ${XDG_CACHE_HOME:-$HOME/.cache}/resonator/libjami/<sha>/ where <sha>
    // is the line from carrier/JAMI_VERSION. All archives (libjami-core.a +
    // ~39 contrib libs) live flat under $JAMI_PREFIX/lib/. See
    // arch/jami-migration.md D21.
    // ------------------------------------------------------------------
    let jami_prefix = if let Ok(val) = std::env::var("JAMI_PREFIX") {
        PathBuf::from(val)
    } else {
        let pin_file = carrier_dir.join("JAMI_VERSION");
        let sha = std::fs::read_to_string(&pin_file)
            .unwrap_or_else(|e| panic!("could not read {}: {}", pin_file.display(), e))
            .trim()
            .to_string();
        if sha.is_empty() {
            panic!("{} is empty; expected a libjami SHA", pin_file.display());
        }
        let cache_root = std::env::var("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                PathBuf::from(std::env::var("HOME").expect("HOME unset")).join(".cache")
            });
        cache_root.join("resonator").join("libjami").join(sha)
    };
    let jami_lib_dir = jami_prefix.join("lib");
    let jami_lib = jami_lib_dir.join("libjami-core.a");
    if !jami_lib.exists() {
        panic!(
            "libjami-core.a not found at {}.\n\
             Build it first:\n  cd {} && make libjami-build\n\
             Or set JAMI_PREFIX to point at an existing libjami install.",
            jami_lib.display(),
            carrier_dir.display(),
        );
    }
    println!("cargo:rustc-link-search=native={}", jami_lib_dir.display());
    println!("cargo:rustc-link-lib=static=jami-core");

    let (_host, pj) = host_triple();

    let contrib_static = [
        "dhtnet",
        "opendht",
        &format!("pjsua2-{pj}"),
        &format!("pjsua-{pj}"),
        &format!("pjsip-ua-{pj}"),
        &format!("pjsip-simple-{pj}"),
        &format!("pjsip-{pj}"),
        &format!("pjmedia-codec-{pj}"),
        &format!("pjmedia-audiodev-{pj}"),
        &format!("pjmedia-videodev-{pj}"),
        &format!("pjmedia-{pj}"),
        &format!("pjnath-{pj}"),
        &format!("pjlib-util-{pj}"),
        &format!("pj-{pj}"),
        &format!("srtp-{pj}"),
        &format!("yuv-{pj}"),
        "avformat",
        "avcodec",
        "avfilter",
        "avdevice",
        "swresample",
        "swscale",
        "avutil",
        "x264",
        "fmt",
        "http_parser",
        "llhttp",
        "natpmp",
        "simdutf",
        "ixml",
        "upnp",
        "speex",
        "speexdsp",
        "minizip",
        "zstd",
        "bzip2",
        "secp256k1",
        "yaml-cpp",
        "git2",
        "jsoncpp",
        "opus",
        "vpx",
        "argon2",
        "gnutls",
        "hogweed",
        "nettle",
        "gmp",
        "ssl",
        "crypto",
        "tls",
        "z",
    ];
    for lib in contrib_static {
        println!("cargo:rustc-link-lib=static={lib}");
    }

    // ------------------------------------------------------------------
    // All third-party C/C++ deps come from contrib (hermetic, D21). Only
    // system frameworks + the C runtime are pulled from outside the prefix.
    // ------------------------------------------------------------------
    if cfg!(target_os = "macos") {
        for fw in &[
            "AVFoundation",
            "CoreAudio",
            "CoreVideo",
            "CoreMedia",
            "CoreGraphics",
            "VideoToolbox",
            "AudioUnit",
            "Foundation",
            "CoreFoundation",
            "Security",
            "SystemConfiguration",
        ] {
            println!("cargo:rustc-link-lib=framework={fw}");
        }
        for sys in &["compression", "resolv", "c++", "iconv"] {
            println!("cargo:rustc-link-lib={sys}");
        }
    } else {
        for sys in &["stdc++", "dl", "rt", "resolv"] {
            println!("cargo:rustc-link-lib={sys}");
        }
    }

    println!("cargo:rustc-link-lib=pthread");

    println!("cargo:rerun-if-changed={}", carrier_lib.display());
    println!("cargo:rerun-if-changed={}", carrier_inc.display());
    println!(
        "cargo:rerun-if-changed={}",
        carrier_dir.join("JAMI_VERSION").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        src_dir.join("quickjs_shim.c").display()
    );
    println!("cargo:rerun-if-env-changed=CARRIER_DIR");
    println!("cargo:rerun-if-env-changed=JAMI_PREFIX");
}
