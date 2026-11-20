//! Agent-v0 validation/rendering, before any native write. No inferred insertion or hidden fallback.
use base64::Engine;
use serde_json::{json,Value};
use super::admission::Refusal;
const INPUT_BYTES:usize=768*1024;

pub struct Context<'a> {
    pub caps_rev:u64,pub epoch:&'a str,pub session:Option<&'a str>,
    /// True only when recorded caller identity proves this origin is the human owner.
    pub owner_proven:bool,pub images:bool,pub literal:bool,
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
    let command=match mode.as_str() {"start"|"admit"=>"prompt","boundary"=>"steer","settle"=>"follow_up",_=>return Err(reject("unsupported","undeclared delivery mode"))};
    let origin=&args["origin"];
    if !origin.is_object() {return Err(invalid("effective origin is required"));}
    for field in ["principal","actor","kind"] {id(&origin[field])?;}
    if let Some(agent)=origin.get("agent") {id(agent)?;}
    if vqro::limits::encoded_bytes(origin)>2048 {return Err(invalid("origin exceeds2048 bytes"));}
    let quoted=args.get("quoted").cloned();
    if let Some(quoted)=&quoted {
        if !quoted.is_object() || quoted["label"].as_str().is_none_or(|s|s.len()>1024) || vqro::limits::encoded_bytes(quoted)>2048 {return Err(invalid("quoted provenance requires a bounded label"));}
        for field in ["principal","kind"] {if let Some(value)=quoted.get(field) {id(value)?;}}
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
    let mut message=if rendering=="attributed_user" {format!("Effective sender (user authority): {origin}\n\n{body}")} else {body.to_string()};
    if let Some(quoted)=&quoted {message=format!("Quoted provenance (asserted, not the effective sender): {quoted}\n{message}");}
    let mut native=json!({"message":message});
    if !images.is_empty() {native["images"]=json!(images);}
    if mode=="admit" {native["streamingBehavior"]=json!("followUp");}
    Ok(Send {delivery,mode,command,args:native,origin:origin.clone(),quoted,rendering,
        cause:args.get("cause").cloned(),correlation:args.get("correlation").and_then(Value::as_str).map(str::to_string)})
}

pub fn input_bytes()->usize {INPUT_BYTES}
/// Native metadata varies between arrays and named containers; unknown is never treated as known empty.
pub fn rows<'a>(value:&'a Value,key:&str)->Option<&'a Vec<Value>> {
    value.as_array().or_else(||value.get(key).and_then(Value::as_array))
}
