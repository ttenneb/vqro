//! Agent-v0 validation/rendering, before any native write. No inferred insertion or hidden fallback.
use base64::Engine;
use serde_json::{json,Value};
use super::admission::Refusal;
const INPUT_BYTES:usize=768*1024;

pub struct Context<'a> {
    pub caps_rev:u64,pub epoch:&'a str,pub session:Option<&'a str>,
    /// True only when recorded caller identity proves this origin is the human owner.
    pub owner_proven:bool,pub images:bool,pub literal:bool,
    /// The native slash names Pi would expand or run (prompt templates, extension commands, skills), from its
    /// get_commands; None while that inventory is unknown or incomplete (then any leading /name is refused).
    pub slash_names:Option<&'a [String]>,
}
#[derive(Debug,Clone)]
pub struct Send {
    pub delivery:String,pub mode:String,pub command:&'static str,pub args:Value,
    pub origin:Value,pub quoted:Option<Value>,pub rendering:&'static str,
    pub cause:Option<Value>,pub correlation:Option<String>,
}
fn reject(kind:&'static str,message:&str)->Refusal { Refusal {kind,message:message.into()} }
fn invalid(message:&str)->Refusal {reject("invalid",message)}
pub fn id(value:&Value)->Result<&str,Refusal> {
    value.as_str().filter(|s|!s.is_empty() && s.len()<=128).ok_or_else(||invalid("expected nonempty identifier of at most128 UTF-8 bytes"))
}
pub fn common(args:&Value,context:&Context<'_>)->Result<(),Refusal> {
    if !args.is_object() {return Err(invalid("arguments must be an object"));}
    match args.get("v").and_then(Value::as_u64) {
        Some(0)=>{},Some(_)=>return Err(reject("unsupported","unsupported agent contract version")),None=>return Err(invalid("v:0 is required")),
    }
    if let Some(rev)=args.get("caps_rev") {
        let rev=rev.as_u64().ok_or_else(||invalid("caps_rev must be an integer"))?;
        if rev!=context.caps_rev {return Err(reject("stale","capabilities changed"));}
    }
    if let Some(epoch)=args.get("epoch") {
        if id(epoch)?!=context.epoch {return Err(reject("stale","observation epoch changed"));}
    }
    if let Some(guard)=args.get("guard") {
        let guard=guard.as_object().ok_or_else(||invalid("guard must be an object"))?;
        for (key,value) in guard {
            if key!="session" {return Err(reject("unsupported","this guard cannot be enforced"));}
            let expected=id(value)?;
            let Some(actual)=context.session else {return Err(reject("unsupported","session guard cannot be checked"));};
            if expected!=actual {return Err(reject("stale","session changed"));}
        }
    }
    if let Some(cause)=args.get("cause") {
        if !cause.is_object() {return Err(invalid("cause must be an object"));}
        for field in ["kind","id","provenance"] {causal_string(&cause[field])?;}
        for field in ["endpoint","epoch","boot"] {if let Some(value)=cause.get(field) {causal_string(value)?;}}
        match cause["kind"].as_str(){Some("entity"|"request")=>{causal_string(&cause["endpoint"])?;causal_string(&cause["epoch"])?;},Some("effect")=>{causal_string(&cause["boot"])?;},_=>{}}
        if vqro::limits::encoded_bytes(cause)>4096 {return Err(invalid("cause exceeds4096 bytes"));}
        if vqro::limits::json_depth(cause,64).is_err(){return Err(invalid("cause exceeds64 container depth"));}
    }
    if let Some(value)=args.get("correlation") {if causal_string(value)?.is_empty(){return Err(invalid("correlation must be nonempty"));}}
    Ok(())
}

fn causal_string(value:&Value)->Result<&str,Refusal>{value.as_str().filter(|value|value.len()<=256).ok_or_else(||invalid("causal string must be at most256 bytes"))}
fn frame_text(text:&str)->Result<(),Refusal>{
    // Shared Unicode16 Cc/Cf profile plus line/paragraph separators; body text is untouched.
    if vqro::limits::text_controls(text) || text.contains(['\u{2028}','\u{2029}']) {
        return Err(invalid("origin/quoted metadata must be single-line text without control or format characters"));
    }
    Ok(())
}

/// Bind identity to C2's recorded attribution, never the caller's display/role claims.
/// This runs before admission, receipts, queue invalidation or any native write.
pub fn bind_origin(args:&Value,attribution:&vqro::principal::Attribution)->Result<Value,Refusal>{
    let mut args=args.clone();
    let origin=args.get_mut("origin").and_then(Value::as_object_mut).ok_or_else(||invalid("effective origin is required"))?;
    for (field,recorded) in [("principal",attribution.principal.as_deref()),("actor",attribution.actor.as_deref())]{
        let actual=recorded.unwrap_or(vqro::principal::UNDECLARED);
        let supplied=match origin.get(field){None=>actual,Some(value)=>value.as_str().ok_or_else(||invalid("origin identity must be a string"))?};
        if supplied!=actual{return Err(reject("forbidden","origin does not match the core-recorded caller"));}
        origin.insert(field.into(),json!(actual));
    }
    if let Some(supplied)=origin.get("initiator"){
        let supplied=supplied.as_str().ok_or_else(||invalid("origin initiator must be a string"))?;
        if Some(supplied)!=attribution.initiator.as_deref(){return Err(reject("forbidden","origin initiator does not match core attribution"));}
    }
    if let Some(initiator)=&attribution.initiator{origin.insert("initiator".into(),json!(initiator));}
    // The sender is coordination's inbox-record assertion, not the core transport identity.
    let claimed:[&str;3]=["kind","agent","sender"];
    let claimed:Vec<_>=claimed.into_iter().filter(|field|origin.contains_key(*field)).collect();
    origin.insert("claimed".into(),json!(claimed));
    Ok(args)
}

/// Only the current valid owner key and core-recorded effective principal can prove ownership.
pub fn recorded_owner(attribution:&vqro::principal::Attribution,owner:&Value)->bool {
    let Some(owner)=owner.get("value").and_then(Value::as_str) else{return false;};
    vqro::principal::validate_assertable(owner).is_ok() && attribution.principal.as_deref()==Some(owner)
}

pub fn send(args:&Value,context:&Context<'_>)->Result<Send,Refusal> {
    common(args,context)?;
    for field in args.as_object().unwrap().keys() {
        if !["v","caps_rev","epoch","guard","cause","correlation","mode","content","delivery","origin","quoted","render","key"].contains(&field.as_str()) {
            return Err(reject("unsupported","undeclared send argument"));
        }
    }
    if vqro::limits::encoded_bytes(args)>INPUT_BYTES {return Err(reject("too_large","input exceeds declared byte limit"));}
    if args.get("key").is_some() {return Err(reject("unsupported","native insertion keys are unavailable"));}
    if !context.literal {return Err(reject("unsupported","literal input requires a controlled launch"));}
    let delivery=id(&args["delivery"])?.to_string();
    let mode=id(&args["mode"])?.to_string();
    // Every mode is Pi's `prompt`: Pi itself decides atomically whether it starts a run (idle) or queues the input
    // (`streamingBehavior` while streaming). Pi's `steer`/`follow_up` commands only queue, so on an idle agent they park
    // input until some later run (pi-coding-agent agent-session.js followUp/_queueFollowUp; prompt/_preparePrompt).
    let command=match mode.as_str() {"start"|"admit"|"boundary"|"settle"=>"prompt",_=>return Err(reject("unsupported","undeclared delivery mode"))};
    let origin=&args["origin"];
    if !origin.is_object() {return Err(invalid("effective origin is required"));}
    for field in ["principal","actor"] {let text=causal_string(&origin[field])?;if text.is_empty(){return Err(invalid("origin identity must be nonempty"));}frame_text(text)?;}
    if let Some(initiator)=origin.get("initiator"){if causal_string(initiator)?.is_empty(){return Err(invalid("origin initiator must be nonempty"));}}
    frame_text(id(&origin["kind"])?)?;
    if let Some(agent)=origin.get("agent") {frame_text(id(agent)?)?;}
    if let Some(sender)=origin.get("sender") {
        let text=sender.as_str().or_else(||sender.get("principal").and_then(Value::as_str)).filter(|text|!text.is_empty())
            .ok_or_else(||invalid("sender requires a nonempty principal string"))?;
        frame_text(text)?;
    }
    if vqro::limits::encoded_bytes(origin)>2048 {return Err(invalid("origin exceeds2048 bytes"));}
    let quoted=args.get("quoted").cloned();
    if let Some(quoted)=&quoted {
        if !quoted.is_object() || quoted["label"].as_str().is_none_or(|s|s.len()>1024) || vqro::limits::encoded_bytes(quoted)>2048 {return Err(invalid("quoted provenance requires a bounded label"));}
        frame_text(quoted["label"].as_str().unwrap())?;
        for field in ["principal","kind"] {if let Some(value)=quoted.get(field) {frame_text(id(value)?)?;}}
    }
    let default=json!(["participant","attributed_user"]);
    let render=args.get("render").unwrap_or(&default).as_array().ok_or_else(||invalid("render must be an ordered array"))?;
    if render.len()>16 || render.iter().any(|value|value.as_str().is_none()) {return Err(invalid("render entries must be bounded names"));}
    let rendering=render.iter().find_map(|value|match value.as_str() {
        Some("user") if context.owner_proven && origin["kind"]=="human"=>Some("user"),
        Some("attributed_user")=>Some("attributed_user"),_=>None,
    }).ok_or_else(||reject("unsupported","none of the requested origin renderings is supported"))?;
    let blocks=args["content"].as_array().ok_or_else(||invalid("content must be an array of typed blocks"))?;
    if blocks.is_empty() || blocks.len()>32 {return Err(invalid("content requires1–32 blocks"));}
    let mut text=None;let mut images=Vec::new();
    for (index,block) in blocks.iter().enumerate() {
        let fields=block.as_object().ok_or_else(||invalid("content block must be an object"))?;
        let allowed: &[&str]=match block["type"].as_str() {Some("text")=>&["type","text","format"],Some("image")=>&["type","mime","data"],_=>&["type"]};
        if fields.keys().any(|field|!allowed.contains(&field.as_str())) && matches!(block["type"].as_str(),Some("text"|"image")) {
            return Err(reject("unsupported","undeclared content block field"));
        }
        match block["type"].as_str() {
            Some("text")=>{
                if block.get("format").is_some_and(|format|!format.is_string()){return Err(invalid("text format must be a string"));}
                // Native RPC text is literal; plain/markdown/unknown presentation formats all fall
                // back to the exact same plain bytes, never a command/template interpretation.
                if index!=0 || text.is_some() {return Err(reject("unsupported","text_then_media requires at most one leading text block"));}
                text=Some(block["text"].as_str().ok_or_else(||invalid("text block requires text"))?);
            }
            Some("image")=>{
                if !context.images {return Err(reject("unsupported","current model does not declare image input"));}
                let mime=block["mime"].as_str().filter(|s|s.starts_with("image/") && s.len()<=128).ok_or_else(||invalid("image requires an image MIME type"))?;
                let data=block["data"].as_str().filter(|s|!s.is_empty()).ok_or_else(||invalid("image requires base64 data"))?;
                base64::engine::general_purpose::STANDARD.decode(data).map_err(|_|invalid("image data is not base64"))?;
                images.push(json!({"type":"image","mimeType":mime,"data":data}));
            }
            Some(_)=>return Err(reject("unsupported","undeclared content block type")),
            None=>return Err(invalid("content block requires a type")),
        }
    }
    let body=text.unwrap_or("");
    let owner_human=context.owner_proven && origin["kind"]=="human";
    // Keep provenance in the receipt. The human owner's native input is the literal typed body,
    // including when the default route is attributed_user; never serialize metadata into a prompt.
    let mut message=if rendering=="attributed_user" && !owner_human {
        let asserted_sender=origin.get("sender").and_then(|sender|sender.get("principal").and_then(Value::as_str).or_else(||sender.as_str()));
        let sender=asserted_sender.unwrap_or_else(||origin["principal"].as_str().unwrap());
        let mut header=format!("from {sender} via {}",origin["actor"].as_str().unwrap());
        if asserted_sender.is_some(){header.push_str(" (sender asserted by extension)");}
        // Classification is caller data even when an identity looks human/agent-like.
        // Never present it as core attestation or let it change the native user role.
        for field in ["kind","agent"]{if let Some(value)=origin.get(field).and_then(Value::as_str){
            header.push_str(&format!("\nasserted {field}: {value}"));
        }}
        format!("{header}\n\n{body}")
    } else {body.to_string()};
    if !owner_human {if let Some(quoted)=&quoted {
        message=format!("Quoted provenance (asserted, not the effective sender): {}\n{message}",quoted["label"].as_str().unwrap());
    }}
    // Literal input (agent-v0 3.2): Pi expands input that starts with /skill:<name> into that skill's file before it
    // starts or queues it (agent-session.js _preparePrompt / _queueUserInput -> _expandSkillCommand). Such text would not
    // reach the model as written, so it is refused here; a skill is invoked explicitly with agent.command. Only the
    // owner's plain input can start that way (everyone else's is framed with a sender header first).
    if message.starts_with("/skill:") {
        return Err(reject("unsupported","text starting with /skill: would be expanded by a native skill, not delivered literally; invoke the skill with agent.command"));
    }
    // Pi's own rule for templates and extension commands (prompt-templates.js expandPromptTemplate: ^/(\S+)(\s+.*)?$
    // against the loaded names; agent-session.js _tryExecuteExtensionCommand): a leading /name it knows is expanded or
    // run, not delivered. Unknown inventory: no leading /name can be proven literal.
    if let Some(name)=message.strip_prefix('/').map(|rest|rest.split(char::is_whitespace).next().unwrap_or("")).filter(|name|!name.is_empty()) {
        if context.slash_names.is_none_or(|names|names.iter().any(|known|known==name)) {
            return Err(reject("unsupported",format!("/{name} would be expanded or run by the native runtime (template or command), not delivered literally; run it with agent.command").as_str()));
        }
    }
    let mut native=json!({"message":message});
    if !images.is_empty() {native["images"]=json!(images);}
    match mode.as_str() {"admit"|"settle"=>{native["streamingBehavior"]=json!("followUp");},"boundary"=>{native["streamingBehavior"]=json!("steer");},_=>{}}
    Ok(Send {delivery,mode,command,args:native,origin:origin.clone(),quoted,rendering,
        cause:args.get("cause").cloned(),correlation:args.get("correlation").and_then(Value::as_str).map(str::to_string)})
}

pub fn input_bytes()->usize {INPUT_BYTES}
/// Native metadata varies between arrays and named containers; unknown is never treated as known empty.
pub fn rows<'a>(value:&'a Value,key:&str)->Option<&'a Vec<Value>> {
    value.as_array().or_else(||value.get(key).and_then(Value::as_array))
}
