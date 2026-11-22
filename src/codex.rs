//! Codex app-server normalization. No queues, retries, terminal typing or delivery compositions.
//!
//! Protocol evidence/declarations: docs/research/codex-adapter.md. The native JSONL transport is
//! the existing bounded `rpc::Peer`; this module only validates and reduces native observations.
mod availability;
pub mod configuration;
mod customization;
pub mod observation;
pub mod runtime;
mod startup;
mod usage_limits;

use crate::rpc::{RpcError, RpcResult};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, VecDeque};

pub const RECEIPTS: usize = 256;
pub const REQUESTS: usize = 32;
pub const REQUEST_BYTES: usize = 32 * 1024;
pub const ENTITY_BYTES: usize = 64 * 1024;

pub fn capabilities() -> Map<String, Value> {
    serde_json::from_str(include_str!("../docs/research/codex-capabilities.json"))
        .expect("checked declaration")
}

/// Registration is a superset descriptor, not a second copy of a resource's selected values.
/// Collections (types/methods/renderings) stay collections; enum fields inside objects are sets.
pub fn provider_capabilities() -> Map<String, Value> {
    fn bound(value: Value) -> Value {
        match value {
            Value::String(value) => json!([value]),
            Value::Object(object) => {
                Value::Object(object.into_iter().map(|(k, v)| (k, bound(v))).collect())
            }
            Value::Array(values) => Value::Array(
                values
                    .into_iter()
                    .map(|v| if v.is_object() { bound(v) } else { v })
                    .collect(),
            ),
            other => other,
        }
    }
    bound(Value::Object(capabilities()))
        .as_object()
        .unwrap()
        .clone()
}

/// Bind the typed SDK effect's C2 identity before admission or any native write.
/// Classification/sender remain claims, matching the Pi adapter's public-origin contract.
pub fn bind_origin(
    args: &Value,
    attribution: &crate::principal::Attribution,
) -> Result<Value, RpcError> {
    let mut args = args.clone();
    let origin = args
        .get_mut("origin")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| rejected("invalid", "effective origin is required"))?;
    for (field, recorded) in [
        ("principal", attribution.principal.as_deref()),
        ("actor", attribution.actor.as_deref()),
    ] {
        let actual = recorded.unwrap_or(crate::principal::UNDECLARED);
        if let Some(supplied) = origin.get(field) {
            let supplied = supplied
                .as_str()
                .ok_or_else(|| rejected("invalid", "origin identity must be a string"))?;
            if supplied != actual {
                return Err(rejected(
                    "forbidden",
                    "origin does not match the core-recorded caller",
                ));
            }
        }
        origin.insert(field.into(), json!(actual));
    }
    if let Some(supplied) = origin.get("initiator") {
        let supplied = supplied
            .as_str()
            .ok_or_else(|| rejected("invalid", "origin initiator must be a string"))?;
        if Some(supplied) != attribution.initiator.as_deref() {
            return Err(rejected(
                "forbidden",
                "origin initiator does not match core attribution",
            ));
        }
    }
    if let Some(initiator) = &attribution.initiator {
        origin.insert("initiator".into(), json!(initiator));
    }
    let claimed: Vec<_> = ["kind", "agent", "sender"]
        .into_iter()
        .filter(|field| origin.contains_key(*field))
        .collect();
    origin.insert("claimed".into(), json!(claimed));
    Ok(args)
}

/// Adapter-internal evidence, never an agent.send argument or caller role claim.
#[derive(Clone, Copy, Debug, Default)]
pub struct SendContext {
    pub owner_proven: bool,
}

/// Match Pi: only the current valid public owner entry and recorded principal prove ownership.
pub fn recorded_owner(attribution: &crate::principal::Attribution, owner: &Value) -> bool {
    let Some(owner) = owner.get("value").and_then(Value::as_str) else {
        return false;
    };
    crate::principal::validate_assertable(owner).is_ok()
        && attribution.principal.as_deref() == Some(owner)
}

fn rejected(kind: &str, message: &str) -> RpcError {
    RpcError::new(kind, message)
}
fn id(value: &Value, field: &str) -> Result<String, RpcError> {
    value[field]
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= 128)
        .map(str::to_owned)
        .ok_or_else(|| {
            rejected(
                "invalid",
                &format!("{field} must be a nonempty string of at most 128 bytes"),
            )
        })
}
fn optional_id(value: &Value, field: &str) -> Result<Option<String>, RpcError> {
    if value.get(field).is_none_or(Value::is_null) {
        Ok(None)
    } else {
        id(value, field).map(Some)
    }
}
fn frame_text(text: &str) -> Result<(), RpcError> {
    // Shared Unicode16 Cc/Cf profile, plus Unicode line/paragraph separators.
    if crate::limits::text_controls(text) || text.contains(['\u{2028}', '\u{2029}']) {
        return Err(rejected(
            "invalid",
            "origin/quoted metadata must be single-line text without control or format characters",
        ));
    }
    Ok(())
}

fn frame_origin(origin: &Value) -> Result<(), RpcError> {
    for field in ["principal", "actor", "kind", "agent"] {
        if let Some(text) = origin.get(field).and_then(Value::as_str) {
            frame_text(text)?;
        }
    }
    if let Some(sender) = origin.get("sender") {
        let text = sender
            .as_str()
            .or_else(|| sender.get("principal").and_then(Value::as_str))
            .filter(|text| !text.is_empty())
            .ok_or_else(|| rejected("invalid", "sender requires a nonempty principal string"))?;
        frame_text(text)?;
    }
    Ok(())
}

fn small(value: &Value, limit: usize) -> bool {
    crate::limits::encoded_bytes(value) <= limit
}

