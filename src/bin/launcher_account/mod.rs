//! Shared bridge ingress for non-secret, launcher-configured account metadata.
//! No native profile access or authentication inference. Reject unknown fields rather than echoing them.
use serde_json::Value;

pub fn parse(raw: Option<&str>) -> Result<Option<Value>, &'static str> {
    let Some(raw) = raw else { return Ok(None) };
    if raw.len() > 2048 { return Err("launcher account metadata exceeds bounds") }
    let value: Value = serde_json::from_str(raw).map_err(|_| "invalid launcher account metadata")?;
    let row = value.as_object().ok_or("invalid launcher account metadata")?;
    if row.keys().any(|key| !matches!(key.as_str(), "id"|"label"|"provider"|"kind"|"sources")) {
        return Err("unknown launcher account field")
    }
    let identifier = |name: &str| row.get(name).and_then(Value::as_str).is_some_and(|v|
        !v.is_empty() && v.len() <= 64 && v.bytes().next().is_some_and(|b| b.is_ascii_alphanumeric())
        && v.bytes().all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b)));
    if !identifier("id") || row.contains_key("provider") && !identifier("provider") {
        return Err("invalid launcher account identifier")
    }
    let label = row.get("label").and_then(Value::as_str).ok_or("missing launcher account label")?;
    if label.trim().is_empty() || label.chars().count() > 128 || label.chars().any(char::is_control) {
        return Err("invalid launcher account label")
    }
    if !matches!(row.get("kind").and_then(Value::as_str), Some("subscription"|"api_key")) {
        return Err("unsupported launcher account kind")
    }
    let sources = row.get("sources").and_then(Value::as_object).ok_or("missing launcher account sources")?;
    if sources.len() != row.len()-1 || row.keys().filter(|k| k.as_str() != "sources")
        .any(|key| sources.get(key).and_then(Value::as_str) != Some("vqro:launcher")) {
        return Err("invalid launcher account sources")
    }
    Ok(Some(value))
}
