//! Conservative reuse boundary approved by vqro-lead (Main), 2026-11-23.
//! Native SDK/toolchain discovery is not emulated: those configurations still build locally.
use std::collections::BTreeMap;
use std::env;
use std::path::Path;
use std::process::Command;

pub const BOUNDARY: &str = "explicit-target-zig-bundled-libc-lld-v1";

// Capture ordinary compiler/toolchain caller settings, including settings that conservatively opt out.
// Cargo linker/target-feature settings are not forwarded to Zig; its actual argv is keyed separately.
// LD_LIBRARY_PATH is also Cargo's build-script runtime search path, not an input to our explicit-target
// archive: Config.zig reads it only in the native-CPU Nix branch. IN_NIX_SHELL already opts out. Including
// Cargo's worktree-specific runtime path would defeat reuse (and conservatively opt out every Cargo build).
pub const ENV: &[&str] = &[
    "CC",
    "CXX",
    "AR",
    "LD",
    "LINK",
    "CFLAGS",
    "CXXFLAGS",
    "CPPFLAGS",
    "LDFLAGS",
    "SYSROOT",
    "CMAKE_SYSROOT",
    "ZIG_LIBC",
    "ZIG_LIB_DIR",
    "ZIG_TARGET",
    "SDKROOT",
    "DEVELOPER_DIR",
    "MACOSX_DEPLOYMENT_TARGET",
    "IPHONEOS_DEPLOYMENT_TARGET",
    "C_INCLUDE_PATH",
    "CPLUS_INCLUDE_PATH",
    "CPATH",
    "LIBRARY_PATH",
    "COMPILER_PATH",
    "GCC_EXEC_PREFIX",
    "NIX_CFLAGS_COMPILE",
    "NIX_CFLAGS_LINK",
    "NIX_LDFLAGS",
    "IN_NIX_SHELL",
    "ZIG_IS_DETECTING_LIBC_PATHS",
    "ZIG_IS_AVOIDING_CALLING_ITSELF",
    "PKG_CONFIG",
    "PKG_CONFIG_PATH",
    "PKG_CONFIG_LIBDIR",
    "PKG_CONFIG_SYSROOT_DIR",
    "DESTDIR",
    "WindowsSdkDir",
    "WindowsSDKVersion",
    "WindowsSdkVerBinPath",
    "UniversalCRTSdkDir",
    "UCRTVersion",
    "VCToolsInstallDir",
    "VCINSTALLDIR",
    "VSINSTALLDIR",
    "INCLUDE",
    "LIB",
    "LIBPATH",
    "VS140COMNTOOLS",
    "PROGRAMDATA",
    "HOMEBREW_PREFIX",
];

pub struct Configuration {
    pub key: BTreeMap<String, Option<String>>,
    pub uncached: Option<String>,
}

pub fn reason(target: &str, settings: &BTreeMap<String, Option<String>>) -> Option<String> {
    if target.contains("apple-darwin") {
        return Some("Darwin SDK/toolchain caching is deferred until SDK keying is proven".into());
    }
    if target.contains("windows-msvc") {
        return Some(
            "MSVC Windows SDK/Visual Studio discovery is outside the shared reuse boundary".into(),
        );
    }
    if !(target.ends_with("-linux-gnu")
        || target.ends_with("-linux-musl")
        || target.ends_with("-windows-gnu")
        || target.ends_with("-windows-gnullvm"))
    {
        return Some("target is outside the explicit bundled-libc/linker reuse boundary".into());
    }
    for name in ENV {
        // PROGRAMDATA alone is a normal Windows host setting, not selection of an external SDK.
        if *name == "PROGRAMDATA" {
            continue;
        }
        if settings
            .get(*name)
            .and_then(|v| v.as_ref())
            .is_some_and(|v| !v.is_empty())
        {
            return Some(format!(
                "external compiler/sysroot/SDK or build setting {name} is set"
            ));
        }
    }
    None
}

/// Zig 0.17 `env` emits ZON. These top-level string fields use JSON-compatible escaping for ordinary
/// install paths. An unrecognized response/path is a documented local-build fallback, never a guessed key.
pub fn parse_zig_env(output: &str) -> Result<BTreeMap<String, Option<String>>, String> {
    let mut result = BTreeMap::new();
    for name in ["zig_exe", "lib_dir", "target"] {
        let prefix = format!(".{name} = ");
        let raw = output
            .lines()
            .find_map(|line| line.trim().strip_prefix(&prefix))
            .and_then(|v| v.strip_suffix(','))
            .ok_or_else(|| format!("zig env did not report {name}"))?;
        let value: String =
            serde_json::from_str(raw).map_err(|_| format!("unrecognized zig env {name}"))?;
        if value.is_empty() {
            return Err(format!("empty zig env {name}"));
        }
        let value = if name == "target" {
            value
        } else {
            Path::new(&value)
                .canonicalize()
                .map_err(|_| format!("cannot resolve zig env {name}"))?
                .to_str()
                .ok_or_else(|| format!("non-UTF-8 zig env {name}"))?
                .to_owned()
        };
        result.insert(format!("zig.{name}"), Some(value));
    }
    Ok(result)
}

pub fn capture(zig: &str, host: &str, target: &str) -> Configuration {
    let mut settings = BTreeMap::new();
    let mut invalid = None;
    for name in ENV {
        let value = match env::var(name) {
            Ok(v) => Some(v),
            Err(env::VarError::NotPresent) => None,
            Err(_) => {
                invalid = Some(format!("non-UTF-8 native setting {name}"));
                None
            }
        };
        settings.insert((*name).to_owned(), value);
    }
    let mut uncached = invalid.or_else(|| reason(target, &settings));
    let mut key = settings
        .into_iter()
        .map(|(k, v)| (format!("env.{k}"), v))
        .collect::<BTreeMap<_, _>>();
    key.insert("boundary".into(), Some(BOUNDARY.into()));
    key.insert("host".into(), Some(host.into()));
    key.insert(
        "target-libc".into(),
        Some("Zig bundled libc for the explicit target".into()),
    );
    key.insert(
        "archive/linker".into(),
        Some("Zig integrated archive tools and target linker".into()),
    );
    match Command::new(zig).arg("env").output() {
        Ok(output) if output.status.success() => {
            match parse_zig_env(&String::from_utf8_lossy(&output.stdout)) {
                Ok(resolved) => key.extend(resolved),
                Err(reason) => {
                    uncached.get_or_insert(reason);
                }
            }
        }
        _ => {
            uncached.get_or_insert(
                "cannot query Zig installation and bundled libc/linker paths".into(),
            );
        }
    }
    Configuration { key, uncached }
}