/// Whole-entity mutations. The item journal owns positions/revisions/sequence and snapshot cuts.
#[derive(Debug, Clone)]
pub struct Change {
    pub kind: String,
    pub id: String,
    pub value: Value,
    pub append: Option<(String, String)>,
    pub create: bool,
}
impl Change {
    fn new(kind: &str, id: impl Into<String>, value: Value) -> Self {
        Self {
            kind: kind.into(),
            id: id.into(),
            value,
            append: None,
            create: true,
        }
    }
    fn append(kind: &str, id: String, base: Value, field: &str, text: String) -> Self {
        Self {
            kind: kind.into(),
            id,
            value: base,
            append: Some((field.into(), text)),
            create: true,
        }
    }
}

#[derive(Debug, Clone)]
pub struct NativeCall {
    pub method: &'static str,
    pub params: Value,
    pub delivery: String,
}

#[derive(Debug, Clone)]
pub struct Answer {
    pub native_id: Value,
    pub result: RpcResult,
    pub change: Change,
}

#[derive(Debug, Clone)]
struct Pending {
    native_id: Value,
    method: String,
    entity: Value,
}

/// One stable logical endpoint; native session and observation epoch are separate identities.
pub struct Endpoint {
    pub reference: String,
    pub incarnation: String,
    pub host: String,
    pub host_provider: String,
    pub host_instance: String,
    pub epoch: String,
    pub session: Option<String>,
    pub title: Option<String>,
    pub cwd: Option<String>,
    pub state: String,
    pub run: Option<String>,
    pub completion: Option<Value>,
    pub lifecycle: String,
    pub caps_rev: u64,
    model: Option<Value>,
    startup: startup::Startup,
    input_transform: Option<Value>,
    model_choices: Vec<Value>,
    config_rev: u64,
    config_pending: bool,
    config_uncertain: bool,
    config_status: String,
    config_diagnostic: Option<Value>,
    configured_model: Option<String>,
    availability: Option<Value>,
    availability_blocks: Vec<Value>,
    availability_retry: Option<Value>,
    availability_generation: u64,
    usage: Option<Value>,
    usage_limits: usage_limits::UsageLimits,
    run_metadata: Option<Value>,
    next_request: u64,
    pending: BTreeMap<String, Pending>,
    receipts: BTreeMap<String, Value>,
    recent: VecDeque<String>,
}

impl Endpoint {
    pub fn new(reference: String, incarnation: String, host: String, epoch: String) -> Self {
        let host_instance = incarnation.clone();
        Self {
            host_provider: "codex".into(),
            host_instance,
            reference,
            incarnation,
            host,
            epoch,
            session: None,
            title: None,
            cwd: None,
            state: "unknown".into(),
            run: None,
            completion: None,
            lifecycle: "live".into(),
            caps_rev: 1,
            model: None,
            startup: startup::Startup::default(),
            input_transform: None,
            model_choices: Vec::new(),
            config_rev: 1,
            config_pending: false,
            config_uncertain: false,
            config_status: "native".into(),
            config_diagnostic: None,
            configured_model: None,
            availability: None,
            availability_blocks: Vec::new(),
            availability_retry: None,
            availability_generation: 0,
            usage: None,
            usage_limits: usage_limits::UsageLimits::default(),
            run_metadata: None,
            next_request: 0,
            pending: BTreeMap::new(),
            receipts: BTreeMap::new(),
            recent: VecDeque::new(),
        }
    }

    pub fn model_config_available(&self) -> bool {
        self.session.is_some() && !self.model_choices.is_empty()
    }

    pub fn install_models(&mut self, choices: Vec<Value>) {
        let before = self.model_config_available();
        self.model_choices = choices;
        self.config_rev = self.config_rev.saturating_add(1);
        if before != self.model_config_available() {
            self.caps_rev = self.caps_rev.saturating_add(1);
        }
    }

    pub fn model_option(&self) -> Value {
        json!({"id":"model","label":"Model","description":"Accepted future-turn model intent: applies from the next native turn/start. Readback is the current configured model, not per-turn execution telemetry.","category":"model",
            "type":"select","value":self.configured_model.as_ref().map(|model|json!(model)).unwrap_or_else(||self.model.as_ref().map(|model|model["id"].clone()).unwrap_or(Value::Null)),
            "pending":self.config_pending,"applies_from":"next_turn","status":self.config_status,"diagnostic":self.config_diagnostic,
            "readback_scope":"current_configured_after_turn_dispatch","choices":self.model_choices,"rev":self.config_rev})
    }

    pub fn prepare_model(&self, args: &Value) -> Result<String, RpcError> {
        self.validate(args)?;
        let option = id(args, "id")?;
        if !self.model_config_available() || option != "model" {
            return Err(rejected(
                "unsupported",
                "native model configuration is not advertised",
            ));
        }
        if args
            .get("rev")
            .is_some_and(|rev| rev.as_u64() != Some(self.config_rev))
        {
            return Err(rejected("stale", "model option revision changed"));
        }
        let value = args["value"]
            .as_str()
            .filter(|value| {
                self.model_choices
                    .iter()
                    .any(|choice| choice["value"] == *value)
            })
            .ok_or_else(|| rejected("invalid", "model is outside the native choices"))?;
        if self.state != "idle" {
            return Err(rejected(
                "busy",
                "future model intent requires an idle thread",
            ));
        }
        Ok(value.to_owned())
    }

    /// Adapter-owned intent only: no native mutation or inference about the current model.
    pub fn select_model(&mut self, requested: String) {
        self.configured_model = Some(requested);
        self.config_pending = true;
        self.config_status = "pending".into();
        self.config_diagnostic = None;
        self.config_rev = self.config_rev.saturating_add(1);
    }

    pub fn begin_model_turn(&mut self) -> Vec<Change> {
        self.config_pending = true;
        // Keep the last actual native model while awaiting the one readback; an ACK is not a model report.
        self.config_status = "pending".into();
        self.config_diagnostic = None;
        self.usage = None;
        self.config_rev = self.config_rev.saturating_add(1);
        vec![
            Change::new("session", "current", self.session_info()),
            self.state_entity(),
        ]
    }

