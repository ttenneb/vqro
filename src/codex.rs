//! Codex app-server normalization. No queues, retries, terminal typing or delivery compositions.
//!
//! Protocol evidence/declarations: docs/research/codex-adapter.md. The native JSONL transport is
//! the existing bounded `rpc::Peer`; this module only validates and reduces native observations.
pub mod runtime;

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
fn small(value: &Value, limit: usize) -> bool {
    crate::limits::encoded_bytes(value) <= limit
}

/// Whole-entity mutations. The item journal owns positions/revisions/sequence and snapshot cuts.
#[derive(Debug, Clone)]
pub struct Change {
    pub kind: String,
    pub id: String,
    pub value: Value,
}
impl Change {
    fn new(kind: &str, id: impl Into<String>, value: Value) -> Self {
        Self {
            kind: kind.into(),
            id: id.into(),
            value,
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
            next_request: 0,
            pending: BTreeMap::new(),
            receipts: BTreeMap::new(),
            recent: VecDeque::new(),
        }
    }

    pub fn facts(&self) -> Value {
        let mut caps = Value::Object(capabilities());
        caps["rev"] = json!(self.caps_rev);
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
        json!({"ref":self.reference,"incarnation":self.incarnation,"kind":"agent",
            "state":if self.lifecycle == "ended" {"finished"} else {"live"},
            "name":self.reference,"title":self.title,"cwd":self.cwd,"status":{"state":status,"app":"codex"},
            "data":{"agent":{"v":0,"state":self.state,"run":self.run,"lifecycle":self.lifecycle,
                "host":{"id":self.host,"provider":self.host_provider,"instance":self.host_instance,
                    "incarnation":self.incarnation,"kind":"server","owns_lifecycle":true},
                "completion":self.completion,"session":self.session.as_ref().map(|id| json!({"id":id,"title":self.title})),
                "epoch":self.epoch,"caps":caps,"queue":null,"requests":requests,"background":null,
                "config":[],"commands":[],"usage":null,"incomplete":[],
                "items":{"retain_bytes":131072,"snapshot_bytes":131072,"coverage":"tail"}}}})
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
        optional_id(args, "correlation")?;
        if let Some(cause) = args.get("cause").filter(|v| !v.is_null()) {
            if !cause.is_object() || !small(cause, 1024) {
                return Err(rejected("invalid", "cause must be a bounded object"));
            }
            let kind = id(cause, "kind")?;
            id(cause, "id")?;
            if cause["provenance"] != "asserted" {
                return Err(rejected(
                    "unsupported",
                    "caller causality must remain asserted; native evidence is not inferred",
                ));
            }
            if matches!(kind.as_str(), "entity" | "request") {
                id(cause, "endpoint")?;
                id(cause, "epoch")?;
            }
            if kind == "effect" {
                id(cause, "boot")?;
            }
        }
        Ok(())
    }

    pub fn prepare_send(&self, args: &Value) -> Result<NativeCall, RpcError> {
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
        if principal.chars().any(char::is_control) || actor.chars().any(char::is_control) {
            return Err(rejected(
                "invalid",
                "origin principal contains control characters",
            ));
        }
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
        // Attribution is its own user-content block; original text bytes are not rewritten, escaped,
        // templated, flattened or upgraded to developer/system authority.
        let mut input = vec![
            json!({"type":"text","text":format!("[vqro sender: {}]",args["origin"]),"text_elements":[]}),
        ];
        if let Some(quoted) = quoted {
            input.push(json!({"type":"text","text":format!("[vqro forwarded-author claim, not the sender: {quoted}]"),"text_elements":[]}));
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
        let (method, params) = if let Some(run) = guarded {
            (
                "turn/steer",
                json!({"threadId":session,"expectedTurnId":run,"input":input}),
            )
        } else {
            ("turn/start", json!({"threadId":session,"input":input}))
        };
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
        let mut receipt = json!({"delivery":delivery,"stage":stage});
        if let Some(native) = native {
            receipt["native"] = native;
        }
        for field in ["cause", "correlation"] {
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
        Change::new(
            "state",
            "current",
            json!({"state":self.state,"run":self.run,"completion":self.completion,"background":null}),
        )
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
        self.session = Some(session);
        self.epoch = epoch;
        self.title = thread
            .get("name")
            .or_else(|| thread.get("title"))
            .and_then(Value::as_str)
            .map(|s| s.chars().take(256).collect());
        self.cwd = thread["cwd"].as_str().map(str::to_owned);
        self.pending.clear();
        self.run = None;
        self.completion = None;
        self.apply_status(&thread["status"]);
        Ok(vec![
            Change::new(
                "session",
                "current",
                json!({"id":self.session,"title":self.title}),
            ),
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
                vec![self.state_entity()]
            }
            "turn/completed" => {
                let turn = &p["turn"];
                let Some(run) = turn["id"].as_str().filter(|s| s.len() <= 128) else {
                    return Vec::new();
                };
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
                vec![self.state_entity()]
            }
            "item/started" | "item/completed" => self.item(&p["item"], method == "item/completed"),
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
                let content = item["aggregatedOutput"]
                    .as_str()
                    .map(|s| vec![json!({"type":"text","text":s})])
                    .unwrap_or_default();
                (
                    "tool",
                    json!({"title":item.get("command").or_else(||item.get("tool")).unwrap_or(&item["type"]),"status":status,
                    "input":item["arguments"],"content":content}),
                )
            }
            Some("contextCompaction") => ("compaction", json!({"native":item})),
            _ => ("_codex_item", json!({"native":item})),
        };
        value["native"] = item.clone();
        if !small(&value, ENTITY_BYTES - 1024) {
            value = json!({"incomplete":true,"native_type":item["type"],"final":final_item});
        }
        vec![Change::new(kind, item_id, value)]
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
        if self.pending.len() >= REQUESTS || !small(params, REQUEST_BYTES / 2) {
            return Err(rejected(
                "unsupported",
                "request exceeds bounded representation",
            ));
        }
        if self.pending.values().any(|p| p.native_id == native_id) {
            return Err(rejected("stale", "native request id is already pending"));
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

fn bounded_error(error: &Value) -> Value {
    error
        .get("message")
        .and_then(Value::as_str)
        .map(|s| json!({"message":s.chars().take(512).collect::<String>()}))
        .unwrap_or(Value::Null)
}
