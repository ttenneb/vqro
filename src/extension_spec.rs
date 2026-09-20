//! Publishable extension package contracts and generated schemas.

use sha2::{Digest, Sha256};

pub const EXTENSION_MANIFEST_CONTRACT_V1: &str = "vqro.extension-manifest.v1";
pub const EXTENSION_MANIFEST_FILE_V1: &str = "vqro-extension.toml";
pub const EXTENSION_MANIFEST_SCHEMA_ID_V1: &str = "https://vqro.dev/schema/extension-manifest/v1";
pub const EXTENSION_SERVICE_CONTRACT_V1: &str = "vqro.extension-service.v1";
pub const EXTENSION_SERVICE_SCHEMA_ID_V1: &str = "https://vqro.dev/schema/extension-service/v1";
pub const RUNTIME_SERVICE_SCHEMA_ID_V1: &str = "https://vqro.dev/schema/service/v1";
pub const HOST_STATE_SCHEMA_ID_V1: &str = "https://vqro.dev/schema/host-state/v1";
pub const HOST_DOCUMENT_SCHEMA_ID_V2: &str = crate::host_document_v2::HOST_DOCUMENT_SCHEMA_ID_V2;
pub const HOST_DOCUMENT_RENDER_SCHEMA_ID_V2: &str =
    crate::host_document_v2::HOST_DOCUMENT_RENDER_SCHEMA_ID_V2;
pub const HOST_TERMINALS_SCHEMA_ID_V1: &str = "https://vqro.dev/schema/host-terminals/v1";
pub const COLLECTIONS_DOCUMENT_PROFILE_SCHEMA_ID_V1: &str =
    "https://vqro.dev/schema/collections/document-profile/v1";
pub const COLLECTIONS_ACTIONS_SCHEMA_ID_V1: &str = "https://vqro.dev/schema/collections/actions/v1";
pub const COLLECTIONS_ACTION_INVOCATION_SCHEMA_ID_V1: &str =
    "https://vqro.dev/schema/collections/action-invocation/v1";
pub const COLLECTIONS_EFFECT_PLAN_SCHEMA_ID_V1: &str =
    "https://vqro.dev/schema/collections/effect-plan/v1";
pub const COLLECTIONS_PRODUCTION_PIN_SCHEMA_ID_V1: &str =
    "https://vqro.dev/schema/collections/production-pin/v1";
pub const COLLECTIONS_AUTHORITY_SCHEMA_ID_V1: &str =
    "https://vqro.dev/schema/collections/authority/v1";
pub const EXTENSION_BUNDLE_SCHEMA_ID_V1: &str = "https://vqro.dev/schema/extensions/v1";
pub const PACKAGE_SCHEMA_ID_V1: &str = crate::extension_package::PACKAGE_SCHEMA_ID_V1;
pub const EXTENSION_MANIFEST_SCHEMA_ID_V2: &str = crate::extension_package::MANIFEST_SCHEMA_ID_V2;
pub const EXTENSION_MANIFEST_SCHEMA_ID_V3: &str = crate::extension_package::MANIFEST_SCHEMA_ID_V3;
pub const SERVICE_CONTRACT_SCHEMA_ID_V1: &str = "https://vqro.dev/schema/service-contract/v1";

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

pub fn manifest_v3_schema_document() -> serde_json::Value {
    let mut schema = serde_json::to_value(schemars::schema_for!(
        crate::extension_package::PackageManifestV3
    ))
    .expect("generated manifest-v3 schema serializes");
    let object = schema
        .as_object_mut()
        .expect("generated manifest-v3 schema is an object");
    object.insert(
        "$id".into(),
        serde_json::Value::String(EXTENSION_MANIFEST_SCHEMA_ID_V3.into()),
    );
    object.insert(
        "title".into(),
        serde_json::Value::String("vqro extension manifest v3".into()),
    );
    object.insert(
        "contract".into(),
        serde_json::Value::String(crate::extension_package::PACKAGE_CONTRACT_V1.into()),
    );
    schema["properties"]["manifest_version"]["const"] =
        serde_json::json!(crate::extension_package::MANIFEST_VERSION_V3);
    schema["properties"]["package_contract"]["const"] =
        serde_json::json!(crate::extension_package::PACKAGE_CONTRACT_V1);
    schema["properties"]["services"]["maxItems"] =
        serde_json::json!(crate::extension_package::MAX_SERVICES);
    schema["properties"]["namespaces"]["maxItems"] =
        serde_json::json!(crate::extension_package::MAX_NAMESPACES);
    schema["$defs"]["PackageServiceV2"]["properties"]["capabilities"]["maxItems"] =
        serde_json::json!(crate::extension_package::MAX_CAPABILITIES);
    for endpoint in ["exports", "imports"] {
        schema["$defs"]["PackageServiceV2"]["properties"][endpoint]["maxItems"] =
            serde_json::json!(crate::extension_package::MAX_SERVICE_ENDPOINTS);
    }
    for endpoint in ["ServiceContractExportV1", "ServiceContractImportV1"] {
        schema["$defs"][endpoint]["properties"]["descriptor_sha256"]["minLength"] =
            serde_json::json!(64);
        schema["$defs"][endpoint]["properties"]["descriptor_sha256"]["maxLength"] =
            serde_json::json!(64);
        schema["$defs"][endpoint]["properties"]["descriptor_sha256"]["pattern"] =
            serde_json::json!("^[0-9a-f]{64}$");
    }
    schema
}

pub fn service_contract_schema_document() -> serde_json::Value {
    let mut schema = serde_json::to_value(schemars::schema_for!(
        crate::extension_package::ServiceContractDescriptorV1
    ))
    .expect("generated service-contract schema serializes");
    let object = schema
        .as_object_mut()
        .expect("generated service-contract schema is an object");
    object.insert(
        "$id".into(),
        serde_json::Value::String(SERVICE_CONTRACT_SCHEMA_ID_V1.into()),
    );
    object.insert(
        "title".into(),
        serde_json::Value::String("vqro service contract descriptor v1".into()),
    );
    object.insert(
        "contract".into(),
        serde_json::Value::String(crate::extension_package::SERVICE_CONTRACT_V1.into()),
    );
    schema["properties"]["contract"]["const"] =
        serde_json::json!(crate::extension_package::SERVICE_CONTRACT_V1);
    schema["properties"]["methods"]["minItems"] = serde_json::json!(1);
    schema["properties"]["methods"]["maxItems"] =
        serde_json::json!(crate::extension_package::MAX_CONTRACT_METHODS);
    for flow in ["request_information", "response_information"] {
        schema["$defs"]["ServiceContractMethodV1"]["properties"][flow]["maxItems"] =
            serde_json::json!(crate::extension_package::MAX_INFORMATION_CATEGORIES);
        schema["$defs"]["ServiceContractMethodV1"]["properties"][flow]["items"]["pattern"] =
            serde_json::json!("^[a-z0-9_-]+(?:\\.[a-z0-9_-]+)+$");
    }
    for bound in ["max_request_bytes", "max_response_bytes"] {
        schema["$defs"]["ServiceContractMethodV1"]["properties"][bound]["minimum"] =
            serde_json::json!(1);
        schema["$defs"]["ServiceContractMethodV1"]["properties"][bound]["maximum"] =
            serde_json::json!(crate::extension_package::MAX_SERVICE_CALL_BYTES);
    }
    schema
}

