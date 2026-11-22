#[allow(dead_code)] #[path="../src/bin/vqro-bridge-pi/native.rs"] mod native;
#[allow(dead_code)] #[path="../src/bin/vqro-bridge-pi/admission.rs"] mod admission;
#[allow(dead_code)] #[path="../src/bin/vqro-bridge-pi/contract.rs"] mod contract;
#[path="../src/bin/vqro-bridge-pi/commands.rs"] mod commands;
use commands::{Commands,Prepared};
use serde_json::json;
#[test]
fn only_advertised_explicit_names_run_and_arguments_are_not_flattened(){
    let mut c=Commands::default();c.observe(&json!({"commands":[{"name":"example","description":"Native command"}]}));
    assert_eq!(c.facts().len(),4);assert!(matches!(c.prepare("example",Some(&json!("a\nb"))).unwrap(),Prepared::Prompt{message} if message=="/example a\nb"));
    assert_eq!(c.prepare("unknown",None).err().unwrap().kind,"unsupported");assert_eq!(c.prepare("example",Some(&json!(["text"]))).err().unwrap().kind,"invalid");
    assert!(matches!(c.prepare("new_session",None).unwrap(),Prepared::NewSession));assert_eq!(c.prepare("new_session",Some(&json!("ignored?"))).err().unwrap().kind,"unsupported");
}
#[test]
fn builtin_rpc_commands_preserve_explicit_instructions(){
    let c=Commands::default();
    assert!(matches!(c.prepare("cycle_model",None).unwrap(),Prepared::Rpc{name,command,args} if name=="cycle_model" && command=="cycle_model" && args==json!({})));
    assert!(matches!(c.prepare("compact",Some(&json!("  exact\ntext  "))).unwrap(),Prepared::Rpc{command,args,..} if command=="compact" && args==json!({"customInstructions":"  exact\ntext  "})));
}
#[test]
fn native_command_catalogs_are_bounded_and_unknown_is_not_empty(){
    let mut c=Commands::default();let rows:Vec<_>=(0..100).map(|n|json!({"name":format!("cmd{n}"),"description":"é".repeat(200)})).collect();c.observe(&json!(rows));assert!(c.incomplete);assert_eq!(c.facts().len(),commands::MAX_COMMANDS+3);
    for row in c.facts().iter().skip(3){assert!(row["description"].as_str().unwrap().len()<=300);}
    c.observe(&json!({"wrong":[]}));assert!(c.incomplete);assert_eq!(c.facts().len(),3,"builtins remain native, unknown custom inventory is not known empty");
    c.observe(&json!({"commands":[{"name":"bad name"},{"name":"compact"}]}));assert!(c.incomplete);assert_eq!(c.facts().len(),3);
}
#[test]
fn cycle_result_projects_no_native_credential_fields_and_requires_reported_shape(){
    let native=json!({"model":{"provider":"p","id":"m","name":"Model","headers":{"Authorization":"fixture-secret"}},"thinkingLevel":"low"});let safe=commands::safe_result("cycle_model",&native);
    assert_eq!(safe["model"]["id"],"m");assert!(!safe.to_string().contains("fixture-secret"));assert!(!safe.to_string().contains("headers"));assert!(commands::observed_result("cycle_model",&native));assert!(!commands::observed_result("cycle_model",&json!({})));
    let compact=json!({"summary":"Native summary","firstKeptEntryId":"entry","tokensBefore":123});assert!(commands::observed_result("compact",&compact));assert_eq!(commands::safe_result("compact",&compact),compact);assert!(!commands::observed_result("compact",&json!({})));
}
/// Skills stay invocable explicitly: agent.command on Pi's advertised skill:<name> sends /skill:<name> [args].
#[test]
fn an_advertised_skill_is_invoked_through_agent_command(){
    let mut c=Commands::default();c.observe(&json!({"commands":[{"name":"skill:review","description":"Review code","source":"skill"}]}));
    assert!(matches!(c.prepare("skill:review",Some(&json!("the diff"))).unwrap(),Prepared::Prompt{message} if message=="/skill:review the diff"));
    assert!(matches!(c.prepare("skill:review",None).unwrap(),Prepared::Prompt{message} if message=="/skill:review"));
}
