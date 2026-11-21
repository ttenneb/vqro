//! Bounded native model catalog, never a synthetic or persisted user setting.
use crate::rpc::RpcError;
use serde_json::{Value, json};

pub const MODELS: usize = 128;
pub const CATALOG_BYTES: usize = 24 * 1024;
pub const PAGES: usize = 4;

/// Only code/message cross the public diagnostic boundary, never native error data or requests.
pub enum FailureStage {
    NativeRpc,
    Readback,
}
pub fn failure(stage: FailureStage, error: &RpcError) -> RpcError {
    let (stage, key, message) = match stage {
        FailureStage::NativeRpc => (
            "native_rpc",
            "native_error",
            "native model outcome unknown; do not replay",
        ),
        FailureStage::Readback => (
            "readback",
            "readback_error",
            "native model readback missing or changed thread; do not replay",
        ),
    };
    let mut detail = json!({"code":error.code});
    if !error.message.is_empty()
        && error.message.len() <= 1024
        && !crate::limits::text_controls(&error.message)
        && !error.message.contains(['\u{2028}', '\u{2029}'])
    {
        detail["message"] = json!(&error.message);
    } else {
        detail["message_omitted"] = json!("bounds_or_controls");
    }
    let mut public = RpcError::new("backend", message);
    let data = public.data.as_mut().unwrap();
    data["stage"] = json!(stage);
    data[key] = detail;
    public
}

fn text(value: &Value, limit: usize) -> Result<&str, RpcError> {
    value
        .as_str()
        .filter(|text| {
            !text.is_empty()
                && text.len() <= limit
                && !crate::limits::text_controls(text)
                && !text.contains(['\u{2028}', '\u{2029}'])
        })
        .ok_or_else(|| RpcError::new("backend", "invalid native model catalog text"))
}

/// Choice values are the native `model` slug, not an assumed alias of the catalog's UI id.
pub fn page(value: &Value, choices: &mut Vec<Value>) -> Result<Option<String>, RpcError> {
    let data = value["data"]
        .as_array()
        .filter(|rows| rows.len() <= 64)
        .ok_or_else(|| RpcError::new("backend", "invalid native model catalog page"))?;
    for row in data {
        let model = text(&row["model"], 256)?;
        let label = text(&row["displayName"], 256)?;
        if choices.len() == MODELS || choices.iter().any(|choice| choice["value"] == model) {
            return Err(RpcError::new(
                "backend",
                "native model catalog exceeds bound or repeats a model",
            ));
        }
        choices.push(json!({"value":model,"label":label}));
        if crate::limits::encoded_bytes(choices) > CATALOG_BYTES {
            return Err(RpcError::new(
                "backend",
                "native model choices exceed facts allowance",
            ));
        }
    }
    match value.get("nextCursor").filter(|cursor| !cursor.is_null()) {
        None => Ok(None),
        Some(cursor) => Ok(Some(text(cursor, 256)?.to_owned())),
    }
}
