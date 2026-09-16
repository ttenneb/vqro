//! Publishable extension package contracts and generated schemas.

use sha2::{Digest, Sha256};

pub const EXTENSION_MANIFEST_CONTRACT_V1: &str = "vqro.extension-manifest.v1";
pub const EXTENSION_MANIFEST_FILE_V1: &str = "vqro-extension.toml";
pub const EXTENSION_MANIFEST_SCHEMA_ID_V1: &str = "https://vqro.dev/schema/extension-manifest/v1";
pub const EXTENSION_SERVICE_CONTRACT_V1: &str = "vqro.extension-service.v1";
pub const EXTENSION_SERVICE_SCHEMA_ID_V1: &str = "https://vqro.dev/schema/extension-service/v1";
pub const EXTENSION_BUNDLE_SCHEMA_ID_V1: &str = "https://vqro.dev/schema/extensions/v1";
pub const PACKAGE_SCHEMA_ID_V1: &str = crate::extension_package::PACKAGE_SCHEMA_ID_V1;
pub const EXTENSION_MANIFEST_SCHEMA_ID_V2: &str = crate::extension_package::MANIFEST_SCHEMA_ID_V2;

pub fn manifest_v2_schema_document() -> serde_json::Value {
    let mut schema = serde_json::to_value(schemars::schema_for!(
        crate::extension_package::PackageManifestV2
    ))
    .expect("generated manifest-v2 schema serializes");
    let object = schema
        .as_object_mut()
        .expect("generated manifest-v2 schema is an object");
    object.insert(
        "$id".into(),
        serde_json::Value::String(EXTENSION_MANIFEST_SCHEMA_ID_V2.into()),
    );
    object.insert(
        "title".into(),
        serde_json::Value::String("vqro extension manifest v2".into()),
    );
    object.insert(
        "contract".into(),
        serde_json::Value::String(crate::extension_package::PACKAGE_CONTRACT_V1.into()),
    );
    schema
}

pub fn package_schema_document() -> serde_json::Value {
    serde_json::json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": PACKAGE_SCHEMA_ID_V1,
        "title": "vqro package contract v1",
        "contract": crate::extension_package::PACKAGE_CONTRACT_V1,
        "archive": {
            "extension": ".vqrox",
            "format": "deterministic uncompressed ustar",
            "manifest": crate::extension_package::PACKAGE_MANIFEST_FILE,
            "checksums": crate::extension_package::PACKAGE_CHECKSUM_FILE,
            "max_archive_bytes": crate::extension_package::MAX_PACKAGE_BYTES,
            "max_expanded_bytes": crate::extension_package::MAX_EXPANDED_BYTES,
            "max_entries": crate::extension_package::MAX_PACKAGE_ENTRIES,
        },
        "schemas": {
            "manifest": scoped_schema::<crate::extension_package::PackageManifestV2>("manifest"),
            "provenance": scoped_schema::<crate::extension_package::PackageProvenanceV1>("provenance"),
            "validated_package": scoped_schema::<crate::extension_package::ValidatedPackageV1>("validated_package"),
            "installed_package": scoped_schema::<crate::extension_package::InstalledPackageV1>("installed_package"),
            "linked_package": scoped_schema::<crate::extension_package::LinkedPackageV1>("linked_package"),
            "registry": scoped_schema::<crate::extension_package::PackageRegistryV1>("registry"),
            "activation_journal": scoped_schema::<crate::extension_package::PackageActivationJournalV1>("activation_journal"),
        }
    })
}

pub fn manifest_schema_document() -> serde_json::Value {
    let mut schema = crate::app::manifest_json_schema();
    let object = schema
        .as_object_mut()
        .expect("generated manifest schema is an object");
    object.insert(
        "$id".into(),
        serde_json::Value::String(EXTENSION_MANIFEST_SCHEMA_ID_V1.into()),
    );
    object.insert(
        "title".into(),
        serde_json::Value::String("vqro extension manifest v1".into()),
    );
    object.insert(
        "contract".into(),
        serde_json::Value::String(EXTENSION_MANIFEST_CONTRACT_V1.into()),
    );
    schema
}

fn scoped_schema<T: schemars::JsonSchema>(scope: &str) -> serde_json::Value {
    fn rewrite(value: &mut serde_json::Value, scope: &str) {
        match value {
            serde_json::Value::Object(object) => {
                if let Some(serde_json::Value::String(reference)) = object.get_mut("$ref") {
                    if let Some(path) = reference.strip_prefix("#/") {
                        *reference = format!("#/schemas/{scope}/{path}");
                    }
                }
                for child in object.values_mut() {
                    rewrite(child, scope);
                }
            }
            serde_json::Value::Array(items) => {
                for item in items {
                    rewrite(item, scope);
                }
            }
            _ => {}
        }
    }

    let mut schema = serde_json::to_value(schemars::schema_for!(T))
        .expect("extension contract schema serializes");
    rewrite(&mut schema, scope);
    schema
}

