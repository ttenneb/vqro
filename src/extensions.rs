//! Curated extension catalog and user-owned marketplace state.
//!
//! The catalog is compiled into every client so browsing never depends on a
//! network request. Runtime state is global to the server user. Repository
//! overrides use the server's stable repository ids, not checkout paths.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub(crate) const CATALOG_SCHEMA_VERSION: u32 = 1;
pub(crate) const STATE_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CuratedIndex {
    pub schema_version: u32,
    pub extensions: Vec<CuratedExtension>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CuratedExtension {
    pub id: String,
    /// Stable package identity. An update may rename `id`, but may not silently
    /// replace another repository identity.
    pub repository: String,
    pub name: String,
    pub summary: String,
    pub details: String,
    pub version: String,
    pub categories: Vec<String>,
    pub package: ExtensionPackage,
    #[serde(default)]
    pub default_installed: bool,
    #[serde(default)]
    pub default_enabled: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum ExtensionPackage {
    Bundled { provider: String },
    AgentIntegration { target: String },
    GithubPlugin { source: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ExtensionStateFile {
    pub schema_version: u32,
    pub entries: BTreeMap<String, ExtensionInstallState>,
    /// Earliest loads first. Conflicting contributions from later entries win.
    pub load_order: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ExtensionInstallState {
    pub installed: bool,
    pub enabled: bool,
    pub active_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rollback_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activation_error: Option<String>,
    /// Overrides are keyed by stable repository ids from the session snapshot.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub repository_overrides: BTreeMap<String, bool>,
}

impl CuratedIndex {
    pub(crate) fn bundled() -> Result<Self, String> {
        let index: Self = serde_json::from_str(include_str!("../extensions/index-v1.json"))
            .map_err(|error| format!("invalid bundled extension index: {error}"))?;
        index.validate()?;
        Ok(index)
    }

    fn validate(&self) -> Result<(), String> {
        if self.schema_version != CATALOG_SCHEMA_VERSION {
            return Err(format!(
                "unsupported extension index schema {}",
                self.schema_version
            ));
        }
        let mut ids = BTreeSet::new();
        let mut repositories = BTreeSet::new();
        for extension in &self.extensions {
            if extension.id.trim().is_empty()
                || extension.repository.trim().is_empty()
                || extension.name.trim().is_empty()
                || extension.version.trim().is_empty()
            {
                return Err("extension index contains empty required metadata".into());
            }
            if !ids.insert(extension.id.as_str()) {
                return Err(format!("duplicate extension id '{}'", extension.id));
            }
            if !repositories.insert(extension.repository.as_str()) {
                return Err(format!(
                    "duplicate extension repository identity '{}'",
                    extension.repository
                ));
            }
            if extension.default_enabled && !extension.default_installed {
                return Err(format!(
                    "extension '{}' is enabled by default but not installed",
                    extension.id
                ));
            }
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn extension(&self, id: &str) -> Option<&CuratedExtension> {
        self.extensions.iter().find(|extension| extension.id == id)
    }

    pub(crate) fn search<'a>(
        &'a self,
        query: &str,
        category: Option<&str>,
    ) -> Vec<&'a CuratedExtension> {
        let query = query.trim().to_ascii_lowercase();
        let category = category.map(str::to_ascii_lowercase);
        self.extensions
            .iter()
            .filter(|extension| {
                category.as_ref().is_none_or(|category| {
                    extension
                        .categories
                        .iter()
                        .any(|candidate| candidate.to_ascii_lowercase() == *category)
                })
            })
            .filter(|extension| {
                query.is_empty()
                    || extension.id.to_ascii_lowercase().contains(&query)
                    || extension.name.to_ascii_lowercase().contains(&query)
                    || extension.summary.to_ascii_lowercase().contains(&query)
                    || extension.details.to_ascii_lowercase().contains(&query)
                    || extension.repository.to_ascii_lowercase().contains(&query)
                    || extension
                        .categories
                        .iter()
                        .any(|value| value.to_ascii_lowercase().contains(&query))
            })
            .collect()
    }
}

impl ExtensionStateFile {
    pub(crate) fn defaults(index: &CuratedIndex) -> Self {
        let entries = index
            .extensions
            .iter()
            .map(|extension| {
                (
                    extension.repository.clone(),
                    ExtensionInstallState {
                        installed: extension.default_installed,
                        enabled: extension.default_enabled,
                        active_version: extension.version.clone(),
                        rollback_version: None,
                        pending_version: None,
                        activation_error: None,
                        repository_overrides: BTreeMap::new(),
                    },
                )
            })
            .collect();
        Self {
            schema_version: STATE_SCHEMA_VERSION,
            entries,
            load_order: index
                .extensions
                .iter()
                .filter(|extension| extension.default_installed)
                .map(|extension| extension.repository.clone())
                .collect(),
        }
    }

    pub(crate) fn normalize(&mut self, index: &CuratedIndex) {
        let known = index
            .extensions
            .iter()
            .map(|extension| extension.repository.as_str())
            .collect::<BTreeSet<_>>();
        self.load_order.retain(|repository| {
            known.contains(repository.as_str())
                && self
                    .entries
                    .get(repository)
                    .is_some_and(|state| state.installed)
        });
        let mut seen = BTreeSet::new();
        self.load_order
            .retain(|repository| seen.insert(repository.clone()));
        for extension in &index.extensions {
            let state = self
                .entries
                .entry(extension.repository.clone())
                .or_insert_with(|| ExtensionInstallState {
                    installed: false,
                    enabled: false,
                    active_version: extension.version.clone(),
                    rollback_version: None,
                    pending_version: None,
                    activation_error: None,
                    repository_overrides: BTreeMap::new(),
                });
            if state.installed && seen.insert(extension.repository.clone()) {
                self.load_order.push(extension.repository.clone());
            }
        }
    }

    pub(crate) fn install(&mut self, extension: &CuratedExtension) {
        let state = self
            .entries
            .entry(extension.repository.clone())
            .or_insert_with(|| ExtensionInstallState {
                installed: false,
                enabled: false,
                active_version: extension.version.clone(),
                rollback_version: None,
                pending_version: None,
                activation_error: None,
                repository_overrides: BTreeMap::new(),
            });
        state.installed = true;
        state.enabled = true;
        state.active_version = extension.version.clone();
        state.activation_error = None;
        if !self.load_order.contains(&extension.repository) {
            self.load_order.push(extension.repository.clone());
        }
    }

    pub(crate) fn finish_activation(
        &mut self,
        repository: &str,
        result: Result<(), String>,
    ) -> Result<(), String> {
        let state = self
            .entries
            .get_mut(repository)
            .ok_or_else(|| format!("unknown extension repository '{repository}'"))?;
        match result {
            Ok(()) => {
                state.enabled = true;
                state.activation_error = None;
            }
            Err(error) => {
                // Keep the package available for diagnostics and retry.
                state.installed = true;
                state.enabled = false;
                state.activation_error = Some(error);
            }
        }
        Ok(())
    }

    pub(crate) fn remove(&mut self, repository: &str) -> Result<(), String> {
        let state = self
            .entries
            .get_mut(repository)
            .ok_or_else(|| format!("unknown extension repository '{repository}'"))?;
        state.installed = false;
        state.enabled = false;
        state.activation_error = None;
        self.load_order.retain(|candidate| candidate != repository);
        Ok(())
    }

    pub(crate) fn set_enabled(&mut self, repository: &str, enabled: bool) -> Result<(), String> {
        let state = self
            .entries
            .get_mut(repository)
            .ok_or_else(|| format!("unknown extension repository '{repository}'"))?;
        if !state.installed {
            return Err("extension is not installed".into());
        }
        state.enabled = enabled;
        if enabled {
            state.activation_error = None;
        }
        Ok(())
    }

    pub(crate) fn set_repository_override(
        &mut self,
        repository: &str,
        repository_id: String,
        enabled: Option<bool>,
    ) -> Result<(), String> {
        if repository_id.trim().is_empty() {
            return Err("repository id must not be empty".into());
        }
        let state = self
            .entries
            .get_mut(repository)
            .ok_or_else(|| format!("unknown extension repository '{repository}'"))?;
        match enabled {
            Some(enabled) => {
                state.repository_overrides.insert(repository_id, enabled);
            }
            None => {
                state.repository_overrides.remove(&repository_id);
            }
        }
        Ok(())
    }

    pub(crate) fn enabled_for_repository(
        &self,
        repository: &str,
        repository_id: Option<&str>,
    ) -> bool {
        let Some(state) = self.entries.get(repository) else {
            return false;
        };
        if !state.installed {
            return false;
        }
        repository_id
            .and_then(|id| state.repository_overrides.get(id).copied())
            .unwrap_or(state.enabled)
    }

    pub(crate) fn move_to(&mut self, repository: &str, index: usize) -> Result<(), String> {
        let Some(current) = self
            .load_order
            .iter()
            .position(|candidate| candidate == repository)
        else {
            return Err("extension is not installed".into());
        };
        let value = self.load_order.remove(current);
        let target = index.min(self.load_order.len());
        self.load_order.insert(target, value);
        Ok(())
    }

    pub(crate) fn stage_update(&mut self, repository: &str, version: String) -> Result<(), String> {
        if version.trim().is_empty() {
            return Err("update version must not be empty".into());
        }
        let state = self
            .entries
            .get_mut(repository)
            .ok_or_else(|| format!("unknown extension repository '{repository}'"))?;
        if !state.installed {
            return Err("extension is not installed".into());
        }
        state.pending_version = Some(version);
        Ok(())
    }

    pub(crate) fn approve_update(&mut self, repository: &str) -> Result<(), String> {
        let state = self
            .entries
            .get_mut(repository)
            .ok_or_else(|| format!("unknown extension repository '{repository}'"))?;
        let pending = state
            .pending_version
            .take()
            .ok_or_else(|| "extension has no pending update".to_string())?;
        state.rollback_version = Some(std::mem::replace(&mut state.active_version, pending));
        state.activation_error = None;
        Ok(())
    }

    pub(crate) fn rollback(&mut self, repository: &str) -> Result<(), String> {
        let state = self
            .entries
            .get_mut(repository)
            .ok_or_else(|| format!("unknown extension repository '{repository}'"))?;
        let previous = state
            .rollback_version
            .take()
            .ok_or_else(|| "extension has no rollback version".to_string())?;
        state.active_version = previous;
        state.pending_version = None;
        state.activation_error = None;
        Ok(())
    }
}

pub(crate) fn state_path() -> PathBuf {
    crate::config::config_dir().join("extensions.json")
}

pub(crate) fn load_state(index: &CuratedIndex) -> ExtensionStateFile {
    let path = state_path();
    match load_state_from_path(&path, index) {
        Ok(state) => state,
        Err(error) => {
            tracing::warn!(path = %path.display(), %error, "failed to load extension state");
            ExtensionStateFile::defaults(index)
        }
    }
}

pub(crate) fn disabled_bundled_providers(
    index: &CuratedIndex,
    state: &ExtensionStateFile,
) -> BTreeSet<String> {
    index
        .extensions
        .iter()
        .filter_map(|extension| match &extension.package {
            ExtensionPackage::Bundled { provider }
                if !state.enabled_for_repository(&extension.repository, None) =>
            {
                Some(format!("vqro.{provider}"))
            }
            ExtensionPackage::Bundled { .. }
            | ExtensionPackage::AgentIntegration { .. }
            | ExtensionPackage::GithubPlugin { .. } => None,
        })
        .collect()
}

pub(crate) fn load_state_from_path(
    path: &Path,
    index: &CuratedIndex,
) -> Result<ExtensionStateFile, String> {
    if !path.exists() {
        return Ok(ExtensionStateFile::defaults(index));
    }
    let mut state: ExtensionStateFile = serde_json::from_slice(
        &std::fs::read(path).map_err(|error| format!("read extension state: {error}"))?,
    )
    .map_err(|error| format!("parse extension state: {error}"))?;
    if state.schema_version != STATE_SCHEMA_VERSION {
        return Err(format!(
            "unsupported extension state schema {}",
            state.schema_version
        ));
    }
    state.normalize(index);
    Ok(state)
}

pub(crate) fn save_state_to_path(path: &Path, state: &ExtensionStateFile) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("create extension state directory: {error}"))?;
    }
    let bytes = serde_json::to_vec_pretty(state)
        .map_err(|error| format!("encode extension state: {error}"))?;
    let temporary = path.with_extension("json.tmp");
    std::fs::write(&temporary, bytes).map_err(|error| format!("write extension state: {error}"))?;
    #[cfg(windows)]
    if path.exists() {
        std::fs::remove_file(path).map_err(|error| format!("replace extension state: {error}"))?;
    }
    std::fs::rename(&temporary, path).map_err(|error| format!("commit extension state: {error}"))
}

pub(crate) fn delete_extension_data(extension_id: &str) -> Result<(), String> {
    for path in [
        crate::plugin_paths::plugin_config_dir(extension_id),
        crate::plugin_paths::plugin_state_dir(extension_id),
    ] {
        match std::fs::remove_dir_all(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("delete {}: {error}", path.display())),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_index_is_valid_searchable_and_has_expected_defaults() {
        let index = CuratedIndex::bundled().unwrap();
        let defaults = index
            .extensions
            .iter()
            .filter(|extension| extension.default_installed && extension.default_enabled)
            .map(|extension| extension.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            defaults,
            [
                "Collections",
                "Spaces",
                "Delegation",
                "Agent Status",
                "Pi",
                "Claude Code",
                "Codex"
            ]
        );
        assert!(index.search("session", Some("Agents")).len() >= 3);
        assert!(index.search("collection", None)[0]
            .categories
            .contains(&"Workflow".to_string()));
    }

    #[test]
    fn removal_retains_overrides_and_reinstall_enables_immediately() {
        let index = CuratedIndex::bundled().unwrap();
        let extension = index.extension("vqro.collections").unwrap();
        let mut state = ExtensionStateFile::defaults(&index);
        state
            .set_repository_override(&extension.repository, "repo_1".into(), Some(false))
            .unwrap();
        state.remove(&extension.repository).unwrap();
        assert!(!state.enabled_for_repository(&extension.repository, Some("repo_1")));
        assert_eq!(
            state.entries[&extension.repository]
                .repository_overrides
                .get("repo_1"),
            Some(&false)
        );
        state.install(extension);
        assert!(state.entries[&extension.repository].enabled);
        assert!(!state.enabled_for_repository(&extension.repository, Some("repo_1")));
    }

    #[test]
    fn activation_failure_keeps_package_installed_but_disabled() {
        let index = CuratedIndex::bundled().unwrap();
        let extension = index.extension("vqro.spaces").unwrap();
        let mut state = ExtensionStateFile::defaults(&index);
        state
            .finish_activation(&extension.repository, Err("service exited".into()))
            .unwrap();
        let installed = &state.entries[&extension.repository];
        assert!(installed.installed);
        assert!(!installed.enabled);
        assert_eq!(
            installed.activation_error.as_deref(),
            Some("service exited")
        );
    }

    #[test]
    fn load_order_is_deterministic_and_later_entries_win() {
        let index = CuratedIndex::bundled().unwrap();
        let mut state = ExtensionStateFile::defaults(&index);
        let first = state.load_order[0].clone();
        state.move_to(&first, usize::MAX).unwrap();
        assert_eq!(state.load_order.last(), Some(&first));
        let json = serde_json::to_string(&state).unwrap();
        let restored: ExtensionStateFile = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.load_order, state.load_order);
    }

    #[test]
    fn updates_require_staging_and_support_one_step_rollback() {
        let index = CuratedIndex::bundled().unwrap();
        let extension = index.extension("vqro.pi").unwrap();
        let mut state = ExtensionStateFile::defaults(&index);
        state
            .stage_update(&extension.repository, "10".into())
            .unwrap();
        assert_eq!(
            state.entries[&extension.repository]
                .pending_version
                .as_deref(),
            Some("10")
        );
        state.approve_update(&extension.repository).unwrap();
        assert_eq!(state.entries[&extension.repository].active_version, "10");
        state.rollback(&extension.repository).unwrap();
        assert_eq!(
            state.entries[&extension.repository].active_version,
            extension.version
        );
    }
}
