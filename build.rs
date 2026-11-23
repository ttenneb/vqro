//! Builds the vendored libghostty-vt (Ghostty's terminal emulation library, MIT) with Zig and links it statically,
//! for the TUI's `libghostty` terminal engine. Adapted from herdr's build.rs (Apache-2.0).
//!
//! The library is built out of tree into a verified per-user content cache, shared by worktrees and profiles.
//! `VQRO_LIBGHOSTTY_CACHE` overrides the cache root; `off` disables sharing and builds in `OUT_DIR` for audits.
//! `VQRO_LIBGHOSTTY_CACHE_KEEP` sets retained shared keys (default 6). GC is off during the machine-wide
//! pause; `VQRO_LIBGHOSTTY_CACHE_GC` uses the standard case-insensitive boolean grammar
//! (`1`/`true`/`yes`/`on` enables; `0`/`false`/`no`/`off` disables; absent is off; other values are refused).
//! Verified link archives are copied into
//! `OUT_DIR` under the key lock so eviction never races a later Cargo link.
//! `OUT_DIR/libghostty-cache.json` records the key, reuse/build result, and elapsed time without Cargo warnings.
//! Environment:
//! `ZIG` (the zig binary; else `zig` on PATH when it is the version the source needs, else a user-local
//! `~/.local/zig-<version>/zig`; the pinned source needs Zig 0.17.1), `LIBGHOSTTY_VT_OPTIMIZE`
//! (default `ReleaseFast`), `LIBGHOSTTY_VT_SIMD` (default on), `LIBGHOSTTY_VT_ZIG_PKG_DIR` (where Zig keeps the
//! packages it fetches; default `$XDG_CACHE_HOME/vqro/zig-pkg`, else `~/.cache/vqro/zig-pkg`: one fetch per
//! machine, never inside the vendored tree). Offline builds work once that directory holds the packages the
//! library needs (see `vendor/libghostty-vt.notices.md`). Pin and local patches: `vendor/libghostty-vt.vendor.json`,
//! `vendor/libghostty-vt.patches.md`.
#[path = "build/ghostty_cache.rs"]
mod ghostty_cache;
#[path = "build/native_config.rs"]
mod native_config;

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
        "x86_64-pc-windows-gnu" => "x86_64-windows-gnu",
        "aarch64-pc-windows-gnullvm" => "aarch64-windows-gnu",
        "x86_64-pc-windows-gnullvm" => "x86_64-windows-gnu",
        other => panic!("unsupported target for libghostty-vt: {other}"),
    }
}

