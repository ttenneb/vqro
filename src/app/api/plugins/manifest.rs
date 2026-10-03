use crate::api::schema::{
    ExtensionServiceCapability, HostDocumentSourceKind, InstalledPluginInfo, PluginManifestAction,
    PluginManifestBuild, PluginManifestEventHook, PluginManifestLinkHandler, PluginManifestPane,
    PluginManifestService, PluginManifestStartup, PluginPanePlacement, PluginPlatform,
    PluginServiceCapability, PluginSourceInfo, PluginSourceKind,
};
use crate::popup_size::PopupSize;

const PLUGIN_ID_MAX_CHARS: usize = 120;
const PLUGIN_ACTION_ID_MAX_CHARS: usize = 120;

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct RawPluginManifest {
    #[serde(default)]
    manifest_version: Option<u32>,
    id: String,
    name: String,
    version: String,
    #[serde(default, alias = "min_herdr_version")]
    min_vqro_version: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    platforms: Option<Vec<RawPlatform>>,
    #[serde(default)]
    build: Vec<RawPluginManifestBuild>,
    #[serde(default)]
    startup: Vec<RawPluginManifestStartup>,
    #[serde(default)]
    services: Vec<RawPluginManifestService>,
    #[serde(default)]
    cli_commands: Vec<crate::extension_cli::CliCommand>,
    #[serde(default)]
    actions: Vec<RawPluginManifestAction>,
    #[serde(default)]
    events: Vec<RawPluginManifestEventHook>,
    #[serde(default)]
    panes: Vec<RawPluginManifestPane>,
    #[serde(default)]
    link_handlers: Vec<RawPluginManifestLinkHandler>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct RawPluginManifestBuild {
    #[serde(default)]
    platforms: Option<Vec<RawPlatform>>,
    command: Vec<String>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct RawPluginManifestStartup {
    #[serde(default)]
    platforms: Option<Vec<RawPlatform>>,
    command: Vec<String>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct RawPluginManifestService {
    id: String,
    #[serde(default)]
    platforms: Option<Vec<RawPlatform>>,
    command: Vec<String>,
    #[serde(default)]
    priority: i32,
    #[serde(default)]
    document_source: HostDocumentSourceKind,
    #[serde(default)]
    provides: Vec<PluginServiceCapability>,
    #[serde(default)]
    capabilities: Vec<ExtensionServiceCapability>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct RawPluginManifestAction {
    id: String,
    title: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    contexts: Vec<crate::api::schema::PluginActionContext>,
    #[serde(default)]
    platforms: Option<Vec<RawPlatform>>,
    command: Vec<String>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct RawPluginManifestEventHook {
    on: String,
    #[serde(default)]
    platforms: Option<Vec<RawPlatform>>,
    command: Vec<String>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct RawPluginManifestPane {
    id: String,
    title: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    platforms: Option<Vec<RawPlatform>>,
    #[serde(default)]
    placement: PluginPanePlacement,
    #[serde(default)]
    width: Option<PopupSize>,
    #[serde(default)]
    height: Option<PopupSize>,
    command: Vec<String>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct RawPluginManifestLinkHandler {
    id: String,
    title: String,
    pattern: String,
    action: String,
    #[serde(default)]
    platforms: Option<Vec<RawPlatform>>,
}

/// Raw string platform value from the manifest, validated before conversion.
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(try_from = "String")]
#[schemars(with = "PluginPlatform")]
struct RawPlatform(PluginPlatform);

impl TryFrom<String> for RawPlatform {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        match value.as_str() {
            "linux" => Ok(RawPlatform(PluginPlatform::Linux)),
            "macos" => Ok(RawPlatform(PluginPlatform::Macos)),
            "windows" => Ok(RawPlatform(PluginPlatform::Windows)),
            other => Err(format!(
                "invalid_plugin_platform: unknown platform '{other}'"
            )),
        }
    }
}

pub(crate) fn manifest_json_schema() -> serde_json::Value {
    fn remove_unknown_enum_values(value: &mut serde_json::Value) {
        match value {
            serde_json::Value::Object(object) => {
                if let Some(serde_json::Value::Array(values)) = object.get_mut("enum") {
                    values.retain(|value| value.as_str() != Some("unknown"));
                }
                for child in object.values_mut() {
                    remove_unknown_enum_values(child);
                }
            }
            serde_json::Value::Array(items) => {
                for item in items {
                    remove_unknown_enum_values(item);
                }
            }
            _ => {}
        }
    }

    let mut schema = serde_json::to_value(schemars::schema_for!(RawPluginManifest))
        .expect("plugin manifest schema serializes");
    let object = schema.as_object_mut().expect("manifest schema object");
    if let Some(properties) = object
        .get_mut("properties")
        .and_then(serde_json::Value::as_object_mut)
    {
        properties.insert(
            "manifest_version".into(),
            serde_json::json!({"const": 1, "type": "integer"}),
        );
    }
    let required = object
        .entry("required")
        .or_insert_with(|| serde_json::Value::Array(Vec::new()))
        .as_array_mut()
        .expect("manifest required fields");
    for field in ["manifest_version", "min_vqro_version"] {
        if !required.iter().any(|value| value.as_str() == Some(field)) {
            required.push(serde_json::Value::String(field.into()));
        }
    }
    remove_unknown_enum_values(&mut schema);
    schema
}

pub(crate) fn load_plugin_manifest(
    path: &str,
    enabled: bool,
) -> Result<InstalledPluginInfo, (&'static str, String)> {
    let path = std::path::PathBuf::from(path);
    let manifest_path = if path.is_dir() {
        let native = path.join(crate::extension_spec::EXTENSION_MANIFEST_FILE_V1);
        if native.is_file() {
            native
        } else {
            // Compatibility for extensions authored before the vqro contract was published.
            path.join("herdr-plugin.toml")
        }
    } else {
        path
    };
    let manifest_path = manifest_path
        .canonicalize()
        .map_err(|err| ("plugin_manifest_not_found", err.to_string()))?;
    let plugin_root =
        crate::platform::plugin_runtime_path(manifest_path.parent().ok_or_else(|| {
            (
                "invalid_plugin_manifest_path",
                "manifest path has no parent directory".to_string(),
            )
        })?);
    let content = std::fs::read_to_string(&manifest_path)
        .map_err(|err| ("plugin_manifest_read_failed", err.to_string()))?;
    let manifest_source: RawPluginManifest = toml::from_str(&content)
        .map_err(|err| ("plugin_manifest_parse_failed", err.to_string()))?;
    if manifest_source
        .manifest_version
        .is_some_and(|version| version != 1)
    {
        return Err((
            "unsupported_extension_manifest_version",
            "only extension manifest version 1 is supported".into(),
        ));
    }
    crate::extension_cli::validate(&manifest_source.cli_commands)
        .map_err(|message| ("invalid_extension_cli_command", message))?;
    let plugin_id = normalize_plugin_id(&manifest_source.id)
        .ok_or_else(|| ("invalid_plugin_id", "invalid plugin id".to_string()))?;
    let name = non_empty_trimmed(
        &manifest_source.name,
        "invalid_plugin_name",
        "plugin name is required",
    )?;
    let version = non_empty_trimmed(
        &manifest_source.version,
        "invalid_plugin_version",
        "plugin version is required",
    )?;
    let min_herdr_version =
        validate_min_herdr_version(manifest_source.min_vqro_version.as_deref())?;
    let description = manifest_source
        .description
        .map(|description| description.trim().to_string())
        .filter(|description| !description.is_empty());
    let platforms = normalize_platforms(manifest_source.platforms)?;
    let build = manifest_source
        .build
        .into_iter()
        .map(normalize_manifest_build)
        .collect::<Result<Vec<_>, _>>()?;
    let startup = manifest_source
        .startup
        .into_iter()
        .map(normalize_manifest_startup)
        .collect::<Result<Vec<_>, _>>()?;
    let services = manifest_source
        .services
        .into_iter()
        .map(normalize_manifest_service)
        .collect::<Result<Vec<_>, _>>()?;
    reject_duplicate_service_ids(&services)?;
    let reserved_document_namespace = matches!(
        plugin_id.as_str(),
        "vqro.collections"
            | "vqro.spaces"
            | "vqro.delegation"
            | "vqro.agent-status"
            | "host.fallback"
    );
    let publishes_host_documents = services.iter().any(|service| {
        service
            .provides
            .contains(&PluginServiceCapability::HostDocument)
    });
    if publishes_host_documents && !plugin_id.contains('.') {
        return Err((
            "invalid_host_document_namespace",
            "host document plugin IDs must be dotted namespaces".to_string(),
        ));
    }
    if reserved_document_namespace && publishes_host_documents {
        return Err((
            "reserved_host_document_namespace",
            format!("plugin namespace {plugin_id} is reserved by the host"),
        ));
    }
    let mut actions = manifest_source
        .actions
        .into_iter()
        .map(normalize_manifest_action)
        .collect::<Result<Vec<_>, _>>()?;
    reject_duplicate_action_ids(&actions)?;
    actions.sort_by(|a, b| a.id.cmp(&b.id));
    let mut events = manifest_source
        .events
        .into_iter()
        .map(normalize_manifest_event)
        .collect::<Result<Vec<_>, _>>()?;
    events.sort_by(|a, b| {
        a.on.cmp(&b.on).then_with(|| {
            a.command
                .iter()
                .map(|arg| arg.trim())
                .cmp(b.command.iter().map(|arg| arg.trim()))
        })
    });
    let mut panes = manifest_source
        .panes
        .into_iter()
        .map(normalize_manifest_pane)
        .collect::<Result<Vec<_>, _>>()?;
    reject_duplicate_pane_ids(&panes)?;
    panes.sort_by(|a, b| a.id.cmp(&b.id));
    let link_handlers = manifest_source
        .link_handlers
        .into_iter()
        .map(normalize_manifest_link_handler)
        .collect::<Result<Vec<_>, _>>()?;
    reject_duplicate_link_handler_ids(&link_handlers)?;
    validate_link_handler_actions(&link_handlers, &actions)?;

    let mut warnings = validate_event_names(&events);
    if platforms.is_none() {
        warnings.push("manifest does not declare platforms; platform support unknown".to_string());
    }

    Ok(InstalledPluginInfo {
        plugin_id,
        name,
        version,
        min_herdr_version,
        description,
        manifest_path: manifest_path.display().to_string(),
        plugin_root: plugin_root.display().to_string(),
        enabled,
        load_order: 0,
        platforms,
        build,
        startup,
        services,
        actions,
        events,
        panes,
        link_handlers,
        source: Default::default(),
        warnings,
    })
}

fn validate_min_herdr_version(value: Option<&str>) -> Result<String, (&'static str, String)> {
    let Some(value) = value else {
        return Err((
            "invalid_plugin_min_herdr_version",
            "plugin min_herdr_version is required".to_string(),
        ));
    };
    let value = non_empty_trimmed(
        value,
        "invalid_plugin_min_herdr_version",
        "plugin min_herdr_version is required",
    )?;
    let required = crate::update::Version::parse(&value).ok_or_else(|| {
        (
            "invalid_plugin_min_herdr_version",
            format!(
                "plugin min_herdr_version must be a semantic version like {}",
                crate::build_info::BASE_VERSION
            ),
        )
    })?;
    let current = crate::update::Version::current();
    if required > current {
        return Err((
            "plugin_requires_newer_herdr",
            format!("plugin requires Herdr {required} or newer; current Herdr is {current}"),
        ));
    }
    Ok(required.to_string())
}

fn normalize_manifest_build(
    build: RawPluginManifestBuild,
) -> Result<PluginManifestBuild, (&'static str, String)> {
    let platforms = normalize_platforms(build.platforms)?;
    let command = normalize_command(build.command)?;
    Ok(PluginManifestBuild { platforms, command })
}

fn normalize_manifest_startup(
    startup: RawPluginManifestStartup,
) -> Result<PluginManifestStartup, (&'static str, String)> {
    let platforms = normalize_platforms(startup.platforms)?;
    let command = normalize_command(startup.command)?;
    Ok(PluginManifestStartup { platforms, command })
}

fn normalize_manifest_service(
    service: RawPluginManifestService,
) -> Result<PluginManifestService, (&'static str, String)> {
    let id = normalize_plugin_id(&service.id).ok_or_else(|| {
        (
            "invalid_plugin_service_id",
            "invalid plugin service id".to_string(),
        )
    })?;
    let platforms = normalize_platforms(service.platforms)?;
    let command = normalize_command(service.command)?;
    if service.provides.is_empty() {
        return Err((
            "invalid_plugin_service_capability",
            "plugin service must declare at least one capability".to_string(),
        ));
    }
    if service.provides.contains(&PluginServiceCapability::Unknown)
        || service
            .capabilities
            .contains(&ExtensionServiceCapability::Unknown)
    {
        return Err((
            "invalid_plugin_service_capability",
            "plugin services cannot request unknown capabilities".to_string(),
        ));
    }
    if service
        .provides
        .contains(&PluginServiceCapability::PaneInspector)
        && !service
            .capabilities
            .contains(&ExtensionServiceCapability::ViewContribution)
    {
        return Err((
            "missing_plugin_service_capability",
            "pane inspector services must request view_contribution".to_string(),
        ));
    }
    if service.document_source == HostDocumentSourceKind::Unknown {
        return Err((
            "invalid_host_document_source",
            "unknown host document source".to_string(),
        ));
    }
    if service.document_source == HostDocumentSourceKind::Spaces {
        return Err((
            "restricted_host_document_source",
            "Spaces document authority belongs to its pinned component package".to_string(),
        ));
    }
    if service.document_source != HostDocumentSourceKind::TerminalGroups
        && !service
            .provides
            .contains(&PluginServiceCapability::HostDocument)
    {
        return Err((
            "invalid_host_document_source",
            "document_source requires a host_document service".to_string(),
        ));
    }
    if service
        .provides
        .contains(&PluginServiceCapability::HostDocument)
        && !service
            .capabilities
            .contains(&ExtensionServiceCapability::HostDocument)
    {
        return Err((
            "missing_plugin_service_capability",
            "host document services must request host_document".to_string(),
        ));
    }
    if service
        .provides
        .contains(&PluginServiceCapability::ActionChoices)
        && !service
            .capabilities
            .contains(&ExtensionServiceCapability::ActionChoices)
    {
        return Err((
            "missing_plugin_service_capability",
            "action choice services must request action_choices".to_string(),
        ));
    }
    if service
        .provides
        .contains(&PluginServiceCapability::ActionInvoke)
        && !service
            .capabilities
            .contains(&ExtensionServiceCapability::ActionInvoke)
    {
        return Err((
            "missing_plugin_service_capability",
            "action invocation services must request action_invoke".into(),
        ));
    }
    let requested_capabilities = service.capabilities;
    // Linking and enabling trusted local code grants only this small protocol allowlist. It does
    // not sandbox the subprocess from the user's operating-system authority.
    let granted_capabilities = requested_capabilities.clone();
    Ok(PluginManifestService {
        id,
        document_source: service.document_source,
        platforms,
        command,
        priority: service.priority,
        provides: service.provides,
        requested_capabilities,
        granted_capabilities,
    })
}

fn reject_duplicate_service_ids(
    services: &[PluginManifestService],
) -> Result<(), (&'static str, String)> {
    let mut seen = std::collections::HashSet::new();
    for service in services {
        if !seen.insert(service.id.as_str()) {
            return Err((
                "duplicate_plugin_service_id",
                format!("duplicate plugin service id '{}'", service.id),
            ));
        }
    }
    Ok(())
}

pub(super) fn normalize_plugin_source(
    plugin: &InstalledPluginInfo,
    source: PluginSourceInfo,
) -> Result<PluginSourceInfo, (&'static str, String)> {
    if source.kind == PluginSourceKind::Local {
        return Ok(source);
    }
    let Some(managed_path) = source.managed_path.as_deref() else {
        return Err((
            "invalid_plugin_source",
            "GitHub plugin source requires managed_path".to_string(),
        ));
    };
    let managed_path = std::path::PathBuf::from(managed_path)
        .canonicalize()
        .map_err(|err| ("invalid_plugin_source", err.to_string()))?;
    let plugin_root = std::path::PathBuf::from(&plugin.plugin_root)
        .canonicalize()
        .map_err(|err| ("invalid_plugin_source", err.to_string()))?;
    let expected = crate::plugin_paths::managed_checkout_path(&plugin.plugin_id)
        .canonicalize()
        .map_err(|err| ("invalid_plugin_source", err.to_string()))?;
    if managed_path != expected {
        return Err((
            "invalid_plugin_source",
            "GitHub plugin managed_path does not match the plugin id".to_string(),
        ));
    }
    if !plugin_root.starts_with(&managed_path) {
        return Err((
            "invalid_plugin_source",
            "plugin manifest is not inside the managed checkout".to_string(),
        ));
    }
    Ok(source)
}

fn reject_duplicate_action_ids(
    actions: &[PluginManifestAction],
) -> Result<(), (&'static str, String)> {
    let mut seen = std::collections::HashSet::new();
    for action in actions {
        if !seen.insert(action.id.as_str()) {
            return Err((
                "duplicate_plugin_action_id",
                format!("duplicate action id '{}'", action.id),
            ));
        }
    }
    Ok(())
}

fn validate_event_names(events: &[crate::api::schema::PluginManifestEventHook]) -> Vec<String> {
    let known = crate::api::schema::plugin_hook_event_names();
    events
        .iter()
        .filter(|hook| !known.contains(&hook.on.as_str()))
        .map(|hook| format!("unknown event '{}'", hook.on))
        .collect()
}

fn reject_duplicate_pane_ids(panes: &[PluginManifestPane]) -> Result<(), (&'static str, String)> {
    let mut seen = std::collections::HashSet::new();
    for pane in panes {
        if !seen.insert(pane.id.as_str()) {
            return Err((
                "duplicate_plugin_pane_id",
                format!("duplicate pane id '{}'", pane.id),
            ));
        }
    }
    Ok(())
}

fn reject_duplicate_link_handler_ids(
    handlers: &[PluginManifestLinkHandler],
) -> Result<(), (&'static str, String)> {
    let mut seen = std::collections::HashSet::new();
    for handler in handlers {
        if !seen.insert(handler.id.as_str()) {
            return Err((
                "duplicate_plugin_link_handler_id",
                format!("duplicate link handler id '{}'", handler.id),
            ));
        }
    }
    Ok(())
}

fn validate_link_handler_actions(
    handlers: &[PluginManifestLinkHandler],
    actions: &[PluginManifestAction],
) -> Result<(), (&'static str, String)> {
    for handler in handlers {
        if !actions.iter().any(|action| action.id == handler.action) {
            return Err((
                "invalid_plugin_link_handler_action",
                format!(
                    "link handler '{}' references unknown action '{}'",
                    handler.id, handler.action
                ),
            ));
        }
    }
    Ok(())
}

fn normalize_manifest_action(
    action: RawPluginManifestAction,
) -> Result<PluginManifestAction, (&'static str, String)> {
    let id = normalize_action_id(&action.id)
        .ok_or_else(|| ("invalid_plugin_action_id", "invalid action id".to_string()))?;
    let title = non_empty_trimmed(
        &action.title,
        "invalid_plugin_action_title",
        "action title is required",
    )?;
    let description = action
        .description
        .map(|description| description.trim().to_string())
        .filter(|description| !description.is_empty());
    let platforms = normalize_platforms(action.platforms)?;
    let command = normalize_command(action.command)?;
    Ok(PluginManifestAction {
        id,
        title,
        description,
        contexts: action.contexts,
        platforms,
        command,
    })
}

fn normalize_manifest_pane(
    pane: RawPluginManifestPane,
) -> Result<PluginManifestPane, (&'static str, String)> {
    let id = normalize_action_id(&pane.id)
        .ok_or_else(|| ("invalid_plugin_pane_id", "invalid pane id".to_string()))?;
    let title = non_empty_trimmed(
        &pane.title,
        "invalid_plugin_pane_title",
        "pane title is required",
    )?;
    let description = pane
        .description
        .map(|description| description.trim().to_string())
        .filter(|description| !description.is_empty());
    let platforms = normalize_platforms(pane.platforms)?;
    let command = normalize_command(pane.command)?;
    if pane.placement != PluginPanePlacement::Popup
        && (pane.width.is_some() || pane.height.is_some())
    {
        return Err((
            "invalid_plugin_pane_size",
            "pane width and height are only supported when placement is popup".to_string(),
        ));
    }
    Ok(PluginManifestPane {
        id,
        title,
        description,
        platforms,
        placement: pane.placement,
        width: pane.width,
        height: pane.height,
        command,
    })
}

fn normalize_manifest_event(
    event: RawPluginManifestEventHook,
) -> Result<PluginManifestEventHook, (&'static str, String)> {
    let on = non_empty_trimmed(&event.on, "invalid_plugin_event", "event name is required")?;
    let platforms = normalize_platforms(event.platforms)?;
    let command = normalize_command(event.command)?;
    Ok(PluginManifestEventHook {
        on,
        platforms,
        command,
    })
}

fn normalize_manifest_link_handler(
    handler: RawPluginManifestLinkHandler,
) -> Result<PluginManifestLinkHandler, (&'static str, String)> {
    let id = normalize_action_id(&handler.id).ok_or_else(|| {
        (
            "invalid_plugin_link_handler_id",
            "invalid link handler id".to_string(),
        )
    })?;
    let title = non_empty_trimmed(
        &handler.title,
        "invalid_plugin_link_handler_title",
        "link handler title is required",
    )?;
    let pattern = non_empty_trimmed(
        &handler.pattern,
        "invalid_plugin_link_handler_pattern",
        "link handler pattern is required",
    )?;
    regex::Regex::new(&pattern)
        .map_err(|err| ("invalid_plugin_link_handler_pattern", err.to_string()))?;
    let action = normalize_action_id(&handler.action).ok_or_else(|| {
        (
            "invalid_plugin_link_handler_action",
            "invalid link handler action".to_string(),
        )
    })?;
    let platforms = normalize_platforms(handler.platforms)?;
    Ok(PluginManifestLinkHandler {
        id,
        title,
        pattern,
        action,
        platforms,
    })
}

fn normalize_platforms(
    raw_platforms: Option<Vec<RawPlatform>>,
) -> Result<Option<Vec<PluginPlatform>>, (&'static str, String)> {
    match raw_platforms {
        None => Ok(None),
        Some(list) if list.is_empty() => Err((
            "invalid_plugin_platform",
            "platforms must not be an empty array; omit the field to leave platforms undeclared"
                .to_string(),
        )),
        Some(list) => Ok(Some(list.into_iter().map(|p| p.0).collect())),
    }
}

/// Returns the platform the current binary was compiled for.
fn current_platform() -> PluginPlatform {
    if cfg!(target_os = "linux") {
        PluginPlatform::Linux
    } else if cfg!(target_os = "macos") {
        PluginPlatform::Macos
    } else {
        PluginPlatform::Windows
    }
}

/// Resolve the effective platforms for an action or event: use the item's own
/// platforms if declared, otherwise inherit from the plugin-level platforms.
/// Returns a reference to whichever `Option<Vec<PluginPlatform>>` applies.
pub(super) fn effective_platforms<'a>(
    item_platforms: &'a Option<Vec<PluginPlatform>>,
    plugin_platforms: &'a Option<Vec<PluginPlatform>>,
) -> &'a Option<Vec<PluginPlatform>> {
    if item_platforms.is_some() {
        item_platforms
    } else {
        plugin_platforms
    }
}

pub(super) fn ensure_platform_supported(
    platforms: &Option<Vec<PluginPlatform>>,
    subject: &str,
) -> Result<(), (&'static str, String)> {
    if let Some(platforms) = platforms {
        let host = current_platform();
        if !platforms.contains(&host) {
            return Err((
                "platform_unsupported",
                format!(
                    "{subject} does not support the current platform ({})",
                    platform_name(host)
                ),
            ));
        }
    }
    Ok(())
}

fn platform_name(p: PluginPlatform) -> &'static str {
    match p {
        PluginPlatform::Linux => "linux",
        PluginPlatform::Macos => "macos",
        PluginPlatform::Windows => "windows",
    }
}

fn normalize_command(command: Vec<String>) -> Result<Vec<String>, (&'static str, String)> {
    if command.is_empty() || command.iter().any(|arg| arg.is_empty()) {
        return Err((
            "invalid_plugin_command",
            "command must contain non-empty argv strings".to_string(),
        ));
    }
    Ok(command)
}

fn non_empty_trimmed(
    value: &str,
    code: &'static str,
    message: &'static str,
) -> Result<String, (&'static str, String)> {
    let value = value.trim().to_string();
    if value.is_empty() {
        Err((code, message.to_string()))
    } else {
        Ok(value)
    }
}

pub(crate) fn normalize_plugin_id(value: &str) -> Option<String> {
    normalize_identifier(value, PLUGIN_ID_MAX_CHARS)
}

pub(super) fn normalize_action_id(value: &str) -> Option<String> {
    normalize_local_identifier(value, PLUGIN_ACTION_ID_MAX_CHARS)
}

fn normalize_identifier(value: &str, max_chars: usize) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()
        && value.chars().count() <= max_chars
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b':' | b'.' | b'_' | b'-')))
    .then(|| value.to_string())
}