pub fn service_schema_document() -> serde_json::Value {
    serde_json::json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": EXTENSION_SERVICE_SCHEMA_ID_V1,
        "title": "vqro extension service protocol v1",
        "contract": EXTENSION_SERVICE_CONTRACT_V1,
        "transport": "bounded newline-delimited JSON over stdio",
        "host_protocol": crate::extension_service::EXTENSION_SERVICE_PROTOCOL,
        "limits": {
            "max_frame_bytes": crate::extension_contract::MAX_EXTENSION_FRAME_BYTES,
            "handshake_timeout_ms": 3000,
            "request_timeout_ms": 750,
        },
        "stability": {
            "stable_methods": ["view.render", "view.action", "host.document.render", "host.document.action"],
            "restricted_sources": ["spaces", "relationships", "agent_activity"],
            "experimental_methods": [
                crate::action_choices::ACTION_CHOICES_LIST_METHOD_V1,
                crate::action_choices::ACTION_CHOICE_INVOKE_METHOD_V1,
            ],
            "foundation_contracts": [
                crate::extension_resource::TERMINAL_RESOURCE_CONTRACT_V1,
                crate::acknowledgement::ACKNOWLEDGEMENT_CONTRACT_V1,
            ],
        },
        "schemas": {
            "hello": scoped_schema::<crate::extension_service::ServiceHello>("hello"),
            "welcome": scoped_schema::<crate::extension_service::ExtensionServiceWelcomeV1>("welcome"),
            "request": scoped_schema::<crate::extension_service::ExtensionServiceRequestV1>("request"),
            "response": scoped_schema::<crate::extension_service::ExtensionServiceResponseV1>("response"),
        }
    })
}

pub fn extension_schema_document() -> serde_json::Value {
    serde_json::json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": EXTENSION_BUNDLE_SCHEMA_ID_V1,
        "title": "vqro extension contracts v1",
        "manifest_contract": EXTENSION_MANIFEST_CONTRACT_V1,
        "service_contract": EXTENSION_SERVICE_CONTRACT_V1,
        "schemas": {
            "manifest": manifest_schema_document(),
            "service": service_schema_document(),
            "spaces": scoped_schema::<crate::extension_contract::HostSpacesV1>("spaces"),
            "relationships": scoped_schema::<crate::extension_contract::HostRelationshipsV1>("relationships"),
            "agent_activity": scoped_schema::<crate::extension_contract::HostAgentActivityV1>("agent_activity"),
            "terminal_groups": scoped_schema::<crate::extension_contract::HostTerminalGroupsV1>("terminal_groups"),
            "host_document": scoped_schema::<crate::extension_contract::HostDocumentV1>("host_document"),
            "host_document_action": scoped_schema::<crate::extension_contract::HostDocumentExtensionActionV1>("host_document_action"),
            "terminal_resource_replacement": scoped_schema::<crate::extension_resource::TerminalResourceReplacementV1>("terminal_resource_replacement"),
            "terminal_resource_interests": scoped_schema::<crate::extension_resource::ClientResourceInterestsV1>("terminal_resource_interests"),
            "terminal_resource_projection": scoped_schema::<crate::extension_resource::TerminalResourceProjectionV1>("terminal_resource_projection"),
            "action_choices_request": scoped_schema::<crate::action_choices::ActionChoicesRequestV1>("action_choices_request"),
            "action_choice_set": scoped_schema::<crate::action_choices::ActionChoiceSetV1>("action_choice_set"),
            "action_choice_invocation": scoped_schema::<crate::action_choices::ActionChoiceInvocationV1>("action_choice_invocation"),
        },
    })
}

pub fn manifest_schema_json() -> &'static str {
    include_str!("../docs/next/api/vqro-extension-manifest.schema.json")
}

pub fn manifest_v2_schema_json() -> &'static str {
    include_str!("../docs/next/api/vqro-extension-manifest-v2.schema.json")
}

pub fn package_schema_json() -> &'static str {
    include_str!("../docs/next/api/vqro-package-v1.schema.json")
}

pub fn service_schema_json() -> &'static str {
    include_str!("../docs/next/api/vqro-extension-service.schema.json")
}

pub fn extension_schema_json() -> &'static str {
    include_str!("../docs/next/api/vqro-extension.schema.json")
}

