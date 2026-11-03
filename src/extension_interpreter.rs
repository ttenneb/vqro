//! Transitional bundled interpreter bootstrap, not package capability consent.
use std::path::Path;

pub(crate) const PYTHON_TOKEN: &str = "vqro:python3";
#[cfg(any(windows, test))]
pub(crate) const REMEDY: &str = "Install CPython 3.9 or newer from python.org, or set [extensions] tasks_python to its absolute python.exe path; then confirm Python in Settings → Tasks.";

#[derive(
    Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct InterpreterStatus {
    pub path: Option<String>,
    pub confirmed: bool,
    pub message: String,
}

/// Recheck the displayed candidate against fresh config before saving approval.
pub(crate) fn confirm(expected: &str) -> Result<crate::config::ExtensionsConfig, String> {
    if crate::platform::tasks_python_status().is_none() {
        return Err("Python confirmation is only needed on Windows".into());
    }
    confirm_at(
        &crate::config::config_path(),
        expected,
        crate::platform::tasks_python_binding,
    )
}

fn confirm_at(
    path: &Path,
    expected: &str,
    resolve: impl FnOnce(&crate::config::ExtensionsConfig) -> Option<InterpreterStatus>,
) -> Result<crate::config::ExtensionsConfig, String> {
    let mut saved = None;
    crate::config::try_update_file_at(path, "Tasks Python confirmation", |content| {
        let document: toml::Value =
            toml::from_str(content).map_err(|error| format!("invalid config: {error}"))?;
        if document
            .get("extensions")
            .is_some_and(|value| !value.is_table())
        {
            return Err("extensions must be a TOML table or inline table".into());
        }
        // Match section-isolated config loading: unrelated invalid settings must
        // not block approval or be rewritten by this targeted edit.
        let config: crate::config::ExtensionsConfig = document
            .get("extensions")
            .cloned()
            .map(toml::Value::try_into)
            .transpose()
            .map_err(|error| format!("invalid extensions config: {error}"))?
            .unwrap_or_default();
        let status = resolve(&config).ok_or("Python confirmation is only needed on Windows")?;
        if status.path.as_deref() != Some(expected) {
            return Err("Python candidate changed or is invalid; reopen Settings → Tasks and confirm the current path".into());
        }
        let updated = crate::config::ConfigEdit::ConfirmedTasksPython(expected).apply(content);
        let document: toml::Value =
            toml::from_str(&updated).map_err(|error| format!("invalid edited config: {error}"))?;
        let written: crate::config::ExtensionsConfig = document
            .get("extensions")
            .cloned()
            .ok_or("edited config lost extensions table")?
            .try_into()
            .map_err(|error| format!("invalid edited extensions config: {error}"))?;
        if written.confirmed_tasks_python.as_deref() != Some(expected)
            || written.tasks_python != config.tasks_python
        {
            return Err("edited config did not retain the exact interpreter binding".into());
        }
        saved = Some(written);
        Ok(updated)
    })?;
    saved.ok_or_else(|| "interpreter confirmation was not saved".into())
}

/// Resolve only the executable, never argument tails or environment templates.
/// Every actual provider spawn and both CLI/proof observations use this seam.
pub(crate) fn resolve(command: &[String], root: &Path) -> Result<Vec<String>, String> {
    resolve_with(
        command,
        root,
        crate::bundled::is_current_default_plugin,
        crate::platform::tasks_python_program,
    )
}

fn resolve_with(
    command: &[String],
    root: &Path,
    admitted: impl FnOnce(&Path, &str) -> bool,
    program: impl FnOnce() -> Result<String, String>,
) -> Result<Vec<String>, String> {
    if command.first().is_none_or(|value| value != PYTHON_TOKEN) {
        return Ok(command.to_vec());
    }
    if !admitted(root, PYTHON_TOKEN) {
        return Err("symbolic Python requires the exact current bundled Tasks artifact".into());
    }
    let mut resolved = command.to_vec();
    resolved[0] = program()?;
    Ok(resolved)
}

