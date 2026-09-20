//! Package-v1 artifacts, provenance, registry state, and activation journals.
//!
//! This module is additive. Manifest-v1 and the legacy curated marketplace stay
//! active through explicit compatibility projections. Package-v1 components can
//! be instantiated by the runtime dispatcher, but production capability leases,
//! provider admission, visible publication, and feature authority remain separate.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Component, Path};
use std::sync::atomic::{AtomicU64, Ordering};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const PACKAGE_CONTRACT_V1: &str = "vqro.package.v1";
pub const MANIFEST_VERSION_V2: u32 = 2;
pub const MANIFEST_VERSION_V3: u32 = 3;
pub const PACKAGE_MANIFEST_FILE: &str = "vqro-extension.toml";
pub const PACKAGE_CHECKSUM_FILE: &str = "checksums.sha256";
pub const PACKAGE_SCHEMA_ID_V1: &str = "https://vqro.dev/schema/package/v1";
pub const MANIFEST_SCHEMA_ID_V2: &str = "https://vqro.dev/schema/extension-manifest/v2";
pub const MANIFEST_SCHEMA_ID_V3: &str = "https://vqro.dev/schema/extension-manifest/v3";
pub const SERVICE_CONTRACT_V1: &str = "vqro.service-contract.v1";
pub const ACTIVATION_JOURNAL_CONTRACT_V1: &str = "vqro.package-activation-journal.v1";
pub const PACKAGE_REGISTRY_CONTRACT_V1: &str = "vqro.package-registry.v1";

pub const MAX_PACKAGE_BYTES: u64 = 8 * 1024 * 1024;
pub const MAX_EXPANDED_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_PACKAGE_ENTRIES: usize = 10_000;
pub const MAX_PACKAGE_FILE_BYTES: u64 = 8 * 1024 * 1024;
pub const MAX_MANIFEST_BYTES: usize = 256 * 1024;
pub const MAX_CHECKSUM_BYTES: usize = 1024 * 1024;
pub const MAX_SERVICES: usize = 32;
pub const MAX_NAMESPACES: usize = 64;
pub const MAX_CAPABILITIES: usize = 64;
pub const MAX_PROCESS_ARGS: usize = 64;
pub const MAX_PROCESS_ARG_BYTES: usize = 4 * 1024;
pub const MAX_PROCESS_ARGS_BYTES: usize = 32 * 1024;
pub const MAX_PATH_BYTES: usize = 512;
pub const MAX_PATH_COMPONENT_BYTES: usize = 128;
pub const MAX_PATH_DEPTH: usize = 16;
pub const MAX_ACTIVATION_JOURNAL_BYTES: u64 = 256 * 1024;
pub const MAX_INSTALLED_PACKAGES: usize = 1_024;
pub const MAX_SERVICE_ENDPOINTS: usize = 32;
pub const MAX_CONTRACT_DESCRIPTOR_BYTES: usize = 64 * 1024;
pub const MAX_CONTRACT_METHODS: usize = 32;
pub const MAX_INFORMATION_CATEGORIES: usize = 32;
// Leaves deterministic room for both runtime envelopes inside the 256 KiB frame cap.
pub const MAX_SERVICE_CALL_BYTES: u64 = 128 * 1024;