pub fn manifest_schema_sha256() -> String {
    format!("{:x}", Sha256::digest(manifest_schema_json().as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standalone_manifest_v2_schema_has_no_dangling_local_references() {
        fn collect_references<'a>(value: &'a serde_json::Value, references: &mut Vec<&'a str>) {
            match value {
                serde_json::Value::Object(object) => {
                    if let Some(reference) = object.get("$ref").and_then(serde_json::Value::as_str)
                    {
                        references.push(reference);
                    }
                    for child in object.values() {
                        collect_references(child, references);
                    }
                }
                serde_json::Value::Array(items) => {
                    for item in items {
                        collect_references(item, references);
                    }
                }
                _ => {}
            }
        }

        let schema = manifest_v2_schema_document();
        let mut references = Vec::new();
        collect_references(&schema, &mut references);
        assert!(!references.is_empty());
        for reference in references {
            let pointer = reference
                .strip_prefix('#')
                .expect("manifest-v2 schema uses only local references");
            assert!(
                schema.pointer(pointer).is_some(),
                "dangling schema reference: {reference}"
            );
        }
    }

    #[test]
    fn generated_package_and_manifest_v2_schema_artifacts_are_current() {
        for (relative, document) in [
            (
                "docs/next/api/vqro-extension-manifest-v2.schema.json",
                manifest_v2_schema_document(),
            ),
            (
                "docs/next/api/vqro-package-v1.schema.json",
                package_schema_document(),
            ),
        ] {
            let actual = format!("{}\n", serde_json::to_string_pretty(&document).unwrap());
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(relative);
            if std::env::var_os("HERDR_UPDATE_API_SCHEMA").is_some() {
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(&path, &actual).unwrap();
            } else {
                assert_eq!(std::fs::read_to_string(path).unwrap(), actual);
            }
        }
    }

    #[test]
    fn generated_manifest_schema_artifact_is_current() {
        let actual = format!(
            "{}\n",
            serde_json::to_string_pretty(&manifest_schema_document()).unwrap()
        );
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("docs/next/api/vqro-extension-manifest.schema.json");
        if std::env::var_os("HERDR_UPDATE_API_SCHEMA").is_some() {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, &actual).unwrap();
            return;
        }
        assert_eq!(std::fs::read_to_string(path).unwrap(), actual);
    }

    #[test]
    fn generated_service_schema_artifact_is_current() {
        let actual = format!(
            "{}\n",
            serde_json::to_string_pretty(&service_schema_document()).unwrap()
        );
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("docs/next/api/vqro-extension-service.schema.json");
        if std::env::var_os("HERDR_UPDATE_API_SCHEMA").is_some() {
            std::fs::write(&path, &actual).unwrap();
            return;
        }
        assert_eq!(std::fs::read_to_string(path).unwrap(), actual);
    }

    #[test]
    fn generated_extension_schema_artifact_is_current() {
        let actual = format!(
            "{}\n",
            serde_json::to_string_pretty(&extension_schema_document()).unwrap()
        );
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("docs/next/api/vqro-extension.schema.json");
        if std::env::var_os("HERDR_UPDATE_API_SCHEMA").is_some() {
            std::fs::write(&path, &actual).unwrap();
            return;
        }
        assert_eq!(std::fs::read_to_string(path).unwrap(), actual);
    }

    fn assert_refs_resolve_against(root: &serde_json::Value, document: &serde_json::Value) {
        fn visit(root: &serde_json::Value, value: &serde_json::Value) {
            match value {
                serde_json::Value::Object(object) => {
                    if let Some(reference) = object.get("$ref").and_then(serde_json::Value::as_str)
                    {
                        if let Some(pointer) = reference.strip_prefix('#') {
                            assert!(
                                root.pointer(pointer).is_some(),
                                "unresolved schema reference {reference}"
                            );
                        }
                    }
                    for child in object.values() {
                        visit(root, child);
                    }
                }
                serde_json::Value::Array(items) => {
                    for item in items {
                        visit(root, item);
                    }
                }
                _ => {}
            }
        }
        visit(root, document);
    }

    #[test]
    fn standalone_schema_references_resolve() {
        for schema in [manifest_schema_document(), service_schema_document()] {
            assert_refs_resolve_against(&schema, &schema);
        }
        let bundle = extension_schema_document();
        for name in [
            "spaces",
            "relationships",
            "agent_activity",
            "terminal_groups",
            "host_document",
            "host_document_action",
            "terminal_resource_replacement",
            "terminal_resource_interests",
            "terminal_resource_projection",
            "action_choices_request",
            "action_choice_set",
            "action_choice_invocation",
        ] {
            let schema = bundle
                .pointer(&format!("/schemas/{name}"))
                .expect("bundle schema");
            assert_refs_resolve_against(&bundle, schema);
        }
    }

    #[test]
    fn manifest_schema_has_stable_public_identity() {
        let schema: serde_json::Value = serde_json::from_str(manifest_schema_json()).unwrap();
        assert_eq!(schema["$id"], EXTENSION_MANIFEST_SCHEMA_ID_V1);
        assert_eq!(schema["contract"], EXTENSION_MANIFEST_CONTRACT_V1);
        assert_eq!(schema["properties"]["manifest_version"]["const"], 1);
        let required = schema["required"].as_array().unwrap();
        assert!(required.iter().any(|field| field == "manifest_version"));
        assert!(required.iter().any(|field| field == "min_vqro_version"));
        assert!(!manifest_schema_json().contains("\"unknown\""));
        assert_eq!(manifest_schema_sha256().len(), 64);
    }
}