pub fn runtime_service_schema_document() -> serde_json::Value {
    serde_json::json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": RUNTIME_SERVICE_SCHEMA_ID_V1,
        "title": "vqro runtime-neutral service contract v1",
        "contract": crate::extension_runtime::EXTENSION_SERVICE_CONTRACT_V1,
        "limits": {
            "max_frame_bytes": crate::extension_runtime::MAX_RUNTIME_FRAME_BYTES,
            "max_queue_depth": crate::extension_runtime::MAX_RUNTIME_QUEUE_DEPTH,
            "max_host_call_depth": crate::extension_runtime::MAX_HOST_CALL_DEPTH,
            "max_outstanding_host_calls": crate::extension_runtime::MAX_OUTSTANDING_HOST_CALLS,
            "max_host_calls_per_invocation": crate::extension_runtime::MAX_HOST_CALLS_PER_INVOCATION,
        },
        "schemas": {
            "request": scoped_schema::<crate::extension_runtime::RuntimeRequestV1>("request"),
            "event": scoped_schema::<crate::extension_runtime::RuntimeEventV1>("event"),
            "host_call": scoped_schema::<crate::extension_runtime::RuntimeHostCallV1>("host_call"),
            "host_call_response": scoped_schema::<crate::extension_runtime::RuntimeHostCallResponseV1>("host_call_response"),
            "error": scoped_schema::<crate::extension_runtime::RuntimeErrorBodyV1>("error"),
        }
    })
}

pub fn host_state_schema_document() -> serde_json::Value {
    let mut document = serde_json::json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": HOST_STATE_SCHEMA_ID_V1,
        "title": "staged host state contract v1",
        "contract": crate::extension_host_state::HOST_STATE_CONTRACT_V1,
        "availability": "staged; production package activation is not enabled",
        "capabilities": {
            "snapshot": crate::extension_host_state::HOST_STATE_READ_CAPABILITY,
            "transact": crate::extension_host_state::HOST_STATE_WRITE_CAPABILITY,
        },
        "schemas": {
            "snapshot_params": scoped_schema::<crate::extension_host_state::HostStateSnapshotParamsV1>("snapshot_params"),
            "snapshot": scoped_schema::<crate::extension_host_state::HostStateSnapshotV1>("snapshot"),
            "transact_params": scoped_schema::<crate::extension_host_state::HostStateTransactParamsV1>("transact_params"),
            "commit": scoped_schema::<crate::extension_host_state::HostStateCommitV1>("commit"),
        }
    });
    for schema in ["snapshot_params", "snapshot", "transact_params", "commit"] {
        document["schemas"][schema]["properties"]["contract"] = serde_json::json!({
            "const": crate::extension_host_state::HOST_STATE_CONTRACT_V1,
            "type": "string",
        });
    }
    document
}