    /// One read-only native thread/read after turn/start ACK; never per-turn execution telemetry.
    pub fn finish_model_readback(
        &mut self,
        response: &Value,
        session: &str,
        epoch: &str,
        requested: &str,
    ) -> Result<Vec<Change>, RpcError> {
        if self.lifecycle == "ended"
            || self.session.as_deref() != Some(session)
            || self.epoch != epoch
            || self.configured_model.as_deref() != Some(requested)
            || response["thread"]["id"] != session
        {
            return Err(rejected(
                "stale",
                "model readback cannot rebind or revive a thread",
            ));
        }
        // Codex0.170 ThreadStatus is a tagged object: notLoaded, idle, systemError,
        // or active with activeFlags (waitingOnApproval/waitingOnUserInput). Unknown
        // or malformed status cannot distinguish current configuration from persisted metadata.
        let status = &response["thread"]["status"];
        let loaded = match status["type"].as_str() {
            Some("idle" | "systemError") => true,
            Some("active") => status["activeFlags"].as_array().is_some_and(|flags| {
                flags.iter().all(|flag| {
                    matches!(
                        flag.as_str(),
                        Some("waitingOnApproval" | "waitingOnUserInput")
                    )
                })
            }),
            _ => false,
        };
        if !loaded {
            return Err(rejected(
                "backend",
                "native thread loaded status is unconfirmed; model may be persisted, not current configured readback",
            ));
        }
        let id = configuration::text(&response["thread"]["model"], 256)?;
        let matched = id == requested;
        let mut model = json!({"id":id});
        if let Some(effort) = reported_string(&response["thread"]["reasoningEffort"], 128) {
            model["effort"] = json!(effort);
        }
        self.model = Some(model);
        self.config_pending = !matched;
        self.config_uncertain = false;
        self.config_status = if matched { "confirmed" } else { "mismatch" }.into();
        self.config_diagnostic = if matched {
            None
        } else {
            Some(
                json!({"kind":"mismatch","requested":requested,"reported":id,"scope":"current_configured_after_turn_dispatch"}),
            )
        };
        self.usage = None;
        self.config_rev = self.config_rev.saturating_add(1);
        Ok(vec![
            Change::new("session", "current", self.session_info()),
            self.state_entity(),
        ])
    }

    pub fn model_unknown(&mut self, diagnostic: Value) -> Vec<Change> {
        self.config_pending = self.configured_model.is_some();
        self.config_uncertain = true;
        self.config_status = "unknown".into();
        self.config_diagnostic = Some(diagnostic);
        self.model = None;
        self.usage = None;
        self.config_rev = self.config_rev.saturating_add(1);
        vec![
            Change::new("session", "current", self.session_info()),
            self.state_entity(),
        ]
    }

    pub fn model_override(&self) -> Option<&str> {
        self.configured_model.as_deref()
    }

    pub fn facts(&self) -> Value {
        let mut caps = Value::Object(capabilities());
        if !self.model_config_available() {
            caps["agent"]["modules"]
                .as_object_mut()
                .unwrap()
                .remove("config");
            caps.as_object_mut().unwrap().remove("agent.configure");
        }
        let customize = self.startup.customization_caps();
        if customize.as_object().is_some_and(|m| !m.is_empty()) {
            caps["agent.customize"] = customize;
        } else {
            caps.as_object_mut().unwrap().remove("agent.customize");
        }
        caps["rev"] = json!(self.caps_rev);
        let items = caps["agent"]["items"].clone();
        let requests: Vec<_> = self
            .pending
            .iter()
            .map(|(key, p)| {
                json!({"request":key,"revision":1,
            "method":p.entity["method"],"title":p.entity["title"]})
            })
            .collect();
        let status = match self.state.as_str() {
            "idle"
                if self
                    .completion
                    .as_ref()
                    .is_some_and(|c| c["reason"] == "error") =>
            {
                "error"
            }
            "idle" => "idle",
            "requires_action" => "blocked",
            _ => "working",
        };
        let mut facts = json!({"ref":self.reference,"incarnation":self.incarnation,"kind":"agent",
            "state":if self.lifecycle == "ended" {"finished"} else {"live"},
            "name":self.reference,"title":self.title,"cwd":self.cwd,"status":{"state":status,"app":"codex"},
            "data":{"agent":{"v":0,"state":self.state,"run":self.run,"lifecycle":self.lifecycle,
                "host":{"id":self.host,"provider":self.host_provider,"instance":self.host_instance,
                    "incarnation":self.incarnation,"kind":"server","owns_lifecycle":true},
                "completion":self.completion,"session":self.session_info(),
                "epoch":self.epoch,"caps":caps,"queue":null,"requests":requests,"background":null,
                "config":if self.model_config_available(){vec![self.model_option()]}else{Vec::<Value>::new()},"commands":[],
                "usage":if self.config_uncertain {None}else{self.usage.as_ref()},
                "incomplete":if self.config_uncertain {vec!["config.model"]}else{vec![]},
                "items":items}}});
        if let Some(availability) = &self.availability {
            facts["data"]["agent"]["availability"] = availability.clone();
        }
        if let Some(transform) = &self.input_transform {
            facts["data"]["agent"]["input_transform"] = transform.clone();
        }
        if let Some(limits) = self.usage_limits.facts() {
            facts["data"]["agent"]["usage_limits"] = limits;
        }
        facts
    }

