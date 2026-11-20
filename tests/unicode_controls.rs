//! Identical Unicode16 format-character profile for labels and known causal strings; opaque extras stay opaque.
use serde_json::{json,Value};
use vqro::{causality::Causality,principal::Attribution};
#[test]
fn unicode_format_vectors_rejected_in_labels_and_known_links_only() {
    let vectors:Value=serde_json::from_str(include_str!("fixtures/text_controls.json")).unwrap();let mut count=0;
    for range in vectors["format_ranges"].as_array().unwrap(){for point in range[0].as_u64().unwrap()..=range[1].as_u64().unwrap(){
        let text=char::from_u32(point as u32).unwrap().to_string();count+=1;
        assert!(Attribution::for_actor("alice",&mut json!({"label":text})).is_err(),"format label {point}");
        assert!(Causality::parse(&json!({"correlation":text})).is_err(),"format correlation {point}");
        for field in ["kind","id","provenance","endpoint","epoch","boot"] {let mut cause=json!({"kind":"future","id":"id","provenance":"asserted"});cause[field]=json!(text);assert!(Causality::parse(&json!({"cause":cause})).is_err(),"format {field} {point}");}
        assert!(Causality::parse(&json!({"cause":{"kind":"future","id":"id","provenance":"asserted","opaque":text}})).is_ok());
    }}assert_eq!(count,170);
    for text in ["plain","é","漢字","e\u{301}","\u{fe0f}"] {assert!(Attribution::for_actor("alice",&mut json!({"label":text})).is_ok());assert!(Causality::parse(&json!({"correlation":text})).is_ok());}
}