pub fn host_document_v2_schema_document() -> serde_json::Value {
    let mut schema = serde_json::to_value(schemars::schema_for!(
        crate::host_document_v2::HostDocumentV2
    ))
    .expect("generated host.document.v2 schema serializes");
    let object = schema
        .as_object_mut()
        .expect("generated host.document.v2 schema is an object");
    object.insert(
        "$id".into(),
        serde_json::Value::String(HOST_DOCUMENT_SCHEMA_ID_V2.into()),
    );
    object.insert(
        "title".into(),
        serde_json::Value::String("canonical host document v2".into()),
    );
    object.insert(
        "contract".into(),
        serde_json::Value::String(crate::host_document_v2::HOST_DOCUMENT_CONTRACT_V2.into()),
    );
    object.insert(
        "validation_authority".into(),
        serde_json::json!({
            "json_schema": "structural and per-field constraints",
            "rust_validator": "normative for encoded/aggregate budgets, sorted dependencies, graph invariants, and terminal scope coverage",
        }),
    );
    object.insert(
        "limits".into(),
        serde_json::json!({
            "max_encoded_bytes": crate::host_document_v2::MAX_DOCUMENT_V2_BYTES,
            "max_nodes": crate::host_document_v2::MAX_DOCUMENT_V2_NODES,
            "max_depth": crate::host_document_v2::MAX_DOCUMENT_V2_DEPTH,
            "max_dependencies": crate::host_document_v2::MAX_DOCUMENT_V2_DEPENDENCIES,
            "max_text_chars": crate::host_document_v2::MAX_DOCUMENT_V2_TEXT_CHARS,
            "max_total_text_chars": crate::host_document_v2::MAX_DOCUMENT_V2_TOTAL_TEXT_CHARS,
            "max_accessible_name_chars": crate::host_document_v2::MAX_DOCUMENT_V2_ACCESSIBLE_NAME_CHARS,
            "max_accessible_description_chars": crate::host_document_v2::MAX_DOCUMENT_V2_ACCESSIBLE_DESCRIPTION_CHARS,
            "actions": 0,
            "effects": 0,
        }),
    );
    schema["properties"]["contract"] = serde_json::json!({
        "const": crate::host_document_v2::HOST_DOCUMENT_CONTRACT_V2,
        "type": "string",
    });
    schema["properties"]["revision"]["minimum"] = serde_json::json!(1);
    schema["properties"]["revision"]["maximum"] = serde_json::json!(u64::MAX);
    schema["properties"]["dependencies"]["maxItems"] =
        serde_json::json!(crate::host_document_v2::MAX_DOCUMENT_V2_DEPENDENCIES);
    schema["properties"]["dependencies"]["uniqueItems"] = serde_json::json!(true);
    schema["properties"]["roots"]["maxItems"] =
        serde_json::json!(crate::host_document_v2::MAX_DOCUMENT_V2_NODES);
    schema["properties"]["roots"]["uniqueItems"] = serde_json::json!(true);
    schema["properties"]["nodes"]["maxItems"] =
        serde_json::json!(crate::host_document_v2::MAX_DOCUMENT_V2_NODES);

    let opaque_id = serde_json::json!({
        "maxLength": crate::host_document_v2::MAX_DOCUMENT_V2_IDENTIFIER_BYTES,
        "minLength": 1,
        "pattern": "^[A-Za-z0-9._:/-]+$",
        "type": "string",
    });
    let contract_name = serde_json::json!({
        "maxLength": crate::host_document_v2::MAX_DOCUMENT_V2_IDENTIFIER_BYTES,
        "pattern": "^[a-z0-9-]+(?:\\.[a-z0-9-]+)+$",
        "type": "string",
    });
    let accessible_name = serde_json::json!({
        "maxLength": crate::host_document_v2::MAX_DOCUMENT_V2_ACCESSIBLE_NAME_CHARS,
        "minLength": 1,
        "pattern": r"^[^\u0000-\u001F\u007F-\u009F]*$",
        "type": "string",
    });
    let accessible_description = serde_json::json!({
        "maxLength": crate::host_document_v2::MAX_DOCUMENT_V2_ACCESSIBLE_DESCRIPTION_CHARS,
        "pattern": r"^[^\u0000-\u001F\u007F-\u009F]*$",
        "type": ["string", "null"],
    });
    schema["$defs"]["DocumentAccessibilityV2"]["properties"]["name"] = accessible_name;
    schema["$defs"]["DocumentAccessibilityV2"]["properties"]["description"] =
        accessible_description;

    for field in [
        "runtime_generation",
        "provider_generation",
        "scope_generation",
    ] {
        schema["$defs"]["DocumentProducerFenceV2"]["properties"][field]["minimum"] =
            serde_json::json!(1);
        schema["$defs"]["DocumentProducerFenceV2"]["properties"][field]["maximum"] =
            serde_json::json!(u64::MAX);
    }
    schema["$defs"]["DocumentProducerFenceV2"]["properties"]["package_id"] = contract_name.clone();
    schema["$defs"]["DocumentProducerFenceV2"]["properties"]["service_id"] = opaque_id.clone();
    schema["$defs"]["DocumentProducerFenceV2"]["properties"]["artifact_sha256"] = serde_json::json!({
        "maxLength": 64,
        "minLength": 64,
        "pattern": "^[0-9a-f]{64}$",
        "type": "string"
    });
    schema["$defs"]["DocumentScopeV2"]["properties"]["contract"] = contract_name.clone();
    schema["$defs"]["DocumentScopeV2"]["properties"]["scope_id"] = opaque_id.clone();
    schema["$defs"]["DocumentDependencyV2"]["properties"]["contract"] = contract_name;
    schema["$defs"]["DocumentDependencyV2"]["properties"]["scope_id"] = opaque_id.clone();
    for field in ["revision", "generation"] {
        schema["$defs"]["DocumentDependencyV2"]["properties"][field]["minimum"] =
            serde_json::json!(1);
        schema["$defs"]["DocumentDependencyV2"]["properties"][field]["maximum"] =
            serde_json::json!(u64::MAX);
    }

    schema["properties"]["roots"]["items"] = opaque_id.clone();
    let variants = schema["$defs"]["DocumentNodeV2"]["oneOf"]
        .as_array_mut()
        .expect("host.document.v2 node variants are an array");
    for variant in variants.iter_mut() {
        variant["properties"]["id"] = opaque_id.clone();
    }
    variants[0]["properties"]["children"]["items"] = opaque_id.clone();
    variants[0]["properties"]["children"]["maxItems"] =
        serde_json::json!(crate::host_document_v2::MAX_DOCUMENT_V2_NODES);
    variants[0]["properties"]["children"]["uniqueItems"] = serde_json::json!(true);
    variants[1]["properties"]["text"] = serde_json::json!({
        "maxLength": crate::host_document_v2::MAX_DOCUMENT_V2_TEXT_CHARS,
        "pattern": r"^[^\u0000-\u001F\u007F-\u009F]*$",
        "type": "string",
    });
    variants[2]["properties"]["terminal_id"] = serde_json::json!({
        "maxLength": crate::terminal::MAX_TERMINAL_ID_BYTES,
        "minLength": crate::terminal::MAX_TERMINAL_ID_BYTES,
        "pattern": "^term_[0-9a-f]{32}$",
        "type": "string"
    });
    schema
}

pub fn host_document_render_v2_schema_document() -> serde_json::Value {
    let mut schema = serde_json::to_value(schemars::schema_for!(
        crate::host_document_v2::HostDocumentRenderParamsV2
    ))
    .expect("generated host.document.render.v2 schema serializes");
    let object = schema
        .as_object_mut()
        .expect("generated host.document.render.v2 schema is an object");
    object.insert(
        "$id".into(),
        serde_json::Value::String(HOST_DOCUMENT_RENDER_SCHEMA_ID_V2.into()),
    );
    object.insert(
        "title".into(),
        serde_json::Value::String("host document render request v2".into()),
    );
    object.insert(
        "contract".into(),
        serde_json::Value::String(crate::host_document_v2::HOST_DOCUMENT_RENDER_CONTRACT_V2.into()),
    );
    object.insert(
        "limits".into(),
        serde_json::json!({
            "max_encoded_bytes": crate::host_document_v2::MAX_DOCUMENT_RENDER_V2_BYTES,
            "actions": 0,
            "effects": 0,
            "authorized_terminal_ids": 0,
            "dependencies": 0,
        }),
    );
    schema["properties"]["contract"] = serde_json::json!({
        "const": crate::host_document_v2::HOST_DOCUMENT_RENDER_CONTRACT_V2,
        "type": "string",
    });
    schema["properties"]["requested_revision"]["minimum"] = serde_json::json!(1);
    schema["properties"]["requested_revision"]["maximum"] = serde_json::json!(u64::MAX);
    for field in [
        "runtime_generation",
        "provider_generation",
        "scope_generation",
    ] {
        schema["$defs"]["DocumentProducerFenceV2"]["properties"][field]["minimum"] =
            serde_json::json!(1);
        schema["$defs"]["DocumentProducerFenceV2"]["properties"][field]["maximum"] =
            serde_json::json!(u64::MAX);
    }
    schema["$defs"]["DocumentProducerFenceV2"]["properties"]["package_id"] = serde_json::json!({
        "maxLength": crate::host_document_v2::MAX_DOCUMENT_V2_IDENTIFIER_BYTES,
        "pattern": "^[a-z0-9-]+(?:\\.[a-z0-9-]+)+$",
        "type": "string",
    });
    schema["$defs"]["DocumentProducerFenceV2"]["properties"]["service_id"] = serde_json::json!({
        "maxLength": crate::host_document_v2::MAX_DOCUMENT_V2_IDENTIFIER_BYTES,
        "minLength": 1,
        "pattern": "^[A-Za-z0-9._:/-]+$",
        "type": "string",
    });
    schema["$defs"]["DocumentProducerFenceV2"]["properties"]["artifact_sha256"] = serde_json::json!({
        "maxLength": 64,
        "minLength": 64,
        "pattern": "^[0-9a-f]{64}$",
        "type": "string",
    });
    schema["$defs"]["DocumentScopeV2"]["properties"]["contract"] = serde_json::json!({
        "maxLength": crate::host_document_v2::MAX_DOCUMENT_V2_IDENTIFIER_BYTES,
        "pattern": "^[a-z0-9-]+(?:\\.[a-z0-9-]+)+$",
        "type": "string",
    });
    schema["$defs"]["DocumentScopeV2"]["properties"]["scope_id"] = serde_json::json!({
        "maxLength": crate::host_document_v2::MAX_DOCUMENT_V2_IDENTIFIER_BYTES,
        "minLength": 1,
        "pattern": "^[A-Za-z0-9._:/-]+$",
        "type": "string",
    });
    schema
}