    /// All adapter-side preconditions are checked before a native request can be constructed.
    pub fn validate(&self, args: &Value) -> Result<(), RpcError> {
        if !args.is_object() || args["v"].as_u64() != Some(0) {
            return Err(rejected("invalid", "args.v must be 0"));
        }
        if self.lifecycle == "ended" {
            return Err(rejected("stale", "endpoint ended; it cannot be revived"));
        }
        if args
            .get("caps_rev")
            .is_some_and(|v| v.as_u64() != Some(self.caps_rev))
        {
            return Err(rejected("stale", "capability revision changed"));
        }
        if args
            .get("epoch")
            .is_some_and(|v| v.as_str() != Some(self.epoch.as_str()))
        {
            return Err(rejected("stale", "observation epoch changed"));
        }
        if let Some(guard) = args.get("guard").filter(|g| !g.is_null()) {
            let guard = guard
                .as_object()
                .ok_or_else(|| rejected("invalid", "guard must be an object"))?;
            if guard.keys().any(|k| k != "session" && k != "run") {
                return Err(rejected("unsupported", "unknown guard"));
            }
            if let Some(session) = guard.get("session") {
                if session.as_str().is_none() || session.as_str() != self.session.as_deref() {
                    return Err(rejected("stale", "session guard no longer matches"));
                }
            }
            if let Some(run) = guard.get("run") {
                if run.as_str().is_none() || run.as_str() != self.run.as_deref() {
                    return Err(rejected("stale", "run guard no longer matches"));
                }
            }
        }
        // Use the generic wire bounds and preserve open provenance as caller data.
        // Accepting a label does not upgrade it to adapter-proved/native evidence.
        crate::causality::Causality::parse(args)?;
        Ok(())
    }

    pub fn prepare_send(&self, args: &Value) -> Result<NativeCall, RpcError> {
        self.prepare_send_with_context(args, SendContext::default())
    }

    pub fn prepare_send_with_context(
        &self,
        args: &Value,
        context: SendContext,
    ) -> Result<NativeCall, RpcError> {
        self.validate(args)?;
        if args["mode"] != "admit" {
            return Err(rejected(
                "unsupported",
                "only native admit is declared; no timing fallback",
            ));
        }
        if args.get("key").is_some_and(|v| !v.is_null()) {
            return Err(rejected(
                "unsupported",
                "native insertion keys are not declared",
            ));
        }
        let delivery = id(args, "delivery")?;
        if self.receipts.contains_key(&delivery) {
            return Err(rejected(
                "conflict",
                "delivery was already submitted; inspect its receipt",
            ));
        }
        let session = self
            .session
            .as_ref()
            .ok_or_else(|| rejected("invalid", "create or resume a session first"))?;
        let principal = id(&args["origin"], "principal")?;
        let actor = id(&args["origin"], "actor")?;
        if !small(&args["origin"], 1024) {
            return Err(rejected("invalid", "origin exceeds 1024-byte bound"));
        }
        frame_text(&principal)?;
        frame_text(&actor)?;
        frame_origin(&args["origin"])?;
        let kind = id(&args["origin"], "kind")?;
        if !matches!(kind.as_str(), "human" | "agent" | "tool") && !kind.starts_with('_') {
            return Err(rejected("unsupported", "unknown origin rendering"));
        }
        optional_id(&args["origin"], "agent")?;
        if let Some(render) = args.get("render").filter(|v| !v.is_null()) {
            let render = render
                .as_array()
                .filter(|a| !a.is_empty() && a.len() <= 16)
                .ok_or_else(|| rejected("invalid", "render must be a bounded nonempty array"))?;
            if render.iter().any(|v| v.as_str().is_none()) {
                return Err(rejected("invalid", "render entries must be names"));
            }
            if !render.iter().any(|v| v == "attributed_user") {
                return Err(rejected(
                    "unsupported",
                    "no permitted rendering matches the declared attributed-user role",
                ));
            }
        }
        let quoted = args.get("quoted").filter(|v| !v.is_null());
        if let Some(quoted) = quoted {
            if !quoted.is_object()
                || !small(quoted, 1024)
                || quoted["label"]
                    .as_str()
                    .is_none_or(|s| s.is_empty() || s.len() > 512)
            {
                return Err(rejected(
                    "invalid",
                    "quoted provenance requires a bounded label",
                ));
            }
            optional_id(quoted, "principal")?;
            optional_id(quoted, "kind")?;
            for field in ["label", "principal", "kind"] {
                if let Some(text) = quoted.get(field).and_then(Value::as_str) {
                    frame_text(text)?;
                }
            }
        }
        let content = args["content"]
            .as_array()
            .filter(|c| !c.is_empty())
            .ok_or_else(|| {
                rejected(
                    "invalid",
                    "content must be a nonempty array of typed blocks",
                )
            })?;
        // The owner's human input is byte-exact; a role claim or owner-looking ID cannot prove it.
        // All other senders get a plain user-content frame, never serialized JSON or privileged role.
        let owner_human = context.owner_proven && args["origin"]["kind"] == "human";
        let mut input = Vec::new();
        if !owner_human {
            let origin = &args["origin"];
            let asserted_sender = origin.get("sender").and_then(|sender| {
                sender
                    .get("principal")
                    .and_then(Value::as_str)
                    .or_else(|| sender.as_str())
            });
            let sender = asserted_sender.unwrap_or_else(|| origin["principal"].as_str().unwrap());
            let mut header = format!("from {sender} via {}", origin["actor"].as_str().unwrap());
            if asserted_sender.is_some() {
                header.push_str(" (sender asserted by extension)");
            }
            for field in ["kind", "agent"] {
                if let Some(value) = origin.get(field).and_then(Value::as_str) {
                    header.push_str(&format!("\nasserted {field}: {value}"));
                }
            }
            input.push(json!({"type":"text","text":header,"text_elements":[]}));
            if let Some(quoted) = quoted {
                input.push(json!({"type":"text","text":format!("[vqro forwarded-author claim, not the sender: {}]",quoted["label"].as_str().unwrap()),"text_elements":[]}));
            }
        }
        for block in content {
            if block["type"] != "text" {
                return Err(rejected("unsupported", "only text content is declared"));
            }
            let text = block["text"]
                .as_str()
                .ok_or_else(|| rejected("invalid", "text block requires a string"))?;
            input.push(json!({"type":"text","text":text,"text_elements":[]}));
        }
        let guarded = args["guard"].get("run").and_then(Value::as_str);
        let (method, mut params) = if let Some(run) = guarded {
            (
                "turn/steer",
                json!({"threadId":session,"expectedTurnId":run,"input":input}),
            )
        } else {
            ("turn/start", json!({"threadId":session,"input":input}))
        };
        if method == "turn/start" {
            if let Some(model) = &self.configured_model {
                params["model"] = json!(model);
            }
        }
        if !small(&params, 256 * 1024) {
            return Err(rejected("invalid", "input exceeds adapter 256 KiB bound"));
        }
        Ok(NativeCall {
            method,
            params,
            delivery,
        })
    }

