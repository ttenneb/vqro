//! Public vqro JSON API identity, independent from private wire protocol and endpoint bridge.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const NATIVE_API_CONTRACT_V1: &str = "vqro.api.v1";
pub const NATIVE_API_MAJOR_VERSION: u32 = 1;
pub const NATIVE_API_SCHEMA_ID: &str = "https://vqro.dev/schema/api/v1";
pub const NATIVE_API_REQUEST_KIND_V1: &str = "vqro.api.request.v1";
pub const NATIVE_API_RESPONSE_KIND_V1: &str = "vqro.api.response.v1";
pub const NATIVE_API_CAPABILITY_V1: &str = "vqro_json_api_v1";

/// Socket-only streaming/lifecycle methods and endpoint presentation controls
/// cannot use the bounded request/response endpoint tunnel.
pub const NATIVE_ENDPOINT_EXCLUDED_METHODS_V1: &[&str] = &[
    "client_shell.surface.set",
    "events.subscribe",
    "pane.graphics.stream.direct",
    "pane.graphics.stream.set",
    "server.live_handoff",
    "server.stop",
];

pub fn endpoint_supports(method: &crate::api::schema::Method) -> bool {
    !NATIVE_ENDPOINT_EXCLUDED_METHODS_V1.contains(&crate::api::api_method_name(method))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NativeEndpointRequestV1 {
    pub boot_id: String,
    pub request: crate::api::schema::Request,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NativeEndpointResponseChunkV1 {
    pub boot_id: String,
    pub request_id: String,
    pub final_chunk: bool,
    /// Base64 keeps arbitrary UTF-8 chunk boundaries out of the JSON control envelope.
    pub data_base64: String,
}

impl NativeEndpointResponseChunkV1 {
    pub fn decode_data(&self) -> Result<Vec<u8>, String> {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD
            .decode(&self.data_base64)
            .map_err(|error| format!("invalid native API response chunk: {error}"))
    }
}

pub fn response_chunk_message(
    boot_id: String,
    request_id: String,
    final_chunk: bool,
    data: &[u8],
) -> Result<crate::protocol::ServerMessage, serde_json::Error> {
    use base64::Engine as _;
    Ok(crate::protocol::ServerMessage::EndpointControl {
        kind: NATIVE_API_RESPONSE_KIND_V1.into(),
        data: serde_json::to_string(&NativeEndpointResponseChunkV1 {
            boot_id,
            request_id,
            final_chunk,
            data_base64: base64::engine::general_purpose::STANDARD.encode(data),
        })?,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct NativeApiDescriptorV1 {
    pub contract: String,
    pub major_version: u32,
    pub schema_id: String,
    pub schema_sha256: String,
}

pub fn descriptor() -> NativeApiDescriptorV1 {
    NativeApiDescriptorV1 {
        contract: NATIVE_API_CONTRACT_V1.into(),
        major_version: NATIVE_API_MAJOR_VERSION,
        schema_id: NATIVE_API_SCHEMA_ID.into(),
        schema_sha256: schema_sha256(),
    }
}

pub fn schema_json() -> &'static str {
    include_str!("../docs/next/api/vqro-api.schema.json")
}

pub fn schema_sha256() -> String {
    format!("{:x}", Sha256::digest(schema_json().as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_api_identity_is_not_the_private_or_bridge_generation() {
        assert_eq!(NATIVE_API_CONTRACT_V1, "vqro.api.v1");
        assert_eq!(NATIVE_API_MAJOR_VERSION, 1);
        assert_eq!(crate::protocol::PROTOCOL_VERSION, 24);
        assert_eq!(crate::protocol::endpoint::ENDPOINT_PROTOCOL_GENERATION, 2);
    }

    #[test]
    fn descriptor_digest_matches_the_packaged_schema() {
        assert_eq!(descriptor().schema_sha256.len(), 64);
        assert_eq!(descriptor().schema_sha256, schema_sha256());
    }

    #[test]
    fn endpoint_lane_rejects_streaming_lifecycle_and_presentation_methods() {
        use crate::api::schema::{EmptyParams, Method};
        assert!(!endpoint_supports(&Method::ServerStop(
            EmptyParams::default()
        )));
        assert!(!endpoint_supports(&Method::EventsSubscribe(
            crate::api::schema::EventsSubscribeParams {
                subscriptions: Vec::new(),
            }
        )));
        assert!(!endpoint_supports(&Method::ClientShellSurfaceSet(
            crate::api::schema::ClientShellSurfaceSetParams { active: true }
        )));
        assert!(endpoint_supports(&Method::CollectionList(
            Default::default()
        )));
    }

    #[test]
    fn endpoint_response_chunks_preserve_arbitrary_byte_boundaries() {
        let bytes = [0xf0, 0x9f, 0x92];
        let crate::protocol::ServerMessage::EndpointControl { kind, data } =
            response_chunk_message("boot".into(), "request".into(), false, &bytes).unwrap()
        else {
            panic!("named endpoint control");
        };
        assert_eq!(kind, NATIVE_API_RESPONSE_KIND_V1);
        let chunk: NativeEndpointResponseChunkV1 = serde_json::from_str(&data).unwrap();
        assert_eq!(chunk.decode_data().unwrap(), bytes);
    }
}