pub fn collections_production_pin_schema_document() -> serde_json::Value {
    let mut schema =
        collections_schema::<crate::collections_extension_contract::CollectionsProductionPinV1>(
            COLLECTIONS_PRODUCTION_PIN_SCHEMA_ID_V1,
            "vqro Collections production pin v1",
            crate::collections_extension_contract::COLLECTIONS_PRODUCTION_PIN_CONTRACT_V1,
        );
    schema["properties"]["package_id"]["const"] =
        serde_json::json!(crate::collections_extension_contract::COLLECTIONS_PACKAGE_ID);
    schema["properties"]["service_id"]["const"] = serde_json::json!("collections");
    schema["properties"]["source_url"] = serde_json::json!({
        "format": "uri",
        "pattern": "^https://[^@]+$",
        "type": "string",
    });
    schema["properties"]["source_commit"] = lower_hex_schema(40);
    for field in ["package_sha256", "manifest_sha256", "component_sha256"] {
        schema["properties"][field] = lower_hex_schema(64);
    }
    schema["properties"]["package_path"] = serde_json::json!({
        "pattern": r"^[A-Za-z0-9._/-]+\.vqrox$",
        "type": "string",
    });
    schema["properties"]["world"]["const"] = serde_json::json!("vqro:extension/service@1.0.0");
    schema["properties"]["capabilities"] = serde_json::json!({
        "const": ["host.state.read", "host.terminals.read"],
        "type": "array",
    });
    schema["properties"]["profile_contract"]["const"] = serde_json::json!(
        crate::collections_extension_contract::COLLECTIONS_DOCUMENT_PROFILE_CONTRACT_V1
    );
    schema["validation_authority"] = serde_json::json!({
        "json_schema": "structural and canonical field forms",
        "rust_validator": "normative for credential-free source URLs, relative package paths, exact capabilities, and digest syntax",
    });
    schema
}

pub fn collections_authority_schema_document() -> serde_json::Value {
    let mut schema =
        collections_schema::<crate::collections_extension_contract::CollectionsAuthorityV1>(
            COLLECTIONS_AUTHORITY_SCHEMA_ID_V1,
            "Vqro Collections durable authority record v1",
            crate::collections_extension_contract::COLLECTIONS_AUTHORITY_CONTRACT_V1,
        );
    schema["properties"]["authority_epoch"]["minimum"] = serde_json::json!(1);
    schema["properties"]["minimum_reader_version"] = serde_json::json!({
        "const": crate::collections_extension_contract::COLLECTIONS_PACKAGE_AUTHORITY_MINIMUM_READER_VERSION,
        "type": "integer",
    });
    schema["$defs"]["CollectionsAuthorityPackageV1"]["properties"]["package_id"]["const"] =
        serde_json::json!(crate::collections_extension_contract::COLLECTIONS_PACKAGE_ID);
    schema["$defs"]["CollectionsAuthorityPackageV1"]["properties"]["service_id"]["const"] =
        serde_json::json!("collections");
    for field in ["package_sha256", "component_sha256"] {
        schema["$defs"]["CollectionsAuthorityPackageV1"]["properties"][field] =
            lower_hex_schema(64);
    }
    schema["$defs"]["CollectionsAuthorityPackageV1"]["properties"]["profile_contract"]["const"] = serde_json::json!(
        crate::collections_extension_contract::COLLECTIONS_DOCUMENT_PROFILE_CONTRACT_V1
    );
    schema["$defs"]["CollectionsAuthorityStateV1"]["properties"]["store_generation"]["minimum"] =
        serde_json::json!(1);
    schema["validation_authority"] = serde_json::json!({
        "json_schema": "structural and canonical field forms",
        "rust_validator": "normative for phase-specific optional fields and package binding",
        "snapshot_version": crate::collections_extension_contract::COLLECTIONS_PACKAGE_AUTHORITY_SNAPSHOT_VERSION,
    });
    schema
}

pub fn collections_document_profile_schema_document() -> serde_json::Value {
    let mut schema =
        collections_schema::<crate::collections_extension_contract::CollectionsDocumentProfileV1>(
            COLLECTIONS_DOCUMENT_PROFILE_SCHEMA_ID_V1,
            "vqro Collections document profile v1",
            crate::collections_extension_contract::COLLECTIONS_DOCUMENT_PROFILE_CONTRACT_V1,
        );
    schema["properties"]["document"] = serde_json::json!({
        "$ref": HOST_DOCUMENT_SCHEMA_ID_V2,
    });
    schema["properties"]["collections"]["maxItems"] =
        serde_json::json!(crate::host_document_v2::MAX_DOCUMENT_V2_NODES);
    let collection_definition = schema["$defs"]["CollectionsDocumentCollectionV1"].take();
    schema["$defs"] = serde_json::json!({
        "CollectionsDocumentCollectionV1": collection_definition,
    });
    schema["$defs"]["CollectionsDocumentCollectionV1"]["properties"]["container_id"] =
        container_id_schema();
    schema["$defs"]["CollectionsDocumentCollectionV1"]["properties"]["label"] =
        collections_text_schema(
            crate::collections_extension_contract::MAX_COLLECTIONS_LABEL_BYTES,
            false,
        );
    schema["$defs"]["CollectionsDocumentCollectionV1"]["properties"]["label"]["type"] =
        serde_json::json!(["string", "null"]);
    schema["validation_authority"] = serde_json::json!({
        "json_schema": "structural and per-field constraints",
        "rust_validator": "normative for exact topology coverage, canonical node ids, archive shape, ordering, and synchronized compatibility projection",
    });
    schema
}

