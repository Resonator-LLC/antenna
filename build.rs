// Copyright (c) 2026-2027 Resonator LLC. Licensed under MIT.

use std::collections::HashSet;
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
    // iOS and Android both stage contrib archives flat (build-libjami.sh strips
    // the per-arch triple suffix at staging time), so pjsip libs are named
    // libpj.a / libpjsip.a / … — return empty strings to drive unsuffixed names.
    if target_os() == "ios" || target_os() == "android" {
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

/// Lipo-thin every `.a` in `src_dir` down to a single architecture into
/// `out_dir`. Used to feed Rust's static-archive bundler one-arch
/// archives — it can't read Mach-O fat magic.
///
/// Returns the set of library stems (without `lib` prefix or `.a`
/// suffix) that landed in `out_dir`. Archives whose only slice is a
/// different architecture (vpx on iOS-sim ships x86_64-only because
/// upstream disables arm64-sim) are skipped — callers should drop the
/// corresponding `-l<name>` from the link list, since the linker will
/// otherwise emit "library 'X' not found".
///
/// Idempotent via mtime: archives are only re-thinned when the source
/// is newer than the destination.
fn thin_archives_for_arch(src_dir: &Path, arch: &str, out_dir: &Path) -> HashSet<String> {
    std::fs::create_dir_all(out_dir)
        .unwrap_or_else(|e| panic!("could not create {}: {}", out_dir.display(), e));
    let mut present = HashSet::new();
    let entries = std::fs::read_dir(src_dir)
        .unwrap_or_else(|e| panic!("could not read {}: {}", src_dir.display(), e));
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("a") {
            continue;
        }
        let name = path.file_name().unwrap();
        let dst = out_dir.join(name);
        // Derive the link-list stem ("libfoo.a" -> "foo") for the
        // returned set. Done before the mtime fast-path so subsequent
        // builds report the same `present` set.
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .and_then(|s| s.strip_prefix("lib"))
            .map(|s| s.to_string());

        // Skip the work if dst exists and is at least as new as the
        // source, but still record the lib stem.
        if let (Ok(src_meta), Ok(dst_meta)) = (path.metadata(), dst.metadata()) {
            if let (Ok(src_mt), Ok(dst_mt)) = (src_meta.modified(), dst_meta.modified()) {
                if dst_mt >= src_mt {
                    if let Some(s) = stem {
                        present.insert(s);
                    }
                    continue;
                }
            }
        }

        // `lipo -info` distinguishes fat vs thin and lists archs.
        let info = std::process::Command::new("lipo")
            .arg("-info")
            .arg(&path)
            .output()
            .unwrap_or_else(|e| panic!("lipo -info {} failed: {}", path.display(), e));
        let info_str = String::from_utf8_lossy(&info.stdout);
        if info_str.contains("Non-fat") {
            // Already thin — copy if the arch matches, skip otherwise.
            // Some Jami contribs are built for a single sim slice only:
            // e.g. libvpx ships as x86_64-only on the iOS simulator
            // because libvpx upstream disables arm64-sim. The contrib_static
            // link list still names libvpx; if libjami-core's arm64-sim
            // build references vpx symbols the linker will surface them as
            // undefined, which is more informative than a silent
            // arch-mismatch here.
            if !info_str.contains(arch) {
                println!(
                    "cargo:warning=skipping {} (thin {}; arch={} requested)",
                    path.display(),
                    info_str.trim(),
                    arch
                );
                continue;
            }
            std::fs::copy(&path, &dst).unwrap_or_else(|e| {
                panic!("copy {} -> {}: {}", path.display(), dst.display(), e)
            });
            if let Some(s) = stem.clone() {
                present.insert(s);
            }
        } else {
            // Fat — extract the requested arch.
            let status = std::process::Command::new("lipo")
                .arg("-thin")
                .arg(arch)
                .arg(&path)
                .arg("-output")
                .arg(&dst)
                .status()
                .unwrap_or_else(|e| {
                    panic!("lipo -thin {} {}: {}", arch, path.display(), e)
                });
            if !status.success() {
                panic!(
                    "lipo -thin {} {} -> {} failed",
                    arch,
                    path.display(),
                    dst.display()
                );
            }
            if let Some(s) = stem {
                present.insert(s);
            }
        }
    }
    present
}

