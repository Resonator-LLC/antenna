use std::path::Path;

fn main() {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let root = Path::new(&manifest).parent().unwrap();

    let serd_src = root.join("serd").join("src");
    let serd_inc = root.join("serd").join("include");
    let carrier_src = root.join("carrier").join("src");
    let carrier_inc = root.join("carrier").join("include");
    let deps_inc = root.join("deps").join("include");
    let deps_lib = root.join("deps").join("lib");
    let quickjs_dir = root.join("quickjs");

    // --- Serd (Turtle parser, C11, no deps) ---
    let serd_sources: Vec<_> = [
        "base64.c",
        "byte_source.c",
        "env.c",
        "n3.c",
        "node.c",
        "read_utf8.c",
        "reader.c",
        "string.c",
        "system.c",
        "uri.c",
        "writer.c",
    ]
    .iter()
    .map(|f| serd_src.join(f))
    .collect();

    cc::Build::new()
        .files(&serd_sources)
        .include(&serd_inc)
        .include(&serd_src)
        .define("SERD_STATIC", None)
        .std("c11")
        .warnings(false)
        .compile("serd");

    // --- Carrier (Tox wrapper, C11) ---
    cc::Build::new()
        .files(&[
            carrier_src.join("carrier.c"),
            carrier_src.join("carrier_events.c"),
        ])
        .include(&carrier_inc)
        .include(&carrier_src)
        .include(&deps_inc)
        .include(&serd_inc)
        .define("SERD_STATIC", None)
        .std("c11")
        .warnings(false)
        .compile("carrier");

    // --- QuickJS (JS engine) ---
    let src_dir = Path::new(&manifest).join("src");
    cc::Build::new()
        .files(&[
            quickjs_dir.join("quickjs.c"),
            quickjs_dir.join("cutils.c"),
            quickjs_dir.join("dtoa.c"),
            quickjs_dir.join("libregexp.c"),
            quickjs_dir.join("libunicode.c"),
            quickjs_dir.join("quickjs-libc.c"),
            src_dir.join("quickjs_shim.c"),
        ])
        .include(&quickjs_dir)
        .define("_GNU_SOURCE", None)
        .define("CONFIG_VERSION", Some("\"2025-04-26\""))
        .define("CONFIG_BIGNUM", None)
        .warnings(false)
        .opt_level(2)
        .compile("quickjs");

    // --- Link toxcore and its transitive deps ---
    println!("cargo:rustc-link-search=native={}", deps_lib.display());
    println!("cargo:rustc-link-lib=static=toxcore");

    for lib in &["libsodium", "opus", "vpx"] {
        if let Ok(output) = std::process::Command::new("pkg-config")
            .args(["--libs-only-L", lib])
            .output()
        {
            let paths = String::from_utf8_lossy(&output.stdout);
            for token in paths.split_whitespace() {
                if let Some(path) = token.strip_prefix("-L") {
                    println!("cargo:rustc-link-search=native={}", path);
                }
            }
        }
    }
    println!("cargo:rustc-link-lib=sodium");
    println!("cargo:rustc-link-lib=opus");
    println!("cargo:rustc-link-lib=vpx");
    println!("cargo:rustc-link-lib=pthread");

    // Link math lib on non-macOS
    #[cfg(not(target_os = "macos"))]
    println!("cargo:rustc-link-lib=m");

    // Rebuild triggers
    println!("cargo:rerun-if-changed={}", carrier_src.display());
    println!("cargo:rerun-if-changed={}", carrier_inc.display());
    println!("cargo:rerun-if-changed={}", serd_src.display());
    println!("cargo:rerun-if-changed={}", quickjs_dir.display());
    println!("cargo:rerun-if-changed={}", src_dir.join("quickjs_shim.c").display());
}