pub fn collections_actions_schema_document() -> serde_json::Value {
    let mut schema = collections_schema::<
        crate::collections_extension_contract::CollectionsActionDeclarationsV1,
    >(
        COLLECTIONS_ACTIONS_SCHEMA_ID_V1,
        "vqro Collections action declarations v1",
        crate::collections_extension_contract::COLLECTIONS_ACTIONS_CONTRACT_V1,
    );
    schema["properties"]["document_revision"]["minimum"] = serde_json::json!(1);
    schema["properties"]["revision"]["minimum"] = serde_json::json!(1);
    schema["properties"]["actions"]["maxItems"] =
        serde_json::json!(crate::collections_extension_contract::MAX_COLLECTIONS_ACTIONS);
    for field in ["id", "subject_node_id"] {
        schema["$defs"]["CollectionsActionDeclarationV1"]["properties"][field] =
            collections_identifier_schema();
    }
    schema["$defs"]["CollectionsActionDeclarationV1"]["properties"]["label"] =
        collections_text_schema(
            crate::collections_extension_contract::MAX_COLLECTIONS_ACTION_LABEL_BYTES,
            true,
        );
    schema["validation_authority"] = serde_json::json!({
        "json_schema": "structural and per-field constraints",
        "rust_validator": "normative for unique ids, typed subjects, and profile/document revision binding",
    });
    schema
}

pub fn collections_action_invocation_schema_document() -> serde_json::Value {
    let mut schema =
        collections_schema::<crate::collections_extension_contract::CollectionsActionInvocationV1>(
            COLLECTIONS_ACTION_INVOCATION_SCHEMA_ID_V1,
            "vqro Collections action invocation v1",
            crate::collections_extension_contract::COLLECTIONS_ACTION_INVOCATION_CONTRACT_V1,
        );
    for field in ["ticket", "idempotency_key", "action_id"] {
        schema["properties"][field] = collections_identifier_schema();
    }
    for field in [
        "document_revision",
        "actions_revision",
        "expected_store_generation",
    ] {
        schema["properties"][field]["minimum"] = serde_json::json!(1);
        schema["properties"][field]["maximum"] = serde_json::json!(u64::MAX);
    }
    schema["properties"]["expected_namespace_revision"]["maximum"] = serde_json::json!(u64::MAX);
    let input_variants = schema["$defs"]["CollectionsActionInputV1"]["oneOf"]
        .as_array_mut()
        .expect("Collections action input variants are an array");
    input_variants[0]["properties"]["container_id"] = container_id_schema();
    input_variants[0]["properties"]["label"] = collections_text_schema(
        crate::collections_extension_contract::MAX_COLLECTIONS_LABEL_BYTES,
        false,
    );
    input_variants[0]["properties"]["label"]["type"] = serde_json::json!(["string", "null"]);
    input_variants[1]["properties"]["container_id"] = container_id_schema();
    input_variants[1]["properties"]["terminal_id"] = terminal_id_schema();
    let dependency_variants = schema["$defs"]["CollectionsActionDependencyV1"]["oneOf"]
        .as_array_mut()
        .expect("Collections action dependency variants are an array");
    dependency_variants[0]["properties"]["container_id"] = container_id_schema();
    dependency_variants[1]["properties"]["container_id"] = container_id_schema();
    dependency_variants[1]["properties"]["terminal_id"] = terminal_id_schema();
    dependency_variants[1]["properties"]["terminal_snapshot_fingerprint_sha256"] = serde_json::json!({
        "maxLength": 64,
        "minLength": 64,
        "pattern": "^[0-9a-f]{64}$",
        "type": "string",
    });
    dependency_variants[1]["properties"]["provider_generation"]["minimum"] = serde_json::json!(1);
    dependency_variants[1]["properties"]["provider_generation"]["maximum"] =
        serde_json::json!(u64::MAX);
    schema["validation_authority"] = serde_json::json!({
        "json_schema": "structural and per-field constraints",
        "rust_validator": "normative for canonical identities, label byte/control bounds, declaration matching, and revision fences",
    });
    schema
}

pub fn collections_effect_plan_schema_document() -> serde_json::Value {
    let mut schema =
        collections_schema::<crate::collections_extension_contract::CollectionsEffectPlanV1>(
            COLLECTIONS_EFFECT_PLAN_SCHEMA_ID_V1,
            "vqro Collections effect plan v1",
            crate::collections_extension_contract::COLLECTIONS_EFFECT_PLAN_CONTRACT_V1,
        );
    for field in ["ticket", "idempotency_key"] {
        schema["properties"][field] = collections_identifier_schema();
    }
    schema["properties"]["expected_store_generation"]["minimum"] = serde_json::json!(1);
    schema["properties"]["expected_store_generation"]["maximum"] = serde_json::json!(u64::MAX);
    schema["properties"]["expected_namespace_revision"]["maximum"] = serde_json::json!(u64::MAX);
    schema["$defs"]["CollectionsStateCasV1"]["properties"]["key"] = container_id_schema();
    schema["limits"] = serde_json::json!({
        "state_operations": 1,
        "max_state_value_bytes": crate::extension_state::MAX_STATE_VALUE_BYTES,
    });
    schema["validation_authority"] = serde_json::json!({
        "json_schema": "structural and per-field constraints",
        "rust_validator": "normative for one-shot ticket, idempotency, CAS, subject-key binding, and value byte bounds",
    });
    schema
}

fn collections_schema<T: schemars::JsonSchema>(
    id: &str,
    title: &str,
    contract: &str,
) -> serde_json::Value {
    let mut schema = serde_json::to_value(schemars::schema_for!(T))
        .expect("generated Collections schema serializes");
    schema["$id"] = serde_json::Value::String(id.into());
    schema["title"] = serde_json::Value::String(title.into());
    schema["contract"] = serde_json::Value::String(contract.into());
    schema["properties"]["contract"] = serde_json::json!({
        "const": contract,
        "type": "string",
    });
    schema
}

