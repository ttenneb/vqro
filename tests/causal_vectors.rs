//! Shared data; explicitly labelled numeric-codec edges retain truthful per-codec results.
use serde_json::Value;
#[test]
fn shared_causal_bounds_vectors() {
    let fixture:Value=serde_json::from_str(include_str!("fixtures/causal_vectors.json")).unwrap();
    for case in fixture["cases"].as_array().unwrap(){
        let parsed=serde_json::from_str::<Value>(case["wire"].as_str().unwrap());
        let accepted=parsed.as_ref().ok().is_some_and(|params|vqro::causality::Causality::parse(params).is_ok());
        let codec=case.get("codec_results").map(|results|&results["serde_json"]);
        let expected=codec.map(|result|&result["valid"]).unwrap_or(&case["valid"]).as_bool().unwrap();
        assert_eq!(accepted,expected,"{}",case["name"]);
        if let Some(result)=codec {
            let cause=&parsed.as_ref().unwrap()["cause"];
            assert_eq!(serde_json::to_vec(cause).unwrap().len() as u64,result["cause_bytes"].as_u64().unwrap(),"{}",case["name"]);
        }
    }
}