#[cfg(any(windows, test))]
pub(crate) fn binding(
    config: &crate::config::ExtensionsConfig,
    discover: impl FnOnce() -> Option<String>,
    valid: impl Fn(&str) -> bool,
) -> InterpreterStatus {
    // Pin an approved path across registry upgrades; an explicit override wins.
    let path = config
        .tasks_python
        .clone()
        .or_else(|| {
            config
                .confirmed_tasks_python
                .clone()
                .filter(|path| valid(path))
        })
        .or_else(discover);
    let usable = path.as_deref().is_some_and(&valid);
    let confirmed = usable && path == config.confirmed_tasks_python;
    let message = if confirmed {
        "Python path confirmed (path approval, not publisher attestation).".into()
    } else if usable {
        format!("Confirm this exact Python executable before Tasks can run. {REMEDY}")
    } else {
        format!("Tasks Python is missing or invalid. {REMEDY}")
    };
    InterpreterStatus {
        path: path.filter(|_| usable),
        confirmed,
        message,
    }
}

#[cfg(any(windows, test))]
pub(crate) fn python_version(tag: &str) -> Option<Vec<u32>> {
    let tag = tag
        .strip_suffix("-32")
        .or_else(|| tag.strip_suffix("-64"))
        .or_else(|| tag.strip_suffix("-arm64"))
        .unwrap_or(tag);
    let tag = tag.strip_suffix('t').unwrap_or(tag);
    let mut version = tag
        .split('.')
        .map(|part| {
            if part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err(());
            }
            part.parse::<u32>().map_err(|_| ())
        })
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    if !(version.len() >= 2 && version.len() <= 3 && version[0] == 3 && version[1] >= 9) {
        return None;
    }
    version.resize(3, 0);
    Some(version)
}

/// Never elect a newest candidate from an incomplete registry prefix.
#[cfg(any(windows, test))]
pub(crate) fn complete_python_tags(
    mut next: impl FnMut(u32) -> Result<Option<String>, ()>,
) -> Result<Vec<String>, ()> {
    let mut tags = Vec::new();
    for index in 0..=256 {
        match next(index)? {
            None => return Ok(tags),
            Some(tag) if index < 256 => tags.push(tag),
            Some(_) => return Err(()),
        }
    }
    Err(())
}

#[cfg(any(windows, test))]
pub(crate) fn registry_python_string(buffer: &[u16], reported_bytes: usize) -> Option<String> {
    if reported_bytes < 2 || !reported_bytes.is_multiple_of(2) || reported_bytes / 2 > buffer.len()
    {
        return None;
    }
    let units = &buffer[..reported_bytes / 2];
    if units.last() != Some(&0) || units[..units.len() - 1].contains(&0) {
        return None;
    }
    String::from_utf16(&units[..units.len() - 1]).ok()
}