fn normalize_local_identifier(value: &str, max_chars: usize) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()
        && value.chars().count() <= max_chars
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b':' | b'_' | b'-')))
    .then(|| value.to_string())
}

pub(crate) struct BundledDocumentServiceDeclaration {
    pub(crate) namespace: String,
    pub(crate) service: PluginManifestService,
}

pub(crate) fn bundled_document_service_declarations(
) -> Result<Vec<BundledDocumentServiceDeclaration>, String> {
    let manifests = [
        include_str!("../../../../extensions/first-party/delegation/vqro-extension.toml"),
        include_str!("../../../../extensions/first-party/agent-status/vqro-extension.toml"),
    ];
    manifests
        .into_iter()
        .map(|content| {
            let manifest: RawPluginManifest = toml::from_str(content)
                .map_err(|error| format!("bundled manifest parse failed: {error}"))?;
            if manifest.manifest_version != Some(1)
                || manifest.min_vqro_version.as_deref() != Some("0.9.0")
                || manifest.services.len() != 1
            {
                return Err(format!(
                    "bundled manifest {} has an invalid contract declaration",
                    manifest.id
                ));
            }
            let namespace = normalize_plugin_id(&manifest.id).ok_or_else(|| {
                format!("bundled manifest {} has an invalid namespace", manifest.id)
            })?;
            let service = normalize_manifest_service(
                manifest
                    .services
                    .into_iter()
                    .next()
                    .ok_or_else(|| "bundled manifest has no document service".to_string())?,
            )
            .map_err(|(_, message)| message)?;
            if !service
                .provides
                .contains(&PluginServiceCapability::HostDocument)
            {
                return Err(format!(
                    "bundled manifest {namespace} is not a document provider"
                ));
            }
            Ok(BundledDocumentServiceDeclaration { namespace, service })
        })
        .collect()
}