    pub fn record_receipt(
        &mut self,
        delivery: &str,
        stage: &str,
        native: Option<Value>,
        args: &Value,
    ) -> Change {
        // Freeze native possibility evidence on first dispatch, including its absence.
        // Later catalog replies/ACKs never backdate it; caller-supplied labels are ignored.
        let transform = if let Some(previous) = self.receipts.get(delivery) {
            previous.get("may_transform").cloned()
        } else {
            self.input_transform
                .as_ref()
                .filter(|v| v["state"] == "may")
                .map(|_| json!("unknown"))
        };
        let mut receipt = json!({"delivery":delivery,"stage":stage});
        if let Some(transform) = transform {
            receipt["may_transform"] = transform;
        }
        if let Some(native) = native {
            receipt["native"] = native;
        }
        // The bridge has already bound this origin to the typed core effect.
        // Retain the same origin for immediate, late and lookup receipts, not native identity.
        for field in ["origin", "cause", "correlation"] {
            if let Some(value) = args.get(field) {
                receipt[field] = value.clone();
            }
        }
        if !self.receipts.contains_key(delivery) {
            self.recent.push_back(delivery.into());
        }
        self.receipts.insert(delivery.into(), receipt.clone());
        while self.recent.len() > RECEIPTS {
            if let Some(old) = self.recent.pop_front() {
                self.receipts.remove(&old);
            }
        }
        Change::new("receipt", delivery, receipt)
    }

    pub fn receipt(&self, delivery: &str) -> Value {
        self.receipts
            .get(delivery)
            .cloned()
            .unwrap_or_else(|| json!({"delivery":delivery,"stage":"unknown"}))
    }

    pub fn interrupt_params(&self, args: &Value) -> Result<Option<Value>, RpcError> {
        self.validate(args)?;
        if self.state == "idle" {
            return Ok(None);
        }
        let thread = self
            .session
            .as_ref()
            .ok_or_else(|| rejected("stale", "no observed session"))?;
        let run = self.run.as_ref().ok_or_else(|| {
            rejected("stale", "no evidenced active turn; cannot safely interrupt")
        })?;
        Ok(Some(json!({"threadId":thread,"turnId":run})))
    }

    pub fn state_entity(&self) -> Change {
        let mut value = json!({"state":self.state,"run":self.run,"completion":self.completion,"background":null});
        if !self.config_uncertain {
            if let Some(usage) = &self.usage {
                value["usage"] = usage.clone();
            }
        }
        if let Some(availability) = &self.availability {
            value["availability"] = availability.clone();
        }
        if let Some(limits) = self.usage_limits.facts() {
            value["usage_limits"] = limits;
        }
        Change::new("state", "current", value)
    }

    pub fn session_info(&self) -> Value {
        let Some(id) = &self.session else {
            return Value::Null;
        };
        let mut session = json!({"id":id,"title":self.title});
        if let Some(cwd) = &self.cwd {
            session["cwd"] = json!(cwd);
        }
        if !self.config_uncertain {
            if let Some(model) = &self.model {
                session["model"] = model.clone();
            }
        }
        session
    }

    /// Read-only settings are native response fields, never bridge defaults or a write capability.
    pub fn set_session_response(
        &mut self,
        response: &Value,
        epoch: String,
    ) -> Result<Vec<Change>, RpcError> {
        self.set_session_response_from(response, epoch, "rpc:thread/start")
    }

    pub fn set_session_response_from(
        &mut self,
        response: &Value,
        epoch: String,
        source: &str,
    ) -> Result<Vec<Change>, RpcError> {
        let _changes = self.set_session(&response["thread"], epoch)?;
        if let Some(cwd) = reported_string(&response["cwd"], 4096) {
            self.cwd = Some(cwd);
        }
        if let Some(id) = reported_string(&response["model"], 256) {
            let mut model = json!({"id":id});
            if let Some(effort) = reported_string(&response["reasoningEffort"], 128) {
                model["effort"] = json!(effort);
            }
            self.model = Some(model);
        }
        let startup = self.capture_startup(response, source);
        Ok(vec![
            startup,
            Change::new("session", "current", self.session_info()),
            self.state_entity(),
        ])
    }

    fn run_change(&mut self, turn: &Value) -> Option<Change> {
        let run = turn["id"]
            .as_str()
            .filter(|id| !id.is_empty() && id.len() <= 128)?;
        let mut value = self
            .run_metadata
            .as_ref()
            .filter(|v| v["id"] == run)
            .cloned()
            .unwrap_or_else(|| json!({"id":run}));
        if let Some(status) = reported_string(&turn["status"], 128) {
            value["status"] = json!(status);
        }
        // Native Turn timestamps are Unix seconds; unlike item timestamps, they are not ms.
        for (native, field) in [("startedAt", "started_ms"), ("completedAt", "ended_ms")] {
            if let Some(ms) = turn[native].as_u64().and_then(|n| n.checked_mul(1000)) {
                value[field] = json!(ms);
            }
        }
        if self.run.as_deref() == Some(run)
            || self.run_metadata.as_ref().is_some_and(|v| v["id"] == run)
        {
            self.run_metadata = Some(value.clone());
        }
        if value.get("started_ms").is_some() || value.get("ended_ms").is_some() {
            Some(Change::new("run", run, value))
        } else {
            None
        }
    }

