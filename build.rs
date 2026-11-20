//! Builds the vendored libghostty-vt (Ghostty's terminal emulation library, MIT) with Zig and links it statically,
//! for the TUI's `libghostty` terminal engine. Adapted from herdr's build.rs (Apache-2.0).
//!
//! The library is built out of tree (`OUT_DIR`), so the vendored source stays untouched. Environment:
//! `ZIG` (the zig binary; default `zig` on PATH; Zig 0.15.2 for the pinned source), `LIBGHOSTTY_VT_OPTIMIZE`
//! (default `ReleaseFast`), `LIBGHOSTTY_VT_SIMD` (default on), `LIBGHOSTTY_VT_ZIG_SYSTEM_DIR` (a directory of
//! Zig packages, to build offline). Pin and local patches: `vendor/libghostty-vt.vendor.json`,
//! `vendor/libghostty-vt.patches.md`.
use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn zig_target(target: &str) -> &str {
    match target {
        "x86_64-unknown-linux-gnu" => "x86_64-linux-gnu",
        "aarch64-unknown-linux-gnu" => "aarch64-linux-gnu",
        "x86_64-unknown-linux-musl" => "x86_64-linux-musl",
        "aarch64-unknown-linux-musl" => "aarch64-linux-musl",
        "x86_64-apple-darwin" => "x86_64-macos",
        "aarch64-apple-darwin" => "aarch64-macos",
        "x86_64-pc-windows-msvc" => "x86_64-windows-msvc",
        "aarch64-pc-windows-msvc" => "aarch64-windows-msvc",
        other => panic!("unsupported target for libghostty-vt: {other}"),
    }
}

fn env_bool(name: &str) -> Option<bool> {
    match env::var(name) {
        Ok(value) => match value.to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => Some(true),
            "0" | "false" | "no" | "off" => Some(false),
            other => panic!("invalid boolean value for {name}: {other}"),
        },
        Err(env::VarError::NotPresent) => None,
        Err(err) => panic!("failed to read {name}: {err}"),
    }
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=vendor/libghostty-vt.vendor.json");
    for path in ["build.zig", "build.zig.zon", "include", "pkg", "src", "VERSION"] {
        println!("cargo:rerun-if-changed=vendor/libghostty-vt/{path}");
    }
    for var in ["LIBGHOSTTY_VT_OPTIMIZE", "LIBGHOSTTY_VT_SIMD", "LIBGHOSTTY_VT_ZIG_SYSTEM_DIR", "ZIG"] {
        println!("cargo:rerun-if-env-changed={var}");
    }

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let vendored = manifest_dir.join("vendor/libghostty-vt");
    let out = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR")).join("libghostty-vt");
    let optimize = env::var("LIBGHOSTTY_VT_OPTIMIZE").unwrap_or_else(|_| "ReleaseFast".into());
    let simd = env_bool("LIBGHOSTTY_VT_SIMD").unwrap_or(true);
    let target = env::var("TARGET").expect("TARGET");
    let version = fs::read_to_string(vendored.join("VERSION")).expect("vendored libghostty-vt VERSION").trim().to_string();

    let zig = env::var("ZIG").unwrap_or_else(|_| "zig".into());
    let mut command = Command::new(&zig);
    command
        .arg("build")
        .arg("-Demit-lib-vt")
        .arg(format!("-Doptimize={optimize}"))
        .arg(format!("-Dsimd={simd}"))
        .arg(format!("-Dtarget={}", zig_target(&target)))
        .arg(format!("-Dversion-string={version}"))
        .arg("-Demit-xcframework=false")
        .arg("--prefix")
        .arg(&out)
        .arg("--cache-dir")
        .arg(out.join("zig-cache"));
    if let Ok(system_dir) = env::var("LIBGHOSTTY_VT_ZIG_SYSTEM_DIR") {
        command.arg("--system").arg(system_dir);
    }
    let status = command.current_dir(&vendored).status().unwrap_or_else(|e| {
        panic!("libghostty-vt needs Zig 0.15.2 to build (set ZIG to its path): running `{zig}` failed: {e}")
    });
    assert!(status.success(), "zig build of the vendored libghostty-vt failed: {status}");

    let lib = out.join("lib");
    println!("cargo:rustc-link-search=native={}", lib.display());
    if target.contains("apple-darwin") {
        println!("cargo:rustc-link-arg={}", lib.join("libghostty-vt.a").display());
    } else if target.contains("windows-msvc") {
        println!("cargo:rustc-link-lib=static=ghostty-vt-static");
    } else {
        println!("cargo:rustc-link-lib=static=ghostty-vt");
    }
}