fn collections_identifier_schema() -> serde_json::Value {
    serde_json::json!({
        "maxLength": crate::collections_extension_contract::MAX_COLLECTIONS_IDENTIFIER_BYTES,
        "minLength": 1,
        "pattern": "^[A-Za-z0-9._:/-]+$",
        "type": "string",
    })
}

fn collections_text_schema(max_length: usize, nonempty: bool) -> serde_json::Value {
    let mut schema = serde_json::json!({
        "maxLength": max_length,
        "pattern": r"^[^\u0000-\u001F\u007F-\u009F]*$",
        "type": "string",
    });
    if nonempty {
        schema["minLength"] = serde_json::json!(1);
    }
    schema
}

fn lower_hex_schema(length: usize) -> serde_json::Value {
    serde_json::json!({
        "maxLength": length,
        "minLength": length,
        "pattern": format!("^[0-9a-f]{{{length}}}$"),
        "type": "string",
    })
}

fn terminal_id_schema() -> serde_json::Value {
    serde_json::json!({
        "maxLength": crate::terminal::MAX_TERMINAL_ID_BYTES,
        "minLength": crate::terminal::MAX_TERMINAL_ID_BYTES,
        "pattern": "^term_[0-9a-f]{32}$",
        "type": "string",
    })
}

fn container_id_schema() -> serde_json::Value {
    serde_json::json!({
        "maxLength": crate::extension_host_terminals::HOST_CONTAINER_ID_BYTES,
        "minLength": crate::extension_host_terminals::HOST_CONTAINER_ID_BYTES,
        "pattern": "^container_[0-9a-f]{16}$",
        "type": "string",
    })
}

pub fn host_terminals_schema_document() -> serde_json::Value {
    let mut document = serde_json::json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": HOST_TERMINALS_SCHEMA_ID_V1,
        "title": "staged read-only host terminals contract v1",
        "contract": crate::extension_host_terminals::HOST_TERMINALS_CONTRACT_V1,
        "availability": "staged; production package activation is not enabled",
        "capabilities": {
            "snapshot": crate::extension_host_terminals::HOST_TERMINALS_READ_CAPABILITY,
        },
        "methods": [crate::extension_host_terminals::HOST_TERMINALS_SNAPSHOT_METHOD],
        "limits": {
            "max_response_bytes": crate::extension_host_terminals::MAX_HOST_TERMINALS_RESPONSE_BYTES,
            "max_terminals_per_tab": crate::admission::MAX_TAB_PANES,
            "max_containers_per_tab": crate::admission::MAX_GROUPS_PER_TAB,
            "max_members_per_container": crate::admission::MAX_GROUP_MEMBERS,
            "max_layout_items": crate::admission::MAX_TAB_PANES + crate::admission::MAX_GROUPS_PER_TAB,
        },
        "schemas": {
            "snapshot_params": scoped_schema::<crate::extension_host_terminals::HostTerminalsSnapshotParamsV1>("snapshot_params"),
            "snapshot": scoped_schema::<crate::extension_host_terminals::HostTerminalsSnapshotV1>("snapshot"),
        }
    });
    for schema in ["snapshot_params", "snapshot"] {
        document["schemas"][schema]["properties"]["contract"] = serde_json::json!({
            "const": crate::extension_host_terminals::HOST_TERMINALS_CONTRACT_V1,
            "type": "string",
        });
    }
    document["validation_authority"] = serde_json::json!({
        "json_schema": "structural, canonical field forms, and per-array bounds",
        "rust_validator": "normative for recomputed fingerprint, global identity uniqueness, container selection integrity, and cross-field topology bounds",
    });
    let snapshot = &mut document["schemas"]["snapshot"];
    for field in [
        "catalog_generation",
        "package_generation",
        "service_generation",
        "artifact_generation",
        "provider_generation",
        "runtime_generation",
    ] {
        snapshot["$defs"]["HostTerminalsLeaseFenceV1"]["properties"][field]["minimum"] =
            serde_json::json!(1);
        snapshot["$defs"]["HostTerminalsLeaseFenceV1"]["properties"][field]["maximum"] =
            serde_json::json!(u64::MAX);
    }
    snapshot["properties"]["fingerprint_sha256"] = serde_json::json!({
        "maxLength": 64,
        "minLength": 64,
        "pattern": "^[0-9a-f]{64}$",
        "type": "string",
    });
    snapshot["$defs"]["TabId"] = serde_json::json!({
        "description": "Stable opaque identity for a server-owned tab.",
        "maxLength": crate::workspace::MAX_TAB_ID_BYTES,
        "minLength": crate::workspace::MAX_TAB_ID_BYTES,
        "pattern": "^tab_[0-9a-f]{32}$",
        "type": "string",
    });
    snapshot["$defs"]["TerminalId"] = serde_json::json!({
        "description": "Stable opaque identity for a server-owned logical terminal.",
        "maxLength": crate::terminal::MAX_TERMINAL_ID_BYTES,
        "minLength": crate::terminal::MAX_TERMINAL_ID_BYTES,
        "pattern": "^term_[0-9a-f]{32}$",
        "type": "string",
    });
    snapshot["properties"]["workspace_id"] = serde_json::json!({
        "maxLength": crate::extension_host_terminals::MAX_HOST_TERMINALS_WORKSPACE_ID_BYTES,
        "minLength": 2,
        "pattern": "^w[123456789ABCDEFGHJKMNPQRSTVWXYZ0]+$",
        "type": "string",
    });
    snapshot["properties"]["layout"]["maxItems"] =
        serde_json::json!(crate::admission::MAX_TAB_PANES + crate::admission::MAX_GROUPS_PER_TAB);
    snapshot["properties"]["layout"]["uniqueItems"] = serde_json::json!(true);
    let variants = snapshot["$defs"]["HostTerminalLayoutItemV1"]["oneOf"]
        .as_array_mut()
        .expect("terminal layout variants are an array");
    variants[1]["properties"]["container_id"] = serde_json::json!({
        "maxLength": crate::extension_host_terminals::HOST_CONTAINER_ID_BYTES,
        "minLength": crate::extension_host_terminals::HOST_CONTAINER_ID_BYTES,
        "pattern": "^container_[0-9a-f]{16}$",
        "type": "string",
    });
    variants[1]["properties"]["terminal_ids"]["maxItems"] =
        serde_json::json!(crate::admission::MAX_GROUP_MEMBERS);
    variants[1]["properties"]["terminal_ids"]["uniqueItems"] = serde_json::json!(true);
    document
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
            "manifest_v3": scoped_schema::<crate::extension_package::PackageManifestV3>("manifest_v3"),
            "service_contract": scoped_schema::<crate::extension_package::ServiceContractDescriptorV1>("service_contract"),
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

