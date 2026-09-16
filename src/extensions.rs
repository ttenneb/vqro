//! Curated extension catalog and user-owned marketplace state.
//!
//! The catalog is compiled into every client so browsing never depends on a
//! network request. Runtime state is global to the server user. Repository
//! overrides use the server's stable repository ids, not checkout paths.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

pub(crate) const CATALOG_SCHEMA_VERSION: u32 = 1;
pub(crate) const STATE_SCHEMA_VERSION: u32 = 1;
const MAX_MARKETPLACE_STATE_BYTES: u64 = 1024 * 1024;
const MAX_MARKETPLACE_JOURNAL_BYTES: u64 = 64 * 1024;
static NEXT_STATE_TEMP_ID: AtomicU64 = AtomicU64::new(1);

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_commit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_sha256: Option<String>,
    #[serde(default = "default_manifest_path")]
    pub manifest_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest_sha256: Option<String>,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CuratedIntegrationTarget {
    Supported(crate::api::schema::IntegrationTarget),
    Letta,
}

impl CuratedIntegrationTarget {
    pub(crate) fn supported(self) -> Option<crate::api::schema::IntegrationTarget> {
        match self {
            Self::Supported(target) => Some(target),
            Self::Letta => None,
        }
    }
}

impl<'de> Deserialize<'de> for CuratedIntegrationTarget {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        if value == "letta" {
            return Ok(Self::Letta);
        }
        serde_json::from_value(serde_json::Value::String(value.replace('-', "_")))
            .map(Self::Supported)
            .map_err(serde::de::Error::custom)
    }
}