#[cfg(test)]
mod contract_tests {
    use super::*;

    #[test]
    fn public_manifest_conformance_fixtures_use_the_production_parser() {
        let root =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/extensions");
        let valid = load_plugin_manifest(root.join("valid").to_str().unwrap(), false).unwrap();
        assert_eq!(valid.plugin_id, "example.conformant");
        assert_eq!(valid.min_herdr_version, "0.9.0");

        let error = load_plugin_manifest(root.join("invalid-platform").to_str().unwrap(), false)
            .unwrap_err();
        assert_eq!(error.0, "plugin_manifest_parse_failed");

        for (fixture, code) in [
            (
                "invalid-unknown-capability",
                "invalid_plugin_service_capability",
            ),
            ("invalid-namespace", "invalid_host_document_namespace"),
            ("missing-version", "invalid_plugin_min_herdr_version"),
        ] {
            let error =
                load_plugin_manifest(root.join(fixture).to_str().unwrap(), false).unwrap_err();
            assert_eq!(error.0, code, "fixture {fixture}");
        }
    }

    #[test]
    fn trusted_non_bundled_services_may_request_neutral_document_sources() {
        let root =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/extensions");
        for (fixture, source) in [
            (
                "valid-public-activity",
                HostDocumentSourceKind::AgentActivity,
            ),
            (
                "valid-public-relationships",
                HostDocumentSourceKind::Relationships,
            ),
        ] {
            let installed =
                load_plugin_manifest(root.join(fixture).to_str().unwrap(), true).unwrap();
            assert!(installed.enabled);
            assert!(installed.plugin_id.starts_with("example.public-"));
            let service = &installed.services[0];
            assert_eq!(service.document_source, source);
            assert_eq!(
                service.granted_capabilities,
                vec![ExtensionServiceCapability::HostDocument]
            );
        }
    }