pub fn host_state_schema_json() -> &'static str {
    include_str!("../docs/next/api/host-state-v1.schema.json")
}

pub fn host_document_v2_schema_json() -> &'static str {
    include_str!("../docs/next/api/host-document-v2.schema.json")
}

pub fn host_document_render_v2_schema_json() -> &'static str {
    include_str!("../docs/next/api/host-document-render-v2.schema.json")
}

pub fn host_terminals_schema_json() -> &'static str {
    include_str!("../docs/next/api/host-terminals-v1.schema.json")
}

pub fn collections_production_pin_schema_json() -> &'static str {
    include_str!("../docs/next/api/vqro-collections-production-pin-v1.schema.json")
}

pub fn collections_authority_schema_json() -> &'static str {
    include_str!("../docs/next/api/collections-authority-v1.schema.json")
}

pub fn collections_document_profile_schema_json() -> &'static str {
    include_str!("../docs/next/api/vqro-collections-document-profile-v1.schema.json")
}

pub fn collections_actions_schema_json() -> &'static str {
    include_str!("../docs/next/api/vqro-collections-actions-v1.schema.json")
}

pub fn collections_action_invocation_schema_json() -> &'static str {
    include_str!("../docs/next/api/vqro-collections-action-invocation-v1.schema.json")
}

pub fn collections_effect_plan_schema_json() -> &'static str {
    include_str!("../docs/next/api/vqro-collections-effect-plan-v1.schema.json")
}

pub fn service_schema_json() -> &'static str {
    include_str!("../docs/next/api/vqro-extension-service.schema.json")
}