    /// Apply a successful metadata/session response before publishing the provider result.
    pub fn set_session(&mut self, thread: &Value, epoch: String) -> Result<Vec<Change>, RpcError> {
        if self.lifecycle == "ended" {
            return Err(rejected(
                "stale",
                "a late native reply cannot revive an ended endpoint",
            ));
        }
        let session = id(thread, "id")?;
        if !self.model_config_available() && !self.model_choices.is_empty() {
            self.caps_rev = self.caps_rev.saturating_add(1);
        }
        self.session = Some(session);
        self.config_pending = self.configured_model.is_some();
        self.config_uncertain = false;
        self.config_status = if self.config_pending {
            "pending"
        } else {
            "native"
        }
        .into();
        self.config_diagnostic = None;
        self.config_rev = self.config_rev.saturating_add(1);
        self.epoch = epoch;
        self.input_transform = None;
        self.reset_availability();
        self.title = thread
            .get("name")
            .or_else(|| thread.get("title"))
            .and_then(Value::as_str)
            .map(|s| s.chars().take(256).collect());
        self.cwd = reported_string(&thread["cwd"], 4096);
        self.model = None;
        self.usage = None;
        self.run_metadata = None;
        self.pending.clear();
        self.run = None;
        self.completion = None;
        self.apply_status(&thread["status"]);
        Ok(vec![
            Change::new("session", "current", self.session_info()),
            self.state_entity(),
        ])
    }

    pub fn may_switch(&self, args: &Value) -> Result<(), RpcError> {
        self.validate(args)?;
        if self.session.is_some() && (self.state != "idle" || !self.pending.is_empty()) {
            return Err(rejected(
                "busy",
                "session work or requests are still active",
            ));
        }
        Ok(())
    }

    fn apply_status(&mut self, status: &Value) {
        self.state = match status["type"].as_str() {
            Some("idle") => "idle",
            Some("active")
                if status["activeFlags"].as_array().is_some_and(|f| {
                    f.iter()
                        .any(|x| x == "waitingOnApproval" || x == "waitingOnUserInput")
                }) =>
            {
                "requires_action"
            }
            Some("active") => "running",
            _ => "unknown",
        }
        .into();
    }

    /// Only runtime evidence (not bridge or viewer disconnection) may end an endpoint.
    pub fn host_exited(&mut self, reason: &str) -> Vec<Change> {
        self.lifecycle = "ended".into();
        self.reset_availability();
        self.state = "unknown".into();
        let mut changes = Vec::new();
        for (key, mut pending) in std::mem::take(&mut self.pending) {
            pending.entity["status"] = json!("cancelled");
            changes.push(Change::new("request", key, pending.entity));
        }
        changes.push(Change::new(
            "state",
            "current",
            json!({"state":"unknown","lifecycle":"ended","reason":reason}),
        ));
        changes
    }

    /// A normal turn completion is never endpoint-end evidence. Native unknown statuses stay unknown.
    pub fn notification(&mut self, method: &str, p: &Value) -> Vec<Change> {
        if self.lifecycle == "ended" {
            return Vec::new();
        }
        if method == "account/updated" {
            let had_limits = self.usage_limits.facts().is_some();
            self.usage_limits = usage_limits::UsageLimits::default();
            return if had_limits {
                vec![self.state_entity()]
            } else {
                Vec::new()
            };
        }
        if let Some(changed) = self.availability_notification(method, p) {
            return if changed {
                vec![self.state_entity()]
            } else {
                Vec::new()
            };
        }
        if p["threadId"].as_str() != self.session.as_deref() {
            return Vec::new();
        }
        match method {
            "thread/status/changed" => {
                self.apply_status(&p["status"]);
                vec![self.state_entity()]
            }
            "turn/started" => {
                self.run = p["turn"]["id"]
                    .as_str()
                    .filter(|s| s.len() <= 128)
                    .map(str::to_owned);
                self.state = "running".into();
                let mut changes = vec![self.state_entity()];
                if let Some(change) = self.run_change(&p["turn"]) {
                    changes.push(change);
                }
                changes
            }
            "turn/completed" => {
                let turn = &p["turn"];
                let Some(run) = turn["id"].as_str().filter(|s| s.len() <= 128) else {
                    return Vec::new();
                };
                self.availability_turn_completed(turn);
                // A late old-turn completion must not mark a newer turn idle.
                let (reason, known) = match turn["status"].as_str() {
                    Some("completed") => ("end_turn", true),
                    Some("interrupted") => ("cancelled", true),
                    Some("failed") => ("error", true),
                    Some(other) => (other, false),
                    None => ("unknown", false),
                };
                let completion =
                    json!({"id":run,"reason":reason,"error":bounded_error(&turn["error"])});
                if self.run.as_deref() == Some(run) {
                    self.run = None;
                    self.state = if !known { "unknown" } else { "idle" }.into();
                    self.completion = Some(completion);
                }
                let mut changes = vec![self.state_entity()];
                if let Some(change) = self.run_change(turn) {
                    changes.push(change);
                }
                changes
            }
            "thread/tokenUsage/updated" => {
                let reported = &p["tokenUsage"];
                let mut usage = serde_json::Map::new();
                if let Some(window) = reported["modelContextWindow"].as_u64() {
                    usage.insert("context".into(), json!({"window":window})); // no native occupancy witness
                }
                let totals = &reported["total"];
                if let (Some(input), Some(output)) = (
                    totals["inputTokens"].as_u64(),
                    totals["outputTokens"].as_u64(),
                ) {
                    let mut tokens = json!({"input":input,"output":output});
                    for (native, field) in [
                        ("cachedInputTokens", "cache_read"),
                        ("cacheWriteInputTokens", "cache_write"),
                    ] {
                        if let Some(n) = totals[native].as_u64() {
                            tokens[field] = json!(n);
                        }
                    }
                    usage.insert("tokens".into(), tokens);
                }
                self.usage = if usage.is_empty() {
                    None
                } else {
                    Some(Value::Object(usage))
                };
                vec![self.state_entity()]
            }
            "item/started" | "item/completed" => self.item(&p["item"], method == "item/completed"),
            "item/agentMessage/delta" => {
                let (Ok(item), Some(text)) = (id(p, "itemId"), p["delta"].as_str()) else {
                    return vec![];
                };
                vec![Change::append(
                    "message",
                    item,
                    json!({"role":"assistant","content":[{"type":"text","text":""}],"final":false}),
                    "/content/0/text",
                    text.into(),
                )]
            }
            "item/commandExecution/outputDelta" => {
                let (Ok(item), Some(text)) = (id(p, "itemId"), p["delta"].as_str()) else {
                    return vec![];
                };
                vec![Change::append(
                    "tool",
                    item,
                    json!({"title":"Codex command","status":"running","content":[{"type":"text","text":""}]}),
                    "/content/0/text",
                    text.into(),
                )]
            }
            "serverRequest/resolved" => {
                let native = &p["requestId"];
                let key = self
                    .pending
                    .iter()
                    .find(|(_, q)| &q.native_id == native)
                    .map(|(key, _)| key.clone());
                if let Some(key) = key {
                    let mut q = self.pending.remove(&key).unwrap();
                    q.entity["status"] = json!("cancelled");
                    q.entity["native_resolved"] = json!(true);
                    vec![Change::new("request", key, q.entity)]
                } else {
                    vec![]
                }
            }
            "turn/plan/updated" => {
                vec![Change::new("plan", "current", json!({"entries":p["plan"]}))]
            }
            _ => vec![],
        }
    }

