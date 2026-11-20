//! Existing Pi fallback-view ownership convention, using only scoped public reads and CAS transactions.
use serde_json::{json,Value};
use vqro::provider_client::{Provider,ProviderError};
pub struct Setup {pub view:Option<String>,pub error:Option<String>}
fn refused(message:&str)->Setup{Setup {view:None,error:Some(message.into())}}
fn owned(entry:&Value,marker:&Value)->bool{entry["value"]["kind"]=="panel" && entry["value"]["bridge"]==*marker}
pub async fn claim(provider:&Provider,name:&str,instance:&str)->Setup{
    // Optional transcript storage has no authority over the owned runtime's lifetime. Even a
    // remote io/overloaded/unavailable error is view-local; a closed connection reconnects normally.
    match claim_inner(provider,name,instance).await{Ok(view)=>view,Err(error)=>refused(&format!("Agent transcript unavailable ({}); controls and its backend terminal remain available.",error.kind.chars().take(64).collect::<String>()))}
}
async fn claim_inner(provider:&Provider,name:&str,instance:&str)->Result<Setup,ProviderError>{
    let scoped=provider.call("resource.list",json!({"provider":name,"backend_ref":"agent"})).await?;
    let rows=scoped["resources"].as_array().ok_or_else(||ProviderError::new("backend","scoped agent inventory unavailable"))?;
    let rows:Vec<_>=rows.iter().filter(|row|row["provider"]==name && row["backend_ref"]=="agent" && row["kind"]=="agent").collect();
    if rows.len()!=1{return Err(ProviderError::new("backend","sync did not publish exactly one owned agent"));}
    let rid=rows[0]["id"].as_str().filter(|id|id.strip_prefix('r').is_some_and(|digits|!digits.is_empty() && digits.bytes().all(|c|c.is_ascii_digit()))).ok_or_else(||ProviderError::new("backend","agent resource ID malformed"))?;
    let view=format!("agent-{rid}");let key=format!("view/{view}");let marker=json!({"instance":instance,"ref":"agent"});
    let setup=async{
        let mut entry=provider.call("workspace.get",json!({"key":key})).await?;
        if entry.is_null(){
            let value=json!({"kind":"panel","title":format!("Pi {rid}"),"bridge":marker,"items":[{"type":"text","text":"Pi bridge reconnecting; use its backend terminal."}]});
            match provider.call("workspace.apply",json!({"note":"Declare Pi agent fallback view","ops":[{"op":"set","key":key,"expect":0,"value":value}]})).await{
                Ok(_)=>return Ok(Setup {view:Some(view),error:None}),
                Err(error) if error.kind=="conflict"=>entry=provider.call("workspace.get",json!({"key":key})).await?,
                Err(error)=>return Err(error),
            }
        }
        if owned(&entry,&marker){Ok(Setup {view:Some(view),error:None})}else{Ok(refused("Agent view belongs to someone else; not overwritten or published."))}
    }.await;
    setup
}