impl Serialize for CuratedIntegrationTarget {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Supported(target) => target.serialize(serializer),
            Self::Letta => serializer.serialize_str("letta"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum ExtensionPackage {
    Bundled { provider: String },
    AgentIntegration { target: CuratedIntegrationTarget },
    GithubPlugin { source: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct MarketplaceStateFile {
    pub schema_version: u32,
    pub entries: BTreeMap<String, MarketplaceInstallState>,
    /// Earliest loads first. Conflicting contributions from later entries win.
    pub load_order: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct VerifiedExtensionArtifact {
    pub extension_id: String,
    pub version: String,
    pub commit: String,
    pub source_sha256: String,
    pub manifest_sha256: String,
    pub manifest_path: String,
    pub path: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct MarketplaceInstallState {
    /// Persistent namespace for config/state deletion. It does not change when
    /// display or manifest ids are renamed under the same repository identity.
    #[serde(default)]
    pub data_id: String,
    pub installed: bool,
    pub enabled: bool,
    pub active_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rollback_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activation_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_artifact: Option<VerifiedExtensionArtifact>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rollback_artifact: Option<VerifiedExtensionArtifact>,
    /// Overrides are keyed by stable repository ids from the session snapshot.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub repository_overrides: BTreeMap<String, bool>,
}

fn default_manifest_path() -> String {
    "herdr-plugin.toml".into()
}

impl CuratedExtension {
    pub(crate) fn has_pinned_source(&self) -> bool {
        self.source_commit.is_some()
            && self.source_sha256.is_some()
            && self.manifest_sha256.is_some()
            && !self.manifest_path.is_empty()
    }
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
            if extension.source_commit.as_ref().is_some_and(|commit| {
                commit.len() != 40 || !commit.bytes().all(|byte| byte.is_ascii_hexdigit())
            }) {
                return Err(format!(
                    "extension '{}' has an invalid source commit",
                    extension.id
                ));
            }
            for (label, digest) in [
                ("source", extension.source_sha256.as_ref()),
                ("manifest", extension.manifest_sha256.as_ref()),
            ] {
                if digest.is_some_and(|digest| {
                    digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit())
                }) {
                    return Err(format!(
                        "extension '{}' has an invalid {label} digest",
                        extension.id
                    ));
                }
            }
            let pin_count = [
                extension.source_commit.is_some(),
                extension.source_sha256.is_some(),
                extension.manifest_sha256.is_some(),
            ]
            .into_iter()
            .filter(|present| *present)
            .count();
            if pin_count != 0 && pin_count != 3 {
                return Err(format!(
                    "extension '{}' has incomplete pinned source metadata",
                    extension.id
                ));
            }
            let remotely_installable =
                !extension.repository.contains('#') && extension.repository.split('/').count() == 2;
            if remotely_installable && !extension.has_pinned_source() {
                return Err(format!(
                    "extension '{}' is remotely installable without exact pins",
                    extension.id
                ));
            }
            let manifest_path = Path::new(&extension.manifest_path);
            if manifest_path.is_absolute()
                || extension.manifest_path.is_empty()
                || manifest_path.components().any(|component| {
                    matches!(
                        component,
                        std::path::Component::ParentDir
                            | std::path::Component::RootDir
                            | std::path::Component::Prefix(_)
                    )
                })
            {
                return Err(format!(
                    "extension '{}' has an unsafe manifest path",
                    extension.id
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

impl MarketplaceStateFile {
    pub(crate) fn defaults(index: &CuratedIndex) -> Self {
        let entries = index
            .extensions
            .iter()
            .map(|extension| {
                (
                    extension.repository.clone(),
                    MarketplaceInstallState {
                        data_id: extension.id.clone(),
                        installed: extension.default_installed,
                        enabled: extension.default_enabled,
                        active_version: extension.version.clone(),
                        rollback_version: None,
                        pending_version: None,
                        activation_error: None,
                        active_artifact: None,
                        rollback_artifact: None,
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
                .or_insert_with(|| MarketplaceInstallState {
                    data_id: extension.id.clone(),
                    installed: false,
                    enabled: false,
                    active_version: extension.version.clone(),
                    rollback_version: None,
                    pending_version: None,
                    activation_error: None,
                    active_artifact: None,
                    rollback_artifact: None,
                    repository_overrides: BTreeMap::new(),
                });
            if state.data_id.is_empty() {
                state.data_id = extension.id.clone();
            }
            if matches!(&extension.package, ExtensionPackage::Bundled { .. }) {
                state.active_version = extension.version.clone();
                state.rollback_version = None;
                state.pending_version = None;
                state.active_artifact = None;
                state.rollback_artifact = None;
            }
            if state.installed && seen.insert(extension.repository.clone()) {
                self.load_order.push(extension.repository.clone());
            }
        }
    }

    fn validate_data_ids(&self) -> Result<(), String> {
        let mut data_ids = BTreeSet::new();
        for (repository, state) in &self.entries {
            if !crate::extension_state::valid_namespace(&state.data_id) {
                return Err(format!(
                    "extension '{repository}' has an invalid data namespace"
                ));
            }
            if !data_ids.insert(state.data_id.as_str()) {
                return Err(format!(
                    "extension state contains duplicate data namespace '{}'",
                    state.data_id
                ));
            }
        }
        Ok(())
    }

    pub(crate) fn install(&mut self, extension: &CuratedExtension) {
        let state = self
            .entries
            .entry(extension.repository.clone())
            .or_insert_with(|| MarketplaceInstallState {
                data_id: extension.id.clone(),
                installed: false,
                enabled: false,
                active_version: extension.version.clone(),
                rollback_version: None,
                pending_version: None,
                activation_error: None,
                active_artifact: None,
                rollback_artifact: None,
                repository_overrides: BTreeMap::new(),
            });
        state.installed = true;
        state.enabled = true;
        state.active_version = extension.version.clone();
        state.activation_error = None;
        if matches!(&extension.package, ExtensionPackage::Bundled { .. }) {
            state.rollback_version = None;
            state.pending_version = None;
            state.active_artifact = None;
            state.rollback_artifact = None;
        }
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
        state.pending_version = None;
        state.rollback_version = None;
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
        let repository_id = repository_id.trim();
        if repository_id.is_empty() || repository_id.len() > 2_048 {
            return Err("repository id must contain 1 to 2048 bytes".into());
        }
        let repository_id = repository_id.to_owned();
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

    pub(crate) fn migrate_repository_override(
        &mut self,
        previous_identity: &str,
        current_identity: &str,
    ) -> bool {
        let mut changed = false;
        for state in self.entries.values_mut() {
            let Some(previous) = state.repository_overrides.remove(previous_identity) else {
                continue;
            };
            state
                .repository_overrides
                .entry(current_identity.to_owned())
                .or_insert(previous);
            changed = true;
        }
        changed
    }

    pub(crate) fn clear_retained_data(&mut self, repository: &str) -> Result<(), String> {
        let state = self
            .entries
            .get_mut(repository)
            .ok_or_else(|| format!("unknown extension repository '{repository}'"))?;
        if state.installed {
            return Err("remove the extension before deleting its data".into());
        }
        state.repository_overrides.clear();
        state.activation_error = None;
        state.pending_version = None;
        state.rollback_version = None;
        state.active_artifact = None;
        state.rollback_artifact = None;
        Ok(())
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
}

pub(crate) fn state_path() -> PathBuf {
    crate::config::config_dir().join("extensions.json")
}

pub(crate) fn state_storage_path() -> Result<PathBuf, String> {
    crate::platform::resolve_write_target(&state_path())
        .map_err(|error| format!("resolve extension state path: {error}"))
}

pub(crate) struct ExtensionStateLock {
    _file: std::fs::File,
    path: PathBuf,
}

impl ExtensionStateLock {
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

pub(crate) fn lock_state_file(path: &Path) -> Result<ExtensionStateLock, String> {
    lock_state_file_inner(path, false)?.ok_or_else(|| "extension state lock is busy".to_string())
}

pub(crate) fn try_lock_state_file(path: &Path) -> Result<Option<ExtensionStateLock>, String> {
    lock_state_file_inner(path, true)
}

fn lock_state_file_inner(
    path: &Path,
    nonblocking: bool,
) -> Result<Option<ExtensionStateLock>, String> {
    let lock_path = path.with_extension("json.lock");
    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("create extension state directory: {error}"))?;
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(lock_path)
        .map_err(|error| format!("open extension state lock: {error}"))?;
    if nonblocking {
        match file.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => return Ok(None),
            Err(std::fs::TryLockError::Error(error)) => {
                return Err(format!("lock extension state: {error}"));
            }
        }
    } else {
        file.lock()
            .map_err(|error| format!("lock extension state: {error}"))?;
    }
    Ok(Some(ExtensionStateLock {
        _file: file,
        path: path.to_path_buf(),
    }))
}

pub(crate) fn load_state(index: &CuratedIndex) -> MarketplaceStateFile {
    let configured_path = state_path();
    let loaded = (|| {
        let path = state_storage_path()?;
        let _guard = lock_state_file(&path)?;
        load_state_from_path(&path, index)
    })();
    match loaded {
        Ok(state) => state,
        Err(error) => {
            tracing::warn!(path = %configured_path.display(), %error, "failed to load extension state; disabling extensions");
            let mut state = MarketplaceStateFile::defaults(index);
            for entry in state.entries.values_mut() {
                entry.enabled = false;
                entry.activation_error = Some("extension state is unreadable".into());
            }
            state
        }
    }
}

pub(crate) fn bundled_provider_priorities(
    index: &CuratedIndex,
    state: &MarketplaceStateFile,
) -> BTreeMap<String, i32> {
    state
        .load_order
        .iter()
        .enumerate()
        .filter_map(|(position, repository)| {
            let extension = index
                .extensions
                .iter()
                .find(|extension| &extension.repository == repository)?;
            let ExtensionPackage::Bundled { provider } = &extension.package else {
                return None;
            };
            let rank = i32::try_from(position.saturating_add(1))
                .unwrap_or(i32::MAX / 1024)
                .min(i32::MAX / 1024);
            Some((format!("vqro.{provider}"), rank.saturating_mul(1024)))
        })
        .collect()
}

pub(crate) fn unavailable_bundled_providers(
    index: &CuratedIndex,
    state: &MarketplaceStateFile,
) -> BTreeSet<String> {
    index
        .extensions
        .iter()
        .filter_map(|extension| match &extension.package {
            ExtensionPackage::Bundled { provider }
                if !state
                    .entries
                    .get(&extension.repository)
                    .is_some_and(|entry| entry.installed && entry.enabled) =>
            {
                Some(format!("vqro.{provider}"))
            }
            ExtensionPackage::Bundled { .. }
            | ExtensionPackage::AgentIntegration { .. }
            | ExtensionPackage::GithubPlugin { .. } => None,
        })
        .collect()
}

fn bounded_read(path: &Path, maximum: u64, label: &str) -> Result<Vec<u8>, String> {
    let metadata = std::fs::metadata(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            format!("{label} not found")
        } else {
            format!("inspect {label}: {error}")
        }
    })?;
    if metadata.len() > maximum {
        return Err(format!("{label} exceeds its size limit"));
    }
    std::fs::read(path).map_err(|error| format!("read {label}: {error}"))
}

pub(crate) fn load_state_from_path(
    path: &Path,
    index: &CuratedIndex,
) -> Result<MarketplaceStateFile, String> {
    if !path.exists() {
        return Ok(MarketplaceStateFile::defaults(index));
    }
    let bytes = bounded_read(path, MAX_MARKETPLACE_STATE_BYTES, "extension state")?;
    let mut state: MarketplaceStateFile = serde_json::from_slice(&bytes)
        .map_err(|error| format!("parse extension state: {error}"))?;
    if state.schema_version != STATE_SCHEMA_VERSION {
        return Err(format!(
            "unsupported extension state schema {}",
            state.schema_version
        ));
    }
    state.normalize(index);
    state.validate_data_ids()?;
    Ok(state)
}

pub(crate) fn save_state_to_path(path: &Path, state: &MarketplaceStateFile) -> Result<(), String> {
    let target = crate::platform::resolve_write_target(path)
        .map_err(|error| format!("resolve extension state path: {error}"))?;
    let path = target.as_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("create extension state directory: {error}"))?;
    }
    let bytes = serde_json::to_vec_pretty(state)
        .map_err(|error| format!("encode extension state: {error}"))?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_MARKETPLACE_STATE_BYTES {
        return Err("extension state exceeds its size limit".into());
    }
    let temporary = path.with_extension(format!(
        "json.{}.{}.tmp",
        std::process::id(),
        NEXT_STATE_TEMP_ID.fetch_add(1, Ordering::Relaxed)
    ));
    let mut file = std::fs::File::create(&temporary)
        .map_err(|error| format!("create extension state: {error}"))?;
    if let Err(error) = file.write_all(&bytes).and_then(|()| file.sync_all()) {
        drop(file);
        let _ = std::fs::remove_file(&temporary);
        return Err(format!("write extension state: {error}"));
    }
    drop(file);
    if let Err(error) = crate::platform::durable_replace_file(&temporary, path) {
        let _ = std::fs::remove_file(&temporary);
        return Err(format!("commit extension state: {error}"));
    }
    Ok(())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct PendingExtensionLifecycle {
    pub(crate) repository: String,
    pub(crate) target: crate::api::schema::IntegrationTarget,
    pub(crate) installed: bool,
    pub(crate) enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) active_artifact: Option<VerifiedExtensionArtifact>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) rollback_artifact: Option<VerifiedExtensionArtifact>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) active_version: Option<String>,
    #[serde(default)]
    pub(crate) load_order: Vec<String>,
}

fn lifecycle_journal_path(state_path: &Path) -> PathBuf {
    state_path.with_extension("lifecycle.json")
}

pub(crate) fn begin_extension_lifecycle(
    state_path: &Path,
    pending: &PendingExtensionLifecycle,
) -> Result<(), String> {
    let bytes = serde_json::to_vec(pending)
        .map_err(|error| format!("encode extension lifecycle journal: {error}"))?;
    let journal_path = lifecycle_journal_path(state_path);
    let temporary = journal_path.with_extension(format!(
        "json.{}.{}.tmp",
        std::process::id(),
        NEXT_STATE_TEMP_ID.fetch_add(1, Ordering::Relaxed)
    ));
    let mut file = std::fs::File::create(&temporary)
        .map_err(|error| format!("create extension lifecycle journal: {error}"))?;
    if let Err(error) = file.write_all(&bytes).and_then(|()| file.sync_all()) {
        drop(file);
        let _ = std::fs::remove_file(&temporary);
        return Err(format!("write extension lifecycle journal: {error}"));
    }
    drop(file);
    if let Err(error) = crate::platform::durable_replace_file(&temporary, &journal_path) {
        let _ = std::fs::remove_file(&temporary);
        return Err(format!("commit extension lifecycle journal: {error}"));
    }
    Ok(())
}

pub(crate) fn pending_extension_lifecycle(
    state_path: &Path,
) -> Result<Option<PendingExtensionLifecycle>, String> {
    match bounded_read(
        &lifecycle_journal_path(state_path),
        MAX_MARKETPLACE_JOURNAL_BYTES,
        "extension lifecycle journal",
    ) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|error| format!("decode extension lifecycle journal: {error}")),
        Err(error) if error.contains("not found") => Ok(None),
        Err(error) => Err(error),
    }
}

pub(crate) fn finish_extension_lifecycle(state_path: &Path) -> Result<(), String> {
    let path = lifecycle_journal_path(state_path);
    match std::fs::remove_file(&path) {
        Ok(()) => crate::platform::sync_containing_directory(&path)
            .map_err(|error| format!("sync extension lifecycle journal removal: {error}")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("remove extension lifecycle journal: {error}")),
    }
}

#[derive(Serialize, Deserialize)]
struct PendingExtensionDataDeletion {
    repository: String,
    data_id: String,
}

fn data_deletion_journal_path(state_path: &Path) -> PathBuf {
    state_path.with_extension("delete.json")
}

pub(crate) fn begin_extension_data_deletion(
    state_path: &Path,
    repository: &str,
    data_id: &str,
) -> Result<(), String> {
    let bytes = serde_json::to_vec(&PendingExtensionDataDeletion {
        repository: repository.to_owned(),
        data_id: data_id.to_owned(),
    })
    .map_err(|error| format!("encode data deletion journal: {error}"))?;
    let journal_path = data_deletion_journal_path(state_path);
    let temporary = journal_path.with_extension(format!(
        "json.{}.{}.tmp",
        std::process::id(),
        NEXT_STATE_TEMP_ID.fetch_add(1, Ordering::Relaxed)
    ));
    let mut file = std::fs::File::create(&temporary)
        .map_err(|error| format!("create data deletion journal: {error}"))?;
    if let Err(error) = file.write_all(&bytes).and_then(|()| file.sync_all()) {
        drop(file);
        let _ = std::fs::remove_file(&temporary);
        return Err(format!("write data deletion journal: {error}"));
    }
    drop(file);
    if let Err(error) = crate::platform::durable_replace_file(&temporary, &journal_path) {
        let _ = std::fs::remove_file(&temporary);
        return Err(format!("commit data deletion journal: {error}"));
    }
    Ok(())
}

pub(crate) fn purge_extension_data(extension_id: &str) -> Result<(), String> {
    crate::extension_artifact::purge(extension_id)?;
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

pub(crate) fn apply_extension_data_deletion(
    state_path: &Path,
    state: &mut MarketplaceStateFile,
) -> Result<Option<String>, String> {
    let journal_path = data_deletion_journal_path(state_path);
    let bytes = match bounded_read(
        &journal_path,
        MAX_MARKETPLACE_JOURNAL_BYTES,
        "data deletion journal",
    ) {
        Ok(bytes) => bytes,
        Err(error) if error.contains("not found") => return Ok(None),
        Err(error) => return Err(error),
    };
    let pending: PendingExtensionDataDeletion = serde_json::from_slice(&bytes)
        .map_err(|error| format!("decode data deletion journal: {error}"))?;
    if !crate::extension_state::valid_namespace(&pending.data_id) {
        return Err("data deletion journal contains an invalid data namespace".into());
    }
    let expected_data_id = state
        .entries
        .get(&pending.repository)
        .ok_or_else(|| "data deletion journal references an unknown extension".to_string())?
        .data_id
        .as_str();
    if expected_data_id != pending.data_id {
        return Err("data deletion journal does not own the requested data namespace".into());
    }
    purge_extension_data(&pending.data_id)?;
    state.clear_retained_data(&pending.repository)?;
    save_state_to_path(state_path, state)?;
    Ok(Some(pending.data_id))
}

pub(crate) fn finish_extension_data_deletion(state_path: &Path) -> Result<(), String> {
    let journal_path = data_deletion_journal_path(state_path);
    match std::fs::remove_file(&journal_path) {
        Ok(()) => crate::platform::sync_containing_directory(&journal_path)
            .map_err(|error| format!("sync data deletion journal removal: {error}")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("remove data deletion journal: {error}")),
    }
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
    fn bundled_marketplace_entries_match_authoritative_declarations() {
        let index = CuratedIndex::bundled().unwrap();
        let mut declarations = crate::app::bundled_document_service_declarations()
            .unwrap()
            .into_iter()
            .map(|declaration| (declaration.namespace.clone(), declaration))
            .collect::<BTreeMap<_, _>>();

        for extension in &index.extensions {
            let ExtensionPackage::Bundled { provider } = &extension.package else {
                continue;
            };
            let declaration = declarations
                .remove(&extension.id)
                .unwrap_or_else(|| panic!("missing bundled declaration for {}", extension.id));
            assert_eq!(
                declaration.service.command.last().map(String::as_str),
                Some(provider.as_str())
            );
        }
        assert!(declarations.is_empty());
    }

    #[test]
    fn curated_pins_and_bundled_service_declarations_are_exact() {
        use sha2::{Digest, Sha256};

        let index = CuratedIndex::bundled().unwrap();
        let expected = [
            (
                "vqro.collections",
                "f1a585cc65b73c871f490d4977905a0d208816a2",
                "eabc2167a5e1d2a2f3fc786482c7c0ed7862588de082d2b6e09b1a9f6124c766",
                "bc28c51e8056c092502a5fe89da7a063b071e40b60faedbff5d25922f18329f5",
            ),
            (
                "vqro.spaces",
                "5a8b9b9b6329fbaec68d100e88d20bc6aad0a6ca",
                "fc2c8d08a069c9d5611ffb75daa220c367f2fff5467fb57b3755bc62ad285b00",
                "605534056a08b304816a9f5b60f774c5d9ef8833ae0a1b796a90768199997ef5",
            ),
            (
                "vqro.delegation",
                "cc8d261cbbb65cd07b070f42b79bdefca500f98a",
                "1d43481a5e8bc3fc78244d9945e931b69054c88de3247ce89f2d3a18fb083b65",
                "6e76acb6f489a8c62d393bedd7dda27b6b9732f9bbebfdd5bbae38b37ec5c416",
            ),
            (
                "vqro.agent-status",
                "71c8faecedb7ac8079704a8250c2c0d737459dca",
                "58b5af316078d0bcef9e6a5462f1aafa7e192dd7b52e770508dec1fe04b1f2e7",
                "4695727a8cfa4cc12856a47cec0621f792e3c5e36bd6d8395ac1c9b212518153",
            ),
            (
                "vqro.pi",
                "fa791b57138f163c4f4538f6545f139b378ef188",
                "a631b0e815fbf3a2b457fd53d4d752fa2a813916175473365d4442e9644ed94e",
                "dc3826d1ec045e626af9cbafd501606fff416965a9622a2c13f5dc47516e577a",
            ),
            (
                "vqro.claude",
                "535ede550c4bf3b97830265b9abca298249617dd",
                "acfc2c07463b1fc3624dea66c02aabcab7ed6a925e5a2e4c41a1290b4c64465b",
                "0d9017f7477e86085fc7d2fddb0e0abd3053597a8b0dc39284179a511b86c2ba",
            ),
            (
                "vqro.codex",
                "4eb9623ffe13c3d9b2115f17225713078ab40a5c",
                "1acdf92f4c67541cff0ed7e6839bb1d086ff944d06c555e2593c82b5dee2abeb",
                "fc92cadf0c2e71f13965c0cb36f97d6f5365c19cbc25884762595e0b3929228d",
            ),
        ];
        for (id, commit, source_digest, manifest_digest) in expected {
            let extension = index.extension(id).unwrap();
            assert_eq!(extension.source_commit.as_deref(), Some(commit));
            assert_eq!(extension.source_sha256.as_deref(), Some(source_digest));
            assert_eq!(extension.manifest_sha256.as_deref(), Some(manifest_digest));
            assert_eq!(extension.manifest_path, "vqro-extension.toml");
        }

        for (id, manifest) in [
            (
                "vqro.collections",
                include_bytes!("../extensions/first-party/collections/vqro-extension.toml")
                    .as_slice(),
            ),
            (
                "vqro.spaces",
                include_bytes!("../extensions/first-party/spaces/vqro-extension.toml").as_slice(),
            ),
            (
                "vqro.delegation",
                include_bytes!("../extensions/first-party/delegation/vqro-extension.toml")
                    .as_slice(),
            ),
            (
                "vqro.agent-status",
                include_bytes!("../extensions/first-party/agent-status/vqro-extension.toml")
                    .as_slice(),
            ),
        ] {
            let extension = index.extension(id).unwrap();
            let digest = format!("{:x}", Sha256::digest(manifest));
            assert_eq!(extension.manifest_sha256.as_deref(), Some(digest.as_str()));
        }
    }

    #[test]
    fn removal_retains_overrides_and_reinstall_enables_immediately() {
        let index = CuratedIndex::bundled().unwrap();
        let extension = index.extension("vqro.collections").unwrap();
        let mut state = MarketplaceStateFile::defaults(&index);
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
    fn data_deletion_clears_retained_repository_configuration() {
        let index = CuratedIndex::bundled().unwrap();
        let extension = index.extension("vqro.collections").unwrap();
        let mut state = MarketplaceStateFile::defaults(&index);
        state
            .set_repository_override(&extension.repository, "repo_1".into(), Some(false))
            .unwrap();
        state.remove(&extension.repository).unwrap();

        state.clear_retained_data(&extension.repository).unwrap();

        assert!(state.entries[&extension.repository]
            .repository_overrides
            .is_empty());
    }

    #[test]
    fn globally_disabled_bundled_provider_is_unavailable_to_runtime_catalogs() {
        let index = CuratedIndex::bundled().unwrap();
        let extension = index.extension("vqro.delegation").unwrap();
        let mut state = MarketplaceStateFile::defaults(&index);
        state.set_enabled(&extension.repository, false).unwrap();

        assert!(unavailable_bundled_providers(&index, &state).contains("vqro.delegation"));
    }

    #[test]
    fn repository_override_changes_effective_bundled_runtime_state() {
        let index = CuratedIndex::bundled().unwrap();
        let mut state = MarketplaceStateFile::defaults(&index);
        let collections = index
            .extensions
            .iter()
            .find(|extension| extension.name == "Collections")
            .unwrap();
        state
            .set_repository_override(
                &collections.repository,
                "https://github.com/example/project".into(),
                Some(false),
            )
            .unwrap();

        assert!(state.enabled_for_repository(&collections.repository, None));
        assert!(!state.enabled_for_repository(
            &collections.repository,
            Some("https://github.com/example/project")
        ));
    }

    #[test]
    fn repository_override_identity_migration_preserves_explicit_state() {
        let index = CuratedIndex::bundled().unwrap();
        let extension = index.extension("vqro.collections").unwrap();
        let mut state = MarketplaceStateFile::defaults(&index);
        state
            .set_repository_override(&extension.repository, "local-id".into(), Some(false))
            .unwrap();

        assert!(state.migrate_repository_override("local-id", "https://github.com/example/project"));
        assert_eq!(
            state.entries[&extension.repository]
                .repository_overrides
                .get("https://github.com/example/project"),
            Some(&false)
        );
        assert!(!state.entries[&extension.repository]
            .repository_overrides
            .contains_key("local-id"));
    }

    #[test]
    fn persisted_data_namespaces_must_be_valid_and_unique() {
        let index = CuratedIndex::bundled().unwrap();
        let directory = std::env::temp_dir().join(format!(
            "vqro-extension-data-id-test-{}-{}",
            std::process::id(),
            NEXT_STATE_TEMP_ID.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let state_path = directory.join("extensions.json");
        let mut state = MarketplaceStateFile::defaults(&index);
        let repositories = state.entries.keys().take(2).cloned().collect::<Vec<_>>();
        state.entries.get_mut(&repositories[0]).unwrap().data_id = "../escape".into();
        std::fs::write(&state_path, serde_json::to_vec(&state).unwrap()).unwrap();
        assert!(load_state_from_path(&state_path, &index)
            .unwrap_err()
            .contains("invalid data namespace"));

        let mut state = MarketplaceStateFile::defaults(&index);
        let duplicate = state.entries[&repositories[0]].data_id.clone();
        state.entries.get_mut(&repositories[1]).unwrap().data_id = duplicate;
        std::fs::write(&state_path, serde_json::to_vec(&state).unwrap()).unwrap();
        assert!(load_state_from_path(&state_path, &index)
            .unwrap_err()
            .contains("duplicate data namespace"));
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn data_deletion_journal_cannot_delete_another_extensions_namespace() {
        let index = CuratedIndex::bundled().unwrap();
        let mut state = MarketplaceStateFile::defaults(&index);
        let repositories = state.entries.keys().take(2).cloned().collect::<Vec<_>>();
        let foreign_data_id = state.entries[&repositories[1]].data_id.clone();
        let directory = std::env::temp_dir().join(format!(
            "vqro-extension-delete-owner-test-{}-{}",
            std::process::id(),
            NEXT_STATE_TEMP_ID.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let state_path = directory.join("extensions.json");
        save_state_to_path(&state_path, &state).unwrap();
        begin_extension_data_deletion(&state_path, &repositories[0], &foreign_data_id).unwrap();

        let error = apply_extension_data_deletion(&state_path, &mut state).unwrap_err();
        assert!(error.contains("does not own"));
        assert!(data_deletion_journal_path(&state_path).exists());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn lifecycle_journal_round_trips_and_clears() {
        let directory = std::env::temp_dir().join(format!(
            "vqro-extension-journal-test-{}-{}",
            std::process::id(),
            NEXT_STATE_TEMP_ID.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let state_path = directory.join("extensions.json");
        let pending = PendingExtensionLifecycle {
            repository: "example/agent".into(),
            target: crate::api::schema::IntegrationTarget::Pi,
            installed: true,
            enabled: true,
            active_artifact: None,
            rollback_artifact: None,
            active_version: Some("1.0.0".into()),
            load_order: vec!["example/agent".into()],
        };

        begin_extension_lifecycle(&state_path, &pending).unwrap();
        let loaded = pending_extension_lifecycle(&state_path).unwrap().unwrap();
        assert_eq!(loaded.repository, pending.repository);
        assert_eq!(loaded.target, pending.target);
        assert!(loaded.installed);
        assert!(loaded.enabled);
        assert_eq!(loaded.load_order, pending.load_order);
        finish_extension_lifecycle(&state_path).unwrap();
        assert!(pending_extension_lifecycle(&state_path).unwrap().is_none());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn activation_failure_keeps_package_installed_but_disabled() {
        let index = CuratedIndex::bundled().unwrap();
        let extension = index.extension("vqro.spaces").unwrap();
        let mut state = MarketplaceStateFile::defaults(&index);
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
        let mut state = MarketplaceStateFile::defaults(&index);
        let first = state.load_order[0].clone();
        state.move_to(&first, usize::MAX).unwrap();
        assert_eq!(state.load_order.last(), Some(&first));
        let json = serde_json::to_string(&state).unwrap();
        let restored: MarketplaceStateFile = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.load_order, state.load_order);
    }
}