/// Collect link-list stems ("libfoo.a" -> "foo") for every static archive in
/// `dir`. Used to drop `-l` directives for contrib archives a given platform
/// slice didn't produce (Android's contrib set isn't identical to the host's),
/// avoiding a hard "library 'X' not found" at link time.
fn present_lib_stems(dir: &Path) -> HashSet<String> {
    let mut present = HashSet::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("a") {
                continue;
            }
            if let Some(stem) = path
                .file_stem()
                .and_then(|s| s.to_str())
                .and_then(|s| s.strip_prefix("lib"))
            {
                present.insert(stem.to_string());
            }
        }
    }
    present
}

fn main() {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let manifest_dir = Path::new(&manifest);
    let out_dir =
        PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR set by cargo for build scripts"));

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
    // Carrier publishes per-platform build dirs (mirrors PLATFORM in
    // carrier/Makefile). For iOS simulator, we deliberately pick the
    // per-arch thin archives (not the lipo'd fat at build-ios-sim-fat/)
    // because Rust's static-library bundler reads the .a file format
    // (`object` crate) and doesn't recognize Mach-O fat archive magic
    // (0xcafebabe) — it errors with "Unsupported archive identifier".
    // The Makefile's libcarrier-ios target produces both:
    //   build-ios-sim-arm64/libcarrier.a   (thin arm64)
    //   build-ios-sim-x86_64/libcarrier.a  (thin x86_64)
    //   build-ios-sim-fat/libcarrier.a     (fat, lipo'd from the two)
    // We use the per-arch slice here so Cargo's default `+bundle`
    // mechanism (which we rely on for the staticlib output Cargokit
    // hands to Xcode) can read and embed it.
    let carrier_build_dir: String = if is_ios_device() {
        "build-ios-device-arm64".to_string()
    } else if is_ios_simulator() {
        if target_arch() == "x86_64" {
            "build-ios-sim-x86_64".to_string()
        } else {
            "build-ios-sim-arm64".to_string()
        }
    } else if target_os() == "android" {
        "build-android-arm64".to_string()
    } else {
        "build".to_string()
    };
    let carrier_lib = carrier_dir.join(&carrier_build_dir).join("libcarrier.a");
    let carrier_inc = carrier_dir.join("include");

    if !carrier_lib.exists() {
        let hint = if target_os() == "ios" {
            format!(
                "  cd {} && make libcarrier-ios PLATFORM={}",
                carrier_dir.display(),
                if is_ios_device() {
                    "ios-device"
                } else {
                    "ios-simulator"
                },
            )
        } else if target_os() == "android" {
            format!(
                "  cd {} && make libjami PLATFORM=android-arm64 && make libcarrier-android",
                carrier_dir.display(),
            )
        } else {
            format!(
                "  cd {} && make libjami-build && make",
                carrier_dir.display(),
            )
        };
        panic!(
            "libcarrier.a not found at {}.\n\
             Build it first:\n{}\n\
             Or point CARRIER_DIR at a tree with {}/libcarrier.a.",
            carrier_lib.display(),
            hint,
            carrier_build_dir,
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
        } else if target_os() == "android" {
            "-android-arm64"
        } else {
            ""
        };
        cache_root
            .join("resonator")
            .join("libjami")
            .join(format!("{sha}{suffix}"))
    };
    let jami_src_lib_dir = jami_prefix.join("lib");
    // For iOS simulator, the published prefix layout is a single fat
    // archive directory (<sha>-ios-sim-fat/lib/) where every .a is a
    // Mach-O universal binary spanning arm64 + x86_64. Rust's static-
    // library bundler can't parse fat magic, so for each cargo invocation
    // we thin every .a down to the current target arch into a per-arch
    // OUT_DIR cache. The first build pays a one-time lipo cost (~50
    // archives, sub-second total); subsequent builds skip via mtime
    // check.
    let (jami_lib_dir, present_libs) = if is_ios_simulator() {
        // Rust's target_arch returns "aarch64"; lipo's arch identifier is
        // "arm64" (Mach-O naming). x86_64 is the same in both.
        let lipo_arch = match target_arch().as_str() {
            "aarch64" => "arm64",
            other => other,
        }
        .to_string();
        let thin_dir = out_dir.join("jami-thin");
        let present = thin_archives_for_arch(&jami_src_lib_dir, &lipo_arch, &thin_dir);
        (thin_dir, Some(present))
    } else if target_os() == "android" {
        // Android's contrib set isn't identical to the host's (a few packages
        // are gated on HAVE_MACOSX / HAVE_IOS upstream). Rather than hardcode
        // the delta, scan the prefix and drop link directives for archives that
        // weren't produced — same defensive approach as the iOS-sim thinned set.
        (
            jami_src_lib_dir.clone(),
            Some(present_lib_stems(&jami_src_lib_dir)),
        )
    } else {
        (jami_src_lib_dir.clone(), None)
    };
    let jami_lib = jami_lib_dir.join("libjami-core.a");
    if !jami_lib.exists() {
        let hint = if target_os() == "ios" {
            format!(
                "  cd {} && make libjami PLATFORM={}",
                carrier_dir.display(),
                if is_ios_device() {
                    "ios-device"
                } else {
                    "ios-simulator"
                },
            )
        } else if target_os() == "android" {
            format!(
                "  cd {} && make libjami PLATFORM=android-arm64",
                carrier_dir.display()
            )
        } else {
            format!("  cd {} && make libjami-build", carrier_dir.display())
        };
        panic!(
            "libjami-core.a not found at {}.\n\
             Build it first:\n{}\n\
             Or set JAMI_PREFIX to point at an existing libjami install.",
            jami_lib.display(),
            hint,
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

    // contrib_static lists the per-platform contrib archives we link against.
    // The bulk overlaps host and iOS; a few archives (minizip, zstd, bzip2)
    // are macOS-only in Jami's contrib (gated on HAVE_MACOSX), because iOS
    // uses Compression.framework instead. Skip those on iOS so the link
    // doesn't fail with "library 'minizip' not found".
    let mut contrib_static: Vec<String> = vec![
        "dhtnet".into(),
        "opendht".into(),
        format!("pjsua2{pj_suffix}"),
        format!("pjsua{pj_suffix}"),
        format!("pjsip-ua{pj_suffix}"),
        format!("pjsip-simple{pj_suffix}"),
        format!("pjsip{pj_suffix}"),
        format!("pjmedia-codec{pj_suffix}"),
        format!("pjmedia-audiodev{pj_suffix}"),
        format!("pjmedia-videodev{pj_suffix}"),
        format!("pjmedia{pj_suffix}"),
        format!("pjnath{pj_suffix}"),
        format!("pjlib-util{pj_suffix}"),
        format!("pj{pj_suffix}"),
        format!("srtp{pj_suffix}"),
        format!("yuv{pj_suffix}"),
        "avformat".into(),
        "avcodec".into(),
        "avfilter".into(),
        "avdevice".into(),
        "swresample".into(),
        "swscale".into(),
        "avutil".into(),
        "x264".into(),
        "fmt".into(),
        "http_parser".into(),
        "llhttp".into(),
        "natpmp".into(),
        "simdutf".into(),
        "ixml".into(),
        "upnp".into(),
        "speex".into(),
        "speexdsp".into(),
        "secp256k1".into(),
        "yaml-cpp".into(),
        "git2".into(),
        "jsoncpp".into(),
        "opus".into(),
        "vpx".into(),
        "argon2".into(),
        "gnutls".into(),
        "hogweed".into(),
        "nettle".into(),
        "gmp".into(),
        "ssl".into(),
        "crypto".into(),
        "tls".into(),
        "z".into(),
    ];
    if target_os() != "ios" {
        // macOS / Linux contribs include these; the iOS slice doesn't ship
        // them (Compression.framework + system zstd-via-libcompression).
        // Note: iOS's libavformat (matroska decoder) still references
        // BZ2_* symbols, so we link system /usr/lib/libbz2 below.
        contrib_static.extend(["minizip".into(), "zstd".into(), "bzip2".into()]);
    }
    if target_os() == "android" {
        // Android-only contrib archives. macOS/iOS don't ship these, and the
        // present-filter above would drop them elsewhere anyway.
        // - webrtc_audio_processing: Android's libjami builds + uses the WebRTC
        //   AEC / noise-suppression module; libjami-core references
        //   webrtc::AudioProcessing::Create (macOS/iOS use a different canceller).
        // - iconv / charset: GNU libiconv from the contrib. macOS/iOS link the
        //   SDK's system -liconv (see sys_libs below); Android has no system
        //   iconv, so ffmpeg/etc.'s libiconv_* symbols come from the contrib .a.
        contrib_static.push("webrtc_audio_processing".into());
        contrib_static.push("iconv".into());
        contrib_static.push("charset".into());
    }
    for lib in &contrib_static {
        // On iOS sim, drop link directives for archives that didn't land
        // in the thinned dir (arch-mismatched contrib slices like
        // libvpx-x86_64-only on arm64-sim). The linker would otherwise
        // emit "library 'X' not found".
        if let Some(present) = &present_libs {
            if !present.contains(lib.as_str()) {
                println!(
                    "cargo:warning=dropping -l{lib} (no archive in the libjami prefix for this target)"
                );
                continue;
            }
        }
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
        // Most Mach-O system frameworks overlap on macOS and iOS. The
        // daemon has no UI surface so we never reach into AppKit / UIKit;
        // the audio + video + Foundation + Security set is iOS-compatible.
        // libcompression / libresolv / libc++ / libiconv all ship in the
        // iOS SDK at the same paths as the macOS SDK.
        let mut frameworks = vec![
            "AVFoundation",
            "CoreAudio",
            "CoreVideo",
            "CoreMedia",
            "CoreGraphics",
            "VideoToolbox",
            "Foundation",
            "CoreFoundation",
            "Security",
            "SystemConfiguration",
        ];
        if target_os_str == "macos" {
            // AudioUnit is a top-level framework on macOS only. On iOS its
            // symbols live inside AudioToolbox.
            frameworks.push("AudioUnit");
        } else {
            // iOS folds the AU APIs into AudioToolbox.
            frameworks.push("AudioToolbox");
        }
        for fw in &frameworks {
            println!("cargo:rustc-link-lib=framework={fw}");
        }
        let mut sys_libs: Vec<&str> = vec!["compression", "resolv", "c++", "iconv"];
        if target_os_str == "ios" {
            // iOS libjami's FFmpeg matroska decoder still calls BZ2_* but
            // the iOS contrib slice doesn't ship libbzip2 — fall back to
            // /usr/lib/libbz2 from the iOS SDK.
            sys_libs.push("bz2");
            // Similarly, libz is referenced by libavcodec / libavformat /
            // libgit2 / libgnutls but the iOS contrib doesn't publish
            // libz.a (Jami's zlib rule is gated on `ifndef HAVE_IOS`).
            // The iOS SDK ships libz.tbd so we link the system zlib.
            sys_libs.push("z");
        }
        for sys in &sys_libs {
            println!("cargo:rustc-link-lib={sys}");
        }
    } else if target_os_str == "android" {
        // Android (Bionic): the C++ runtime is libc++ — link c++_shared to match
        // the contrib's ANDROID_STL=c++_shared; libc++_shared.so is packaged
        // into jniLibs by the antenna plugin's gradle. The platform libs the
        // daemon's media backends reach into: aaudio (the primary audio layer —
        // why minSdk is 26), OpenSLES (fallback audio), mediandk (FFmpeg
        // MediaCodec hwaccel), log, android. m/dl round out the NDK runtime.
        // Deliberately omitted vs the Linux branch: `stdc++` (GNU libstdc++
        // doesn't exist on Android), `rt` and `resolv` (folded into Bionic
        // libc), and `-lpthread` (also in libc — see the guard below). libz
        // comes from the contrib (libz.a).
        for sys in &["c++_shared", "log", "m", "dl", "android", "aaudio", "OpenSLES", "mediandk"] {
            println!("cargo:rustc-link-lib={sys}");
        }
        // The android cdylib is dlopen'd at runtime, so an unresolved strong
        // symbol is a load-time crash, not a link error. Promote it to a link
        // error: --no-undefined fails the build if any non-weak symbol is
        // missing (weak undefs like getrandom on API<28 stay exempt). This is
        // what would have caught the gnutls/brotli, webrtc, iconv and aaudio
        // gaps without a device round-trip.
        println!("cargo:rustc-link-arg=-Wl,--no-undefined");
    } else {
        for sys in &["stdc++", "dl", "rt", "resolv"] {
            println!("cargo:rustc-link-lib={sys}");
        }
    }

    // pthread is a standalone library on macOS/iOS (libSystem stub) and Linux,
    // but on Android it's folded into Bionic libc — `-lpthread` there fails with
    // "library 'pthread' not found".
    if target_os_str != "android" {
        println!("cargo:rustc-link-lib=pthread");
    }

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