pub(crate) fn env_bool(name: &str) -> Option<bool> {
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

/// The Zig the vendored source asks for (`minimum_zig_version` in its build.zig.zon).
fn wanted_zig(vendored: &std::path::Path) -> String {
    let zon = fs::read_to_string(vendored.join("build.zig.zon")).expect("vendored build.zig.zon");
    zon.lines().find_map(|l| l.trim().strip_prefix(".minimum_zig_version = \"").and_then(|r| r.split('"').next()).map(String::from))
        .expect("minimum_zig_version in build.zig.zon")
}

/// `ZIG` if set (and it must be the wanted version: a wrong one is refused, naming the right one found); else `zig`
/// on PATH if it is the wanted version; else the user-local `~/.local/zig-<version>/zig` (installed next to other
/// versions, never on PATH); else refuse the missing or wrong-version compiler before building.
fn find_zig(vendored: &std::path::Path) -> String {
    let wanted = wanted_zig(vendored);
    let version = |zig: &str| Command::new(zig).arg("version").output().ok().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
    let exe = if cfg!(windows) { "zig.exe" } else { "zig" };
    let local = home().map(|home| home.join(".local").join(format!("zig-{wanted}")).join(exe).display().to_string())
        .filter(|local| version(local).as_deref() == Some(wanted.as_str()));
    if let Ok(zig) = env::var("ZIG") {
        // An explicit choice is never replaced silently: a wrong version stops the build and says what would work.
        match version(&zig) {
            Some(v) if v == wanted => return zig,
            found => panic!(
                "ZIG={zig} is {}, but the vendored libghostty-vt needs Zig {wanted}. {}",
                found.map(|v| format!("Zig {v}")).unwrap_or_else(|| "not a Zig that runs".into()),
                match (&local, version("zig").as_deref() == Some(wanted.as_str())) {
                    (_, true) => format!("Unset ZIG to use the `zig` on PATH (Zig {wanted})."),
                    (Some(local), false) => format!("Unset ZIG to use {local}, or set ZIG={local}."),
                    (None, false) => format!("Set ZIG to a Zig {wanted} (none found on PATH or at ~/.local/zig-{wanted}/zig)."),
                }
            ),
        }
    }
    let found = version("zig");
    if found.as_deref() == Some(wanted.as_str()) { return "zig".into(); }
    local.unwrap_or_else(|| panic!(
        "libghostty-vt needs Zig {wanted} (found {} at PATH zig). Set ZIG to a Zig {wanted}, or install it at ~/.local/zig-{wanted}/zig.",
        found.map(|v| format!("Zig {v}")).unwrap_or_else(|| "no Zig that runs".into())
    ))
}

/// The build host's home: `HOME`, else (Windows, where it is usually unset) `USERPROFILE`.
fn home() -> Option<PathBuf> {
    ["HOME", "USERPROFILE"].iter().find_map(|v| env::var_os(v).filter(|d| !d.is_empty())).map(PathBuf::from)
}

/// Where Zig keeps fetched packages: `LIBGHOSTTY_VT_ZIG_PKG_DIR`, else the user's cache (`XDG_CACHE_HOME`, else
/// `%LOCALAPPDATA%` on a Windows host, else `~/.cache`) under `vqro/zig-pkg`, else (no home) the build's own out dir.
fn pkg_dir() -> PathBuf {
    if let Ok(dir) = env::var("LIBGHOSTTY_VT_ZIG_PKG_DIR") { return dir.into(); }
    cache_root().join("zig-pkg")
}

fn cache_root() -> PathBuf {
    let local_app_data = if cfg!(windows) { env::var_os("LOCALAPPDATA").filter(|d| !d.is_empty()).map(PathBuf::from) } else { None };
    env::var_os("XDG_CACHE_HOME").filter(|d| !d.is_empty()).map(PathBuf::from)
        .or(local_app_data)
        .or_else(|| home().map(|h| h.join(".cache")))
        .map(|base| base.join("vqro"))
        .unwrap_or_else(|| PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR")))
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=build/ghostty_cache.rs");
    println!("cargo:rerun-if-changed=build/native_config.rs");
    for var in native_config::ENV { println!("cargo:rerun-if-env-changed={var}"); }
    println!("cargo:rerun-if-env-changed=HOST");
    println!("cargo:rerun-if-changed=vendor/libghostty-vt.vendor.json");
    println!("cargo:rerun-if-changed=vendor/libghostty-vt.sha256");
    // All source files (including additions), not only the previously known source subdirectories.
    println!("cargo:rerun-if-changed=vendor/libghostty-vt");
    for path in ["build.zig", "build.zig.zon", "include", "pkg", "src", "LIB_VERSION"] {
        println!("cargo:rerun-if-changed=vendor/libghostty-vt/{path}");
    }
    for var in ["LIBGHOSTTY_VT_OPTIMIZE", "LIBGHOSTTY_VT_SIMD", "LIBGHOSTTY_VT_ZIG_PKG_DIR", "ZIG", "VQRO_LIBGHOSTTY_CACHE", "VQRO_LIBGHOSTTY_CACHE_KEEP", "VQRO_LIBGHOSTTY_CACHE_GC", "XDG_CACHE_HOME", "HOME", "USERPROFILE", "LOCALAPPDATA"] {
        println!("cargo:rerun-if-env-changed={var}");
    }

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let vendored = manifest_dir.join("vendor/libghostty-vt");
    let out = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR")).join("libghostty-vt");
    let optimize = env::var("LIBGHOSTTY_VT_OPTIMIZE").unwrap_or_else(|_| "ReleaseFast".into());
    let simd = env_bool("LIBGHOSTTY_VT_SIMD").unwrap_or(true);
    let target = env::var("TARGET").expect("TARGET");
    let zig = find_zig(&vendored);
    // The hand-written FFI declarations against these headers (src/tui/ghostty/bindings_check.c): a size, offset,
    // value or signature that no longer matches fails the build here.
    println!("cargo:rerun-if-changed=src/tui/ghostty/bindings_check.c");
    fs::create_dir_all(&out).expect("libghostty-vt out dir");
    let check = Command::new(&zig)
        .args(["cc", "-c", "-std=c11", "-target", zig_target(&target), "-I"])
        .arg(vendored.join("include"))
        .arg(manifest_dir.join("src/tui/ghostty/bindings_check.c"))
        .arg("-o")
        .arg(out.join("bindings_check.o"))
        .status()
        .unwrap_or_else(|e| panic!("libghostty-vt needs Zig {} (set ZIG to its path): running `{zig}` failed: {e}", wanted_zig(&vendored)));
    assert!(check.success(), "src/tui/ghostty/bindings.rs no longer matches the vendored libghostty-vt headers (bindings_check.c)");
    let started = std::time::Instant::now();
    let version = Command::new(&zig).arg("version").output().expect("zig version");
    assert!(version.status.success(), "zig version failed");
    let flags = vec![
        "build".into(),
        "-Demit-lib-vt".into(),
        format!("-Doptimize={optimize}"),
        format!("-Dsimd={simd}"),
        format!("-Dtarget={}", zig_target(&target)),
        "-Demit-xcframework=false".into(),
        // Only the grid/color engine, snapshot decoder and VT formatter; no other VT features.
        "-Dvt-features=-all,+grid-introspection,+color,+snapshot,+formatter".into(),
        "--prefix".into(), "{prefix}".into(),
        "--cache-dir".into(), "{work}".into(),
    ];
    let native = native_config::capture(&zig, &env::var("HOST").expect("HOST"), &target);
    let native_uncached = native.uncached;
    let inputs = ghostty_cache::Inputs {
        schema: ghostty_cache::SCHEMA,
        source: ghostty_cache::source_digest(&vendored).expect("hash vendored libghostty-vt source"),
        target: target.clone(),
        optimize,
        zig_version: String::from_utf8(version.stdout).expect("UTF-8 zig version").trim().into(),
        native: native.key,
        flags,
    };
    let build = |prefix: &std::path::Path, work: &std::path::Path| -> std::io::Result<()> {
        let mut command = Command::new(&zig);
        // Execute exactly the flags in the key. Only storage locations are checkout-independent placeholders.
        for flag in &inputs.flags {
            match flag.as_str() {
                "{prefix}" => { command.arg(prefix); }
                "{work}" => { command.arg(work); }
                other => { command.arg(other); }
            }
        }
        // Preserve the existing content-addressed package directory; never fetch in the vendored tree.
        // Zig 0.17.1's --system mode panics; pre-populated packages are required for offline builds.
        command.env("ZIG_LOCAL_PKG_DIR", pkg_dir());
        let status = command.current_dir(&vendored).status()?;
        if !status.success() { return Err(std::io::Error::other(format!("zig build failed: {status}"))); }
        if target.contains("-windows-") && !target.contains("-windows-msvc") {
            // GNU linking looks for lib<name>.a; include the alias in the verified immutable entry.
            fs::copy(prefix.join("lib/ghostty-vt-static.lib"), prefix.join("lib/libghostty-vt-static.a"))?;
        }
        Ok(())
    };
    let required = if target.contains("-windows-msvc") { "lib/ghostty-vt-static.lib" }
        else if target.contains("-windows-") { "lib/libghostty-vt-static.a" }
        else { "lib/libghostty-vt.a" };
    let cache_setting = env::var_os("VQRO_LIBGHOSTTY_CACHE");
    let gc = env_bool("VQRO_LIBGHOSTTY_CACHE_GC").unwrap_or(false);
    let disabled = cache_setting.as_deref() == Some(std::ffi::OsStr::new("off"));
    let (result, cache_entry, reason) = if disabled || native_uncached.is_some() {
        let reason = if disabled { "cache disabled explicitly".to_owned() } else { native_uncached.expect("uncached reason") };
        eprintln!("libghostty-vt built locally, not cached: {reason}");
        build(&out, &out.join("zig-cache")).expect("uncached libghostty-vt build");
        (if disabled { "disabled" } else { "uncached-native-configuration" }, None, Some(reason))
    } else {
        let root = cache_setting.filter(|p| !p.is_empty()).map(PathBuf::from)
            .unwrap_or_else(|| cache_root().join("libghostty-vt"));
        let keep = env::var("VQRO_LIBGHOSTTY_CACHE_KEEP").map(|v| v.parse::<usize>().expect("VQRO_LIBGHOSTTY_CACHE_KEEP must be a positive integer")).unwrap_or(ghostty_cache::DEFAULT_KEEP);
        let entry = ghostty_cache::ensure_checked(&root, &inputs, &[required], keep, gc,
            || ghostty_cache::source_digest(&vendored), build,
            |path| ghostty_cache::materialize(path, &[required], &out)).expect("verified libghostty-vt cache");
        if !entry.cached {
            eprintln!("vendored source changed during the build; not cached");
            ("uncached-source-changed", None, Some("vendored source changed during the build; not cached".to_owned()))
        } else {
            (if entry.reused { "reused" } else { "built" }, Some(entry.path), None)
        }
    };
    let note = serde_json::json!({"key": inputs.key(), "result": result, "reason": reason, "cache_entry": cache_entry, "gc_opt_in": gc, "elapsed_ms": started.elapsed().as_millis()});
    fs::write(out.parent().expect("OUT_DIR").join("libghostty-cache.json"), serde_json::to_vec_pretty(&note).expect("cache note")).expect("write cache note");
    println!("cargo:rustc-env=VQRO_LIBGHOSTTY_CACHE_KEY={}", inputs.key());
    // Cargo links a verified private snapshot, never an evictable shared path. Watch only the archive,
    // not the changing FFI-check object or shared LRU timestamps.
    if result != "disabled" && result != "uncached-native-configuration" {
        println!("cargo:rerun-if-changed={}", out.join(required).display());
    }
    let lib = out.join("lib");
    println!("cargo:rustc-link-search=native={}", lib.display());
    if target.contains("apple-darwin") {
        println!("cargo:rustc-link-arg={}", lib.join("libghostty-vt.a").display());
    } else if target.contains("-windows-msvc") {
        // `ghostty-vt.lib` is the DLL's import library; the static archive is `ghostty-vt-static.lib`.
        println!("cargo:rustc-link-lib=static=ghostty-vt-static");
    } else if target.contains("-windows-") {
        // Zig names the GNU-ABI archive the Windows way too, but GNU linking looks for `lib<name>.a`.
        println!("cargo:rustc-link-lib=static=ghostty-vt-static");
    } else {
        println!("cargo:rustc-link-lib=static=ghostty-vt");
    }
}
