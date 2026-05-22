// Copyright (c) 2026-2027 Resonator LLC. Licensed under MIT.

use std::path::{Path, PathBuf};

fn target_os() -> String {
    std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default()
}

fn target_arch() -> String {
    std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default()
}

fn target_abi() -> String {
    std::env::var("CARGO_CFG_TARGET_ABI").unwrap_or_default()
}

/// True for iOS simulator targets.
///
/// Rust ships two simulator target tuples and they disagree on how to
/// advertise sim-ness through cfg:
///
/// * `aarch64-apple-ios-sim` reports `target_abi = "sim"`.
/// * `x86_64-apple-ios` reports `target_abi = ""` — the tuple only exists
///   for the Intel-Mac simulator; there is no on-device x86_64-apple-ios.
///
/// Detect both by ORing target_abi == "sim" with target_arch == "x86_64".
fn is_ios_simulator() -> bool {
    target_os() == "ios" && (target_abi() == "sim" || target_arch() == "x86_64")
}

fn is_ios_device() -> bool {
    target_os() == "ios" && !is_ios_simulator()
}

/// Returns `(host_triple_dir, pj_lib_suffix)` for the libjami contrib layout.
///
/// On host targets, contribs are staged under `contrib/<host>/lib/` and
/// pjsip libraries carry a `-<gnu-arch>-apple-darwin` (macOS) /
/// `-<gnu-arch>-linux-gnu` (Linux) suffix. On iOS, build-libjami.sh's
/// staging step strips the triple suffix entirely, so the iOS prefix lists
/// libraries flat (libpj.a, libpjsip.a, …); return empty strings to drive
/// the contrib name list below to emit unsuffixed lib names.
fn host_triple() -> (String, String) {
    if target_os() == "ios" {
        return (String::new(), String::new());
    }

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

    let src_dir = manifest_dir.join("src");

    // ------------------------------------------------------------------
    // QuickJS (vendored at third_party/quickjs, pinned via submodule).
    // Mirror of upstream Makefile's QJS_LIB_OBJS minus quickjs-libc.o —
    // antenna's script_vm.rs only touches the core engine.
    // ------------------------------------------------------------------
    let qjs_dir = manifest_dir.join("third_party").join("quickjs");
    if !qjs_dir.join("quickjs.h").exists() {
        panic!(
            "QuickJS sources missing at {}.\n\
             Run: git submodule update --init third_party/quickjs",
            qjs_dir.display(),
        );
    }
    let qjs_version = std::fs::read_to_string(qjs_dir.join("VERSION"))
        .unwrap_or_else(|e| panic!("could not read {}/VERSION: {}", qjs_dir.display(), e))
        .trim()
        .to_string();

    let mut qjs_build = cc::Build::new();
    qjs_build
        .files([
            qjs_dir.join("quickjs.c"),
            qjs_dir.join("dtoa.c"),
            qjs_dir.join("libregexp.c"),
            qjs_dir.join("libunicode.c"),
            qjs_dir.join("cutils.c"),
        ])
        .include(&qjs_dir)
        .define("_GNU_SOURCE", None)
        .define("CONFIG_VERSION", format!("\"{qjs_version}\"").as_str())
        .flag_if_supported("-fwrapv")
        .flag_if_supported("-Wno-sign-compare")
        .flag_if_supported("-Wno-implicit-fallthrough")
        .flag_if_supported("-Wno-unused-parameter")
        .flag_if_supported("-Wno-unused-but-set-variable")
        .flag_if_supported("-Wno-array-bounds")
        .flag_if_supported("-Wno-format-truncation")
        .warnings(false);
    qjs_build.compile("quickjs");

    cc::Build::new()
        .file(src_dir.join("quickjs_shim.c"))
        .include(&qjs_dir)
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
    // ${XDG_CACHE_HOME:-$HOME/.cache}/resonator/libjami/<key>/ where <key>
    // selects host vs iOS slice, matching carrier/tools/build-libjami.sh:
    //
    //   host targets:             <sha>/
    //   aarch64-apple-ios:        <sha>-ios-device-arm64/
    //   aarch64-apple-ios-sim:    <sha>-ios-sim-fat/
    //   x86_64-apple-ios:         <sha>-ios-sim-fat/   (Intel-Mac simulator)
    //
    // <sha> is the line from carrier/JAMI_VERSION. All archives
    // (libjami-core.a + ~39 contrib libs) live flat under
    // $JAMI_PREFIX/lib/. See arch/jami-migration.md D21.
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
        let suffix = if is_ios_device() {
            "-ios-device-arm64"
        } else if is_ios_simulator() {
            "-ios-sim-fat"
        } else {
            ""
        };
        cache_root
            .join("resonator")
            .join("libjami")
            .join(format!("{sha}{suffix}"))
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
    // pj is empty on iOS — build-libjami.sh's iOS staging step strips the
    // pjsip per-arch triple suffix so iOS archives are flat-named (libpj.a,
    // libpjsip.a, …). Render the suffix conditionally so iOS gets the bare
    // name and host targets keep their `-<arch>-apple-darwin` discriminator.
    let pj_suffix = if pj.is_empty() {
        String::new()
    } else {
        format!("-{pj}")
    };

    let contrib_static = [
        "dhtnet",
        "opendht",
        &format!("pjsua2{pj_suffix}"),
        &format!("pjsua{pj_suffix}"),
        &format!("pjsip-ua{pj_suffix}"),
        &format!("pjsip-simple{pj_suffix}"),
        &format!("pjsip{pj_suffix}"),
        &format!("pjmedia-codec{pj_suffix}"),
        &format!("pjmedia-audiodev{pj_suffix}"),
        &format!("pjmedia-videodev{pj_suffix}"),
        &format!("pjmedia{pj_suffix}"),
        &format!("pjnath{pj_suffix}"),
        &format!("pjlib-util{pj_suffix}"),
        &format!("pj{pj_suffix}"),
        &format!("srtp{pj_suffix}"),
        &format!("yuv{pj_suffix}"),
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
    //
    // Use CARGO_CFG_TARGET_OS rather than `cfg!(target_os = …)` here:
    // `cfg!()` in build.rs evaluates against the HOST, but the link flags
    // we emit are read by the TARGET linker. The two diverge for iOS
    // cross-compilation from a macOS host.
    // ------------------------------------------------------------------
    let target_os_str = target_os();
    if target_os_str == "macos" || target_os_str == "ios" {
        // Same Mach-O system frameworks apply on macOS and iOS. The daemon
        // has no UI surface so we never reach into AppKit / UIKit; the
        // existing list (audio + video + Foundation + Security) is iOS-
        // compatible verbatim. libcompression / libresolv / libc++ /
        // libiconv all ship in the iOS SDK at the same paths.
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
    println!("cargo:rerun-if-changed={}", qjs_dir.join("VERSION").display());
    println!("cargo:rerun-if-changed={}", qjs_dir.join("quickjs.h").display());
    println!("cargo:rerun-if-env-changed=CARRIER_DIR");
    println!("cargo:rerun-if-env-changed=JAMI_PREFIX");
}
