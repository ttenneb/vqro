//! Builds the vendored libghostty-vt (Ghostty's terminal emulation library, MIT) with Zig and links it statically,
//! for the TUI's `libghostty` terminal engine. Adapted from herdr's build.rs (Apache-2.0).
//!
//! The library is built out of tree (`OUT_DIR`), so the vendored source stays untouched. Environment:
//! `ZIG` (the zig binary; else `zig` on PATH when it is the version the source needs, else a user-local
//! `~/.local/zig-<version>/zig`; the pinned source needs Zig 0.17.1), `LIBGHOSTTY_VT_OPTIMIZE`
//! (default `ReleaseFast`), `LIBGHOSTTY_VT_SIMD` (default on), `LIBGHOSTTY_VT_ZIG_PKG_DIR` (where Zig keeps the
//! packages it fetches; default `$XDG_CACHE_HOME/vqro/zig-pkg`, else `~/.cache/vqro/zig-pkg`: one fetch per
//! machine, never inside the vendored tree). Offline builds work once that directory holds the packages the
//! library needs (see `vendor/libghostty-vt.notices.md`). Pin and local patches: `vendor/libghostty-vt.vendor.json`,
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
        "x86_64-pc-windows-gnu" => "x86_64-windows-gnu",
        "aarch64-pc-windows-gnullvm" => "aarch64-windows-gnu",
        "x86_64-pc-windows-gnullvm" => "x86_64-windows-gnu",
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
    let local_app_data = if cfg!(windows) { env::var_os("LOCALAPPDATA").filter(|d| !d.is_empty()).map(PathBuf::from) } else { None };
    let cache = env::var("XDG_CACHE_HOME").ok().filter(|d| !d.is_empty()).map(PathBuf::from)
        .or(local_app_data)
        .or_else(|| home().map(|h| h.join(".cache")));
    match cache {
        Some(cache) => cache.join("vqro").join("zig-pkg"),
        None => PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR")).join("zig-pkg"),
    }
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=vendor/libghostty-vt.vendor.json");
    for path in ["build.zig", "build.zig.zon", "include", "pkg", "src", "LIB_VERSION"] {
        println!("cargo:rerun-if-changed=vendor/libghostty-vt/{path}");
    }
    for var in ["LIBGHOSTTY_VT_OPTIMIZE", "LIBGHOSTTY_VT_SIMD", "LIBGHOSTTY_VT_ZIG_PKG_DIR", "ZIG"] {
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
    let mut command = Command::new(&zig);
    command
        .arg("build")
        .arg("-Demit-lib-vt")
        .arg(format!("-Doptimize={optimize}"))
        .arg(format!("-Dsimd={simd}"))
        .arg(format!("-Dtarget={}", zig_target(&target)))
        .arg("-Demit-xcframework=false")
        // Only the parts vqro uses: grid references and colors (the TUI engine), snapshot decoding and the VT
        // formatter (`vqro-rex` turns a Rex terminal's initial Ghostty snapshot into a VT repaint; nothing in the TUI
        // calls them). The rest is left out of the archive, so it is neither linked nor reachable: no Kitty graphics
        // (and its image decoder), selection, search, render state or input encoding. Left-out functions return
        // GHOSTTY_UNSUPPORTED.
        .arg("-Dvt-features=-all,+grid-introspection,+color,+snapshot,+formatter")
        .arg("--prefix")
        .arg(&out)
        .arg("--cache-dir")
        .arg(out.join("zig-cache"));
    // Zig 0.17 puts fetched packages in the project (`zig-pkg/`, here the vendored tree) unless ZIG_LOCAL_PKG_DIR
    // names another place. Packages are named by their content hash (build.zig.zon), so one shared directory per
    // machine serves every checkout, worktree and target, and a fresh checkout needs no network once it is filled.
    // (Zig 0.17.1's `--system` offline mode panics, so the package directory is the offline route.)
    command.env("ZIG_LOCAL_PKG_DIR", pkg_dir());
    let status = command.current_dir(&vendored).status().unwrap_or_else(|e| {
        panic!("libghostty-vt needs Zig {} to build (set ZIG to its path): running `{zig}` failed: {e}", wanted_zig(&vendored))
    });
    assert!(status.success(), "zig build of the vendored libghostty-vt failed: {status}");

    let lib = out.join("lib");
    println!("cargo:rustc-link-search=native={}", lib.display());
    if target.contains("apple-darwin") {
        println!("cargo:rustc-link-arg={}", lib.join("libghostty-vt.a").display());
    } else if target.contains("-windows-msvc") {
        // `ghostty-vt.lib` is the DLL's import library; the static archive is `ghostty-vt-static.lib`.
        println!("cargo:rustc-link-lib=static=ghostty-vt-static");
    } else if target.contains("-windows-") {
        // Zig names the GNU-ABI archive the Windows way too, but GNU linking looks for `lib<name>.a`.
        fs::copy(lib.join("ghostty-vt-static.lib"), lib.join("libghostty-vt-static.a")).expect("libghostty-vt static archive");
        println!("cargo:rustc-link-lib=static=ghostty-vt-static");
    } else {
        println!("cargo:rustc-link-lib=static=ghostty-vt");
    }
}