    #[test]
    fn bundled_first_party_packages_match_the_runtime_service_contract() {
        let declarations = bundled_document_service_declarations().unwrap();
        assert_eq!(declarations.len(), 2);
        assert!(declarations.iter().all(|declaration| {
            declaration.namespace.starts_with("vqro.") && declaration.service.priority == 100
        }));
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("extensions/first-party");
        for (directory, namespace, source, entrypoint, capabilities) in [
            (
                "delegation",
                "vqro.delegation",
                HostDocumentSourceKind::Relationships,
                "delegation",
                vec![ExtensionServiceCapability::HostDocument],
            ),
            (
                "agent-status",
                "vqro.agent-status",
                HostDocumentSourceKind::AgentActivity,
                "agent-status",
                vec![ExtensionServiceCapability::HostDocument],
            ),
        ] {
            let text =
                std::fs::read_to_string(root.join(directory).join("vqro-extension.toml")).unwrap();
            let manifest: RawPluginManifest = toml::from_str(&text).unwrap();
            assert_eq!(manifest.manifest_version, Some(1));
            assert_eq!(manifest.id, namespace);
            assert_eq!(manifest.services.len(), 1);
            let service =
                normalize_manifest_service(manifest.services.into_iter().next().unwrap()).unwrap();
            assert_eq!(service.document_source, source);
            assert_eq!(service.priority, 100);
            assert_eq!(service.granted_capabilities, capabilities);
            assert_eq!(
                service.command,
                ["vqro", "_extension-service", entrypoint]
                    .into_iter()
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn native_manifest_name_wins_over_legacy_compatibility_file() {
        let root =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("extensions/terminal-inspector");
        let manifest = load_plugin_manifest(root.to_str().unwrap(), false).unwrap();
        assert_eq!(manifest.plugin_id, "vqro.terminal-inspector");
        assert_eq!(manifest.min_herdr_version, "0.8.2");
    }
}