static NEXT_PACKAGE_TEMP: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PackageManifestV2 {
    pub manifest_version: u32,
    pub package_contract: String,
    pub id: String,
    pub version: String,
    pub min_vqro_version: String,
    #[serde(default)]
    pub namespaces: Vec<String>,
    #[serde(default)]
    pub services: Vec<PackageServiceV1>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PackageServiceV1 {
    pub id: String,
    #[serde(default)]
    pub provides: Vec<String>,
    #[serde(default)]
    pub capabilities: Vec<String>,
    pub runtime: PackageRuntimeV1,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PackageManifestV3 {
    pub manifest_version: u32,
    pub package_contract: String,
    pub id: String,
    pub version: String,
    pub min_vqro_version: String,
    #[serde(default)]
    pub namespaces: Vec<String>,
    #[serde(default)]
    pub services: Vec<PackageServiceV2>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PackageServiceV2 {
    pub id: String,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub exports: Vec<ServiceContractExportV1>,
    #[serde(default)]
    pub imports: Vec<ServiceContractImportV1>,
    pub runtime: PackageRuntimeV1,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ServiceContractExportV1 {
    pub id: String,
    pub descriptor: String,
    pub descriptor_sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ServiceContractImportV1 {
    pub id: String,
    pub descriptor: String,
    pub descriptor_sha256: String,
    #[serde(default = "default_required_contract")]
    pub required: bool,
}

fn default_required_contract() -> bool {
    true
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ServiceContractDescriptorV1 {
    pub contract: String,
    pub contract_id: String,
    pub contract_version: String,
    pub methods: Vec<ServiceContractMethodV1>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ServiceContractMethodV1 {
    pub id: String,
    pub request_schema: serde_json::Value,
    pub response_schema: serde_json::Value,
    #[serde(default)]
    pub request_information: Vec<String>,
    #[serde(default)]
    pub response_information: Vec<String>,
    pub max_request_bytes: u64,
    pub max_response_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ValidatedPackageManifestV1 {
    pub manifest_version: u32,
    pub package_contract: String,
    pub id: String,
    pub version: String,
    pub min_vqro_version: String,
    pub namespaces: Vec<String>,
    pub services: Vec<ValidatedPackageServiceV1>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ValidatedPackageServiceV1 {
    pub id: String,
    #[serde(default)]
    pub provides: Vec<String>,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub exports: Vec<ServiceContractExportV1>,
    #[serde(default)]
    pub imports: Vec<ServiceContractImportV1>,
    pub runtime: PackageRuntimeV1,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PackageRuntimeV1 {
    Component {
        component: String,
        world: String,
    },
    Process {
        executables: BTreeMap<String, String>,
        #[serde(default)]
        args: Vec<String>,
    },
}

impl From<PackageManifestV2> for ValidatedPackageManifestV1 {
    fn from(manifest: PackageManifestV2) -> Self {
        Self {
            manifest_version: manifest.manifest_version,
            package_contract: manifest.package_contract,
            id: manifest.id,
            version: manifest.version,
            min_vqro_version: manifest.min_vqro_version,
            namespaces: manifest.namespaces,
            services: manifest
                .services
                .into_iter()
                .map(|service| ValidatedPackageServiceV1 {
                    id: service.id,
                    provides: service.provides,
                    capabilities: service.capabilities,
                    exports: Vec::new(),
                    imports: Vec::new(),
                    runtime: service.runtime,
                })
                .collect(),
        }
    }
}

impl From<PackageManifestV3> for ValidatedPackageManifestV1 {
    fn from(manifest: PackageManifestV3) -> Self {
        Self {
            manifest_version: manifest.manifest_version,
            package_contract: manifest.package_contract,
            id: manifest.id,
            version: manifest.version,
            min_vqro_version: manifest.min_vqro_version,
            namespaces: manifest.namespaces,
            services: manifest
                .services
                .into_iter()
                .map(|service| ValidatedPackageServiceV1 {
                    id: service.id,
                    provides: Vec::new(),
                    capabilities: service.capabilities,
                    exports: service.exports,
                    imports: service.imports,
                    runtime: service.runtime,
                })
                .collect(),
        }
    }
}

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum PackageTargetV1 {
    LinuxX86_64,
    LinuxAarch64,
    MacosX86_64,
    MacosAarch64,
    WindowsX86_64,
}

impl PackageTargetV1 {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LinuxX86_64 => "linux-x86_64",
            Self::LinuxAarch64 => "linux-aarch64",
            Self::MacosX86_64 => "macos-x86_64",
            Self::MacosAarch64 => "macos-aarch64",
            Self::WindowsX86_64 => "windows-x86_64",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PackageProvenanceV1 {
    Curated {
        catalog: String,
        publisher: String,
        repository: String,
        package_id: String,
        version: String,
        source_commit: String,
        package_sha256: String,
        manifest_sha256: String,
        #[serde(default)]
        authorized_namespaces: Vec<String>,
    },
    File {
        package_sha256: String,
    },
    Git {
        url: String,
        source_commit: String,
        package_path: String,
        package_id: String,
        version: String,
        package_sha256: String,
        manifest_sha256: String,
    },
    Link {
        canonical_path: String,
        trust_id: String,
        trusted: bool,
        package_id: String,
        version: String,
        package_sha256: String,
        manifest_sha256: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PackageFileV1 {
    pub path: String,
    pub sha256: String,
    pub size: u64,
    pub executable: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ValidatedPackageV1 {
    pub contract: String,
    pub package_sha256: String,
    pub manifest_sha256: String,
    pub manifest: ValidatedPackageManifestV1,
    #[serde(default)]
    pub contract_descriptors: BTreeMap<String, ServiceContractDescriptorV1>,
    pub files: Vec<PackageFileV1>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ResolvedPackageRuntimeV1 {
    pub service_id: String,
    pub kind: String,
    pub payload_path: String,
    pub payload_sha256: String,
    pub args: Vec<String>,
    pub world: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct InstalledPackageV1 {
    pub package_id: String,
    pub version: String,
    pub package_sha256: String,
    pub manifest_sha256: String,
    pub immutable_root: String,
    pub provenance: PackageProvenanceV1,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LinkedPackageV1 {
    pub package_id: String,
    pub version: String,
    pub manifest_sha256: String,
    pub namespaces: Vec<String>,
    pub inventory_sha256: String,
    pub canonical_root: String,
    pub provenance: PackageProvenanceV1,
}

#[derive(Clone, Debug)]
struct PackageEntry {
    path: String,
    bytes: Vec<u8>,
    mode: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidatedPackageContents {
    pub descriptor: ValidatedPackageV1,
    files: BTreeMap<String, (Vec<u8>, u32)>,
}

impl ValidatedPackageContents {
    pub fn file(&self, path: &str) -> Option<&[u8]> {
        self.files.get(path).map(|(bytes, _)| bytes.as_slice())
    }
}

pub fn validate_package_file(
    path: &Path,
    expected_package_sha256: Option<&str>,
) -> Result<ValidatedPackageContents, String> {
    let file = crate::platform::read_bounded_regular_nofollow(path, MAX_PACKAGE_BYTES)
        .map_err(|error| format!("read bounded package file: {error}"))?;
    validate_package_bytes(&file.bytes, expected_package_sha256)
}

pub fn validate_package_bytes(
    bytes: &[u8],
    expected_package_sha256: Option<&str>,
) -> Result<ValidatedPackageContents, String> {
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_PACKAGE_BYTES {
        return Err("package archive exceeds 8 MiB".into());
    }
    let package_sha256 = sha256(bytes);
    if let Some(expected) = expected_package_sha256 {
        validate_digest(expected, "expected package")?;
        if !package_sha256.eq_ignore_ascii_case(expected) {
            return Err("package digest mismatch".into());
        }
    }

    let mut archive = tar::Archive::new(bytes);
    let mut entries = Vec::new();
    let mut keys = BTreeSet::new();
    let mut previous_path: Option<String> = None;
    let mut expanded = 0_u64;
    for (index, entry) in archive
        .entries()
        .map_err(|error| format!("read package archive: {error}"))?
        .enumerate()
    {
        if index >= MAX_PACKAGE_ENTRIES {
            return Err("package archive contains too many entries".into());
        }
        let mut entry = entry.map_err(|error| format!("read package entry: {error}"))?;
        if !entry.header().entry_type().is_file() {
            return Err("package archive contains a link, directory, or special file".into());
        }
        let path = entry
            .path()
            .map_err(|error| format!("read package path: {error}"))?
            .into_owned();
        let path = portable_package_path(&path)?;
        if previous_path
            .as_deref()
            .is_some_and(|previous| previous >= path.as_str())
        {
            return Err("package archive entries are not in strict bytewise order".into());
        }
        previous_path = Some(path.clone());
        let key = path.to_ascii_lowercase();
        if !keys.insert(key) {
            return Err("package archive contains a duplicate or case-colliding path".into());
        }
        for existing in entries
            .iter()
            .map(|entry: &PackageEntry| entry.path.as_str())
        {
            if path.starts_with(&format!("{existing}/"))
                || existing.starts_with(&format!("{path}/"))
            {
                return Err("package archive contains a file/descendant collision".into());
            }
        }
        let size = entry
            .header()
            .size()
            .map_err(|error| format!("read package entry size: {error}"))?;
        if size > MAX_PACKAGE_FILE_BYTES {
            return Err("package file exceeds 8 MiB".into());
        }
        expanded = expanded
            .checked_add(size)
            .ok_or_else(|| "package expanded size overflow".to_string())?;
        if expanded > MAX_EXPANDED_BYTES {
            return Err("package expanded contents exceed 64 MiB".into());
        }
        let mtime = entry
            .header()
            .mtime()
            .map_err(|error| format!("read package mtime: {error}"))?;
        let uid = entry
            .header()
            .uid()
            .map_err(|error| format!("read package uid: {error}"))?;
        let gid = entry
            .header()
            .gid()
            .map_err(|error| format!("read package gid: {error}"))?;
        let mode = entry
            .header()
            .mode()
            .map_err(|error| format!("read package mode: {error}"))?;
        if mtime != 0 || uid != 0 || gid != 0 || !matches!(mode, 0o644 | 0o755) {
            return Err("package archive is not in the deterministic tar profile".into());
        }
        let mut contents = Vec::with_capacity(size as usize);
        entry
            .read_to_end(&mut contents)
            .map_err(|error| format!("read package contents: {error}"))?;
        if contents.len() as u64 != size {
            return Err("package entry size does not match streamed contents".into());
        }
        entries.push(PackageEntry {
            path,
            bytes: contents,
            mode,
        });
    }

    if deterministic_archive(&entries)? != bytes {
        return Err("package archive bytes are not canonical deterministic tar".into());
    }

    let manifest_entry = required_entry(&entries, PACKAGE_MANIFEST_FILE)?;
    let checksum_entry = required_entry(&entries, PACKAGE_CHECKSUM_FILE)?;
    if manifest_entry.bytes.len() > MAX_MANIFEST_BYTES {
        return Err("package manifest exceeds 256 KiB".into());
    }
    if checksum_entry.bytes.len() > MAX_CHECKSUM_BYTES {
        return Err("package checksum manifest exceeds 1 MiB".into());
    }
    let checksums = parse_checksums(&checksum_entry.bytes)?;
    let expected_paths = entries
        .iter()
        .filter(|entry| entry.path != PACKAGE_CHECKSUM_FILE)
        .map(|entry| entry.path.as_str())
        .collect::<Vec<_>>();
    if checksums.keys().map(String::as_str).collect::<Vec<_>>() != expected_paths {
        return Err("package checksum inventory does not exactly match archive files".into());
    }
    for entry in entries
        .iter()
        .filter(|entry| entry.path != PACKAGE_CHECKSUM_FILE)
    {
        if checksums[&entry.path] != sha256(&entry.bytes) {
            return Err(format!("package checksum mismatch for {}", entry.path));
        }
    }

    let manifest_text = std::str::from_utf8(&manifest_entry.bytes)
        .map_err(|error| format!("package manifest is not UTF-8: {error}"))?;
    let (manifest, contract_descriptors) = parse_and_validate_manifest(manifest_text, &entries)?;
    let manifest_sha256 = sha256(&manifest_entry.bytes);
    let file_descriptors = entries
        .iter()
        .filter(|entry| entry.path != PACKAGE_CHECKSUM_FILE)
        .map(|entry| PackageFileV1 {
            path: entry.path.clone(),
            sha256: sha256(&entry.bytes),
            size: entry.bytes.len() as u64,
            executable: entry.mode & 0o111 != 0,
        })
        .collect::<Vec<_>>();
    let files = entries
        .into_iter()
        .map(|entry| (entry.path, (entry.bytes, entry.mode)))
        .collect();
    let validated = ValidatedPackageContents {
        descriptor: ValidatedPackageV1 {
            contract: PACKAGE_CONTRACT_V1.into(),
            package_sha256,
            manifest_sha256,
            manifest,
            contract_descriptors,
            files: file_descriptors,
        },
        files,
    };
    validate_runtime_inventory(&validated)?;
    Ok(validated)
}

#[derive(Deserialize)]
struct ManifestVersionProbe {
    manifest_version: u32,
}

fn parse_and_validate_manifest(
    manifest_text: &str,
    entries: &[PackageEntry],
) -> Result<
    (
        ValidatedPackageManifestV1,
        BTreeMap<String, ServiceContractDescriptorV1>,
    ),
    String,
> {
    let probe: ManifestVersionProbe = toml::from_str(manifest_text)
        .map_err(|error| format!("parse package manifest version: {error}"))?;
    match probe.manifest_version {
        MANIFEST_VERSION_V2 => {
            let manifest: PackageManifestV2 = toml::from_str(manifest_text)
                .map_err(|error| format!("parse package manifest v2: {error}"))?;
            validate_manifest(&manifest)?;
            Ok((manifest.into(), BTreeMap::new()))
        }
        MANIFEST_VERSION_V3 => {
            let manifest: PackageManifestV3 = toml::from_str(manifest_text)
                .map_err(|error| format!("parse package manifest v3: {error}"))?;
            validate_manifest_v3(&manifest)?;
            let descriptors = validate_contract_inventory(&manifest, entries)?;
            Ok((manifest.into(), descriptors))
        }
        version => Err(format!("unsupported package manifest version {version}")),
    }
}

fn validate_contract_inventory(
    manifest: &PackageManifestV3,
    entries: &[PackageEntry],
) -> Result<BTreeMap<String, ServiceContractDescriptorV1>, String> {
    let mut descriptors = BTreeMap::new();
    for declaration in manifest.services.iter().flat_map(|service| {
        service
            .exports
            .iter()
            .map(|export| (&export.descriptor, &export.descriptor_sha256))
            .chain(
                service
                    .imports
                    .iter()
                    .map(|import| (&import.descriptor, &import.descriptor_sha256)),
            )
    }) {
        let (path, expected_digest) = declaration;
        validate_contract_descriptor_path(path)?;
        if !path.ends_with(".json") {
            return Err("service contract descriptor must be a JSON file".into());
        }
        validate_digest(expected_digest, "service contract descriptor")?;
        let entry = required_entry(entries, path)?;
        if entry.bytes.len() > MAX_CONTRACT_DESCRIPTOR_BYTES {
            return Err("service contract descriptor exceeds 64 KiB".into());
        }
        let (descriptor, digest) = parse_service_contract_descriptor(&entry.bytes)?;
        if digest != *expected_digest {
            return Err(format!(
                "service contract descriptor digest mismatch for {path}"
            ));
        }
        if let Some(previous) = descriptors.insert(digest, descriptor.clone()) {
            if previous != descriptor {
                return Err("service contract descriptor digest collision".into());
            }
        }
    }
    for service in &manifest.services {
        let mut exported_methods = BTreeSet::new();
        for export in &service.exports {
            let descriptor = descriptors
                .get(&export.descriptor_sha256)
                .ok_or_else(|| "service export descriptor is unavailable".to_string())?;
            for method in &descriptor.methods {
                if !exported_methods.insert(&method.id) {
                    return Err(format!(
                        "service {} exports ambiguous method {} through multiple contracts",
                        service.id, method.id
                    ));
                }
            }
        }
    }
    Ok(descriptors)
}

pub fn parse_service_contract_descriptor(
    bytes: &[u8],
) -> Result<(ServiceContractDescriptorV1, String), String> {
    if bytes.len() > MAX_CONTRACT_DESCRIPTOR_BYTES {
        return Err("service contract descriptor exceeds 64 KiB".into());
    }
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    let unique = UniqueJsonValue::deserialize(&mut deserializer)
        .map_err(|error| format!("parse service contract descriptor: {error}"))?;
    deserializer
        .end()
        .map_err(|error| format!("parse service contract descriptor: {error}"))?;
    let descriptor: ServiceContractDescriptorV1 = serde_json::from_value(unique.0.clone())
        .map_err(|error| format!("decode service contract descriptor: {error}"))?;
    validate_service_contract_descriptor(&descriptor)?;
    let normalized = serde_json::to_value(&descriptor)
        .map_err(|error| format!("normalize service contract descriptor: {error}"))?;
    let mut canonical = Vec::new();
    write_canonical_json(&normalized, &mut canonical)?;
    Ok((descriptor, sha256(&canonical)))
}

fn validate_service_contract_descriptor(
    descriptor: &ServiceContractDescriptorV1,
) -> Result<(), String> {
    if descriptor.contract != SERVICE_CONTRACT_V1 {
        return Err("service contract descriptor has an unsupported contract".into());
    }
    validate_package_id(&descriptor.contract_id)?;
    validate_version(&descriptor.contract_version, "service contract version")?;
    if descriptor.methods.is_empty() || descriptor.methods.len() > MAX_CONTRACT_METHODS {
        return Err("service contract descriptor has an invalid method count".into());
    }
    let mut previous_method: Option<&str> = None;
    for method in &descriptor.methods {
        validate_identifier(&method.id, 128, "service contract method")?;
        if previous_method.is_some_and(|previous| previous >= method.id.as_str()) {
            return Err("service contract methods must be in strict bytewise order".into());
        }
        previous_method = Some(&method.id);
        validate_contract_schema(&method.request_schema, "request")?;
        validate_contract_schema(&method.response_schema, "response")?;
        validate_information_categories(&method.request_information)?;
        validate_information_categories(&method.response_information)?;
        if method.max_request_bytes == 0
            || method.max_response_bytes == 0
            || method.max_request_bytes > MAX_SERVICE_CALL_BYTES
            || method.max_response_bytes > MAX_SERVICE_CALL_BYTES
        {
            return Err("service contract method byte bound is invalid".into());
        }
    }
    Ok(())
}

fn validate_information_categories(values: &[String]) -> Result<(), String> {
    if values.len() > MAX_INFORMATION_CATEGORIES {
        return Err("service contract method has too many information categories".into());
    }
    let mut previous: Option<&str> = None;
    for value in values {
        validate_package_id(value)?;
        if previous.is_some_and(|previous| previous >= value.as_str()) {
            return Err("information categories must be in strict bytewise order".into());
        }
        previous = Some(value);
    }
    Ok(())
}

fn validate_contract_schema(value: &serde_json::Value, label: &str) -> Result<(), String> {
    if !value.is_object() {
        return Err(format!("service contract {label} schema must be an object"));
    }
    fn visit(value: &serde_json::Value, depth: usize, nodes: &mut usize) -> Result<(), String> {
        *nodes = nodes.saturating_add(1);
        if depth > 32 || *nodes > 4_096 {
            return Err("service contract schema exceeds structural limits".into());
        }
        match value {
            serde_json::Value::Array(values) => {
                for value in values {
                    visit(value, depth + 1, nodes)?;
                }
            }
            serde_json::Value::Object(values) => {
                for value in values.values() {
                    visit(value, depth + 1, nodes)?;
                }
            }
            serde_json::Value::Number(number) if !number.is_i64() && !number.is_u64() => {
                return Err("service contract schema cannot contain floating-point numbers".into());
            }
            _ => {}
        }
        Ok(())
    }
    visit(value, 0, &mut 0)?;
    validate_contract_schema_subset(value, label)
}

fn validate_contract_schema_subset(value: &serde_json::Value, path: &str) -> Result<(), String> {
    let object = value
        .as_object()
        .ok_or_else(|| format!("service contract schema {path} must be an object"))?;
    for (key, value) in object {
        match key.as_str() {
            "$schema" => {
                if value.as_str() != Some("https://json-schema.org/draft/2020-12/schema") {
                    return Err(format!(
                        "service contract schema {path} has unsupported $schema"
                    ));
                }
            }
            "$id" | "title" | "description" => {
                if value.as_str().is_none_or(|value| value.len() > 4_096) {
                    return Err(format!(
                        "service contract schema {path}.{key} must be a bounded string"
                    ));
                }
            }
            "$ref" => {
                if value
                    .as_str()
                    .is_none_or(|value| !value.starts_with("#/$defs/") || value.len() > 512)
                {
                    return Err(format!(
                        "service contract schema {path} has a non-local $ref"
                    ));
                }
            }
            "type" => {
                if !matches!(
                    value.as_str(),
                    Some("null" | "boolean" | "object" | "array" | "number" | "integer" | "string")
                ) {
                    return Err(format!("service contract schema {path}.type is invalid"));
                }
            }
            "$defs" | "properties" => {
                let children = value.as_object().ok_or_else(|| {
                    format!("service contract schema {path}.{key} must be an object")
                })?;
                for (name, schema) in children {
                    if name.is_empty() || name.len() > 128 {
                        return Err(format!(
                            "service contract schema {path}.{key} has an invalid name"
                        ));
                    }
                    validate_contract_schema_subset(schema, &format!("{path}.{key}.{name}"))?;
                }
            }
            "required" => {
                let names = value.as_array().ok_or_else(|| {
                    format!("service contract schema {path}.required must be an array")
                })?;
                if names.len() > 256 {
                    return Err(format!(
                        "service contract schema {path}.required is too large"
                    ));
                }
                let mut unique = std::collections::BTreeSet::new();
                for name in names {
                    let name = name
                        .as_str()
                        .filter(|name| !name.is_empty())
                        .ok_or_else(|| {
                            format!("service contract schema {path}.required must contain strings")
                        })?;
                    if !unique.insert(name) {
                        return Err(format!(
                            "service contract schema {path}.required has duplicates"
                        ));
                    }
                }
            }
            "additionalProperties" => {
                if !value.is_boolean() {
                    validate_contract_schema_subset(
                        value,
                        &format!("{path}.additionalProperties"),
                    )?;
                }
            }
            "items" | "not" => {
                validate_contract_schema_subset(value, &format!("{path}.{key}"))?;
            }
            "oneOf" | "anyOf" | "allOf" => {
                let schemas = value
                    .as_array()
                    .filter(|values| !values.is_empty())
                    .ok_or_else(|| {
                        format!("service contract schema {path}.{key} must be a non-empty array")
                    })?;
                for (index, schema) in schemas.iter().enumerate() {
                    validate_contract_schema_subset(schema, &format!("{path}.{key}[{index}]"))?;
                }
            }
            "minItems" | "maxItems" | "minLength" | "maxLength" => {
                if value.as_u64().is_none() {
                    return Err(format!(
                        "service contract schema {path}.{key} must be an unsigned integer"
                    ));
                }
            }
            "minimum" | "maximum" => {
                if value.as_i64().is_none() && value.as_u64().is_none() {
                    return Err(format!(
                        "service contract schema {path}.{key} must be an integer"
                    ));
                }
            }
            "pattern" => {
                let pattern = value.as_str().ok_or_else(|| {
                    format!("service contract schema {path}.pattern must be a string")
                })?;
                regex::Regex::new(pattern)
                    .map_err(|_| format!("service contract schema {path}.pattern is invalid"))?;
            }
            "format" => {
                if !matches!(
                    value.as_str(),
                    Some(
                        "date-time"
                            | "date"
                            | "time"
                            | "email"
                            | "hostname"
                            | "ipv4"
                            | "ipv6"
                            | "uri"
                            | "uuid"
                    )
                ) {
                    return Err(format!(
                        "service contract schema {path}.format is unsupported"
                    ));
                }
            }
            "enum" => {
                let values = value
                    .as_array()
                    .filter(|values| !values.is_empty())
                    .ok_or_else(|| {
                        format!("service contract schema {path}.enum must be a non-empty array")
                    })?;
                let mut unique = std::collections::BTreeSet::new();
                for value in values {
                    let mut encoded = Vec::new();
                    write_canonical_json(value, &mut encoded)?;
                    if !unique.insert(encoded) {
                        return Err(format!(
                            "service contract schema {path}.enum has duplicates"
                        ));
                    }
                }
            }
            "const" | "default" => {}
            _ => {
                return Err(format!(
                    "service contract schema {path} uses unsupported keyword {key}"
                ));
            }
        }
    }
    for (minimum, maximum) in [
        ("minItems", "maxItems"),
        ("minLength", "maxLength"),
        ("minimum", "maximum"),
    ] {
        if let (Some(minimum), Some(maximum)) = (object.get(minimum), object.get(maximum)) {
            let ordered = minimum
                .as_i64()
                .zip(maximum.as_i64())
                .is_some_and(|(minimum, maximum)| minimum <= maximum)
                || minimum
                    .as_u64()
                    .zip(maximum.as_u64())
                    .is_some_and(|(minimum, maximum)| minimum <= maximum);
            if !ordered {
                return Err(format!(
                    "service contract schema {path} has inverted bounds"
                ));
            }
        }
    }
    Ok(())
}

fn write_canonical_json(value: &serde_json::Value, output: &mut Vec<u8>) -> Result<(), String> {
    match value {
        serde_json::Value::Null => output.extend_from_slice(b"null"),
        serde_json::Value::Bool(true) => output.extend_from_slice(b"true"),
        serde_json::Value::Bool(false) => output.extend_from_slice(b"false"),
        serde_json::Value::Number(number) if number.is_i64() || number.is_u64() => {
            output.extend_from_slice(number.to_string().as_bytes());
        }
        serde_json::Value::Number(_) => {
            return Err("canonical service contract JSON cannot contain floats".into());
        }
        serde_json::Value::String(value) => {
            output.extend_from_slice(
                serde_json::to_string(value)
                    .map_err(|error| error.to_string())?
                    .as_bytes(),
            );
        }
        serde_json::Value::Array(values) => {
            output.push(b'[');
            for (index, value) in values.iter().enumerate() {
                if index > 0 {
                    output.push(b',');
                }
                write_canonical_json(value, output)?;
            }
            output.push(b']');
        }
        serde_json::Value::Object(values) => {
            output.push(b'{');
            let mut entries = values.iter().collect::<Vec<_>>();
            entries.sort_by(|left, right| left.0.as_bytes().cmp(right.0.as_bytes()));
            for (index, (key, value)) in entries.into_iter().enumerate() {
                if index > 0 {
                    output.push(b',');
                }
                output.extend_from_slice(
                    serde_json::to_string(key)
                        .map_err(|error| error.to_string())?
                        .as_bytes(),
                );
                output.push(b':');
                write_canonical_json(value, output)?;
            }
            output.push(b'}');
        }
    }
    Ok(())
}

struct UniqueJsonValue(serde_json::Value);

impl<'de> Deserialize<'de> for UniqueJsonValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = UniqueJsonValue;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("JSON without duplicate object keys or floats")
            }

            fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E> {
                Ok(UniqueJsonValue(value.into()))
            }
            fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E> {
                Ok(UniqueJsonValue(value.into()))
            }
            fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
                Ok(UniqueJsonValue(value.into()))
            }
            fn visit_f64<E: serde::de::Error>(self, _value: f64) -> Result<Self::Value, E> {
                Err(E::custom("floating-point JSON is not canonical"))
            }
            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(UniqueJsonValue(value.into()))
            }
            fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
                Ok(UniqueJsonValue(value.into()))
            }
            fn visit_none<E>(self) -> Result<Self::Value, E> {
                Ok(UniqueJsonValue(serde_json::Value::Null))
            }
            fn visit_unit<E>(self) -> Result<Self::Value, E> {
                Ok(UniqueJsonValue(serde_json::Value::Null))
            }
            fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
            where
                A: serde::de::SeqAccess<'de>,
            {
                let mut values = Vec::new();
                while let Some(value) = sequence.next_element::<UniqueJsonValue>()? {
                    values.push(value.0);
                }
                Ok(UniqueJsonValue(values.into()))
            }
            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: serde::de::MapAccess<'de>,
            {
                let mut values = serde_json::Map::new();
                while let Some((key, value)) = map.next_entry::<String, UniqueJsonValue>()? {
                    if values.insert(key.clone(), value.0).is_some() {
                        return Err(serde::de::Error::custom(format!(
                            "duplicate JSON object key {key}"
                        )));
                    }
                }
                Ok(UniqueJsonValue(values.into()))
            }
        }
        deserializer.deserialize_any(Visitor)
    }
}

fn deterministic_archive(entries: &[PackageEntry]) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut bytes);
        builder.mode(tar::HeaderMode::Deterministic);
        for entry in entries {
            let mut header = tar::Header::new_ustar();
            header
                .set_path(&entry.path)
                .map_err(|error| format!("encode deterministic package path: {error}"))?;
            header.set_size(entry.bytes.len() as u64);
            header.set_mode(entry.mode);
            header.set_uid(0);
            header.set_gid(0);
            header.set_mtime(0);
            header.set_cksum();
            builder
                .append(&header, entry.bytes.as_slice())
                .map_err(|error| format!("encode deterministic package entry: {error}"))?;
        }
        builder
            .finish()
            .map_err(|error| format!("finish deterministic package: {error}"))?;
    }
    Ok(bytes)
}

fn required_entry<'a>(entries: &'a [PackageEntry], path: &str) -> Result<&'a PackageEntry, String> {
    entries
        .iter()
        .find(|entry| entry.path == path)
        .ok_or_else(|| format!("package is missing {path}"))
}

fn parse_checksums(bytes: &[u8]) -> Result<BTreeMap<String, String>, String> {
    let text = std::str::from_utf8(bytes)
        .map_err(|error| format!("checksum manifest is not UTF-8: {error}"))?;
    if !text.is_empty() && !text.ends_with('\n') {
        return Err("checksum manifest must end with LF".into());
    }
    if text.contains('\r') {
        return Err("checksum manifest must use LF line endings".into());
    }
    let mut result = BTreeMap::new();
    let mut previous: Option<&str> = None;
    for line in text.lines() {
        let (digest, path) = line
            .split_once("  ")
            .ok_or_else(|| "checksum line must use two spaces".to_string())?;
        validate_digest(digest, "file")?;
        if path.contains('\\') {
            return Err("checksum path must use canonical forward slashes".into());
        }
        let portable = portable_package_path(Path::new(path))?;
        if portable == PACKAGE_CHECKSUM_FILE {
            return Err("checksum manifest cannot checksum itself".into());
        }
        if previous.is_some_and(|previous| previous >= portable.as_str()) {
            return Err("checksum entries are not in strict bytewise order".into());
        }
        previous = Some(path);
        if result
            .keys()
            .any(|existing: &String| existing.eq_ignore_ascii_case(&portable))
        {
            return Err("checksum manifest contains a duplicate or case collision".into());
        }
        result.insert(portable, digest.to_ascii_lowercase());
    }
    Ok(result)
}

pub fn validate_manifest(manifest: &PackageManifestV2) -> Result<(), String> {
    if manifest.manifest_version != MANIFEST_VERSION_V2
        || manifest.package_contract != PACKAGE_CONTRACT_V1
    {
        return Err("manifest does not declare vqro.package.v1 manifest version 2".into());
    }
    validate_package_id(&manifest.id)?;
    validate_version(&manifest.version, "package version")?;
    let minimum = crate::update::Version::parse(&manifest.min_vqro_version)
        .ok_or_else(|| "min_vqro_version is not canonical semantic versioning".to_string())?;
    if minimum > crate::update::Version::current() {
        return Err(format!("package requires vqro {minimum}"));
    }
    if manifest.namespaces.len() > MAX_NAMESPACES || manifest.services.len() > MAX_SERVICES {
        return Err("manifest exceeds namespace or service limits".into());
    }
    let mut namespaces = BTreeSet::new();
    for namespace in &manifest.namespaces {
        validate_package_id(namespace)?;
        if !namespaces.insert(namespace) {
            return Err("manifest contains duplicate namespace claims".into());
        }
    }
    let mut service_ids = BTreeSet::new();
    let mut runtime_paths = BTreeSet::new();
    for service in &manifest.services {
        validate_identifier(&service.id, 128, "service id")?;
        if !service_ids.insert(service.id.as_str()) {
            return Err("manifest contains duplicate service IDs".into());
        }
        if service.capabilities.len() > MAX_CAPABILITIES {
            return Err("service has too many capabilities".into());
        }
        for value in service.provides.iter().chain(&service.capabilities) {
            validate_identifier(value, 128, "service capability")?;
        }
        match &service.runtime {
            PackageRuntimeV1::Component { component, world } => {
                validate_runtime_path(component, false)?;
                if !component.ends_with(".wasm") || world != "vqro:extension/service@1.0.0" {
                    return Err("component runtime has an invalid payload or world".into());
                }
                if !runtime_paths.insert(component.to_ascii_lowercase()) {
                    return Err("runtime payload is owned by multiple services".into());
                }
            }
            PackageRuntimeV1::Process { executables, args } => {
                if executables.is_empty() || executables.len() > 5 {
                    return Err(
                        "process runtime must declare one to five target executables".into(),
                    );
                }
                if args.len() > MAX_PROCESS_ARGS {
                    return Err("process runtime has too many arguments".into());
                }
                let mut argument_bytes = 0usize;
                for argument in args {
                    if argument.contains('\0') || argument.len() > MAX_PROCESS_ARG_BYTES {
                        return Err("process runtime contains an invalid argument".into());
                    }
                    argument_bytes = argument_bytes.saturating_add(argument.len());
                }
                if argument_bytes > MAX_PROCESS_ARGS_BYTES
                    || args.iter().any(|argument| argument == "_extension-service")
                {
                    return Err("process runtime arguments are unsafe or self-spawn Vqro".into());
                }
                for (target, executable) in executables {
                    if !all_target_names().contains(&target.as_str()) {
                        return Err(format!("process runtime has unknown target {target}"));
                    }
                    validate_runtime_path(executable, true)?;
                    if target == "windows-x86_64" && !executable.ends_with(".exe") {
                        return Err("Windows process payload must end in .exe".into());
                    }
                    let file_name = Path::new(executable)
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or_default();
                    if matches!(file_name.to_ascii_lowercase().as_str(), "vqro" | "vqro.exe") {
                        return Err("installed package cannot execute the Vqro binary".into());
                    }
                    if !runtime_paths.insert(executable.to_ascii_lowercase()) {
                        return Err("runtime payload is owned by multiple services".into());
                    }
                }
            }
        }
    }
    Ok(())
}

pub fn validate_manifest_v3(manifest: &PackageManifestV3) -> Result<(), String> {
    if manifest.manifest_version != MANIFEST_VERSION_V3
        || manifest.package_contract != PACKAGE_CONTRACT_V1
    {
        return Err("manifest does not declare vqro.package.v1 manifest version 3".into());
    }
    validate_package_id(&manifest.id)?;
    validate_version(&manifest.version, "package version")?;
    let minimum = crate::update::Version::parse(&manifest.min_vqro_version)
        .ok_or_else(|| "min_vqro_version is not canonical semantic versioning".to_string())?;
    if minimum > crate::update::Version::current() {
        return Err(format!("package requires vqro {minimum}"));
    }
    if manifest.namespaces.len() > MAX_NAMESPACES || manifest.services.len() > MAX_SERVICES {
        return Err("manifest exceeds namespace or service limits".into());
    }
    let mut namespaces = BTreeSet::new();
    for namespace in &manifest.namespaces {
        validate_package_id(namespace)?;
        if !namespaces.insert(namespace) {
            return Err("manifest contains duplicate namespace claims".into());
        }
    }
    let mut service_ids = BTreeSet::new();
    let mut runtime_paths = BTreeSet::new();
    for service in &manifest.services {
        validate_identifier(&service.id, 128, "service id")?;
        if !service_ids.insert(service.id.as_str()) {
            return Err("manifest contains duplicate service IDs".into());
        }
        if service.capabilities.len() > MAX_CAPABILITIES
            || service.exports.len() > MAX_SERVICE_ENDPOINTS
            || service.imports.len() > MAX_SERVICE_ENDPOINTS
        {
            return Err("service exceeds capability or endpoint limits".into());
        }
        let mut capabilities = BTreeSet::new();
        for capability in &service.capabilities {
            validate_identifier(capability, 128, "service capability")?;
            if !capabilities.insert(capability) {
                return Err("service contains duplicate capabilities".into());
            }
        }
        let mut endpoints = BTreeSet::new();
        for export in &service.exports {
            validate_identifier(&export.id, 128, "service export id")?;
            if !endpoints.insert(export.id.as_str()) {
                return Err("service contains duplicate endpoint IDs".into());
            }
            validate_contract_descriptor_path(&export.descriptor)?;
            validate_digest(&export.descriptor_sha256, "service contract descriptor")?;
        }
        for import in &service.imports {
            validate_identifier(&import.id, 128, "service import id")?;
            if !endpoints.insert(import.id.as_str()) {
                return Err("service contains duplicate endpoint IDs".into());
            }
            validate_contract_descriptor_path(&import.descriptor)?;
            validate_digest(&import.descriptor_sha256, "service contract descriptor")?;
        }
        match &service.runtime {
            PackageRuntimeV1::Component { component, world } => {
                validate_runtime_path(component, false)?;
                if !component.ends_with(".wasm") || world != "vqro:extension/service@1.0.0" {
                    return Err("component runtime has an invalid payload or world".into());
                }
                if !runtime_paths.insert(component.to_ascii_lowercase()) {
                    return Err("runtime payload is owned by multiple services".into());
                }
            }
            PackageRuntimeV1::Process { executables, args } => {
                if executables.is_empty() || executables.len() > 5 {
                    return Err(
                        "process runtime must declare one to five target executables".into(),
                    );
                }
                if args.len() > MAX_PROCESS_ARGS {
                    return Err("process runtime has too many arguments".into());
                }
                let mut argument_bytes = 0usize;
                for argument in args {
                    if argument.contains('\0') || argument.len() > MAX_PROCESS_ARG_BYTES {
                        return Err("process runtime contains an invalid argument".into());
                    }
                    argument_bytes = argument_bytes.saturating_add(argument.len());
                }
                if argument_bytes > MAX_PROCESS_ARGS_BYTES
                    || args.iter().any(|argument| argument == "_extension-service")
                {
                    return Err("process runtime arguments are unsafe or self-spawn Vqro".into());
                }
                for (target, executable) in executables {
                    if !all_target_names().contains(&target.as_str()) {
                        return Err(format!("process runtime has unknown target {target}"));
                    }
                    validate_runtime_path(executable, true)?;
                    if target == "windows-x86_64" && !executable.ends_with(".exe") {
                        return Err("Windows process payload must end in .exe".into());
                    }
                    let file_name = Path::new(executable)
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or_default();
                    if matches!(file_name.to_ascii_lowercase().as_str(), "vqro" | "vqro.exe") {
                        return Err("installed package cannot execute the Vqro binary".into());
                    }
                    if !runtime_paths.insert(executable.to_ascii_lowercase()) {
                        return Err("runtime payload is owned by multiple services".into());
                    }
                }
            }
        }
    }
    Ok(())
}

fn validate_runtime_inventory(package: &ValidatedPackageContents) -> Result<(), String> {
    for service in &package.descriptor.manifest.services {
        match &service.runtime {
            PackageRuntimeV1::Component { component, .. } => {
                let Some((bytes, _)) = package.files.get(component) else {
                    return Err(format!(
                        "component payload {component} is absent from package"
                    ));
                };
                if !bytes.starts_with(b"\0asm") {
                    return Err("component payload is not WebAssembly".into());
                }
            }
            PackageRuntimeV1::Process { executables, .. } => {
                for (target, executable) in executables {
                    let Some((_, mode)) = package.files.get(executable) else {
                        return Err(format!(
                            "process payload {executable} is absent from package"
                        ));
                    };
                    if target != "windows-x86_64" && mode & 0o111 == 0 {
                        return Err("Unix process payload is not executable".into());
                    }
                }
            }
        }
    }
    Ok(())
}

pub fn resolve_runtime(
    package: &ValidatedPackageV1,
    service_id: &str,
    target: PackageTargetV1,
) -> Result<ResolvedPackageRuntimeV1, String> {
    let service = package
        .manifest
        .services
        .iter()
        .find(|service| service.id == service_id)
        .ok_or_else(|| format!("unknown package service {service_id}"))?;
    let (kind, path, args, world) = match &service.runtime {
        PackageRuntimeV1::Component { component, world } => (
            "component",
            component.as_str(),
            Vec::new(),
            Some(world.clone()),
        ),
        PackageRuntimeV1::Process { executables, args } => (
            "process",
            executables
                .get(target.as_str())
                .ok_or_else(|| format!("service has no payload for {}", target.as_str()))?
                .as_str(),
            args.clone(),
            None,
        ),
    };
    let file = package
        .files
        .iter()
        .find(|file| file.path == path)
        .ok_or_else(|| "resolved runtime payload is absent from package inventory".to_string())?;
    Ok(ResolvedPackageRuntimeV1 {
        service_id: service.id.clone(),
        kind: kind.into(),
        payload_path: path.into(),
        payload_sha256: file.sha256.clone(),
        args,
        world,
    })
}

pub fn validate_namespace_claims(
    manifest: &ValidatedPackageManifestV1,
    provenance: &PackageProvenanceV1,
    occupied: &BTreeMap<String, String>,
    trusted_curated: Option<&PackageProvenanceV1>,
) -> Result<(), String> {
    match provenance {
        PackageProvenanceV1::Curated {
            catalog,
            publisher,
            repository,
            package_id,
            version,
            source_commit,
            package_sha256,
            manifest_sha256,
            ..
        } => {
            if trusted_curated != Some(provenance) {
                return Err(
                    "curated provenance is not exactly authorized by the trusted catalog".into(),
                );
            }
            validate_identifier(catalog, 128, "curated catalog")?;
            validate_identifier(publisher, 128, "curated publisher")?;
            validate_package_id(package_id)?;
            validate_version(version, "curated package version")?;
            if package_id != &manifest.id || version != &manifest.version {
                return Err("curated manifest identity mismatch".into());
            }
            if repository.trim().is_empty()
                || source_commit.len() != 40
                || !source_commit
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
            {
                return Err("curated source identity is not canonical".into());
            }
            validate_digest(package_sha256, "curated package")?;
            validate_digest(manifest_sha256, "curated manifest")?;
        }
        PackageProvenanceV1::File { package_sha256 } => {
            validate_digest(package_sha256, "local file package")?;
        }
        PackageProvenanceV1::Git {
            url,
            source_commit,
            package_path,
            package_id,
            version,
            package_sha256,
            manifest_sha256,
        } => {
            let authority = url
                .strip_prefix("https://")
                .and_then(|rest| rest.split('/').next())
                .unwrap_or_default();
            if authority.is_empty()
                || authority.contains('@')
                || source_commit.len() != 40
                || !source_commit
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
                || portable_package_path(Path::new(package_path)).as_deref() != Ok(package_path)
                || !package_path.ends_with(".vqrox")
            {
                return Err("Git package source identity is not canonical".into());
            }
            validate_package_id(package_id)?;
            validate_version(version, "Git package version")?;
            if package_id != &manifest.id || version != &manifest.version {
                return Err("Git package manifest identity mismatch".into());
            }
            validate_digest(package_sha256, "Git package")?;
            validate_digest(manifest_sha256, "Git package manifest")?;
        }
        PackageProvenanceV1::Link {
            canonical_path,
            trust_id,
            trusted,
            ..
        } => {
            if !trusted || canonical_path.is_empty() || trust_id.is_empty() || trust_id.len() > 256
            {
                return Err("developer link provenance lacks explicit trust".into());
            }
        }
    }
    for namespace in &manifest.namespaces {
        if namespace == "host" || namespace.starts_with("host.") {
            return Err("host.* namespaces are permanently reserved".into());
        }
        if let Some(owner) = occupied.get(namespace) {
            if owner != &manifest.id {
                return Err(format!("namespace {namespace} is already owned by {owner}"));
            }
        }
        match provenance {
            PackageProvenanceV1::Curated {
                publisher,
                authorized_namespaces,
                ..
            } => {
                if namespace.starts_with("vqro.")
                    && (publisher != "vqro" || !authorized_namespaces.contains(namespace))
                {
                    return Err(format!("curated provenance does not authorize {namespace}"));
                }
            }
            PackageProvenanceV1::Git { .. }
                if namespace.starts_with("vqro.")
                    && !trusted_first_party_git_namespace(provenance, manifest, namespace) =>
            {
                return Err(
                    "Git package cannot claim vqro.* without an exact production pin".into(),
                );
            }
            PackageProvenanceV1::File { .. } if namespace.starts_with("vqro.") => {
                return Err(
                    "untrusted packages cannot claim vqro.* without a trusted approval".into(),
                );
            }
            PackageProvenanceV1::Link { trusted, .. } if !trusted => {
                return Err("developer link namespace claims require explicit trust".into());
            }
            PackageProvenanceV1::Link { .. }
            | PackageProvenanceV1::File { .. }
            | PackageProvenanceV1::Git { .. } => {}
        }
    }
    Ok(())
}

fn trusted_first_party_git_namespace(
    provenance: &PackageProvenanceV1,
    manifest: &ValidatedPackageManifestV1,
    namespace: &str,
) -> bool {
    let pin = if namespace == crate::collections_extension_contract::COLLECTIONS_PACKAGE_ID {
        crate::collections_extension_contract::embedded_collections_production_pin().map(|pin| {
            (
                pin.package_id,
                pin.source_url,
                pin.source_commit,
                pin.package_path,
                pin.package_sha256,
                pin.manifest_sha256,
            )
        })
    } else if namespace == crate::spaces_extension_contract::SPACES_PACKAGE_ID {
        crate::spaces_extension_contract::embedded_spaces_production_pin().map(|pin| {
            (
                pin.package_id,
                pin.source_url,
                pin.source_commit,
                pin.package_path,
                pin.package_sha256,
                pin.manifest_sha256,
            )
        })
    } else {
        return false;
    };
    let Ok((package, source_url, source_commit, pinned_path, package_digest, manifest_digest)) =
        pin
    else {
        return false;
    };
    matches!(provenance,
        PackageProvenanceV1::Git {
            url,
            source_commit: commit,
            package_path,
            package_id,
            version: _,
            package_sha256,
            manifest_sha256,
        } if namespace == package
            && manifest.id == package
            && manifest.namespaces == [package.as_str()]
            && url == &source_url
            && commit == &source_commit
            && package_path == &pinned_path
            && package_id == &package
            && package_sha256 == &package_digest
            && manifest_sha256 == &manifest_digest)
}

pub fn install_local_file(
    source: &Path,
    store_root: &Path,
    expected_package_sha256: Option<&str>,
    provenance: PackageProvenanceV1,
    occupied: &BTreeMap<String, String>,
    trusted_curated: Option<&PackageProvenanceV1>,
) -> Result<InstalledPackageV1, String> {
    let archive_file = crate::platform::read_bounded_regular_nofollow(source, MAX_PACKAGE_BYTES)
        .map_err(|error| format!("read bounded package source: {error}"))?;
    let archive_bytes = archive_file.bytes;
    let validated = validate_package_bytes(&archive_bytes, expected_package_sha256)?;
    if matches!(provenance, PackageProvenanceV1::Link { .. }) {
        return Err("immutable package installs cannot use developer-link provenance".into());
    }
    validate_provenance_digest(&validated.descriptor, &provenance)?;
    validate_namespace_claims(
        &validated.descriptor.manifest,
        &provenance,
        occupied,
        trusted_curated,
    )?;
    let digest = validated.descriptor.package_sha256.clone();
    let content_root = store_root.join("sha256");
    let staging_root = store_root.join(".staging");
    for directory in [&content_root, &staging_root] {
        fs::create_dir_all(directory)
            .map_err(|error| format!("create package storage directory: {error}"))?;
        let metadata = fs::symlink_metadata(directory)
            .map_err(|error| format!("inspect package storage directory: {error}"))?;
        if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
            return Err("package storage contains a link or non-directory".into());
        }
    }
    let destination = content_root.join(&digest);
    if destination.exists() {
        verify_installed_package(&destination, &digest)?;
        return installed_descriptor(&validated.descriptor, &destination, provenance);
    }
    let staging = crate::platform::SecurePackageStaging::create(&staging_root)
        .map_err(|error| format!("create private package staging directory: {error}"))?;
    for (path, (bytes, mode)) in &validated.files {
        staging
            .write_file(Path::new(path), bytes, *mode)
            .map_err(|error| format!("write package output safely: {error}"))?;
    }
    staging
        .write_file(Path::new("package.vqrox"), &archive_bytes, 0o600)
        .map_err(|error| format!("retain package archive safely: {error}"))?;
    match fs::rename(staging.path(), &destination) {
        Ok(()) => {}
        Err(error) if destination.exists() => {
            verify_installed_package(&destination, &digest)?;
            fs::remove_dir_all(staging.path()).map_err(|remove| {
                format!("remove duplicate package stage after {error}: {remove}")
            })?;
        }
        Err(error) => return Err(format!("commit immutable package: {error}")),
    }
    set_tree_readonly(&destination)?;
    crate::platform::sync_containing_directory(&destination)
        .map_err(|error| format!("sync immutable package: {error}"))?;
    installed_descriptor(&validated.descriptor, &destination, provenance)
}

pub fn register_developer_link(
    source: &Path,
    trust_id: String,
    explicitly_trusted: bool,
    occupied: &BTreeMap<String, String>,
) -> Result<LinkedPackageV1, String> {
    if !explicitly_trusted || trust_id.trim().is_empty() || trust_id.len() > 256 {
        return Err("developer link requires an explicit bounded trust approval".into());
    }
    let metadata =
        fs::symlink_metadata(source).map_err(|error| format!("inspect developer link: {error}"))?;
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return Err("developer link root must be a real directory".into());
    }
    let canonical = fs::canonicalize(source)
        .map_err(|error| format!("canonicalize developer link: {error}"))?;
    let validated = validate_directory_package(&canonical)?;
    let canonical_text = canonical
        .to_str()
        .ok_or_else(|| "developer link path is not UTF-8".to_string())?
        .to_owned();
    let provenance = PackageProvenanceV1::Link {
        canonical_path: canonical_text.clone(),
        trust_id,
        trusted: true,
        package_id: validated.manifest.id.clone(),
        version: validated.manifest.version.clone(),
        package_sha256: validated.package_sha256.clone(),
        manifest_sha256: validated.manifest_sha256.clone(),
    };
    validate_namespace_claims(&validated.manifest, &provenance, occupied, None)?;
    Ok(LinkedPackageV1 {
        package_id: validated.manifest.id,
        version: validated.manifest.version,
        manifest_sha256: validated.manifest_sha256,
        namespaces: validated.manifest.namespaces,
        inventory_sha256: validated.package_sha256,
        canonical_root: canonical_text,
        provenance,
    })
}

pub fn revalidate_developer_link(
    record: &PackageLifecycleRecordV1,
    occupied: &BTreeMap<String, String>,
) -> Result<LinkedPackageV1, String> {
    let PackageProvenanceV1::Link {
        canonical_path,
        trust_id,
        trusted,
        ..
    } = &record.source
    else {
        return Err("package is not a developer link".into());
    };
    let linked = register_developer_link(
        Path::new(canonical_path),
        trust_id.clone(),
        *trusted,
        occupied,
    )?;
    if linked.package_id != record.package_id
        || linked.namespaces != record.namespaces
        || linked.provenance != record.source
    {
        return Err("developer link package identity, content, or namespace claims changed".into());
    }
    Ok(linked)
}

pub fn validate_directory_package(root: &Path) -> Result<ValidatedPackageV1, String> {
    let canonical_root = fs::canonicalize(root)
        .map_err(|error| format!("canonicalize developer link root: {error}"))?;
    let mut pending = vec![canonical_root.clone()];
    let mut entries = BTreeMap::new();
    let mut expanded = 0_u64;
    let mut visited_entries = 0_usize;
    while let Some(directory) = pending.pop() {
        let directory_metadata = fs::symlink_metadata(&directory)
            .map_err(|error| format!("inspect developer link directory: {error}"))?;
        if !directory_metadata.file_type().is_dir()
            || directory_metadata.file_type().is_symlink()
            || !fs::canonicalize(&directory)
                .map_err(|error| format!("canonicalize developer link directory: {error}"))?
                .starts_with(&canonical_root)
        {
            return Err("developer link directory traversal escaped through a link".into());
        }
        for entry in fs::read_dir(&directory)
            .map_err(|error| format!("read developer link directory: {error}"))?
        {
            visited_entries = visited_entries
                .checked_add(1)
                .ok_or_else(|| "developer link entry count overflow".to_string())?;
            if visited_entries > MAX_PACKAGE_ENTRIES {
                return Err("developer link contains too many files or directories".into());
            }
            let entry = entry.map_err(|error| format!("read developer link entry: {error}"))?;
            let path = entry.path();
            let relative_path = path
                .strip_prefix(&canonical_root)
                .map_err(|_| "developer link escaped its root".to_string())?;
            let relative = portable_package_path(relative_path)?;
            let metadata = fs::symlink_metadata(&path)
                .map_err(|error| format!("inspect developer link entry: {error}"))?;
            if metadata.file_type().is_symlink() {
                return Err("developer link cannot contain symlinks or reparse points".into());
            }
            if metadata.is_dir() {
                pending.push(path);
                continue;
            }
            if !metadata.file_type().is_file() {
                return Err("developer link contains a special file".into());
            }
            if entries
                .keys()
                .any(|existing: &String| existing.eq_ignore_ascii_case(&relative))
            {
                return Err("developer link contains a duplicate or case-colliding path".into());
            }
            let file =
                crate::platform::read_bounded_regular_nofollow(&path, MAX_PACKAGE_FILE_BYTES)
                    .map_err(|error| format!("read bounded developer link file: {error}"))?;
            reject_hard_link(&file.metadata)?;
            expanded = expanded
                .checked_add(file.bytes.len() as u64)
                .ok_or_else(|| "developer link expanded size overflow".to_string())?;
            if expanded > MAX_EXPANDED_BYTES {
                return Err("developer link exceeds expanded size limits".into());
            }
            entries.insert(relative, (file.bytes, metadata_mode(&file.metadata)));
        }
    }
    let checksum_bytes = &entries
        .get(PACKAGE_CHECKSUM_FILE)
        .ok_or_else(|| format!("developer link is missing {PACKAGE_CHECKSUM_FILE}"))?
        .0;
    let checksums = parse_checksums(checksum_bytes)?;
    let expected = entries
        .keys()
        .filter(|path| path.as_str() != PACKAGE_CHECKSUM_FILE)
        .map(String::as_str)
        .collect::<Vec<_>>();
    if checksums.keys().map(String::as_str).collect::<Vec<_>>() != expected {
        return Err("developer link checksum inventory does not match files".into());
    }
    for (path, (bytes, _)) in entries
        .iter()
        .filter(|(path, _)| path.as_str() != PACKAGE_CHECKSUM_FILE)
    {
        if checksums[path] != sha256(bytes) {
            return Err(format!("developer link checksum mismatch for {path}"));
        }
    }
    let manifest_bytes = &entries
        .get(PACKAGE_MANIFEST_FILE)
        .ok_or_else(|| format!("developer link is missing {PACKAGE_MANIFEST_FILE}"))?
        .0;
    let manifest_text = std::str::from_utf8(manifest_bytes)
        .map_err(|error| format!("developer link manifest is not UTF-8: {error}"))?;
    let package_entries = entries
        .iter()
        .map(|(path, (bytes, mode))| PackageEntry {
            path: path.clone(),
            bytes: bytes.clone(),
            mode: *mode,
        })
        .collect::<Vec<_>>();
    let (manifest, contract_descriptors) =
        parse_and_validate_manifest(manifest_text, &package_entries)?;
    let mut inventory = Sha256::new();
    let files = entries
        .iter()
        .filter(|(path, _)| path.as_str() != PACKAGE_CHECKSUM_FILE)
        .map(|(path, (bytes, mode))| {
            let digest = sha256(bytes);
            inventory.update(path.as_bytes());
            inventory.update([0]);
            inventory.update(digest.as_bytes());
            PackageFileV1 {
                path: path.clone(),
                sha256: digest,
                size: bytes.len() as u64,
                executable: mode & 0o111 != 0,
            }
        })
        .collect();
    let descriptor = ValidatedPackageV1 {
        contract: PACKAGE_CONTRACT_V1.into(),
        package_sha256: format!("{:x}", inventory.finalize()),
        manifest_sha256: sha256(manifest_bytes),
        manifest,
        contract_descriptors,
        files,
    };
    let contents = ValidatedPackageContents {
        descriptor: descriptor.clone(),
        files: entries,
    };
    validate_runtime_inventory(&contents)?;
    Ok(descriptor)
}

pub fn verify_installed_package(root: &Path, expected_digest: &str) -> Result<(), String> {
    validate_digest(expected_digest, "installed package")?;
    let root_metadata = fs::symlink_metadata(root)
        .map_err(|error| format!("inspect installed package root: {error}"))?;
    if !root_metadata.file_type().is_dir() || root_metadata.file_type().is_symlink() {
        return Err("installed package root is not a real directory".into());
    }
    let canonical_parent = fs::canonicalize(
        root.parent()
            .ok_or_else(|| "installed package has no digest parent".to_string())?,
    )
    .map_err(|error| format!("canonicalize package store: {error}"))?;
    let canonical_root = fs::canonicalize(root)
        .map_err(|error| format!("canonicalize installed package: {error}"))?;
    if canonical_root.parent() != Some(canonical_parent.as_path())
        || root.file_name().and_then(|v| v.to_str()) != Some(expected_digest)
    {
        return Err("installed package is outside its digest-addressed path".into());
    }
    let archive = root.join("package.vqrox");
    let validated = validate_package_file(&archive, Some(expected_digest))?;
    let expected = validated
        .descriptor
        .files
        .iter()
        .map(|file| file.path.clone())
        .chain([PACKAGE_CHECKSUM_FILE.into(), "package.vqrox".into()])
        .collect::<BTreeSet<_>>();
    let actual = installed_file_inventory(root)?;
    if actual != expected {
        return Err("installed package inventory changed".into());
    }
    for file in &validated.descriptor.files {
        let path = root.join(&file.path);
        let installed =
            crate::platform::read_bounded_regular_nofollow(&path, MAX_PACKAGE_FILE_BYTES)
                .map_err(|error| format!("read installed package file safely: {error}"))?;
        if installed.bytes.len() as u64 != file.size || sha256(&installed.bytes) != file.sha256 {
            return Err(format!("installed package file changed: {}", file.path));
        }
    }
    Ok(())
}

fn installed_file_inventory(root: &Path) -> Result<BTreeSet<String>, String> {
    let mut pending = vec![root.to_path_buf()];
    let mut files = BTreeSet::new();
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory)
            .map_err(|error| format!("inspect installed package directory: {error}"))?
        {
            let entry =
                entry.map_err(|error| format!("inspect installed package entry: {error}"))?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)
                .map_err(|error| format!("inspect installed package metadata: {error}"))?;
            if metadata.file_type().is_symlink() {
                return Err("installed package contains a link or reparse point".into());
            }
            if metadata.is_dir() {
                pending.push(path);
            } else if metadata.file_type().is_file() {
                files
                    .insert(portable_package_path(path.strip_prefix(root).map_err(
                        |_| "installed package escaped its root".to_string(),
                    )?)?);
            } else {
                return Err("installed package contains a special file".into());
            }
        }
    }
    Ok(files)
}

fn installed_descriptor(
    package: &ValidatedPackageV1,
    root: &Path,
    provenance: PackageProvenanceV1,
) -> Result<InstalledPackageV1, String> {
    Ok(InstalledPackageV1 {
        package_id: package.manifest.id.clone(),
        version: package.manifest.version.clone(),
        package_sha256: package.package_sha256.clone(),
        manifest_sha256: package.manifest_sha256.clone(),
        immutable_root: root
            .to_str()
            .ok_or_else(|| "installed package path is not UTF-8".to_string())?
            .into(),
        provenance,
    })
}

fn validate_provenance_digest(
    package: &ValidatedPackageV1,
    provenance: &PackageProvenanceV1,
) -> Result<(), String> {
    let expected = match provenance {
        PackageProvenanceV1::Curated {
            package_id,
            version,
            package_sha256,
            manifest_sha256,
            authorized_namespaces,
            ..
        } => {
            if package_id != &package.manifest.id || version != &package.manifest.version {
                return Err("curated package identity mismatch".into());
            }
            if manifest_sha256 != &package.manifest_sha256 {
                return Err("curated manifest digest mismatch".into());
            }
            if authorized_namespaces != &package.manifest.namespaces {
                return Err("curated namespace authorization mismatch".into());
            }
            package_sha256
        }
        PackageProvenanceV1::File { package_sha256 }
        | PackageProvenanceV1::Git { package_sha256, .. } => package_sha256,
        PackageProvenanceV1::Link { .. } => {
            return Err("developer-link provenance cannot install an immutable file".into());
        }
    };
    validate_digest(expected, "provenance package")?;
    if expected != &package.package_sha256 {
        return Err("provenance package digest mismatch".into());
    }
    Ok(())
}

fn portable_package_path(path: &Path) -> Result<String, String> {
    if path.as_os_str().is_empty() || path.is_absolute() {
        return Err("package contains an unsafe path".into());
    }
    let mut parts = Vec::new();
    for component in path.components() {
        let Component::Normal(component) = component else {
            return Err("package contains an unsafe path".into());
        };
        let component = component
            .to_str()
            .ok_or_else(|| "package path is not UTF-8".to_string())?;
        if component.is_empty()
            || component.len() > MAX_PATH_COMPONENT_BYTES
            || component.ends_with(['.', ' '])
            || !component
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
            || windows_reserved(component)
        {
            return Err("package contains a non-portable path".into());
        }
        parts.push(component);
    }
    let value = parts.join("/");
    if parts.len() > MAX_PATH_DEPTH || value.len() > MAX_PATH_BYTES {
        return Err("package path exceeds depth or length limits".into());
    }
    Ok(value)
}

fn windows_reserved(component: &str) -> bool {
    let uppercase = component.to_ascii_uppercase();
    let stem = uppercase.split('.').next().unwrap_or(&uppercase);
    matches!(
        stem,
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    )
}

fn validate_contract_descriptor_path(path: &str) -> Result<(), String> {
    let portable = portable_package_path(Path::new(path))?;
    if !portable.starts_with("contracts/") || portable.split('/').count() < 2 {
        return Err("contract descriptor must be package-relative under contracts/".into());
    }
    Ok(())
}

fn validate_runtime_path(path: &str, process: bool) -> Result<(), String> {
    let portable = portable_package_path(Path::new(path))?;
    if !portable.starts_with("services/") || (process && portable.split('/').count() < 3) {
        return Err("runtime payload must be package-relative under services/".into());
    }
    Ok(())
}

pub(crate) fn validate_package_id(value: &str) -> Result<(), String> {
    if value == "host" || value.starts_with("host.") {
        return Err("host.* package identities are reserved".into());
    }
    if value.len() > 128
        || value.split('.').count() < 2
        || value.split('.').any(|segment| {
            segment.is_empty()
                || !segment.bytes().all(|byte| {
                    byte.is_ascii_lowercase()
                        || byte.is_ascii_digit()
                        || matches!(byte, b'-' | b'_')
                })
        })
    {
        return Err("package identity is not canonical".into());
    }
    Ok(())
}

fn validate_identifier(value: &str, maximum: usize, label: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > maximum
        || !value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'-' | b'_')
        })
    {
        return Err(format!("{label} is not canonical"));
    }
    Ok(())
}