    fn item(&self, item: &Value, final_item: bool) -> Vec<Change> {
        let Ok(item_id) = id(item, "id") else {
            return vec![];
        };
        let (kind, mut value) = match item["type"].as_str() {
            Some("userMessage") => (
                "message",
                json!({"role":"user","content":item["content"],"final":final_item}),
            ),
            Some("agentMessage") => (
                "message",
                json!({"role":"assistant","content":[{"type":"text","text":item["text"]}],"final":final_item}),
            ),
            Some("reasoning") => {
                let text = item["summary"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                    .unwrap_or_default();
                (
                    "message",
                    json!({"role":"thought","content":[{"type":"text","text":text}],"final":final_item}),
                )
            }
            Some("commandExecution" | "fileChange" | "webSearch" | "dynamicToolCall") => {
                let status = match item["status"].as_str() {
                    Some("completed") => "completed",
                    Some("failed" | "declined") => "failed",
                    Some("cancelled") => "cancelled",
                    _ if final_item => "unknown",
                    _ => "running",
                };
                let content = vec![
                    json!({"type":"text","text":item["aggregatedOutput"].as_str().unwrap_or("")}),
                ];
                (
                    "tool",
                    json!({"title":item.get("command").or_else(||item.get("tool")).unwrap_or(&item["type"]),"status":status,
                    "input":item["arguments"],"content":content}),
                )
            }
            Some("contextCompaction") => ("compaction", json!({"native":item})),
            _ => ("_codex_item", json!({"native":item})),
        };
        let mut native = item.clone();
        if let Some(fields) = native.as_object_mut() {
            if item["type"] == "agentMessage" {
                fields.remove("text");
            }
            if item["type"] == "commandExecution" {
                fields.remove("aggregatedOutput");
            }
        }
        value["native"] = native;
        if !small(&value, ENTITY_BYTES - 1024) {
            let mut short =
                json!({"incomplete":true,"native_type":item["type"],"final":final_item});
            if kind == "message" {
                short["role"] = value["role"].clone();
                short["content"] = json!([]);
            }
            if kind == "tool" {
                short["title"] = json!("Codex tool (details truncated)");
                short["status"] = value["status"].clone();
                short["content"] = json!([]);
            }
            value = short;
        }
        let mut change = Change::new(kind, item_id, value);
        change.create = !final_item;
        vec![change]
    }

    /// Surface only a bounded, representable native request. Failure returns a safe native cancel
    /// response to be sent once by the caller; it must never turn an unknown method into approval.
    pub fn request(
        &mut self,
        native_id: Value,
        method: &str,
        params: &Value,
    ) -> Result<Change, RpcError> {
        if params["threadId"].as_str() != self.session.as_deref() {
            return Err(rejected(
                "unsupported",
                "request does not belong to this endpoint session",
            ));
        }
        if self.pending.values().any(|p| p.native_id == native_id) {
            return Err(rejected("stale", "native request id is already pending"));
        }
        if self.pending.len() >= REQUESTS || !small(params, REQUEST_BYTES / 2) {
            return Err(rejected(
                "unsupported",
                "request exceeds bounded representation",
            ));
        }
        let mut entity = match method {
            "item/commandExecution/requestApproval" | "item/fileChange/requestApproval" => {
                let mut choices = Vec::new();
                let available = params.get("availableDecisions").filter(|v| !v.is_null());
                for (choice, effect, label) in [
                    ("accept", "allow_once", "Allow once"),
                    ("decline", "reject_once", "Deny once"),
                ] {
                    if available
                        .is_none_or(|a| a.as_array().is_some_and(|a| a.iter().any(|v| v == choice)))
                    {
                        choices.push(json!({"id":choice,"effect":effect,"label":label}));
                    }
                }
                if choices.is_empty() {
                    return Err(rejected(
                        "unsupported",
                        "no representable native approval decisions",
                    ));
                }
                json!({"method":"permission","title":params.get("command").and_then(Value::as_str).unwrap_or("Approve Codex action"),"choices":choices})
            }
            "item/tool/requestUserInput" => {
                let questions = params["questions"]
                    .as_array()
                    .filter(|q| !q.is_empty() && q.len() <= 16)
                    .ok_or_else(|| rejected("unsupported", "unrepresentable native questions"))?;
                let mut properties = Map::new();
                let mut required = Vec::new();
                for q in questions {
                    let key = id(q, "id")?;
                    if properties.contains_key(&key) {
                        return Err(rejected("unsupported", "duplicate question id"));
                    }
                    let mut item = json!({"type":"string"});
                    if q["isOther"] != true {
                        if let Some(options) = q["options"].as_array() {
                            let labels: Vec<_> =
                                options.iter().filter_map(|o| o["label"].as_str()).collect();
                            if labels.len() != options.len() || labels.is_empty() {
                                return Err(rejected("unsupported", "invalid question options"));
                            }
                            item["enum"] = json!(labels);
                        }
                    }
                    properties.insert(key.clone(),json!({"type":"array","items":item,"maxItems":16,"title":q["header"],"description":q["question"],"writeOnly":q["isSecret"]==true}));
                    required.push(key);
                }
                json!({"method":"form","title":"Codex questions","schema":{"type":"object","properties":properties,"required":required,"additionalProperties":false}})
            }
            _ => {
                return Err(rejected(
                    "unsupported",
                    "unknown native request method; cancel once",
                ));
            }
        };
        if !small(&entity, REQUEST_BYTES / 2) {
            return Err(rejected("unsupported", "request entity too large"));
        }
        if let Some(title) = entity["title"].as_str() {
            entity["title"] = json!(title.chars().take(256).collect::<String>());
        }
        self.next_request += 1;
        let key = format!("q{}", self.next_request);
        entity["request"] = json!(key);
        entity["revision"] = json!(1);
        entity["status"] = json!("pending");
        entity["params"] = params.clone();
        entity["incomplete"] = json!(false);
        entity["scope"] =
            json!({"session":self.session,"run":params["turnId"],"subject":params["itemId"]});
        let bytes: usize = self
            .pending
            .values()
            .map(|p| crate::limits::encoded_bytes(&p.entity))
            .sum();
        if bytes + crate::limits::encoded_bytes(&entity) > REQUEST_BYTES {
            return Err(rejected(
                "unsupported",
                "pending request byte budget exhausted",
            ));
        }
        self.pending.insert(
            key.clone(),
            Pending {
                native_id,
                method: method.into(),
                entity: entity.clone(),
            },
        );
        Ok(Change::new("request", key, entity))
    }

    /// A bounded, unanswerable host diagnostic, not a fabricated native resolution. Never copy
    /// unknown request parameters here: host-wide requests can contain authentication material.
    pub fn request_cancellation_notice(&mut self) -> Change {
        self.next_request += 1;
        Change::new(
            "message",
            format!("_codex_notice_{}", self.next_request),
            json!({
                "role":"system", "content":[{"type":"text","format":"plain",
                    "text":"Cancellation requested for an unrepresentable request from the shared Codex host. Native resolution is unknown."}],
                "final":true, "notice":{"kind":"unrepresentable_request","scope":"host",
                    "host":self.host,"cancellation":"requested","native_outcome":"unknown"}
            }),
        )
    }

    pub fn prepare_answer(&mut self, args: &Value) -> Result<Answer, RpcError> {
        self.validate(args)?;
        if args["epoch"].as_str() != Some(self.epoch.as_str()) {
            return Err(rejected("stale", "responses require the current epoch"));
        }
        let key = id(args, "request")?;
        let p = self
            .pending
            .get(&key)
            .ok_or_else(|| rejected("stale", "request is no longer pending"))?;
        if args["revision"].as_u64() != Some(1) {
            return Err(rejected("stale", "request revision changed"));
        }
        if p.entity["scope"]["session"].as_str() != self.session.as_deref() {
            return Err(rejected("stale", "request session ended"));
        }
        // A native request remains live until its explicit serverRequest/resolved (or endpoint
        // end/session switch); background requests need not belong to the foreground run.
        let response = &args["response"];
        if !small(response, REQUEST_BYTES) {
            return Err(rejected("invalid", "response too large"));
        }
        let cancelled = response["action"] == "cancel";
        let result = if p.entity["method"] == "permission" {
            let choice = if cancelled {
                "cancel".into()
            } else {
                id(response, "choice")?
            };
            if !cancelled
                && !p.entity["choices"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|c| c["id"] == choice)
            {
                return Err(rejected("invalid", "unknown approval choice"));
            }
            Ok(json!({"decision":choice}))
        } else if cancelled || response["action"] == "decline" {
            Err(RpcError {
                code: -32800,
                message: "request cancelled".into(),
                data: None,
            })
        } else {
            if response["action"] != "accept" {
                return Err(rejected(
                    "invalid",
                    "form response requires accept, decline or cancel",
                ));
            }
            let content = response["content"]
                .as_object()
                .ok_or_else(|| rejected("invalid", "form content must be an object"))?;
            let props = p.entity["schema"]["properties"].as_object().unwrap();
            if content.len() != props.len() || content.keys().any(|k| !props.contains_key(k)) {
                return Err(rejected(
                    "invalid",
                    "answer every question, with no unknown fields",
                ));
            }
            let mut answers = Map::new();
            for (key, schema) in props {
                let values = content[key]
                    .as_array()
                    .filter(|a| a.len() <= 16)
                    .ok_or_else(|| {
                        rejected("invalid", "question answers must be bounded string arrays")
                    })?;
                for value in values {
                    if value.as_str().is_none()
                        || schema["items"]["enum"]
                            .as_array()
                            .is_some_and(|choices| !choices.contains(value))
                    {
                        return Err(rejected("invalid", "invalid question answer"));
                    }
                }
                answers.insert(key.clone(), json!({"answers":values}));
            }
            Ok(json!({"answers":answers}))
        };
        // Claim only after every validation. The caller serializes this mutation; a second answer
        // now rejects even if native flushing is slow or uncertain. It is never automatically retried.
        let mut pending = self.pending.remove(&key).unwrap();
        pending.entity["status"] = json!(if cancelled { "cancelled" } else { "answered" });
        pending.entity["native_method"] = json!(pending.method);
        Ok(Answer {
            native_id: pending.native_id,
            result,
            change: Change::new("request", key, pending.entity),
        })
    }
}

fn reported_string(value: &Value, limit: usize) -> Option<String> {
    value
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= limit)
        .map(str::to_owned)
}

fn bounded_error(error: &Value) -> Value {
    error
        .get("message")
        .and_then(Value::as_str)
        .map(|s| json!({"message":s.chars().take(512).collect::<String>()}))
        .unwrap_or(Value::Null)
}
