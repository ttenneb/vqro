//! Test-only possession of the existing managed-lifetime credential, never a production bypass.
#[path = "python.rs"] mod test_python;
use std::path::Path;
use serde_json::{json,Value};
pub fn spec(token:&Path)->Value {json!({"enabled":true,"argv":[test_python::python(),Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/managed_identity.py"),token]})}
pub async fn token(path:&Path)->String {
    let deadline=std::time::Instant::now()+std::time::Duration::from_secs(5);
    loop {if let Ok(value)=std::fs::read_to_string(path){if value.len()==64{return value;}}assert!(std::time::Instant::now()<deadline,"managed fixture credential missing");tokio::time::sleep(std::time::Duration::from_millis(10)).await;}
}
