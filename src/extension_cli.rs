//! Local executable-extension CLI contributions. No product syntax or authority.
use std::path::{Component, Path};

use crate::api::schema::{InstalledPluginInfo, PluginPlatform};

/// Internal manifest data only: never add this to a frozen endpoint codec.
#[derive(Debug, Clone, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct CliCommand {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub command: Vec<String>,
    #[serde(default)]
    pub platforms: Option<Vec<PluginPlatform>>,
}

pub(crate) fn validate(commands: &[CliCommand]) -> Result<(), String> {
    if commands.len() > 64 {
        return Err("extension has more than 64 CLI commands".into());
    }
    let mut names = std::collections::HashSet::new();
    for contribution in commands {
        let name = &contribution.name;
        if name.is_empty()
            || name.len() > 64
            || !name.as_bytes()[0].is_ascii_lowercase()
            || !name
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'-' | b'_'))
            || !names.insert(name)
        {
            return Err("invalid or duplicate extension CLI command name".into());
        }
        if contribution.description.len() > 256
            || contribution.description.chars().any(char::is_control)
        {
            return Err("invalid extension CLI description".into());
        }
        if contribution.command.is_empty()
            || contribution.command.len() > 128
            || contribution.command[0].is_empty()
            || contribution
                .command
                .iter()
                .any(|arg| arg.len() > 4096 || arg.contains('\0'))
            || contribution.command.iter().map(String::len).sum::<usize>() > 64 * 1024
        {
            return Err("invalid or oversized extension CLI argv".into());
        }
        for arg in &contribution.command {
            if arg.starts_with("./")
                && Path::new(arg)
                    .components()
                    .any(|part| !matches!(part, Component::CurDir | Component::Normal(_)))
            {
                return Err("extension CLI path escapes its root".into());
            }
        }
        if contribution
            .platforms
            .as_ref()
            .is_some_and(|platforms| platforms.is_empty())
        {
            return Err("invalid extension CLI platforms".into());
        }
    }
    Ok(())
}

pub(crate) fn declarations(
    plugin: &InstalledPluginInfo,
) -> Result<(InstalledPluginInfo, Vec<CliCommand>), String> {
    let (fresh, commands) =
        crate::app::load_plugin_manifest_with_cli(&plugin.manifest_path, plugin.enabled)
            .map_err(|(_, message)| message)?;
    if fresh.plugin_id != plugin.plugin_id
        || Path::new(&fresh.plugin_root).canonicalize().ok()
            != Path::new(&plugin.plugin_root).canonicalize().ok()
    {
        return Err("extension CLI manifest no longer matches its registered identity/root".into());
    }
    Ok((fresh, commands))
}

pub(crate) fn argv(command: &[String], root: &Path, args: &[String]) -> Vec<String> {
    command
        .iter()
        .map(|arg| {
            if let Some(path) = arg.strip_prefix("./") {
                root.join(path).to_string_lossy().into_owned()
            } else {
                arg.clone()
            }
        })
        .chain(args.iter().cloned())
        .collect()
}

fn supported(platforms: &Option<Vec<PluginPlatform>>) -> bool {
    let current = match std::env::consts::OS {
        "linux" => PluginPlatform::Linux,
        "macos" => PluginPlatform::Macos,
        "windows" => PluginPlatform::Windows,
        _ => return false,
    };
    platforms
        .as_ref()
        .is_none_or(|platforms| platforms.contains(&current))
}

pub(crate) fn run(name: &str, args: &[String], remote: bool) -> std::io::Result<i32> {
    if remote {
        eprintln!("extension CLI commands are not supported yet for remote targets");
        return Ok(2);
    }
    let entries = crate::persist::plugin_registry::try_load()?;
    let mut selected = None;
    for plugin in entries.iter().filter(|plugin| plugin.enabled) {
        let (fresh, commands) = match declarations(plugin) {
            Ok(declarations) => declarations,
            Err(error) => {
                tracing::warn!(plugin = %plugin.plugin_id, %error, "extension CLI declarations unavailable");
                continue;
            }
        };
        if let Some(command) = commands.into_iter().find(|command| command.name == name) {
            if selected.is_some() {
                eprintln!("extension CLI command {name} is ambiguous; no execution attempted");
                return Ok(2);
            }
            selected = Some((fresh, command));
        }
    }
    let Some((plugin, contribution)) = selected else {
        eprintln!("unknown command: {name} (no available extension contribution)");
        return Ok(2);
    };
    if !supported(&plugin.platforms) || !supported(&contribution.platforms) {
        eprintln!("extension CLI command {name} is not supported yet on this platform");
        return Ok(2);
    }
    let argv = argv(&contribution.command, Path::new(&plugin.plugin_root), args);
    let mut command = std::process::Command::new(&argv[0]);
    command.args(&argv[1..]);
    command.env("VQRO_EXTENSION_ROOT", &plugin.plugin_root);
    command.env("VQRO_BIN_PATH", std::env::current_exe()?);
    command.env("VQRO_SOCKET_PATH", crate::api::socket_path());
    // Foreground replacement preserves the original direct shell child/Pi
    // ancestry. Ordinary spawn/wait would change human callers to Unknown.
    crate::platform::foreground_cli_command(command)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn declaration(name: &str, command: &[&str]) -> CliCommand {
        CliCommand {
            name: name.into(),
            description: "Example".into(),
            command: command.iter().map(|value| (*value).into()).collect(),
            platforms: None,
        }
    }
    #[test]
    fn validates_aliases_bounds_and_rooted_paths() {
        let good = declaration("example", &["python3", "./cli.py", ""]);
        validate(std::slice::from_ref(&good)).unwrap();
        assert!(validate(&[good.clone(), good]).is_err());
        for bad in [
            declaration("--help", &["echo"]),
            declaration("Example", &["echo"]),
            declaration("example", &["python3", "./../outside.py"]),
            declaration("example", &[""]),
        ] {
            assert!(validate(&[bad]).is_err());
        }
    }
    #[test]
    fn only_static_marked_paths_expand_user_argv_stays_literal() {
        let command = declaration("example", &["python3", "./cli.py", "$HOME; echo bad"]);
        let args = vec!["./input.json".into(), "two words".into(), "".into()];
        assert_eq!(
            argv(&command.command, Path::new("/extension"), &args),
            [
                "python3",
                "/extension/cli.py",
                "$HOME; echo bad",
                "./input.json",
                "two words",
                ""
            ]
        );
    }
}