/// Registry precedence and version election are pure and shared-testable.
#[cfg(any(windows, test))]
pub(crate) fn select_python_candidate(
    mut candidates: Vec<(u8, Vec<u32>, u8, String)>,
) -> Option<String> {
    candidates.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| b.1.cmp(&a.1))
            .then_with(|| a.2.cmp(&b.2))
            .then_with(|| a.3.cmp(&b.3))
    });
    candidates.into_iter().next().map(|candidate| candidate.3)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn registry_election_requires_a_complete_bounded_enumeration() {
        assert!(complete_python_tags(|index| if index == 0 {
            Ok(Some("3.9".into()))
        } else {
            Err(())
        })
        .is_err());
        assert!(complete_python_tags(|_| Ok(Some("3.9".into()))).is_err());
        assert_eq!(
            complete_python_tags(|index| Ok((index < 256).then(|| "3.9".into())))
                .unwrap()
                .len(),
            256
        );
        assert!(complete_python_tags(|_| Ok(None)).unwrap().is_empty());
    }
    #[test]
    fn bounded_registry_strings_reject_partial_malformed_and_interior_nul_data() {
        assert_eq!(registry_python_string(&[65, 0], 4).as_deref(), Some("A"));
        for (buffer, bytes) in [
            (vec![], 0),
            (vec![65, 0], 3),
            (vec![65, 0], 6),
            (vec![65], 2),
            (vec![65, 0, 66, 0], 8),
            (vec![0xd800, 0], 4),
        ] {
            assert_eq!(registry_python_string(&buffer, bytes), None);
        }
    }
    #[test]
    fn numeric_supported_versions_only() {
        assert!(python_version("3.13") > python_version("3.9"));
        assert_eq!(python_version("3.13-32"), python_version("3.13"));
        assert_eq!(python_version("3.13t-arm64"), python_version("3.13"));
        for tag in ["3.8", "2.7", "+3.13", "3.14rc1", "", "4.0", "3.9.1.2"] {
            assert!(python_version(tag).is_none(), "{tag}");
        }
    }
    #[test]
    fn consent_is_exact_and_pinned_override_never_falls_back() {
        let mut config = crate::config::ExtensionsConfig::default();
        let status = binding(&config, || Some("old".into()), |_| true);
        assert!(!status.confirmed);
        config.confirmed_tasks_python = Some("old".into());
        assert_eq!(
            binding(&config, || panic!("pinned"), |_| true)
                .path
                .as_deref(),
            Some("old")
        );
        config.tasks_python = Some("new".into());
        assert!(!binding(&config, || panic!("override"), |_| true).confirmed);
        let status = binding(
            &config,
            || panic!("invalid override must not fall back"),
            |_| false,
        );
        assert!(!status.confirmed);
        assert_eq!(status.path, None);
        assert!(status.message.contains("python.org"));
    }
    #[test]
    fn hive_then_numeric_newest_then_view_then_path() {
        let candidate = |hive, version: &str, view, path: &str| {
            (hive, python_version(version).unwrap(), view, path.into())
        };
        let mut values = vec![
            candidate(1, "3.99", 0, "machine"),
            candidate(0, "3.9", 0, "old"),
            candidate(0, "3.13", 1, "other-view"),
            candidate(0, "3.13", 0, "b"),
            candidate(0, "3.13", 0, "a"),
        ];
        assert_eq!(
            select_python_candidate(values.clone()).as_deref(),
            Some("a")
        );
        values.reverse();
        assert_eq!(select_python_candidate(values).as_deref(), Some("a"));
        assert_eq!(select_python_candidate(vec![]), None);
    }
    #[test]
    fn lost_confirmed_path_reelects_but_does_not_transfer_consent() {
        let config = crate::config::ExtensionsConfig {
            confirmed_tasks_python: Some("gone".into()),
            ..Default::default()
        };
        let status = binding(
            &config,
            || Some("replacement".into()),
            |path| path != "gone",
        );
        assert_eq!(status.path.as_deref(), Some("replacement"));
        assert!(!status.confirmed);
    }
    #[test]
    fn confirmation_preserves_edits_roundtrips_and_rejects_stale_candidate() {
        let root = std::env::temp_dir().join(format!("vqro-python-confirm-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("config.toml");
        let original = "# unrelated user comment\n[theme]\nname = 'dark'\n[extensions]\ntasks_python = 'C:\\Python A\\python.exe'\n";
        std::fs::write(&path, original).unwrap();
        let selected = "C:\\Python A\\python.exe";
        let resolve =
            |config: &crate::config::ExtensionsConfig| Some(binding(config, || None, |_| true));
        assert!(confirm_at(&path, "C:\\other.exe", resolve).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        confirm_at(&path, selected, resolve).unwrap();
        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(saved.starts_with(original));
        let config: crate::config::Config = toml::from_str(&saved).unwrap();
        assert_eq!(
            config.extensions.confirmed_tasks_python.as_deref(),
            Some(selected)
        );
        assert!(binding(&config.extensions, || None, |_| true).confirmed);
        let unrelated_invalid_setting = original.replace("name = 'dark'", "name = 42");
        std::fs::write(&path, &unrelated_invalid_setting).unwrap();
        confirm_at(&path, selected, resolve).unwrap();
        assert!(std::fs::read_to_string(&path)
            .unwrap()
            .starts_with(&unrelated_invalid_setting));
        std::fs::write(&path, "invalid = [").unwrap();
        assert!(confirm_at(&path, selected, resolve).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "invalid = [");
        assert!(confirm_at(&path.join("child"), selected, resolve).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn confirmation_preserves_quoted_commented_dotted_and_inline_tables() {
        let root = std::env::temp_dir().join(format!("vqro-python-layouts-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("config.toml");
        let selected = "C:\\Python A\\python.exe";
        for original in [
            "[extensions] # keep\ntasks_python='C:\\Python A\\python.exe'\n[theme] # following table\nname='dark'\n",
            "[\"extensions\"] # keep\ntasks_python='C:\\Python A\\python.exe'\n",
            "# keep\nextensions.tasks_python='C:\\Python A\\python.exe'\n",
            "# keep\nextensions={tasks_python='C:\\Python A\\python.exe'}\n",
            "[extensions] # keep\ntasks_python='C:\\Python A\\python.exe'\n'confirmed_tasks_python'='old' # retain value comment\n",
        ] {
            std::fs::write(&path, original).unwrap();
            let written = confirm_at(&path, selected, |config| Some(binding(config, || None, |_| true))).unwrap();
            assert_eq!(written.confirmed_tasks_python.as_deref(), Some(selected));
            let saved = std::fs::read_to_string(&path).unwrap();
            assert!(saved.contains("# keep"));
            if original.contains("# retain value comment") { assert!(saved.contains("# retain value comment")); }
            let mut expected: toml::Value = toml::from_str(original).unwrap();
            expected["extensions"].as_table_mut().unwrap().insert("confirmed_tasks_python".into(), selected.into());
            assert_eq!(toml::from_str::<toml::Value>(&saved).unwrap(), expected);
        }
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn confirmation_rejects_positional_arrays_and_other_non_table_shapes_without_writing() {
        let root =
            std::env::temp_dir().join(format!("vqro-python-invalid-shapes-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("config.toml");
        let selected = "C:\\Python A\\python.exe";
        for original in [
            "extensions=['C:\\Python A\\python.exe','old',[]]\n",
            "extensions=[]\n",
            "extensions='bad'\n",
            "extensions=1\n",
            "extensions=true\n",
        ] {
            std::fs::write(&path, original).unwrap();
            assert!(confirm_at(&path, selected, |config| Some(binding(
                config,
                || Some(selected.into()),
                |_| true
            )))
            .is_err());
            assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        }
        std::fs::write(&path, "# keep\n").unwrap();
        confirm_at(&path, selected, |config| {
            Some(binding(config, || Some(selected.into()), |_| true))
        })
        .unwrap();
        assert!(std::fs::read_to_string(&path).unwrap().contains("# keep"));
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn cli_proof_and_provider_share_literal_executable_resolution() {
        let fixed = vec![PYTHON_TOKEN.into(), "./cli.py".into(), "task".into()];
        let root = Path::new("/bundle/tasks-current");
        let program = || Ok("C:\\Python A\\python.exe".into());
        let command = resolve_with(&fixed, root, |_, _| true, program).unwrap();
        assert_eq!(command[0], "C:\\Python A\\python.exe");
        assert_eq!(&command[1..], &fixed[1..]);
        let tail = vec!["$HOME; literal".into(), "a  b".into()];
        let actual_cli = crate::extension_cli::argv(&command, root, &tail);
        let fresh_proof = crate::extension_cli::argv(
            &resolve_with(&fixed, root, |_, _| true, program).unwrap(),
            root,
            &tail,
        );
        assert_eq!(actual_cli, fresh_proof);
        assert_eq!(&actual_cli[actual_cli.len() - 2..], &tail);
        assert!(resolve_with(&fixed, root, |_, _| true, || Err("revoked".into())).is_err());
        assert!(resolve_with(&fixed, root, |_, _| false, program).is_err());
    }
    #[test]
    fn only_executable_token_is_resolved() {
        let command = vec!["echo".into(), PYTHON_TOKEN.into(), "$HOME".into()];
        assert_eq!(resolve(&command, Path::new(".")), Ok(command));
        assert!(resolve(&[PYTHON_TOKEN.into()], Path::new(".")).is_err());
    }
}