pub fn runtime_service_schema_json() -> &'static str {
    include_str!("../docs/next/api/vqro-service-v1.schema.json")
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
    fn runtime_service_schema_has_no_dangling_local_references() {
        fn visit(root: &serde_json::Value, value: &serde_json::Value) {
            match value {
                serde_json::Value::Object(object) => {
                    if let Some(reference) = object.get("$ref").and_then(serde_json::Value::as_str)
                    {
                        let pointer = reference
                            .strip_prefix('#')
                            .expect("runtime schema uses only local references");
                        assert!(
                            root.pointer(pointer).is_some(),
                            "dangling reference: {reference}"
                        );
                    }
                    for child in object.values() {
                        visit(root, child);
                    }
                }
                serde_json::Value::Array(items) => {
                    for child in items {
                        visit(root, child);
                    }
                }
                _ => {}
            }
        }

        let schema = runtime_service_schema_document();
        visit(&schema, &schema);
    }

    #[test]
    fn host_document_v2_schema_covers_expressible_validator_constraints() {
        let schema = host_document_v2_schema_document();
        assert_eq!(
            schema["properties"]["contract"]["const"],
            crate::host_document_v2::HOST_DOCUMENT_CONTRACT_V2
        );
        assert_eq!(schema["additionalProperties"], false);
        assert_eq!(
            schema["properties"]["revision"]["maximum"],
            serde_json::json!(u64::MAX)
        );
        assert_eq!(
            schema["properties"]["nodes"]["maxItems"],
            crate::host_document_v2::MAX_DOCUMENT_V2_NODES
        );
        let opaque_pattern = "^[A-Za-z0-9._:/-]+$";
        let contract_pattern = "^[a-z0-9-]+(?:\\.[a-z0-9-]+)+$";
        assert_eq!(
            schema["properties"]["roots"]["items"]["pattern"],
            opaque_pattern
        );
        assert_eq!(
            schema["$defs"]["DocumentProducerFenceV2"]["properties"]["package_id"]["pattern"],
            contract_pattern
        );
        assert_eq!(
            schema["$defs"]["DocumentProducerFenceV2"]["properties"]["service_id"]["pattern"],
            opaque_pattern
        );
        assert_eq!(
            schema["$defs"]["DocumentProducerFenceV2"]["properties"]["artifact_sha256"]["pattern"],
            "^[0-9a-f]{64}$"
        );
        assert_eq!(
            schema["$defs"]["DocumentScopeV2"]["properties"]["contract"]["pattern"],
            contract_pattern
        );
        assert_eq!(
            schema["$defs"]["DocumentScopeV2"]["properties"]["scope_id"]["pattern"],
            opaque_pattern
        );
        assert_eq!(
            schema["$defs"]["DocumentDependencyV2"]["properties"]["contract"]["pattern"],
            contract_pattern
        );
        assert_eq!(
            schema["$defs"]["DocumentDependencyV2"]["properties"]["scope_id"]["pattern"],
            opaque_pattern
        );
        assert_eq!(
            schema["$defs"]["DocumentNodeV2"]["oneOf"][0]["properties"]["children"]["items"]
                ["pattern"],
            opaque_pattern
        );
        for variant in schema["$defs"]["DocumentNodeV2"]["oneOf"]
            .as_array()
            .unwrap()
        {
            assert_eq!(variant["properties"]["id"]["pattern"], opaque_pattern);
        }
        assert_eq!(
            schema["$defs"]["DocumentNodeV2"]["oneOf"][2]["properties"]["terminal_id"]["pattern"],
            "^term_[0-9a-f]{32}$"
        );
        for field in [
            "runtime_generation",
            "provider_generation",
            "scope_generation",
        ] {
            assert_eq!(
                schema["$defs"]["DocumentProducerFenceV2"]["properties"][field]["maximum"],
                serde_json::json!(u64::MAX)
            );
        }
        for field in ["revision", "generation"] {
            assert_eq!(
                schema["$defs"]["DocumentDependencyV2"]["properties"][field]["maximum"],
                serde_json::json!(u64::MAX)
            );
        }
    }

    #[test]
    fn host_terminals_schema_matches_strict_snapshot_validation() {
        let schema = host_terminals_schema_document();
        let snapshot = &schema["schemas"]["snapshot"];
        assert_eq!(
            schema["schemas"]["snapshot_params"]["properties"]["contract"]["const"],
            crate::extension_host_terminals::HOST_TERMINALS_CONTRACT_V1
        );
        assert_eq!(
            snapshot["properties"]["contract"]["const"],
            crate::extension_host_terminals::HOST_TERMINALS_CONTRACT_V1
        );
        for field in [
            "catalog_generation",
            "package_generation",
            "service_generation",
            "artifact_generation",
            "provider_generation",
            "runtime_generation",
        ] {
            let generation = &snapshot["$defs"]["HostTerminalsLeaseFenceV1"]["properties"][field];
            assert_eq!(generation["minimum"], 1);
            assert_eq!(generation["maximum"], serde_json::json!(u64::MAX));
        }
        let fingerprint = &snapshot["properties"]["fingerprint_sha256"];
        assert_eq!(fingerprint["pattern"], "^[0-9a-f]{64}$");
        assert_eq!(fingerprint["minLength"], 64);
        assert_eq!(fingerprint["maxLength"], 64);
        let tab_id = &snapshot["$defs"]["TabId"];
        assert_eq!(tab_id["pattern"], "^tab_[0-9a-f]{32}$");
        assert_eq!(tab_id["minLength"], crate::workspace::MAX_TAB_ID_BYTES);
        assert_eq!(tab_id["maxLength"], crate::workspace::MAX_TAB_ID_BYTES);
        let terminal_id = &snapshot["$defs"]["TerminalId"];
        assert_eq!(terminal_id["pattern"], "^term_[0-9a-f]{32}$");
        assert_eq!(
            terminal_id["minLength"],
            crate::terminal::MAX_TERMINAL_ID_BYTES
        );
        assert_eq!(
            terminal_id["maxLength"],
            crate::terminal::MAX_TERMINAL_ID_BYTES
        );
        let workspace_id = &snapshot["properties"]["workspace_id"];
        assert_eq!(
            workspace_id["pattern"],
            "^w[123456789ABCDEFGHJKMNPQRSTVWXYZ0]+$"
        );
        assert_eq!(workspace_id["minLength"], 2);
        assert_eq!(
            workspace_id["maxLength"],
            crate::extension_host_terminals::MAX_HOST_TERMINALS_WORKSPACE_ID_BYTES
        );
        assert_eq!(
            snapshot["properties"]["layout"]["maxItems"],
            crate::admission::MAX_TAB_PANES + crate::admission::MAX_GROUPS_PER_TAB
        );
        assert_eq!(snapshot["properties"]["layout"]["uniqueItems"], true);
        let container = &snapshot["$defs"]["HostTerminalLayoutItemV1"]["oneOf"][1];
        let container_id = &container["properties"]["container_id"];
        assert_eq!(container_id["pattern"], "^container_[0-9a-f]{16}$");
        assert_eq!(
            container_id["minLength"],
            crate::extension_host_terminals::HOST_CONTAINER_ID_BYTES
        );
        assert_eq!(
            container_id["maxLength"],
            crate::extension_host_terminals::HOST_CONTAINER_ID_BYTES
        );
        assert_eq!(
            container["properties"]["terminal_ids"]["maxItems"],
            crate::admission::MAX_GROUP_MEMBERS
        );
        assert_eq!(container["properties"]["terminal_ids"]["uniqueItems"], true);
    }

    #[test]
    fn host_document_render_v2_schema_is_strict_and_authority_free() {
        let schema = host_document_render_v2_schema_document();
        assert_eq!(
            schema["properties"]["contract"]["const"],
            crate::host_document_v2::HOST_DOCUMENT_RENDER_CONTRACT_V2
        );
        assert_eq!(schema["additionalProperties"], false);
        assert_eq!(schema["properties"]["requested_revision"]["minimum"], 1);
        let properties = schema["properties"].as_object().unwrap();
        assert_eq!(
            properties.keys().map(String::as_str).collect::<Vec<_>>(),
            ["contract", "producer", "requested_revision", "scope"]
        );
        for forbidden in [
            "dependencies",
            "terminal_ids",
            "authorized_terminal_ids",
            "actions",
            "effects",
        ] {
            assert!(!properties.contains_key(forbidden));
        }
    }

    #[test]
    fn golden_fixtures_observe_the_structural_schema_boundary() {
        let fixture = |name: &str| {
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("docs/next/api/host-document-v2.fixtures")
                .join(name);
            std::fs::read(path).unwrap()
        };
        serde_json::from_slice::<crate::host_document_v2::HostDocumentV2>(&fixture("valid.json"))
            .unwrap();
        for invalid in ["invalid-unknown-field.json", "invalid-action.json"] {
            assert!(
                serde_json::from_slice::<crate::host_document_v2::HostDocumentV2>(&fixture(
                    invalid
                ))
                .is_err()
            );
        }
        // Reachability is deliberately validator-normative, not structural JSON Schema.
        serde_json::from_slice::<crate::host_document_v2::HostDocumentV2>(&fixture(
            "invalid-unreachable.json",
        ))
        .unwrap();
    }

    #[test]
    fn generated_package_and_manifest_v2_schema_artifacts_are_current() {
        for (relative, document) in [
            (
                "docs/next/api/host-state-v1.schema.json",
                host_state_schema_document(),
            ),
            (
                "docs/next/api/host-document-v2.schema.json",
                host_document_v2_schema_document(),
            ),
            (
                "docs/next/api/host-document-render-v2.schema.json",
                host_document_render_v2_schema_document(),
            ),
            (
                "docs/next/api/host-terminals-v1.schema.json",
                host_terminals_schema_document(),
            ),
            (
                "docs/next/api/vqro-collections-production-pin-v1.schema.json",
                collections_production_pin_schema_document(),
            ),
            (
                "docs/next/api/collections-authority-v1.schema.json",
                collections_authority_schema_document(),
            ),
            (
                "docs/next/api/vqro-collections-document-profile-v1.schema.json",
                collections_document_profile_schema_document(),
            ),
            (
                "docs/next/api/vqro-collections-actions-v1.schema.json",
                collections_actions_schema_document(),
            ),
            (
                "docs/next/api/vqro-collections-action-invocation-v1.schema.json",
                collections_action_invocation_schema_document(),
            ),
            (
                "docs/next/api/vqro-collections-effect-plan-v1.schema.json",
                collections_effect_plan_schema_document(),
            ),
            (
                "docs/next/api/vqro-service-v1.schema.json",
                runtime_service_schema_document(),
            ),
            (
                "docs/next/api/vqro-extension-manifest-v2.schema.json",
                manifest_v2_schema_document(),
            ),
            (
                "docs/next/api/vqro-extension-manifest-v3.schema.json",
                manifest_v3_schema_document(),
            ),
            (
                "docs/next/api/vqro-service-contract-v1.schema.json",
                service_contract_schema_document(),
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
        for schema in [
            manifest_schema_document(),
            service_schema_document(),
            host_document_v2_schema_document(),
            host_document_render_v2_schema_document(),
        ] {
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