fn validate_version(value: &str, label: &str) -> Result<(), String> {
    if crate::update::Version::parse(value).is_none()
        || value.starts_with('v')
        || value
            .split('.')
            .any(|part| part.len() > 1 && part.starts_with('0'))
    {
        return Err(format!("{label} is not canonical semantic versioning"));
    }
    Ok(())
}

fn validate_digest(value: &str, label: &str) -> Result<(), String> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Err(format!("{label} digest is not SHA-256"));
    }
    Ok(())
}

fn all_target_names() -> [&'static str; 5] {
    [
        "linux-x86_64",
        "linux-aarch64",
        "macos-x86_64",
        "macos-aarch64",
        "windows-x86_64",
    ]
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn set_tree_readonly(root: &Path) -> Result<(), String> {
    let mut pending = vec![root.to_path_buf()];
    let mut paths = Vec::new();
    while let Some(path) = pending.pop() {
        for entry in fs::read_dir(&path)
            .map_err(|error| format!("inspect installed package tree: {error}"))?
        {
            let entry =
                entry.map_err(|error| format!("inspect installed package entry: {error}"))?;
            let metadata = entry
                .metadata()
                .map_err(|error| format!("inspect installed package metadata: {error}"))?;
            if metadata.is_dir() {
                pending.push(entry.path());
            }
            paths.push(entry.path());
        }
    }
    paths.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
    paths.push(root.to_path_buf());
    for path in paths {
        let mut permissions = fs::metadata(&path)
            .map_err(|error| format!("inspect package permissions: {error}"))?
            .permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&path, permissions)
            .map_err(|error| format!("make installed package immutable: {error}"))?;
    }
    Ok(())
}

