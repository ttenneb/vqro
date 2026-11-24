//! Nonsecret native extension bootstrap and positive startup binding. No credential I/O.
use anyhow::{ensure, Result};
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Metadata { id:String, label:String, provider:String, native_provider:String }
fn identifier(value:&str)->bool {
    !value.is_empty() && value.len()<=64 && value.bytes().next().is_some_and(|b|b.is_ascii_alphanumeric())
        && value.bytes().all(|b|b.is_ascii_alphanumeric() || matches!(b,b'_'|b'-'|b'.'))
}
/// Add only the shipped public extension AFTER validating caller native argv.
/// CLI explicit extensions load first, ahead of discovered user extensions;
/// all extension factories still finish before the startup handler sees ctx.
pub fn attach(argv:&mut Vec<String>, extension:Option<&std::path::Path>, raw:Option<&str>)->Result<Option<String>> {
    let expected=if let Some(raw)=raw {
        ensure!(raw.len()<=2048,"account metadata exceeds bounds");
        let row:Metadata=serde_json::from_str(raw).map_err(|_|anyhow::anyhow!("invalid account metadata"))?;
        ensure!(identifier(&row.id) && row.id!="normal" && identifier(&row.provider) && identifier(&row.native_provider)
            && row.native_provider!=row.provider && !row.label.trim().is_empty() && row.label.chars().count()<=128
            && !row.label.chars().any(char::is_control),"invalid account metadata");
        ensure!(extension.is_some(),"native account extension is required");
        Some(row.native_provider)
    }else{None};
    if let Some(extension)=extension {
        let path=extension.to_str().ok_or_else(||anyhow::anyhow!("native account extension path is not UTF-8"))?;
        ensure!(!path.is_empty() && !path.contains('\0'),"invalid native account extension path");
        argv.extend(["-e".to_owned(),path.to_owned()]);
    }
    if let Some(raw)=raw {argv.extend(["--vqro-account".to_owned(),raw.to_owned()]);}
    Ok(expected)
}

pub fn ready(expected:Option<&str>, state:&Value)->bool {
    expected.is_none_or(|id|state["model"]["provider"].as_str()==Some(id)
        && state["model"]["id"].as_str().is_some_and(|value|!value.is_empty() && value.len()<=256)
        && state["model"]["api"].as_str().is_some_and(|api|!api.is_empty() && api!="pi-virtual"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn metadata_cannot_silently_fall_back_to_a_native_base_login() {
        assert!(!ready(Some("vqro-account-b"),&json!({"model":{"provider":"anthropic"}})));
        assert!(!ready(Some("vqro-account-b"),&json!({})));
        assert!(!ready(Some("vqro-account-b"),&json!({"model":{"provider":"vqro-account-b","api":"pi-virtual"}})));
        assert!(!ready(Some("vqro-account-b"),&json!({"model":{"provider":"vqro-account-b","api":"anthropic-messages"}})));
        assert!(ready(Some("vqro-account-b"),&json!({"model":{"id":"native-model","provider":"vqro-account-b","api":"anthropic-messages"}})));
        assert!(ready(None,&json!({})));
    }
    #[test]
    fn explicit_shipped_extension_carries_metadata_only() {
        let raw=json!({"id":"b","label":"B","provider":"anthropic","native_provider":"vqro-account-b"}).to_string();
        let mut argv=vec!["pi1".to_owned(),"--mode".to_owned(),"rpc".to_owned()];
        assert_eq!(attach(&mut argv,Some(std::path::Path::new("fake/index.mjs")),Some(&raw)).unwrap(),Some("vqro-account-b".into()));
        assert_eq!(&argv[3..],&["-e","fake/index.mjs","--vqro-account",raw.as_str()]);
        assert!(attach(&mut vec![],None,Some(&raw)).is_err());
        assert!(attach(&mut vec![],None,Some(r#"{"token":"forbidden"}"#)).is_err());
    }
}