#[cfg(unix)]
fn metadata_mode(metadata: &fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o777
}

#[cfg(not(unix))]
fn metadata_mode(_metadata: &fs::Metadata) -> u32 {
    0o644
}

#[cfg(unix)]
fn reject_hard_link(metadata: &fs::Metadata) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt;
    if metadata.nlink() > 1 {
        return Err("developer link cannot contain hard-linked files".into());
    }
    Ok(())
}

#[cfg(not(unix))]
fn reject_hard_link(_metadata: &fs::Metadata) -> Result<(), String> {
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PackageLifecycleStatusV1 {
    Disabled,
    Enabled,
    Activating,
    Failed,
    RemovedRetainingData,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PackageArtifactPointerV1 {
    pub package_sha256: String,
    pub version: String,
    pub runtime_generation: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PackageLifecycleRecordV1 {
    pub package_id: String,
    pub source: PackageProvenanceV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_source: Option<PackageProvenanceV1>,
    #[serde(default)]
    pub namespaces: Vec<String>,
    pub revision: u64,
    pub status: PackageLifecycleStatusV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<PackageArtifactPointerV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous: Option<PackageArtifactPointerV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activation_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_activation: Option<PendingActivationV1>,
    #[serde(default)]
    pub retained_data: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PendingActivationV1 {
    pub operation_id: String,
    pub candidate: PackageArtifactPointerV1,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PackageRegistryV1 {
    #[serde(default = "default_package_registry_contract")]
    pub contract: String,
    pub revision: u64,
    pub packages: BTreeMap<String, PackageLifecycleRecordV1>,
    #[serde(default)]
    pub load_order: Vec<String>,
}

fn default_package_registry_contract() -> String {
    PACKAGE_REGISTRY_CONTRACT_V1.into()
}

impl Default for PackageRegistryV1 {
    fn default() -> Self {
        Self {
            contract: default_package_registry_contract(),
            revision: 0,
            packages: BTreeMap::new(),
            load_order: Vec::new(),
        }
    }
}

fn validate_lifecycle_record(record: &PackageLifecycleRecordV1) -> Result<(), String> {
    if record.revision == 0 {
        return Err("package lifecycle revision must be nonzero".into());
    }
    if (record.status == PackageLifecycleStatusV1::Activating)
        != record.pending_activation.is_some()
    {
        return Err("package pending activation does not match lifecycle status".into());
    }
    if let Some(pending) = &record.pending_activation {
        validate_identifier(
            &pending.operation_id,
            128,
            "pending activation operation id",
        )?;
        validate_digest(
            &pending.candidate.package_sha256,
            "pending activation package",
        )?;
        validate_version(&pending.candidate.version, "pending activation version")?;
        if pending.candidate.runtime_generation == 0 {
            return Err("pending activation generation must be nonzero".into());
        }
    }
    let expected_digest = match &record.source {
        PackageProvenanceV1::Curated {
            package_id,
            version,
            package_sha256,
            ..
        } => {
            validate_package_id(package_id)?;
            validate_version(version, "curated package version")?;
            validate_digest(package_sha256, "curated package")?;
            if package_id != &record.package_id {
                return Err("curated source package ID does not match lifecycle record".into());
            }
            if record
                .active
                .as_ref()
                .is_some_and(|active| &active.version != version)
            {
                return Err("curated source version does not match active artifact".into());
            }
            Some(package_sha256)
        }
        PackageProvenanceV1::File { package_sha256 } => {
            validate_digest(package_sha256, "local package")?;
            Some(package_sha256)
        }
        PackageProvenanceV1::Git {
            url,
            source_commit,
            package_path,
            package_id,
            version,
            package_sha256,
            manifest_sha256,
        } => {
            validate_package_id(package_id)?;
            validate_version(version, "Git package version")?;
            validate_digest(package_sha256, "Git package")?;
            validate_digest(manifest_sha256, "Git package manifest")?;
            if url.is_empty()
                || package_path.is_empty()
                || source_commit.len() != 40
                || package_id != &record.package_id
                || record
                    .active
                    .as_ref()
                    .is_some_and(|active| &active.version != version)
            {
                return Err("Git package source does not match lifecycle record".into());
            }
            Some(package_sha256)
        }
        PackageProvenanceV1::Link {
            canonical_path,
            trust_id,
            trusted,
            package_id,
            version,
            package_sha256,
            manifest_sha256,
        } => {
            validate_identifier(trust_id, 128, "developer-link trust id")?;
            validate_package_id(package_id)?;
            validate_version(version, "developer-link version")?;
            validate_digest(package_sha256, "developer-link inventory")?;
            validate_digest(manifest_sha256, "developer-link manifest")?;
            if !trusted
                || !Path::new(canonical_path).is_absolute()
                || package_id != &record.package_id
            {
                return Err("developer link is not explicitly trusted and canonical".into());
            }
            None
        }
    };
    if record.previous.is_some() != record.previous_source.is_some() {
        return Err("previous package pointer and provenance must be recorded together".into());
    }
    if let (Some(previous), Some(previous_source)) = (&record.previous, &record.previous_source) {
        validate_digest(&previous.package_sha256, "previous package")?;
        validate_version(&previous.version, "previous package version")?;
        if previous.runtime_generation == 0
            || !source_matches_pointer(previous_source, &record.package_id, previous)
        {
            return Err("previous package pointer does not match its source".into());
        }
    }
    if let Some(active) = &record.active {
        validate_digest(&active.package_sha256, "active package")?;
        validate_version(&active.version, "active package version")?;
        if active.runtime_generation == 0
            || expected_digest.is_some_and(|expected| expected != &active.package_sha256)
        {
            return Err("active package pointer does not match its source".into());
        }
    }
    Ok(())
}

fn lifecycle_manifest(record: &PackageLifecycleRecordV1) -> ValidatedPackageManifestV1 {
    PackageManifestV2 {
        manifest_version: MANIFEST_VERSION_V2,
        package_contract: PACKAGE_CONTRACT_V1.into(),
        id: record.package_id.clone(),
        version: match &record.source {
            PackageProvenanceV1::Curated { version, .. }
            | PackageProvenanceV1::Git { version, .. }
            | PackageProvenanceV1::Link { version, .. } => version.clone(),
            PackageProvenanceV1::File { .. } => record
                .active
                .as_ref()
                .map_or_else(|| "0.0.0".into(), |active| active.version.clone()),
        },
        min_vqro_version: "0.0.0".into(),
        namespaces: record.namespaces.clone(),
        services: Vec::new(),
    }
    .into()
}

impl PackageRegistryV1 {
    pub fn validate(
        &self,
        trusted_curated: &BTreeMap<String, PackageProvenanceV1>,
    ) -> Result<(), String> {
        if self.contract != PACKAGE_REGISTRY_CONTRACT_V1 {
            return Err("unsupported package registry contract".into());
        }
        if self.packages.len() > MAX_INSTALLED_PACKAGES
            || self.load_order.len() > self.packages.len()
        {
            return Err("package registry exceeds package limits".into());
        }
        let mut ordered = BTreeSet::new();
        for package_id in &self.load_order {
            let record = self
                .packages
                .get(package_id)
                .ok_or_else(|| "package load order contains an unknown package".to_string())?;
            if !ordered.insert(package_id.as_str())
                || record.status == PackageLifecycleStatusV1::RemovedRetainingData
            {
                return Err("package load order is inconsistent".into());
            }
        }
        let mut namespaces = BTreeMap::new();
        for (package_id, record) in &self.packages {
            if package_id != &record.package_id {
                return Err("package registry key does not match package ID".into());
            }
            validate_lifecycle_record(record)?;
            let manifest = lifecycle_manifest(record);
            validate_namespace_claims(
                &manifest,
                &record.source,
                &BTreeMap::new(),
                trusted_curated.get(package_id),
            )?;
            if record.status != PackageLifecycleStatusV1::RemovedRetainingData
                && !ordered.contains(package_id.as_str())
            {
                return Err("active package is absent from load order".into());
            }
            if record.retained_data
                || record.status != PackageLifecycleStatusV1::RemovedRetainingData
            {
                for namespace in &record.namespaces {
                    if namespaces.insert(namespace, package_id).is_some() {
                        return Err("package registry has colliding namespace claims".into());
                    }
                }
            }
        }
        Ok(())
    }

    pub fn is_empty(&self) -> bool {
        self.packages.is_empty() && self.load_order.is_empty() && self.revision == 0
    }

    pub fn register(
        &mut self,
        record: PackageLifecycleRecordV1,
        expected_revision: u64,
        trusted_curated: &BTreeMap<String, PackageProvenanceV1>,
    ) -> Result<u64, String> {
        self.validate(trusted_curated)?;
        if self.packages.len() >= MAX_INSTALLED_PACKAGES {
            return Err("package registry is full".into());
        }
        if expected_revision != self.revision {
            return Err(format!(
                "package registry revision conflict: {}",
                self.revision
            ));
        }
        validate_package_id(&record.package_id)?;
        validate_lifecycle_record(&record)?;
        if self.packages.contains_key(&record.package_id) {
            return Err("package ID is already registered".into());
        }
        let occupied = self.namespace_owners();
        let manifest = lifecycle_manifest(&record);
        validate_namespace_claims(
            &manifest,
            &record.source,
            &occupied,
            trusted_curated.get(&record.package_id),
        )?;
        self.revision = self
            .revision
            .checked_add(1)
            .ok_or("registry revision exhausted")?;
        if record.status != PackageLifecycleStatusV1::RemovedRetainingData {
            self.load_order.push(record.package_id.clone());
        }
        self.packages.insert(record.package_id.clone(), record);
        Ok(self.revision)
    }

    pub fn prepare_activation(
        &mut self,
        package_id: &str,
        expected_registry_revision: u64,
        expected_package_revision: u64,
        operation_id: String,
        candidate: PackageArtifactPointerV1,
        candidate_source: PackageProvenanceV1,
        trusted_curated: &BTreeMap<String, PackageProvenanceV1>,
    ) -> Result<PackageActivationJournalV1, String> {
        self.validate(trusted_curated)?;
        if self.revision != expected_registry_revision {
            return Err(format!(
                "package registry revision conflict: {}",
                self.revision
            ));
        }
        let occupied = self.namespace_owners();
        let record = self
            .packages
            .get_mut(package_id)
            .ok_or_else(|| "package is not registered".to_string())?;
        if record.revision != expected_package_revision {
            return Err(format!("package revision conflict: {}", record.revision));
        }
        validate_identifier(&operation_id, 128, "activation operation id")?;
        validate_digest(&candidate.package_sha256, "candidate package")?;
        validate_version(&candidate.version, "candidate version")?;
        if candidate.runtime_generation == 0
            || !source_matches_pointer(&candidate_source, package_id, &candidate)
        {
            return Err("candidate source does not match its artifact pointer".into());
        }
        let candidate_manifest = PackageManifestV2 {
            manifest_version: MANIFEST_VERSION_V2,
            package_contract: PACKAGE_CONTRACT_V1.into(),
            id: package_id.into(),
            version: candidate.version.clone(),
            min_vqro_version: "0.0.0".into(),
            namespaces: record.namespaces.clone(),
            services: Vec::new(),
        };
        validate_namespace_claims(
            &candidate_manifest.into(),
            &candidate_source,
            &occupied,
            trusted_curated.get(package_id),
        )?;
        if matches!(candidate_source, PackageProvenanceV1::Link { .. }) {
            let candidate_record = PackageLifecycleRecordV1 {
                package_id: package_id.into(),
                source: candidate_source.clone(),
                previous_source: None,
                namespaces: record.namespaces.clone(),
                revision: 1,
                status: PackageLifecycleStatusV1::Disabled,
                active: None,
                previous: None,
                activation_error: None,
                pending_activation: None,
                retained_data: true,
            };
            revalidate_developer_link(&candidate_record, &occupied)?;
        }
        let previous = record.active.clone();
        if previous
            .as_ref()
            .is_some_and(|active| candidate.runtime_generation <= active.runtime_generation)
        {
            return Err("candidate runtime generation must advance".into());
        }
        let previous_source = previous.as_ref().map(|_| record.source.clone());
        let next_package_revision = record
            .revision
            .checked_add(1)
            .ok_or("package revision exhausted")?;
        let next_registry_revision = self
            .revision
            .checked_add(1)
            .ok_or("registry revision exhausted")?;
        let journal = PackageActivationJournalV1 {
            contract: ACTIVATION_JOURNAL_CONTRACT_V1.into(),
            operation_id,
            package_id: package_id.into(),
            expected_registry_revision: next_registry_revision,
            expected_package_revision: next_package_revision,
            phase: ActivationPhaseV1::Prepared,
            previous: previous.clone(),
            previous_source: previous_source.clone(),
            candidate,
            candidate_source,
        };
        journal.validate()?;
        record.previous = previous;
        record.previous_source = previous_source;
        record.status = PackageLifecycleStatusV1::Activating;
        record.activation_error = None;
        record.pending_activation = Some(PendingActivationV1 {
            operation_id: journal.operation_id.clone(),
            candidate: journal.candidate.clone(),
        });
        record.revision = next_package_revision;
        self.revision = next_registry_revision;
        Ok(journal)
    }

    pub fn commit_activation(
        &mut self,
        journal: &PackageActivationJournalV1,
    ) -> Result<(), String> {
        journal.validate()?;
        if journal.phase != ActivationPhaseV1::ArtifactCommitted {
            return Err("candidate artifact is not committed".into());
        }
        let record = self
            .packages
            .get_mut(&journal.package_id)
            .ok_or_else(|| "package is not registered".to_string())?;
        if record.active.as_ref() == Some(&journal.candidate) {
            return Ok(());
        }
        if self.revision != journal.expected_registry_revision
            || record.revision != journal.expected_package_revision
            || record.active != journal.previous
            || record.previous != journal.previous
            || record.previous_source != journal.previous_source
            || record.status != PackageLifecycleStatusV1::Activating
            || !record.pending_activation.as_ref().is_some_and(|pending| {
                pending.operation_id == journal.operation_id
                    && pending.candidate == journal.candidate
            })
        {
            return Err("package activation state changed after journal preparation".into());
        }
        record.active = Some(journal.candidate.clone());
        record.source = journal.candidate_source.clone();
        record.previous = journal.previous.clone();
        record.previous_source = journal.previous_source.clone();
        record.status = PackageLifecycleStatusV1::Enabled;
        record.activation_error = None;
        record.pending_activation = None;
        record.revision = record
            .revision
            .checked_add(1)
            .ok_or("package revision exhausted")?;
        self.revision = self
            .revision
            .checked_add(1)
            .ok_or("registry revision exhausted")?;
        Ok(())
    }

    pub fn fail_activation(
        &mut self,
        journal: &PackageActivationJournalV1,
        error: String,
    ) -> Result<(), String> {
        journal.validate()?;
        if journal.phase == ActivationPhaseV1::RegistryCommitted {
            return Err("committed activation cannot be failed".into());
        }
        let record = self
            .packages
            .get_mut(&journal.package_id)
            .ok_or_else(|| "package is not registered".to_string())?;
        if self.revision != journal.expected_registry_revision
            || record.revision != journal.expected_package_revision
            || record.status != PackageLifecycleStatusV1::Activating
            || record.active != journal.previous
            || record.previous != journal.previous
            || record.previous_source != journal.previous_source
            || !record.pending_activation.as_ref().is_some_and(|pending| {
                pending.operation_id == journal.operation_id
                    && pending.candidate == journal.candidate
            })
        {
            return Err("stale or unrelated activation failure".into());
        }
        let next_package_revision = record
            .revision
            .checked_add(1)
            .ok_or("package revision exhausted")?;
        let next_registry_revision = self
            .revision
            .checked_add(1)
            .ok_or("registry revision exhausted")?;
        record.status = PackageLifecycleStatusV1::Failed;
        record.activation_error = Some(error);
        record.pending_activation = None;
        record.revision = next_package_revision;
        self.revision = next_registry_revision;
        Ok(())
    }

    pub fn rollback(&mut self, package_id: &str) -> Result<(), String> {
        let record = self
            .packages
            .get_mut(package_id)
            .ok_or_else(|| "package is not registered".to_string())?;
        let previous = record
            .previous
            .take()
            .ok_or_else(|| "package has no rollback artifact".to_string())?;
        let previous_source = record
            .previous_source
            .take()
            .ok_or_else(|| "package has no rollback provenance".to_string())?;
        if record
            .active
            .as_ref()
            .is_some_and(|active| previous.runtime_generation >= active.runtime_generation)
        {
            return Err("rollback generation is not older than the active generation".into());
        }
        record.previous = record.active.replace(previous);
        record.previous_source = Some(std::mem::replace(&mut record.source, previous_source));
        record.status = PackageLifecycleStatusV1::Enabled;
        record.activation_error = None;
        record.revision = record
            .revision
            .checked_add(1)
            .ok_or("package revision exhausted")?;
        self.revision = self
            .revision
            .checked_add(1)
            .ok_or("registry revision exhausted")?;
        Ok(())
    }

    pub fn remove_retaining_data(&mut self, package_id: &str) -> Result<(), String> {
        let record = self
            .packages
            .get_mut(package_id)
            .ok_or_else(|| "package is not registered".to_string())?;
        record.status = PackageLifecycleStatusV1::RemovedRetainingData;
        record.retained_data = true;
        record.activation_error = None;
        record.revision = record
            .revision
            .checked_add(1)
            .ok_or("package revision exhausted")?;
        self.revision = self
            .revision
            .checked_add(1)
            .ok_or("registry revision exhausted")?;
        self.load_order.retain(|id| id != package_id);
        Ok(())
    }

    pub fn namespace_owners(&self) -> BTreeMap<String, String> {
        self.packages
            .iter()
            .filter(|(_, record)| {
                record.retained_data
                    || record.status != PackageLifecycleStatusV1::RemovedRetainingData
            })
            .flat_map(|(id, record)| {
                record
                    .namespaces
                    .iter()
                    .cloned()
                    .map(|namespace| (namespace, id.clone()))
            })
            .collect()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ActivationPhaseV1 {
    Prepared,
    ArtifactCommitted,
    RegistryCommitted,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PackageActivationJournalV1 {
    pub contract: String,
    pub operation_id: String,
    pub package_id: String,
    pub expected_registry_revision: u64,
    pub expected_package_revision: u64,
    pub phase: ActivationPhaseV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous: Option<PackageArtifactPointerV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_source: Option<PackageProvenanceV1>,
    pub candidate: PackageArtifactPointerV1,
    pub candidate_source: PackageProvenanceV1,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActivationRecoveryV1 {
    CommitCandidate,
    KeepCommittedCandidate,
    RollBackCandidate,
    Conflict,
}

fn source_matches_pointer(
    source: &PackageProvenanceV1,
    package_id: &str,
    pointer: &PackageArtifactPointerV1,
) -> bool {
    match source {
        PackageProvenanceV1::Curated {
            package_id: source_id,
            version,
            package_sha256,
            ..
        } => {
            source_id == package_id
                && version == &pointer.version
                && package_sha256 == &pointer.package_sha256
        }
        PackageProvenanceV1::File { package_sha256 } => package_sha256 == &pointer.package_sha256,
        PackageProvenanceV1::Git {
            package_id: source_id,
            version,
            package_sha256,
            ..
        } => {
            source_id == package_id
                && version == &pointer.version
                && package_sha256 == &pointer.package_sha256
        }
        PackageProvenanceV1::Link {
            package_id: source_id,
            version,
            package_sha256,
            ..
        } => {
            source_id == package_id
                && version == &pointer.version
                && package_sha256 == &pointer.package_sha256
        }
    }
}

impl PackageActivationJournalV1 {
    pub fn validate(&self) -> Result<(), String> {
        if self.contract != ACTIVATION_JOURNAL_CONTRACT_V1 {
            return Err("unsupported package activation journal contract".into());
        }
        if self.expected_registry_revision == 0 || self.expected_package_revision == 0 {
            return Err("activation journal revisions must be nonzero".into());
        }
        validate_identifier(&self.operation_id, 128, "activation operation id")?;
        validate_package_id(&self.package_id)?;
        validate_digest(&self.candidate.package_sha256, "candidate package")?;
        validate_version(&self.candidate.version, "candidate version")?;
        if self.candidate.runtime_generation == 0
            || !source_matches_pointer(&self.candidate_source, &self.package_id, &self.candidate)
        {
            return Err("candidate source does not match its artifact pointer".into());
        }
        if self.previous.is_some() != self.previous_source.is_some() {
            return Err("previous artifact and provenance must be recorded together".into());
        }
        if let Some(previous) = &self.previous {
            validate_digest(&previous.package_sha256, "previous package")?;
            validate_version(&previous.version, "previous version")?;
            let Some(previous_source) = self.previous_source.as_ref() else {
                return Err("previous artifact has no provenance".into());
            };
            if !source_matches_pointer(previous_source, &self.package_id, previous) {
                return Err("previous source does not match its artifact pointer".into());
            }
            if self.candidate.runtime_generation <= previous.runtime_generation {
                return Err("candidate runtime generation does not advance".into());
            }
        }
        Ok(())
    }

    pub fn advance(
        &mut self,
        expected: ActivationPhaseV1,
        next: ActivationPhaseV1,
    ) -> Result<(), String> {
        if self.phase != expected
            || !matches!(
                (expected, next),
                (
                    ActivationPhaseV1::Prepared,
                    ActivationPhaseV1::ArtifactCommitted
                ) | (
                    ActivationPhaseV1::ArtifactCommitted,
                    ActivationPhaseV1::RegistryCommitted
                )
            )
        {
            return Err("invalid package activation phase transition".into());
        }
        self.phase = next;
        Ok(())
    }

    pub fn recovery(&self, active: Option<&PackageArtifactPointerV1>) -> ActivationRecoveryV1 {
        if active == Some(&self.candidate) {
            return ActivationRecoveryV1::KeepCommittedCandidate;
        }
        if active == self.previous.as_ref() {
            return match self.phase {
                ActivationPhaseV1::Prepared => ActivationRecoveryV1::RollBackCandidate,
                ActivationPhaseV1::ArtifactCommitted => ActivationRecoveryV1::CommitCandidate,
                ActivationPhaseV1::RegistryCommitted => ActivationRecoveryV1::Conflict,
            };
        }
        ActivationRecoveryV1::Conflict
    }
}

pub fn write_activation_journal(
    path: &Path,
    journal: &PackageActivationJournalV1,
) -> Result<(), String> {
    journal.validate()?;
    let bytes = serde_json::to_vec(journal)
        .map_err(|error| format!("encode package activation journal: {error}"))?;
    if bytes.len() as u64 > MAX_ACTIVATION_JOURNAL_BYTES {
        return Err("package activation journal exceeds its size limit".into());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("create package journal directory: {error}"))?;
    }
    let temporary = path.with_extension(format!(
        "{}.{}.tmp",
        std::process::id(),
        NEXT_PACKAGE_TEMP.fetch_add(1, Ordering::Relaxed)
    ));
    let mut file = File::create(&temporary)
        .map_err(|error| format!("create package activation journal: {error}"))?;
    if let Err(error) = file.write_all(&bytes).and_then(|()| file.sync_all()) {
        let _ = fs::remove_file(&temporary);
        return Err(format!("write package activation journal: {error}"));
    }
    if let Err(error) = crate::platform::durable_replace_file(&temporary, path) {
        let _ = fs::remove_file(&temporary);
        return Err(format!("commit package activation journal: {error}"));
    }
    Ok(())
}

pub fn read_activation_journal(path: &Path) -> Result<Option<PackageActivationJournalV1>, String> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("inspect package activation journal: {error}")),
    };
    if metadata.len() > MAX_ACTIVATION_JOURNAL_BYTES {
        return Err("package activation journal exceeds its size limit".into());
    }
    let journal: PackageActivationJournalV1 = serde_json::from_slice(
        &fs::read(path).map_err(|error| format!("read package activation journal: {error}"))?,
    )
    .map_err(|error| format!("decode package activation journal: {error}"))?;
    journal.validate()?;
    Ok(Some(journal))
}

pub fn remove_activation_journal(path: &Path) -> Result<(), String> {
    match fs::remove_file(path) {
        Ok(()) => crate::platform::sync_containing_directory(path)
            .map_err(|error| format!("sync package journal removal: {error}")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("remove package activation journal: {error}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn asset_manifest(id: &str, namespaces: &[&str]) -> String {
        format!(
            "manifest_version = 2\npackage_contract = \"vqro.package.v1\"\nid = \"{id}\"\nversion = \"1.2.3\"\nmin_vqro_version = \"0.1.0\"\nnamespaces = [{}]\n",
            namespaces
                .iter()
                .map(|namespace| format!("\"{namespace}\""))
                .collect::<Vec<_>>()
                .join(", ")
        )
    }

    fn process_manifest() -> String {
        format!(
            "{}\n[[services]]\nid = \"main\"\nprovides = [\"host_document\"]\ncapabilities = [\"state\"]\n\n[services.runtime]\nkind = \"process\"\nexecutables = {{ linux-x86_64 = \"services/linux-x86_64/service\", linux-aarch64 = \"services/linux-aarch64/service\", macos-x86_64 = \"services/macos-x86_64/service\", macos-aarch64 = \"services/macos-aarch64/service\", windows-x86_64 = \"services/windows-x86_64/service.exe\" }}\nargs = [\"--stdio\"]\n",
            asset_manifest("example.process", &["example.process"])
        )
    }

    fn package_bytes(mut files: Vec<(String, Vec<u8>, u32)>) -> Vec<u8> {
        files.sort_by(|left, right| left.0.cmp(&right.0));
        let checksums = files
            .iter()
            .map(|(path, bytes, _)| format!("{}  {path}\n", sha256(bytes)))
            .collect::<String>();
        files.push((PACKAGE_CHECKSUM_FILE.into(), checksums.into_bytes(), 0o644));
        files.sort_by(|left, right| left.0.cmp(&right.0));
        archive_entries(files, None)
    }

    fn archive_entries(
        files: Vec<(String, Vec<u8>, u32)>,
        special: Option<(String, tar::EntryType)>,
    ) -> Vec<u8> {
        let mut bytes = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut bytes);
            builder.mode(tar::HeaderMode::Deterministic);
            for (path, contents, mode) in files {
                let mut header = tar::Header::new_ustar();
                header.set_path(path).unwrap();
                header.set_size(contents.len() as u64);
                header.set_mode(mode);
                header.set_uid(0);
                header.set_gid(0);
                header.set_mtime(0);
                header.set_cksum();
                builder.append(&header, contents.as_slice()).unwrap();
            }
            if let Some((path, entry_type)) = special {
                let mut header = tar::Header::new_ustar();
                header.set_path(path).unwrap();
                header.set_size(0);
                header.set_mode(0o644);
                header.set_uid(0);
                header.set_gid(0);
                header.set_mtime(0);
                header.set_entry_type(entry_type);
                if entry_type.is_symlink() || entry_type.is_hard_link() {
                    header.set_link_name("target").unwrap();
                }
                header.set_cksum();
                builder.append(&header, std::io::empty()).unwrap();
            }
            builder.finish().unwrap();
        }
        bytes
    }

    fn asset_package(id: &str, namespaces: &[&str]) -> Vec<u8> {
        package_bytes(vec![(
            PACKAGE_MANIFEST_FILE.into(),
            asset_manifest(id, namespaces).into_bytes(),
            0o644,
        )])
    }

    fn write_directory_package(root: &Path, package: &[u8]) {
        let validated = validate_package_bytes(package, None).unwrap();
        for (path, (bytes, _)) in &validated.files {
            let output = root.join(path);
            if let Some(parent) = output.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(output, bytes).unwrap();
        }
    }

    #[cfg(unix)]
    fn make_writable(path: &Path) {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(path).unwrap().permissions();
        permissions.set_mode(permissions.mode() | 0o200);
        fs::set_permissions(path, permissions).unwrap();
    }

    // Windows exposes only its native read-only attribute through Permissions.
    #[cfg(windows)]
    #[allow(clippy::permissions_set_readonly_false)]
    fn make_writable(path: &Path) {
        let mut permissions = fs::metadata(path).unwrap().permissions();
        permissions.set_readonly(false);
        fs::set_permissions(path, permissions).unwrap();
    }

    fn temp_dir(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "vqro-package-{label}-{}-{}",
            std::process::id(),
            NEXT_PACKAGE_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn package_source_is_reproducible_and_inventory_is_exact() {
        let first = asset_package("example.assets", &["example.assets"]);
        let second = asset_package("example.assets", &["example.assets"]);
        assert_eq!(first, second);
        let validated = validate_package_bytes(&first, None).unwrap();
        assert_eq!(validated.descriptor.package_sha256, sha256(&second));
        assert_eq!(validated.descriptor.files.len(), 1);
        assert_eq!(validated.descriptor.files[0].path, PACKAGE_MANIFEST_FILE);
    }

    #[test]
    fn archive_rejects_links_special_types_and_non_deterministic_headers() {
        for entry_type in [
            tar::EntryType::Symlink,
            tar::EntryType::Link,
            tar::EntryType::Char,
            tar::EntryType::Block,
            tar::EntryType::Fifo,
            tar::EntryType::GNUSparse,
        ] {
            let bytes = archive_entries(
                vec![(
                    PACKAGE_MANIFEST_FILE.into(),
                    asset_manifest("example.bad", &[]).into_bytes(),
                    0o644,
                )],
                Some(("zz-special".into(), entry_type)),
            );
            assert!(
                validate_package_bytes(&bytes, None).is_err(),
                "{entry_type:?}"
            );
        }
        let mut files: Vec<(String, Vec<u8>, u32)> = vec![(
            PACKAGE_MANIFEST_FILE.into(),
            asset_manifest("example.bad", &[]).into_bytes(),
            0o600,
        )];
        let checksums = format!("{}  {}\n", sha256(&files[0].1), PACKAGE_MANIFEST_FILE);
        files.push((PACKAGE_CHECKSUM_FILE.into(), checksums.into_bytes(), 0o644));
        files.sort_by(|left, right| left.0.cmp(&right.0));
        assert!(validate_package_bytes(&archive_entries(files, None), None)
            .unwrap_err()
            .contains("deterministic tar profile"));
    }

    #[test]
    fn portable_paths_reject_traversal_windows_names_and_bounds() {
        for invalid in [
            "../escape",
            "/absolute",
            "assets/CON.txt",
            "assets/name.",
            "assets/name ",
            "assets/back\\slash",
            "assets/colon:name",
        ] {
            assert!(
                portable_package_path(Path::new(invalid)).is_err(),
                "{invalid}"
            );
        }
        assert!(portable_package_path(Path::new("assets/good-name_1.json")).is_ok());
        let deep = std::iter::repeat_n("a", MAX_PATH_DEPTH + 1)
            .collect::<Vec<_>>()
            .join("/");
        assert!(portable_package_path(Path::new(&deep)).is_err());
        assert!(
            portable_package_path(Path::new(&"a".repeat(MAX_PATH_COMPONENT_BYTES + 1))).is_err()
        );
    }

    #[test]
    fn archive_rejects_order_case_and_file_descendant_collisions() {
        let manifest = asset_manifest("example.collision", &[]).into_bytes();
        let checksum = format!("{}  {}\n", sha256(&manifest), PACKAGE_MANIFEST_FILE);
        let unsorted = archive_entries(
            vec![
                (PACKAGE_MANIFEST_FILE.into(), manifest.clone(), 0o644),
                (
                    PACKAGE_CHECKSUM_FILE.into(),
                    checksum.clone().into_bytes(),
                    0o644,
                ),
            ],
            None,
        );
        assert!(validate_package_bytes(&unsorted, None)
            .unwrap_err()
            .contains("strict bytewise order"));
        for files in [
            vec![
                ("A".into(), Vec::new(), 0o644),
                ("a".into(), Vec::new(), 0o644),
            ],
            vec![
                ("a".into(), Vec::new(), 0o644),
                ("a/b".into(), Vec::new(), 0o644),
            ],
        ] {
            assert!(validate_package_bytes(&archive_entries(files, None), None).is_err());
        }
    }

    #[test]
    fn checksum_manifest_rejects_missing_extra_malformed_unsorted_and_mismatch() {
        let manifest = asset_manifest("example.checksum", &[]).into_bytes();
        let valid_digest = sha256(&manifest);
        assert_eq!(
            parse_checksums(format!("{}  nested\\file\n", "0".repeat(64)).as_bytes()).unwrap_err(),
            "checksum path must use canonical forward slashes"
        );
        for checksum in [
            String::new(),
            format!("{}  missing.txt\n", "0".repeat(64)),
            format!("{} {}\n", valid_digest, PACKAGE_MANIFEST_FILE),
            format!("{}  {}\r\n", valid_digest, PACKAGE_MANIFEST_FILE),
            format!("{}  {}", valid_digest, PACKAGE_MANIFEST_FILE),
            format!("{}  {}\n", "0".repeat(64), PACKAGE_MANIFEST_FILE),
            format!(
                "{}  {}\n",
                valid_digest.to_uppercase(),
                PACKAGE_MANIFEST_FILE
            ),
        ] {
            let bytes = archive_entries(
                vec![
                    (PACKAGE_CHECKSUM_FILE.into(), checksum.into_bytes(), 0o644),
                    (PACKAGE_MANIFEST_FILE.into(), manifest.clone(), 0o644),
                ],
                None,
            );
            assert!(validate_package_bytes(&bytes, None).is_err());
        }
    }

    #[test]
    fn package_digest_is_checked_before_archive_parsing() {
        let error = validate_package_bytes(b"not a tar", Some(&"0".repeat(64))).unwrap_err();
        assert_eq!(error, "package digest mismatch");
        assert!(validate_package_bytes(b"not a tar", Some("short"))
            .unwrap_err()
            .contains("not SHA-256"));
    }

    #[cfg(unix)]
    #[test]
    fn package_file_reads_are_no_follow_and_stream_bounded() {
        use std::os::unix::fs::symlink;

        let root = temp_dir("bounded-read");
        let package = root.join("package.vqrox");
        fs::write(&package, asset_package("example.read", &[])).unwrap();
        symlink(&package, root.join("linked.vqrox")).unwrap();
        assert!(validate_package_file(&root.join("linked.vqrox"), None).is_err());
        let oversized = root.join("oversized.vqrox");
        let file = File::create(&oversized).unwrap();
        file.set_len(MAX_PACKAGE_BYTES + 1).unwrap();
        assert!(validate_package_file(&oversized, None).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn manifest_v2_rejects_identity_version_runtime_and_self_spawn_errors() {
        let base = PackageManifestV2 {
            manifest_version: 2,
            package_contract: PACKAGE_CONTRACT_V1.into(),
            id: "example.valid".into(),
            version: "1.2.3".into(),
            min_vqro_version: "0.1.0".into(),
            namespaces: vec!["example.valid".into()],
            services: Vec::new(),
        };
        let mut cases = Vec::new();
        let mut invalid = base.clone();
        invalid.manifest_version = 1;
        cases.push(invalid);
        let mut invalid = base.clone();
        invalid.id = "HOST.bad".into();
        cases.push(invalid);
        let mut invalid = base.clone();
        invalid.version = "01.2.3".into();
        cases.push(invalid);
        let mut invalid = base.clone();
        invalid.services.push(PackageServiceV1 {
            id: "main".into(),
            provides: Vec::new(),
            capabilities: Vec::new(),
            runtime: PackageRuntimeV1::Process {
                executables: BTreeMap::from([(
                    "linux-x86_64".into(),
                    "services/linux-x86_64/vqro".into(),
                )]),
                args: vec!["_extension-service".into()],
            },
        });
        cases.push(invalid);
        for manifest in cases {
            assert!(validate_manifest(&manifest).is_err());
        }
        assert!(
            validate_manifest(&base).is_ok(),
            "asset-only packages are valid"
        );
    }

    #[test]
    fn manifest_parser_rejects_unknown_fields_and_ambiguous_runtime_shapes() {
        let unknown = format!("{}unknown = true\n", asset_manifest("example.unknown", &[]));
        assert!(toml::from_str::<PackageManifestV2>(&unknown).is_err());
        let ambiguous = format!(
            "{}\n[[services]]\nid = \"main\"\nprovides = []\ncapabilities = []\n[services.runtime]\nkind = \"component\"\ncomponent = \"services/main.wasm\"\nworld = \"vqro:extension/service@1.0.0\"\nexecutables = {{ linux-x86_64 = \"services/linux-x86_64/service\" }}\n",
            asset_manifest("example.ambiguous", &[])
        );
        assert!(toml::from_str::<PackageManifestV2>(&ambiguous).is_err());
        let unknown_runtime = format!(
            "{}\n[[services]]\nid = \"main\"\nprovides = []\ncapabilities = []\n[services.runtime]\nkind = \"future\"\npayload = \"services/main\"\n",
            asset_manifest("example.future", &[])
        );
        assert!(toml::from_str::<PackageManifestV2>(&unknown_runtime).is_err());
    }

    #[test]
    fn manifest_v3_uses_exact_canonical_contract_descriptors() {
        let descriptor = br#"{
          "contract":"vqro.service-contract.v1",
          "contract_id":"example.echo",
          "contract_version":"1.0.0",
          "methods":[{
            "id":"echo",
            "request_schema":{"type":"object"},
            "response_schema":{"type":"object"},
            "request_information":["user.supplied.text"],
            "response_information":["provider.generated.text"],
            "max_request_bytes":4096,
            "max_response_bytes":4096
          }]
        }"#;
        let (_, digest) = parse_service_contract_descriptor(descriptor).unwrap();
        let manifest = format!(
            r#"manifest_version = 3
package_contract = "vqro.package.v1"
id = "example.consumer"
version = "1.0.0"
min_vqro_version = "0.1.0"
namespaces = ["example.consumer"]

[[services]]
id = "main"
capabilities = []

[[services.exports]]
id = "echo"
descriptor = "contracts/example.echo.v1.json"
descriptor_sha256 = "{digest}"

[[services.imports]]
id = "echo-source"
descriptor = "contracts/example.echo.v1.json"
descriptor_sha256 = "{digest}"
required = true

[services.runtime]
kind = "component"
component = "services/main.wasm"
world = "vqro:extension/service@1.0.0"
"#
        );
        let package = validate_package_bytes(
            &package_bytes(vec![
                (PACKAGE_MANIFEST_FILE.into(), manifest.into_bytes(), 0o644),
                (
                    "contracts/example.echo.v1.json".into(),
                    descriptor.to_vec(),
                    0o644,
                ),
                ("services/main.wasm".into(), b"\0asm".to_vec(), 0o644),
            ]),
            None,
        )
        .unwrap();
        assert_eq!(package.descriptor.manifest.manifest_version, 3);
        assert_eq!(package.descriptor.manifest.services[0].exports.len(), 1);
        assert_eq!(package.descriptor.manifest.services[0].imports.len(), 1);
        assert_eq!(
            package
                .descriptor
                .contract_descriptors
                .get(&digest)
                .unwrap()
                .contract_id,
            "example.echo"
        );

        let differently_formatted = br#"{"methods":[{"response_information":["provider.generated.text"],"request_schema":{"type":"object"},"id":"echo","max_response_bytes":4096,"response_schema":{"type":"object"},"request_information":["user.supplied.text"],"max_request_bytes":4096}],"contract_version":"1.0.0","contract_id":"example.echo","contract":"vqro.service-contract.v1"}"#;
        assert_eq!(
            parse_service_contract_descriptor(differently_formatted)
                .unwrap()
                .1,
            digest
        );
        let omitted_defaults = br#"{"contract":"vqro.service-contract.v1","contract_id":"example.defaults","contract_version":"1.0.0","methods":[{"id":"echo","request_schema":{},"response_schema":{},"max_request_bytes":1,"max_response_bytes":1}]}"#;
        let explicit_defaults = br#"{"contract":"vqro.service-contract.v1","contract_id":"example.defaults","contract_version":"1.0.0","methods":[{"id":"echo","request_schema":{},"response_schema":{},"request_information":[],"response_information":[],"max_request_bytes":1,"max_response_bytes":1}]}"#;
        assert_eq!(
            parse_service_contract_descriptor(omitted_defaults)
                .unwrap()
                .1,
            parse_service_contract_descriptor(explicit_defaults)
                .unwrap()
                .1
        );
    }

    #[test]
    fn manifest_v3_rejects_v2_provides_unknown_fields_and_duplicate_endpoints() {
        let raw = r#"manifest_version = 3
package_contract = "vqro.package.v1"
id = "example.invalid-v3"
version = "1.0.0"
min_vqro_version = "0.1.0"
[[services]]
id = "main"
provides = ["legacy"]
[services.runtime]
kind = "component"
component = "services/main.wasm"
world = "vqro:extension/service@1.0.0"
"#;
        assert!(toml::from_str::<PackageManifestV3>(raw).is_err());

        let endpoint = ServiceContractExportV1 {
            id: "same".into(),
            descriptor: "contracts/echo.json".into(),
            descriptor_sha256: "a".repeat(64),
        };
        let manifest = PackageManifestV3 {
            manifest_version: 3,
            package_contract: PACKAGE_CONTRACT_V1.into(),
            id: "example.invalid-v3".into(),
            version: "1.0.0".into(),
            min_vqro_version: "0.1.0".into(),
            namespaces: Vec::new(),
            services: vec![PackageServiceV2 {
                id: "main".into(),
                capabilities: Vec::new(),
                exports: vec![endpoint],
                imports: vec![ServiceContractImportV1 {
                    id: "same".into(),
                    descriptor: "contracts/echo.json".into(),
                    descriptor_sha256: "a".repeat(64),
                    required: true,
                }],
                runtime: PackageRuntimeV1::Component {
                    component: "services/main.wasm".into(),
                    world: "vqro:extension/service@1.0.0".into(),
                },
            }],
        };
        assert!(validate_manifest_v3(&manifest)
            .unwrap_err()
            .contains("duplicate endpoint"));
    }

    #[test]
    fn manifest_v3_rejects_ambiguous_provider_methods_across_contracts() {
        let first = br#"{"contract":"vqro.service-contract.v1","contract_id":"example.echo","contract_version":"1.0.0","methods":[{"id":"echo","request_schema":{},"response_schema":{},"request_information":[],"response_information":[],"max_request_bytes":1,"max_response_bytes":1}]}"#;
        let second = br#"{"contract":"vqro.service-contract.v1","contract_id":"example.other","contract_version":"1.0.0","methods":[{"id":"echo","request_schema":{},"response_schema":{},"request_information":[],"response_information":[],"max_request_bytes":1,"max_response_bytes":1}]}"#;
        let first_digest = parse_service_contract_descriptor(first).unwrap().1;
        let second_digest = parse_service_contract_descriptor(second).unwrap().1;
        let manifest = format!(
            r#"manifest_version = 3
package_contract = "vqro.package.v1"
id = "example.ambiguous"
version = "1.0.0"
min_vqro_version = "0.1.0"
[[services]]
id = "main"
[[services.exports]]
id = "first"
descriptor = "contracts/first.json"
descriptor_sha256 = "{first_digest}"
[[services.exports]]
id = "second"
descriptor = "contracts/second.json"
descriptor_sha256 = "{second_digest}"
[services.runtime]
kind = "component"
component = "services/main.wasm"
world = "vqro:extension/service@1.0.0"
"#
        );
        assert!(validate_package_bytes(
            &package_bytes(vec![
                (PACKAGE_MANIFEST_FILE.into(), manifest.into_bytes(), 0o644),
                ("contracts/first.json".into(), first.to_vec(), 0o644),
                ("contracts/second.json".into(), second.to_vec(), 0o644),
                ("services/main.wasm".into(), b"\0asm".to_vec(), 0o644),
            ]),
            None,
        )
        .unwrap_err()
        .contains("ambiguous method echo"));
    }

    #[test]
    fn contract_descriptor_rejects_duplicate_keys_floats_and_digest_mismatch() {
        assert!(parse_service_contract_descriptor(
            br#"{"contract":"vqro.service-contract.v1","contract":"vqro.service-contract.v1"}"#
        )
        .unwrap_err()
        .contains("duplicate JSON object key"));
        assert!(parse_service_contract_descriptor(
            br#"{"contract":"vqro.service-contract.v1","contract_id":"example.echo","contract_version":"1.0.0","methods":[],"extra":0.5}"#
        )
        .unwrap_err()
        .contains("floating-point"));
        assert!(parse_service_contract_descriptor(
            br#"{"contract":"vqro.service-contract.v1","contract_id":"example.echo","contract_version":"1.0.0","methods":[{"id":"echo","request_schema":{"type":7},"response_schema":{},"request_information":[],"response_information":[],"max_request_bytes":1,"max_response_bytes":1}]}"#
        )
        .unwrap_err()
        .contains("type is invalid"));
        assert!(parse_service_contract_descriptor(
            br#"{"contract":"vqro.service-contract.v1","contract_id":"example.echo","contract_version":"1.0.0","methods":[{"id":"echo","request_schema":{"invented":true},"response_schema":{},"request_information":[],"response_information":[],"max_request_bytes":1,"max_response_bytes":1}]}"#
        )
        .unwrap_err()
        .contains("unsupported keyword"));

        let descriptor = br#"{"contract":"vqro.service-contract.v1","contract_id":"example.echo","contract_version":"1.0.0","methods":[{"id":"echo","request_schema":{},"response_schema":{},"request_information":[],"response_information":[],"max_request_bytes":1,"max_response_bytes":1}]}"#;
        let manifest = format!(
            r#"manifest_version = 3
package_contract = "vqro.package.v1"
id = "example.bad-digest"
version = "1.0.0"
min_vqro_version = "0.1.0"
[[services]]
id = "main"
[[services.exports]]
id = "echo"
descriptor = "contracts/echo.json"
descriptor_sha256 = "{}"
[services.runtime]
kind = "component"
component = "services/main.wasm"
world = "vqro:extension/service@1.0.0"
"#,
            "0".repeat(64)
        );
        assert!(validate_package_bytes(
            &package_bytes(vec![
                (PACKAGE_MANIFEST_FILE.into(), manifest.into_bytes(), 0o644),
                ("contracts/echo.json".into(), descriptor.to_vec(), 0o644),
                ("services/main.wasm".into(), b"\0asm".to_vec(), 0o644),
            ]),
            None,
        )
        .unwrap_err()
        .contains("descriptor digest mismatch"));
    }

    #[test]
    fn manifest_v2_does_not_gain_fabric_fields() {
        let manifest = format!(
            "{}\n[[services]]\nid = \"main\"\nprovides = []\ncapabilities = []\nexports = []\n[services.runtime]\nkind = \"component\"\ncomponent = \"services/main.wasm\"\nworld = \"vqro:extension/service@1.0.0\"\n",
            asset_manifest("example.v2-fabric", &[])
        );
        assert!(toml::from_str::<PackageManifestV2>(&manifest).is_err());
    }

    #[test]
    fn package_size_and_entry_limits_fail_before_unbounded_allocation() {
        let oversized = vec![0_u8; MAX_PACKAGE_BYTES as usize + 1];
        assert_eq!(
            validate_package_bytes(&oversized, None).unwrap_err(),
            "package archive exceeds 8 MiB"
        );
        let entries = (0..=MAX_PACKAGE_ENTRIES)
            .map(|index| (format!("f{index:05}"), Vec::new(), 0o644))
            .collect();
        assert!(
            validate_package_bytes(&archive_entries(entries, None), None)
                .unwrap_err()
                .contains("too many entries")
        );
    }

    #[test]
    fn developer_link_counts_directories_and_enforces_directory_depth() {
        let root = temp_dir("link-directory-limits");
        for index in 0..=MAX_PACKAGE_ENTRIES {
            fs::create_dir(root.join(format!("d{index:05}"))).unwrap();
        }
        assert!(validate_directory_package(&root)
            .unwrap_err()
            .contains("too many files or directories"));
        fs::remove_dir_all(&root).unwrap();

        let root = temp_dir("link-directory-depth");
        let mut deep = root.clone();
        for index in 0..=MAX_PATH_DEPTH {
            deep.push(format!("d{index}"));
        }
        fs::create_dir_all(&deep).unwrap();
        assert!(validate_directory_package(&root)
            .unwrap_err()
            .contains("depth or length"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn process_runtime_resolves_exact_payload_for_all_targets() {
        let files = all_target_names()
            .into_iter()
            .map(|target| {
                let suffix = if target == "windows-x86_64" {
                    ".exe"
                } else {
                    ""
                };
                (
                    format!("services/{target}/service{suffix}"),
                    format!("payload-{target}").into_bytes(),
                    if target == "windows-x86_64" {
                        0o644
                    } else {
                        0o755
                    },
                )
            })
            .chain(std::iter::once((
                PACKAGE_MANIFEST_FILE.into(),
                process_manifest().into_bytes(),
                0o644,
            )))
            .collect();
        let package = validate_package_bytes(&package_bytes(files), None).unwrap();
        for target in [
            PackageTargetV1::LinuxX86_64,
            PackageTargetV1::LinuxAarch64,
            PackageTargetV1::MacosX86_64,
            PackageTargetV1::MacosAarch64,
            PackageTargetV1::WindowsX86_64,
        ] {
            let resolved = resolve_runtime(&package.descriptor, "main", target).unwrap();
            assert!(resolved.payload_path.contains(target.as_str()));
            assert_eq!(resolved.kind, "process");
        }
    }

    #[test]
    fn runtime_inventory_rejects_missing_non_executable_and_invalid_component_payloads() {
        let missing = package_bytes(vec![(
            PACKAGE_MANIFEST_FILE.into(),
            process_manifest().into_bytes(),
            0o644,
        )]);
        assert!(validate_package_bytes(&missing, None)
            .unwrap_err()
            .contains("absent"));

        let component = format!(
            "{}\n[[services]]\nid = \"main\"\nprovides = []\ncapabilities = []\n[services.runtime]\nkind = \"component\"\ncomponent = \"services/main.wasm\"\nworld = \"vqro:extension/service@1.0.0\"\n",
            asset_manifest("example.component", &[])
        );
        let invalid = package_bytes(vec![
            ("services/main.wasm".into(), b"not wasm".to_vec(), 0o644),
            (PACKAGE_MANIFEST_FILE.into(), component.into_bytes(), 0o644),
        ]);
        assert!(validate_package_bytes(&invalid, None)
            .unwrap_err()
            .contains("not WebAssembly"));
    }

    #[test]
    fn provenance_enforces_reserved_first_party_trust_and_collisions() {
        let manifest: PackageManifestV2 =
            toml::from_str(&asset_manifest("example.package", &["vqro.collections"])).unwrap();
        let file = PackageProvenanceV1::File {
            package_sha256: "0".repeat(64),
        };
        assert!(
            validate_namespace_claims(&manifest.clone().into(), &file, &BTreeMap::new(), None)
                .is_err()
        );
        let curated = PackageProvenanceV1::Curated {
            catalog: "builtin-v1".into(),
            publisher: "vqro".into(),
            repository: "vqro/collections".into(),
            package_id: "example.package".into(),
            version: "1.2.3".into(),
            source_commit: "1".repeat(40),
            package_sha256: "0".repeat(64),
            manifest_sha256: "2".repeat(64),
            authorized_namespaces: vec!["vqro.collections".into()],
        };
        let manifest: ValidatedPackageManifestV1 = manifest.into();
        assert!(validate_namespace_claims(&manifest, &curated, &BTreeMap::new(), None).is_err());
        assert!(
            validate_namespace_claims(&manifest, &curated, &BTreeMap::new(), Some(&curated))
                .is_ok()
        );
        let occupied = BTreeMap::from([("vqro.collections".into(), "other.package".into())]);
        assert!(validate_namespace_claims(&manifest, &curated, &occupied, Some(&curated)).is_err());
        let host: PackageManifestV2 =
            toml::from_str(&asset_manifest("example.package", &["host.terminals"])).unwrap();
        assert!(validate_namespace_claims(
            &host.into(),
            &curated,
            &BTreeMap::new(),
            Some(&curated)
        )
        .is_err());
    }

    #[test]
    fn spaces_reserved_namespace_requires_the_exact_production_pin() {
        let pin = crate::spaces_extension_contract::embedded_spaces_production_pin().unwrap();
        let manifest: PackageManifestV2 =
            toml::from_str(&asset_manifest(&pin.package_id, &[&pin.namespace])).unwrap();
        let exact = PackageProvenanceV1::Git {
            url: pin.source_url.clone(),
            source_commit: pin.source_commit.clone(),
            package_path: pin.package_path.clone(),
            package_id: pin.package_id.clone(),
            version: pin.version.clone(),
            package_sha256: pin.package_sha256.clone(),
            manifest_sha256: pin.manifest_sha256.clone(),
        };
        let manifest: ValidatedPackageManifestV1 = manifest.into();
        assert!(trusted_first_party_git_namespace(
            &exact,
            &manifest,
            &pin.namespace
        ));
        let mut wrong_commit = exact.clone();
        let PackageProvenanceV1::Git { source_commit, .. } = &mut wrong_commit else {
            unreachable!()
        };
        *source_commit = "0".repeat(40);
        assert!(!trusted_first_party_git_namespace(
            &wrong_commit,
            &manifest,
            &pin.namespace
        ));
    }

    #[test]
    fn immutable_file_install_is_content_addressed_idempotent_and_detects_mutation() {
        let root = temp_dir("install");
        let source = root.join("example.vqrox");
        let bytes = asset_package("example.install", &["example.install"]);
        fs::write(&source, &bytes).unwrap();
        let digest = sha256(&bytes);
        let provenance = PackageProvenanceV1::File {
            package_sha256: digest.clone(),
        };
        let store = root.join("store");
        let first = install_local_file(
            &source,
            &store,
            Some(&digest),
            provenance.clone(),
            &BTreeMap::new(),
            None,
        )
        .unwrap();
        let second = install_local_file(
            &source,
            &store,
            Some(&digest),
            provenance,
            &BTreeMap::new(),
            None,
        )
        .unwrap();
        assert_eq!(first, second);
        assert!(first.immutable_root.ends_with(&digest));
        let installed_root = Path::new(&first.immutable_root);
        let manifest = installed_root.join(PACKAGE_MANIFEST_FILE);
        make_writable(&manifest);
        fs::write(&manifest, b"tampered").unwrap();
        assert!(verify_installed_package(installed_root, &digest).is_err());
        make_writable(&store);
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn immutable_install_rejects_a_symlink_at_the_digest_destination() {
        use std::os::unix::fs::symlink;

        let root = temp_dir("install-symlink");
        let source = root.join("example.vqrox");
        let bytes = asset_package("example.symlink", &["example.symlink"]);
        fs::write(&source, &bytes).unwrap();
        let digest = sha256(&bytes);
        let destination_parent = root.join("store/sha256");
        let external = root.join("external");
        fs::create_dir_all(&destination_parent).unwrap();
        fs::create_dir_all(&external).unwrap();
        symlink(&external, destination_parent.join(&digest)).unwrap();
        assert!(install_local_file(
            &source,
            &root.join("store"),
            Some(&digest),
            PackageProvenanceV1::File {
                package_sha256: digest.clone(),
            },
            &BTreeMap::new(),
            None,
        )
        .unwrap_err()
        .contains("not a real directory"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn developer_link_requires_explicit_trust_and_rejects_symlinks() {
        let root = temp_dir("link");
        let package = asset_package("example.link", &["example.link"]);
        write_directory_package(&root, &package);
        assert!(
            register_developer_link(&root, "approval".into(), false, &BTreeMap::new()).is_err()
        );
        let linked =
            register_developer_link(&root, "approval".into(), true, &BTreeMap::new()).unwrap();
        assert_eq!(linked.package_id, "example.link");
        let record = PackageLifecycleRecordV1 {
            package_id: linked.package_id.clone(),
            source: linked.provenance.clone(),
            previous_source: None,
            namespaces: linked.namespaces.clone(),
            revision: 1,
            status: PackageLifecycleStatusV1::Disabled,
            active: None,
            previous: None,
            activation_error: None,
            pending_activation: None,
            retained_data: true,
        };
        assert_eq!(
            revalidate_developer_link(&record, &BTreeMap::new())
                .unwrap()
                .inventory_sha256,
            linked.inventory_sha256
        );
        #[cfg(unix)]
        {
            let mut registry = PackageRegistryV1::default();
            registry.register(record, 0, &BTreeMap::new()).unwrap();
            std::os::unix::fs::symlink(PACKAGE_MANIFEST_FILE, root.join("linked-file")).unwrap();
            assert!(
                register_developer_link(&root, "approval".into(), true, &BTreeMap::new()).is_err()
            );
            let before = registry.clone();
            assert!(registry
                .prepare_activation(
                    "example.link",
                    1,
                    1,
                    "linked-activation".into(),
                    PackageArtifactPointerV1 {
                        package_sha256: linked.inventory_sha256,
                        version: linked.version,
                        runtime_generation: 1,
                    },
                    linked.provenance,
                    &BTreeMap::new(),
                )
                .is_err());
            assert_eq!(registry, before);
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn production_state_reload_revalidates_trusted_developer_links() {
        let root = temp_dir("link-reload");
        let link_root = root.join("link");
        fs::create_dir(&link_root).unwrap();
        write_directory_package(
            &link_root,
            &asset_package("example.reload", &["example.reload"]),
        );
        let linked =
            register_developer_link(&link_root, "reload-approval".into(), true, &BTreeMap::new())
                .unwrap();
        let index = crate::extensions::CuratedIndex::bundled().unwrap();
        let mut state = crate::extensions::MarketplaceStateFile::defaults(&index);
        state
            .register_package_v1(
                &index,
                PackageLifecycleRecordV1 {
                    package_id: linked.package_id.clone(),
                    source: linked.provenance,
                    previous_source: None,
                    namespaces: linked.namespaces,
                    revision: 1,
                    status: PackageLifecycleStatusV1::Disabled,
                    active: None,
                    previous: None,
                    activation_error: None,
                    pending_activation: None,
                    retained_data: true,
                },
                0,
            )
            .unwrap();
        let state_path = root.join("extensions.json");
        crate::extensions::save_state_to_path(&state_path, &state).unwrap();
        crate::extensions::load_state_from_path(&state_path, &index).unwrap();
        fs::write(link_root.join(PACKAGE_MANIFEST_FILE), b"mutated").unwrap();
        assert!(crate::extensions::load_state_from_path(&state_path, &index).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    fn pointer(byte: char, generation: u64) -> PackageArtifactPointerV1 {
        PackageArtifactPointerV1 {
            package_sha256: byte.to_string().repeat(64),
            version: format!("1.0.{generation}"),
            runtime_generation: generation,
        }
    }

    #[test]
    fn lifecycle_registry_enforces_cas_claims_and_retained_data() {
        let mut registry = PackageRegistryV1::default();
        let record = PackageLifecycleRecordV1 {
            package_id: "example.one".into(),
            source: PackageProvenanceV1::File {
                package_sha256: "a".repeat(64),
            },
            previous_source: None,
            namespaces: vec!["example.shared".into()],
            revision: 1,
            status: PackageLifecycleStatusV1::Enabled,
            active: Some(pointer('a', 1)),
            previous: None,
            activation_error: None,
            pending_activation: None,
            retained_data: true,
        };
        assert_eq!(registry.register(record, 0, &BTreeMap::new()).unwrap(), 1);
        let mut collision = PackageLifecycleRecordV1 {
            package_id: "example.two".into(),
            source: PackageProvenanceV1::File {
                package_sha256: "b".repeat(64),
            },
            previous_source: None,
            namespaces: vec!["example.shared".into()],
            revision: 1,
            status: PackageLifecycleStatusV1::RemovedRetainingData,
            active: None,
            previous: None,
            activation_error: None,
            pending_activation: None,
            retained_data: true,
        };
        assert!(registry
            .register(collision.clone(), 0, &BTreeMap::new())
            .is_err());
        assert!(registry
            .register(collision.clone(), 1, &BTreeMap::new())
            .is_err());
        collision.namespaces = vec!["example.two".into()];
        assert_eq!(
            registry.register(collision, 1, &BTreeMap::new()).unwrap(),
            2
        );
    }

    #[test]
    fn first_activation_is_validated_before_atomic_mutation() {
        let mut registry = PackageRegistryV1::default();
        registry
            .register(
                PackageLifecycleRecordV1 {
                    package_id: "example.first".into(),
                    source: PackageProvenanceV1::File {
                        package_sha256: "a".repeat(64),
                    },
                    previous_source: None,
                    namespaces: vec!["example.first".into()],
                    revision: 1,
                    status: PackageLifecycleStatusV1::Disabled,
                    active: None,
                    previous: None,
                    activation_error: None,
                    pending_activation: None,
                    retained_data: true,
                },
                0,
                &BTreeMap::new(),
            )
            .unwrap();
        let before = registry.clone();
        assert!(registry
            .prepare_activation(
                "example.first",
                1,
                1,
                "invalid-first".into(),
                PackageArtifactPointerV1 {
                    package_sha256: "a".repeat(64),
                    version: "1.0.0".into(),
                    runtime_generation: 0,
                },
                PackageProvenanceV1::File {
                    package_sha256: "a".repeat(64),
                },
                &BTreeMap::new(),
            )
            .is_err());
        assert_eq!(registry, before);

        let journal = registry
            .prepare_activation(
                "example.first",
                1,
                1,
                "valid-first".into(),
                PackageArtifactPointerV1 {
                    package_sha256: "a".repeat(64),
                    version: "1.0.0".into(),
                    runtime_generation: 1,
                },
                PackageProvenanceV1::File {
                    package_sha256: "a".repeat(64),
                },
                &BTreeMap::new(),
            )
            .unwrap();
        assert!(journal.previous.is_none());
        assert!(journal.previous_source.is_none());
        registry.validate(&BTreeMap::new()).unwrap();
    }

    #[test]
    fn package_lifecycle_state_machine_is_cas_atomic_and_rollback_safe() {
        let active = pointer('a', 1);
        let candidate = pointer('b', 2);
        let mut registry = PackageRegistryV1::default();
        registry
            .register(
                PackageLifecycleRecordV1 {
                    package_id: "example.lifecycle".into(),
                    source: PackageProvenanceV1::File {
                        package_sha256: "a".repeat(64),
                    },
                    previous_source: None,
                    namespaces: vec!["example.lifecycle".into()],
                    revision: 1,
                    status: PackageLifecycleStatusV1::Enabled,
                    active: Some(active.clone()),
                    previous: None,
                    activation_error: None,
                    pending_activation: None,
                    retained_data: true,
                },
                0,
                &BTreeMap::new(),
            )
            .unwrap();
        assert!(registry
            .prepare_activation(
                "example.lifecycle",
                0,
                1,
                "update-stale".into(),
                candidate.clone(),
                PackageProvenanceV1::File {
                    package_sha256: "b".repeat(64),
                },
                &BTreeMap::new(),
            )
            .is_err());
        let mut journal = registry
            .prepare_activation(
                "example.lifecycle",
                1,
                1,
                "update-2".into(),
                candidate.clone(),
                PackageProvenanceV1::File {
                    package_sha256: "b".repeat(64),
                },
                &BTreeMap::new(),
            )
            .unwrap();
        assert!(registry.commit_activation(&journal).is_err());
        journal
            .advance(
                ActivationPhaseV1::Prepared,
                ActivationPhaseV1::ArtifactCommitted,
            )
            .unwrap();
        registry.commit_activation(&journal).unwrap();
        registry.commit_activation(&journal).unwrap();
        assert_eq!(
            registry.packages["example.lifecycle"].active.as_ref(),
            Some(&candidate)
        );
        registry.rollback("example.lifecycle").unwrap();
        assert_eq!(
            registry.packages["example.lifecycle"].active.as_ref(),
            Some(&active)
        );
        let failure = registry
            .prepare_activation(
                "example.lifecycle",
                registry.revision,
                registry.packages["example.lifecycle"].revision,
                "update-3".into(),
                pointer('c', 3),
                PackageProvenanceV1::File {
                    package_sha256: "c".repeat(64),
                },
                &BTreeMap::new(),
            )
            .unwrap();
        let before_failure = registry.clone();
        let mut wrong_operation = failure.clone();
        wrong_operation.operation_id = "other-operation".into();
        assert!(registry
            .fail_activation(&wrong_operation, "wrong operation".into())
            .is_err());
        let mut wrong_revision = failure.clone();
        wrong_revision.expected_package_revision += 1;
        assert!(registry
            .fail_activation(&wrong_revision, "wrong revision".into())
            .is_err());
        let mut wrong_generation = failure.clone();
        wrong_generation.candidate.runtime_generation = 1;
        assert!(registry
            .fail_activation(&wrong_generation, "wrong generation".into())
            .is_err());
        assert_eq!(registry, before_failure);
        registry
            .fail_activation(&failure, "preflight failed".into())
            .unwrap();
        assert_eq!(
            registry.packages["example.lifecycle"].status,
            PackageLifecycleStatusV1::Failed
        );
        registry.remove_retaining_data("example.lifecycle").unwrap();
        assert!(registry
            .namespace_owners()
            .contains_key("example.lifecycle"));
    }

    #[test]
    fn activation_journal_is_phase_checked_and_recovery_is_idempotent() {
        let previous = pointer('a', 1);
        let candidate = pointer('b', 2);
        let mut journal = PackageActivationJournalV1 {
            contract: ACTIVATION_JOURNAL_CONTRACT_V1.into(),
            operation_id: "update-1".into(),
            package_id: "example.package".into(),
            expected_registry_revision: 4,
            expected_package_revision: 3,
            phase: ActivationPhaseV1::Prepared,
            previous: Some(previous.clone()),
            previous_source: Some(PackageProvenanceV1::File {
                package_sha256: "a".repeat(64),
            }),
            candidate: candidate.clone(),
            candidate_source: PackageProvenanceV1::File {
                package_sha256: "b".repeat(64),
            },
        };
        assert_eq!(
            journal.recovery(Some(&previous)),
            ActivationRecoveryV1::RollBackCandidate
        );
        journal
            .advance(
                ActivationPhaseV1::Prepared,
                ActivationPhaseV1::ArtifactCommitted,
            )
            .unwrap();
        assert_eq!(
            journal.recovery(Some(&previous)),
            ActivationRecoveryV1::CommitCandidate
        );
        journal
            .advance(
                ActivationPhaseV1::ArtifactCommitted,
                ActivationPhaseV1::RegistryCommitted,
            )
            .unwrap();
        assert_eq!(
            journal.recovery(Some(&candidate)),
            ActivationRecoveryV1::KeepCommittedCandidate
        );
        assert_eq!(journal.recovery(None), ActivationRecoveryV1::Conflict);
        assert!(journal
            .advance(
                ActivationPhaseV1::RegistryCommitted,
                ActivationPhaseV1::Prepared
            )
            .is_err());

        let root = temp_dir("journal");
        let path = root.join("activation.json");
        write_activation_journal(&path, &journal).unwrap();
        assert_eq!(read_activation_journal(&path).unwrap(), Some(journal));
        remove_activation_journal(&path).unwrap();
        assert_eq!(read_activation_journal(&path).unwrap(), None);
        fs::remove_dir_all(root).unwrap();
    }
}
