//! Native Claude observations projected through the shared bounded item journal.
use super::{availability::Availability, requests::Requests, startup::Startup};
use crate::native_process::{Event, Exit};
use crate::provider_client::items::{Coverage, ItemJournal, JournalConfig};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

pub const ENTITY_BYTES: usize = 60 * 1024;
const ENTITY_TOTAL: usize = 512 * 1024;
const NORMAL_BYTES: usize = 192 * 1024;
const RETAIN: usize = 128 * 1024;
type Key = (String, String);
#[cfg(test)]
#[path = "state_child_tests.rs"]
mod child_tests;
pub struct State {
    pub journal: ItemJournal,
    pub requests: Requests,
    pub startup: Startup,
    pub availability: Availability,
    pub(crate) work: super::work::Work,
    plans: super::plans::Plans,
    pub ready: bool,
    pub restart_configured: bool,
    pub session: Option<String>,
    retired_session: Option<String>,
    pub completion: Option<Value>,
    pub foreground: &'static str,
    pub run: Option<String>,
    pub exit: Option<Exit>,
    pub end_ready: bool,
    pub incomplete: BTreeSet<String>,
    pub receipts: BTreeMap<String, Value>,
    pub inputs: BTreeMap<String, (String, String)>,
    pub outgoing: VecDeque<super::requests::Outbound>,
    pub account: Option<Value>,
    pub principal: Option<String>,
    pub generation: u64,
    pub changed: u64,
    pub version: Option<String>,
    pub native_caps: BTreeSet<String>,
    pub background: Option<Vec<Value>>,
    background_revision: u64,
    ledger_complete: bool,
    boot: String,
    epoch_number: u64,
    serial: u64,
    position: u64,
    caps_rev: u64,
    last_caps: Value,
    entries: BTreeMap<Key, (usize, bool)>,
    normal: VecDeque<Key>,
    receipt_order: VecDeque<String>,
    pub tools: BTreeMap<String, String>,
    pub active_tools: BTreeSet<String>,
}
impl State {
    pub fn new(boot: String) -> Self {
        let mut journal = ItemJournal::new(
            format!("{boot}.e1"),
            JournalConfig {
                retain_bytes: RETAIN,
                max_record_bytes: ENTITY_BYTES,
                max_entities: 512,
                max_entity_bytes: ENTITY_TOTAL,
                snapshot_buffer_bytes: 1024 * 1024,
            },
        );
        journal.set_coverage(Coverage::Tail);
        let mut state = Self {
            journal,
            requests: Requests::default(),
            startup: Startup::new(),
            availability: Availability::default(),
            work: super::work::Work::default(),
            plans: super::plans::Plans::default(),
            ready: false,
            restart_configured: false,
            session: None,
            retired_session: None,
            completion: None,
            foreground: "unknown",
            run: None,
            exit: None,
            end_ready: false,
            incomplete: BTreeSet::new(),
            receipts: BTreeMap::new(),
            inputs: BTreeMap::new(),
            outgoing: VecDeque::new(),
            account: None,
            principal: None,
            generation: 1,
            changed: 0,
            version: None,
            native_caps: BTreeSet::new(),
            background: None,
            background_revision: 0,
            ledger_complete: true,
            boot,
            epoch_number: 1,
            serial: 0,
            position: 0,
            caps_rev: 1,
            last_caps: Value::Null,
            entries: BTreeMap::new(),
            normal: VecDeque::new(),
            receipt_order: VecDeque::new(),
            tools: BTreeMap::new(),
            active_tools: BTreeSet::new(),
        };
        state.refresh();
        state
    }
    pub fn epoch(&self) -> &str {
        self.journal.epoch()
    }
    pub fn incarnation(&self) -> &str {
        &self.boot
    }
    fn next(&mut self, prefix: &str) -> String {
        self.serial += 1;
        format!("{prefix}{}", self.serial)
    }
    pub fn status(&self) -> &'static str {
        if self.exit.is_some() {
            "unknown"
        } else if !self.requests.root_is_empty() {
            "requires_action"
        } else if self.foreground == "idle"
            && (self.requests.root_has_unconfirmed()
                || self.receipts.keys().any(|id| self.delivery_active(id)))
        {
            "unknown"
        } else {
            self.foreground
        }
    }
    /// This prototype qualifies the native literal-message field against the matched public SDK.
    /// An unknown CLI stays observable but is not granted a guessed literal-send guarantee.
    pub fn literal_supported(&self) -> bool {
        self.version.as_deref() == Some("2.1.329")
    }
    fn declarations(&self, upper: bool) -> Value {
        let mut caps = json!({"agent":{"v":0},"host":{"kind":"process","endpoints":"one"},
            "agent.respond":{"ack":"dispatched","methods":["form"],"scopes":["operation"],"background_requests":true,
                "max_pending":super::requests::MAX_PENDING,"request_bytes":super::requests::REQUEST_BYTES},
            "agent.interrupt":{"pending":"unknown","scope":"all_work","waits":"none"},"agent.receipt":{},
            "agent.availability":{"reasons":["auth","quota","rate_limited","overloaded","_native_error","_retrying"]},
            "items.stream":{"retain_bytes":RETAIN,"entities":512,"entity_bytes":ENTITY_TOTAL,"snapshot_buffer_bytes":1048576,"max_viewers":4}});
        if upper || (self.ready && self.literal_supported()) {
            caps["agent.send"] = json!({"modes":{"admit":{}},"content":{"types":["text"],"shape":"ordered","max_bytes":65536},
                "literal":"always","origin":[{"as":"user","role":"user"},{"as":"attributed_user","role":"user","frame":"sender_header"}],
                "correlation":"acceptance","handled":"none","keys":null,"guards":{"session":"adapter","run":"none"},"receipts":{"retain":256}});
        }
        if upper || (self.ready && self.literal_supported()) {
            caps["agent.object"] = json!({"actions":["cancel"],"kinds":["task","child"]});
        }
        if self.restart_configured && (upper || (self.ready && self.literal_supported())) {
            caps["claude.restart"] = json!({"v":0,"session":"resume","background":"refuse_unless_explicit_loss","allow_background_loss":true});
        }
        caps
    }
    pub fn provider_caps(&self) -> Map<String, Value> {
        fn bound(value: Value) -> Value {
            match value {
                Value::String(s) => json!([s]),
                Value::Object(o) => {
                    Value::Object(o.into_iter().map(|(k, v)| (k, bound(v))).collect())
                }
                Value::Array(a) => Value::Array(
                    a.into_iter()
                        .map(|v| if v.is_object() { bound(v) } else { v })
                        .collect(),
                ),
                other => other,
            }
        }
        bound(self.declarations(true)).as_object().unwrap().clone()
    }
    pub fn caps_rev(&self) -> u64 {
        self.caps_rev
    }
    pub fn facts(&self, provider: &str, instance: &str) -> Value {
        let state = self.status();
        let mut caps = self.declarations(false);
        caps["rev"] = json!(self.caps_rev);
        let mut facts = json!({"ref":"agent","incarnation":self.boot,"kind":"agent","state":if self.end_ready{"finished"}else{"live"},"name":"Claude Code",
            "status":{"state":match state {"idle"=>"idle","requires_action"=>"blocked",_=>"working"}},
            "exit":self.exit.as_ref().filter(|_|self.end_ready).map(|e|json!({"code":e.code,"success":e.success})),
            "data":{"agent":{"v":0,"state":state,"lifecycle":if self.end_ready{"ended"}else if self.exit.is_some(){"ending"}else{"live"},
                "host":{"id":format!("{}.native{}",self.boot,self.generation),"provider":provider,"instance":instance,
                    "incarnation":self.generation.to_string(),"kind":"process","owns_lifecycle":true},
                "session":self.session.as_ref().map(|id|json!({"id":id})),"epoch":self.epoch(),"caps":caps,
                "completion":self.completion,"availability":self.availability.value(),"requests":self.requests.summaries(),
                "queue":null,"background":self.background,"_claude":{"input_ledger":self.input_ledger(),"background_level":{"v":0,"source":if self.background_revision>0{Some("native:background_tasks_changed")}else{None},"process_incarnation":self.generation.to_string(),"revision":self.background_revision,"complete":self.background.is_some()}}, "config":[],"commands":[],"account":self.account,"incomplete":self.incomplete,
                "items":{"retain_bytes":RETAIN,"entities":512,"entity_bytes":ENTITY_TOTAL,"snapshot_buffer_bytes":1048576,"coverage":"tail"}}}});
        if let Some(principal) = &self.principal {
            facts["data"]["agent"]["principal"] = json!(principal);
        }
        if self.account.is_none() {
            facts["data"]["agent"]
                .as_object_mut()
                .unwrap()
                .remove("account");
        }
        facts
    }
    fn protected(&self, key: &Key) -> bool {
        self.journal.entity(&key.0, &key.1).is_some_and(|(_, v)| {
            key.0 == "tool" && matches!(v["status"].as_str(), Some("pending" | "running"))
        })
    }
    fn evict(&mut self) -> bool {
        let Some(at) = self.normal.iter().position(|key| !self.protected(key)) else {
            return false;
        };
        let key = self.normal.remove(at).unwrap();
        if self.journal.remove(&key.0, &key.1).is_err() {
            self.incomplete.insert("items".into());
            return false;
        }
        self.entries.remove(&key);
        true
    }
    pub fn put(&mut self, kind: &str, id: &str, value: Value) -> bool {
        let size = crate::limits::encoded_bytes(&value);
        if size > ENTITY_BYTES - 1024 {
            self.incomplete.insert("items".into());
            return false;
        }
        let key = (kind.to_owned(), id.to_owned());
        let control = matches!(kind, "state" | "session" | "startup")
            || (kind == "request" && value["status"] == "pending");
        for _ in 0..=512 {
            let normal_bytes: usize = self
                .entries
                .iter()
                .filter(|(k, entry)| *k != &key && !entry.1)
                .map(|(_, entry)| entry.0)
                .sum();
            let count = self.normal.iter().filter(|k| **k != key).count();
            if !control && (normal_bytes + size > NORMAL_BYTES || count >= 384) {
                if self.evict() {
                    continue;
                }
                self.incomplete.insert("items".into());
                return false;
            }
            match self.journal.put(kind, id, value.clone()) {
                Ok(_) => {
                    if let Some(at) = self.normal.iter().position(|k| *k == key) {
                        self.normal.remove(at);
                    }
                    if !control {
                        self.normal.push_back(key.clone());
                    }
                    self.entries.insert(key, (size, control));
                    return true;
                }
                Err(_) => {
                    if !self.evict() {
                        self.incomplete.insert("items".into());
                        return false;
                    }
                }
            }
        }
        self.incomplete.insert("items".into());
        false
    }
    pub fn refresh(&mut self) {
        let caps = self.declarations(false);
        if self.last_caps != caps {
            self.caps_rev += 1;
            self.last_caps = caps;
        }
        self.put(
            "state",
            "current",
            json!({"state":self.status(),"run":self.run,"completion":self.completion}),
        );
        self.put("startup", "current", self.startup.value());
        if self.restart_configured && self.journal.entity("notice", "restart-lifetime").is_none() {
            self.put("notice","restart-lifetime",json!({"text":"During restart, EOF or a handled signal waits behind earlier admitted effects, then stops the current owned child. Each effect has its absolute deadline; preflight is limited to 35 s. Stop is not immediate under contention; OS spawn cannot be pre-empted. After end publication, no new input or child is dispatched."}));
        }
        if let Some(reason) = self.work.foreground_loss_reason() {
            if self
                .journal
                .entity("notice", "foreground-work-loss")
                .is_none()
            {
                self.put("notice","foreground-work-loss",json!({"text":format!("restart blocked: foreground work may be running ({reason}). Automatic restart/cutover is refused until this process is replaced another way. Close and relaunch this agent to recover.")}));
            }
        }
        self.changed += 1;
    }
    pub fn initialized(&mut self, data: &Value) {
        if self.exit.is_some() {
            return;
        }
        self.ready = true;
        self.version = data["claude_code_version"]
            .as_str()
            .filter(|s| s.len() <= 128)
            .map(str::to_owned);
        self.startup.initialize(data);
        self.refresh();
    }
    pub fn background_known_empty(&self) -> bool {
        self.background_revision > 0 && self.background.as_ref().is_some_and(Vec::is_empty)
    }
    /// Only after the runtime has independently observed old-child exit. No requested session is
    /// written as observed; the new child must supply its own native session readback.
    pub fn replace_owned(&mut self, account: Value, generation: u64) {
        self.work = super::work::Work::default();
        self.reset();
        self.requests = Requests::default();
        self.generation = generation;
        self.ready = false;
        self.exit = None;
        self.end_ready = false;
        self.version = None;
        self.native_caps.clear();
        self.background = None;
        self.background_revision = 0;
        self.account = Some(account);
        self.incomplete.clear();
        self.refresh();
    }
    fn reset(&mut self) {
        self.epoch_number += 1;
        self.journal
            .new_epoch(format!("{}.e{}", self.boot, self.epoch_number));
        self.journal.set_coverage(Coverage::Tail);
        self.entries.clear();
        self.normal.clear();
        self.tools.clear();
        self.active_tools.clear();
        self.position = 0;
        if self.session.is_some() {
            self.retired_session = self.session.take();
        }
        self.run = None;
        self.completion = None;
        self.foreground = "unknown";
        self.requests.expire();
        self.outgoing.clear();
        self.availability = Availability::default();
        self.startup = Startup::new();
        self.plans = super::plans::Plans::default();
        self.work_changes(self.work.rows());
    }
    fn work_changes(&mut self, changes: Vec<super::work::Change>) {
        for change in changes {
            self.put(change.kind, &change.id, change.value);
        }
        if self.work.incomplete {
            self.incomplete.insert("work".into());
        }
    }
    fn native_session(&mut self, id: &Value) {
        let Some(id) = id.as_str().filter(|s| !s.is_empty() && s.len() <= 128) else {
            return;
        };
        if self.session.as_deref().is_some_and(|old| old != id) {
            self.reset();
        }
        self.session = Some(id.into());
        if self.retired_session.as_deref() == Some(id) {
            self.retired_session = None;
        }
        self.put("session", "current", json!({"id":id}));
    }
    pub fn observe(&mut self, event: &Event) {
        match event {
            Event::Exited(exit) => {
                self.exit = Some(exit.clone());
                self.ready = false;
                self.background = None;
                self.foreground = "unknown";
                for change in self.requests.expire() {
                    self.put("request", &change.id, change.value);
                }
            }
            Event::ProtocolError(_) => {
                self.ready = false;
                self.background = None;
                self.foreground = "unknown";
                self.incomplete.insert("native_protocol".into());
            }
            Event::Record(frame) => self.record(frame),
        }
        self.refresh();
    }
    fn record(&mut self, frame: &Value) {
        let kind = frame["type"].as_str().unwrap_or("");
        // Native background level is PER PROCESS, not the session_id on the event. A task from
        // the previous conversation can still run after a session reset; never drop its level.
        if kind == "system" && frame["subtype"] == "background_tasks_changed" {
            self.background_revision += 1;
            let (changes, background) = self.work.level(frame);
            self.work_changes(changes);
            self.background = background;
            if self.background.is_some() {
                self.incomplete.remove("background");
            } else {
                self.incomplete.insert("background".into());
            }
            // Membership is its own full per-process level, NEVER an edge-derived completion
            // or evidence of a new foreground turn. Preserve the limit episode independently.
            return;
        }
        if kind == "system"
            && matches!(
                frame["subtype"].as_str(),
                Some("task_started" | "task_progress" | "task_updated" | "task_notification")
            )
        {
            let changes = self.work.event(frame);
            if self
                .work
                .uncovers_background(frame, self.background.as_ref())
            {
                self.background = None;
                self.incomplete.insert("background".into());
            }
            self.work_changes(changes);
            return;
        }
        let (child, changes, scoped) = self.work.scope(frame);
        self.work_changes(changes);
        if scoped {
            if let Some(child) = child {
                self.child_record(frame, &child);
            } else {
                self.incomplete.insert("child_items".into());
                if matches!(kind, "control_request" | "control_cancel_request") {
                    self.request_frame(frame, None, true);
                }
            }
            return;
        }
        if kind == "conversation_reset" {
            self.reset();
            return;
        }
        // Operation-scoped native consent can settle after the foreground session changed.
        // Retire that exact request before deciding whether to omit the old session's content.
        let mut settled = Vec::new();
        if kind == "tool_progress" {
            if let Some(id) = frame["tool_use_id"].as_str() {
                settled.push(id);
            }
        }
        if kind == "user" {
            if let Some(blocks) = frame["message"]["content"].as_array() {
                for block in blocks {
                    if block["type"] == "tool_result" {
                        if let Some(id) = block["tool_use_id"].as_str() {
                            settled.push(id);
                        }
                    }
                }
            }
        }
        for id in settled {
            for change in self.requests.tool_started(id) {
                self.put("request", &change.id, change.value);
            }
        }
        let init = kind == "system" && frame["subtype"] == "init";
        let state = kind == "system" && frame["subtype"] == "session_state_changed";
        if self.exit.is_some()
            && matches!(
                kind,
                "control_request" | "control_cancel_request" | "control_response"
            )
        {
            return;
        }
        if init || state {
            self.native_session(&frame["session_id"]);
        }
        if !init
            && !state
            && self.session.is_none()
            && frame["session_id"]
                .as_str()
                .is_some_and(|id| self.retired_session.as_deref() == Some(id))
        {
            self.incomplete.insert("foreign_session".into());
            return;
        }
        if let (Some(current), Some(actual)) = (&self.session, frame["session_id"].as_str()) {
            if current != actual {
                self.incomplete.insert("foreign_session".into());
                return;
            }
        }
        if init {
            self.startup.init(frame);
            if let Some(version) = frame["claude_code_version"]
                .as_str()
                .filter(|v| v.len() <= 128)
            {
                self.version = Some(version.into());
            }
            if let Some(caps) = frame["capabilities"].as_array() {
                self.native_caps = caps
                    .iter()
                    .filter_map(Value::as_str)
                    .take(256)
                    .filter(|s| s.len() <= 128)
                    .map(str::to_owned)
                    .collect();
            }
            return;
        }
        if state {
            if self.run.is_none()
                && matches!(frame["state"].as_str(), Some("running" | "requires_action"))
            {
                self.availability.begin_turn();
            }
            if matches!(frame["state"].as_str(), Some("running" | "requires_action")) {
                self.availability.invalidate_episode();
            }
            self.foreground = match frame["state"].as_str() {
                Some("idle") => "idle",
                Some("running") => "running",
                Some("requires_action") => "requires_action",
                _ => "unknown",
            };
            if self.foreground == "idle" {
                self.run = None;
            } else if self.foreground == "running" && self.run.is_none() {
                self.run = Some(self.next("turn"));
            }
            return;
        }
        if matches!(kind, "control_request" | "control_cancel_request") {
            self.request_frame(frame, None, false);
            return;
        }
        if kind == "control_response" {
            // These arrays are on the response WRAPPER, not its convenience .response payload.
            for field in [
                "pending_permission_requests",
                "pending_user_dialog_requests",
                "pending_elicitation_requests",
            ] {
                if let Some(rows) = frame["response"][field].as_array() {
                    for request in rows.iter().take(32) {
                        self.record(request);
                    }
                    if rows.len() > 32 {
                        self.incomplete.insert("requests".into());
                    }
                }
            }
            return;
        }
        if matches!(kind, "assistant" | "tool_progress")
            || (kind == "user" && frame["isReplay"] == true)
            || (kind == "system"
                && matches!(
                    frame["subtype"].as_str(),
                    Some("api_retry" | "task_started")
                ))
        {
            self.availability.invalidate_episode();
        }
        let active = self.run.is_some()
            || kind == "assistant"
            || (kind == "system" && frame["subtype"] == "api_retry");
        self.availability.observe(frame, active);
        match kind {
            "assistant" | "user" => self.message(frame, None),
            "result" => self.result(frame),
            "tool_progress" => {
                if let Some(id) = frame["tool_use_id"].as_str() {
                    for change in self.requests.tool_started(id) {
                        self.put("request", &change.id, change.value);
                    }
                    if let Some(tool) = self.tools.get(id).cloned() {
                        if let Some((_, value)) = self.journal.entity("tool", &tool) {
                            let mut value = value.clone();
                            value["status"] = json!("running");
                            self.put("tool", &tool, value);
                        }
                    }
                }
            }
            "active_goal" => {
                let changes = self.plans.goal(frame);
                self.work_changes(changes);
                if self.plans.incomplete {
                    self.incomplete.insert("plans".into());
                }
            }
            "rate_limit_event" => {}
            "auth_status" => {} // Never retain login output, which can contain authentication material.
            "system" if frame["subtype"] == "api_retry" => {}
            "system" if frame["subtype"] == "session_writes_lost" => {
                self.incomplete.insert("persistence".into());
            }
            _ => {
                self.incomplete.insert("native_events".into());
            }
        }
    }
    fn request_frame(&mut self, frame: &Value, child: Option<&str>, unscoped: bool) {
        let incoming = if unscoped {
            self.requests.observe_unscoped(frame)
        } else {
            self.requests.observe(frame, child)
        };
        for change in incoming.changes {
            if !self.put("request", &change.id, change.value) {
                self.incomplete.insert("requests".into());
                if let Some(response) = self.requests.unrepresentable(&change.id) {
                    if self.outgoing.len() < 16 {
                        self.outgoing.push_back(response);
                    }
                }
            }
        }
        for response in incoming.outgoing {
            if self.outgoing.len() < 16 {
                self.outgoing.push_back(response);
            } else {
                self.incomplete.insert("requests".into());
            }
        }
    }
    fn child_record(&mut self, frame: &Value, child: &str) {
        match frame["type"].as_str() {
            Some("control_request" | "control_cancel_request") => {
                self.request_frame(frame, Some(child), false)
            }
            Some("assistant" | "user") => self.message(frame, Some(child)),
            Some("tool_progress") => {
                if let Some(native) = super::work::native_id(&frame["tool_use_id"]) {
                    for change in self.requests.tool_started_scoped(native, Some(child)) {
                        self.put("request", &change.id, change.value);
                    }
                    if let Some(id) = self.work.identity(child, "tool", native, false) {
                        if let Some((_, value)) = self.journal.entity("tool", &id) {
                            let mut value = value.clone();
                            value["status"] = json!("running");
                            self.put("tool", &id, value);
                        }
                    }
                }
            }
            Some("auth_status" | "rate_limit_event" | "result" | "system") => {}
            _ => {
                self.incomplete.insert("child_items".into());
            }
        }
    }
    fn message(&mut self, frame: &Value, child: Option<&str>) {
        let uuid = frame["uuid"]
            .as_str()
            .filter(|s| s.len() <= 128)
            .map(str::to_owned)
            .unwrap_or_else(|| self.next("record"));
        let role = if frame["type"] == "assistant" {
            "assistant"
        } else {
            "user"
        };
        if role == "assistant" && child.is_none() {
            self.foreground = "running";
            if self.run.is_none() {
                self.run = Some(self.next("turn"));
            }
        }
        let blocks = match &frame["message"]["content"] {
            Value::String(s) => vec![json!({"type":"text","text":s})],
            Value::Array(a) => a.clone(),
            _ => {
                self.incomplete.insert("messages".into());
                return;
            }
        };
        if blocks.len() > 64 {
            self.incomplete.insert("messages".into());
        }
        for (index, block) in blocks.iter().take(64).enumerate() {
            match block["type"].as_str() {
                Some("tool_use") => {
                    if let (Some(id), Some(name)) = (
                        block["id"].as_str().filter(|s| s.len() <= 128),
                        block["name"].as_str().filter(|s| s.len() <= 256),
                    ) {
                        if child.is_none()
                            && self.tools.len() >= 512
                            && !self.tools.contains_key(id)
                        {
                            if let Some(old) = self
                                .tools
                                .keys()
                                .find(|old| !self.active_tools.contains(*old))
                                .cloned()
                            {
                                self.tools.remove(&old);
                            } else {
                                self.incomplete.insert("tools".into());
                                continue;
                            }
                        }
                        let entity = if let Some(child) = child {
                            let Some(entity) = self.work.identity(child, "tool", id, true) else {
                                self.incomplete.insert("child_items".into());
                                continue;
                            };
                            entity
                        } else {
                            format!("tool-{id}")
                        };
                        self.position += 1;
                        let mut value = json!({"position":self.position,"title":name,"status":"pending","native":{"tool_use_id":id}});
                        if crate::limits::encoded_bytes(&block["input"]) < 32 * 1024 {
                            value["input"] = block["input"].clone();
                        } else {
                            value["incomplete"] = json!(true);
                            self.incomplete.insert("tools".into());
                        }
                        if child.is_none() {
                            self.tools.insert(id.into(), entity.clone());
                            self.active_tools.insert(id.into());
                        } else {
                            value["child"] = json!(child);
                        }
                        self.put("tool", &entity, value);
                    } else {
                        self.incomplete.insert("tools".into());
                    }
                }
                Some("tool_result") => {
                    if let Some(id) = block["tool_use_id"].as_str() {
                        if child.is_none() {
                            self.active_tools.remove(id);
                        }
                        for change in self.requests.tool_started_scoped(id, child) {
                            self.put("request", &change.id, change.value);
                        }
                        let entity = if let Some(child) = child {
                            self.work.identity(child, "tool", id, false)
                        } else {
                            self.tools.get(id).cloned()
                        };
                        if let Some(entity) = entity {
                            if let Some((_, old)) = self.journal.entity("tool", &entity) {
                                let mut value = old.clone();
                                value["status"] = json!(if block["is_error"] == true {
                                    "failed"
                                } else {
                                    "completed"
                                });
                                let content = &block["content"];
                                if crate::limits::encoded_bytes(content) < 32 * 1024 {
                                    value["content"] = content.clone();
                                } else {
                                    value["incomplete"] = json!(true);
                                    self.incomplete.insert("tools".into());
                                }
                                let name = value["title"].as_str().unwrap_or("").to_owned();
                                self.put("tool", &entity, value);
                                let result_count =
                                    blocks.iter().filter(|b| b["type"] == "tool_result").count();
                                let output = if result_count == 1 {
                                    &frame["tool_use_result"]
                                } else {
                                    &Value::Null
                                };
                                let changes = self.plans.observe(
                                    &name,
                                    &entity,
                                    child,
                                    output,
                                    !block.get("is_error").is_none_or(|v| v == false),
                                );
                                self.work_changes(changes);
                                if self.plans.incomplete {
                                    self.incomplete.insert("plans".into());
                                }
                            }
                        }
                    }
                }
                Some("text" | "thinking") => {
                    let thought = block["type"] == "thinking";
                    let text = &block[if thought { "thinking" } else { "text" }];
                    if let Some(text) = text.as_str() {
                        if text.len() > ENTITY_BYTES - 4096 {
                            self.incomplete.insert("messages".into());
                            continue;
                        }
                        let native_id = format!("{uuid}.{index}");
                        let id = if let Some(child) = child {
                            let Some(id) = self.work.identity(child, "message", &native_id, true)
                            else {
                                self.incomplete.insert("child_items".into());
                                continue;
                            };
                            id
                        } else {
                            native_id
                        };
                        let position = self
                            .journal
                            .entity("message", &id)
                            .and_then(|(_, v)| v["position"].as_u64())
                            .unwrap_or_else(|| {
                                self.position += 1;
                                self.position
                            });
                        let mut value = json!({"position":position,"role":if thought{"thought"}else{role},"content":[{"type":"text","text":text}],
                            "final":frame["aborted"]!=true,"native":{"uuid":uuid,"message_id":frame["message"]["id"],"echo":frame["isReplay"]==true}});
                        if child.is_some() {
                            value["child"] = json!(child);
                        }
                        if let Some((delivery, epoch)) =
                            self.inputs.get(&uuid).filter(|_| child.is_none())
                        {
                            if epoch == self.epoch() {
                                value["delivery"] = json!(delivery);
                                if let Some(receipt) = self.receipts.get(delivery) {
                                    for field in ["origin", "quoted", "rendering"] {
                                        if let Some(field_value) = receipt.get(field) {
                                            value[field] = field_value.clone();
                                        }
                                    }
                                }
                            }
                        }
                        self.put("message", &id, value);
                    } else {
                        self.incomplete.insert("messages".into());
                    }
                }
                _ => {
                    self.incomplete.insert("message_blocks".into());
                }
            }
        }
        if frame["isReplay"] == true && child.is_none() {
            self.correlate(&[uuid], "admitted", None, None);
        }
    }
    fn result(&mut self, frame: &Value) {
        let native_uuid = frame["uuid"].as_str().filter(|s| s.len() <= 128);
        let id = native_uuid
            .map(str::to_owned)
            .unwrap_or_else(|| self.next("completion"));
        let reason = match frame["terminal_reason"].as_str() {
            Some("aborted_streaming" | "aborted_tools") => "cancelled",
            Some("completed") => "end_turn",
            _ if frame["is_error"] == true => "error",
            _ => "unknown",
        };
        self.completion = Some(json!({"id":id,"reason":reason}));
        let run = self.run.clone().unwrap_or_else(|| self.next("turn"));
        let mut value = json!({"native":{"terminal_reason":frame["terminal_reason"]}});
        if let Some(native_uuid) = native_uuid {
            value["native"]["result_uuid"] = json!(native_uuid);
        }
        if let (Some(input), Some(output)) = (
            frame["usage"]["input_tokens"].as_u64(),
            frame["usage"]["output_tokens"].as_u64(),
        ) {
            value["usage"] = json!({"tokens":{"input":input,"output":output}});
        }
        self.put("run", &run, value);
        let uuids: Vec<String> = frame["user_message_uuids"]
            .as_array()
            .map(|a| {
                a.iter()
                    .take(64)
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_else(|| {
                frame["user_message_uuid"]
                    .as_str()
                    .map(|s| vec![s.into()])
                    .unwrap_or_default()
            });
        if frame["user_message_uuids"]
            .as_array()
            .is_some_and(|a| a.len() > 64)
        {
            self.incomplete.insert("receipt_correlations".into());
        }
        self.correlate(&uuids, "completed", Some(&run), native_uuid);
        // Result is not an invented idle signal; session_state_changed:idle is authoritative.
    }
    /// vqro reservations, NOT a native queue. Count unresolved old epochs too: changing the
    /// observation epoch does not establish that a previously accepted input cannot still run.
    pub fn input_ledger(&self) -> Value {
        let pending = self
            .inputs
            .values()
            .filter(|(id, _)| {
                self.receipts.get(id).is_some_and(|r| {
                    matches!(
                        r["stage"].as_str(),
                        Some("unknown" | "dispatched" | "admitted")
                    )
                })
            })
            .count();
        json!({"v":0,"source":"vqro-ledger","epoch":self.epoch(),"complete":self.ledger_complete,
            "pending":if self.ledger_complete{Some(pending)}else{None}})
    }
    pub fn delivery_active(&self, delivery: &str) -> bool {
        self.inputs
            .values()
            .any(|(id, epoch)| id == delivery && epoch == self.epoch())
            && self.receipts.get(delivery).is_some_and(|r| {
                matches!(
                    r["stage"].as_str(),
                    Some("unknown" | "dispatched" | "admitted")
                )
            })
    }
    pub fn receipt(&self, delivery: &str) -> Value {
        self.receipts
            .get(delivery)
            .cloned()
            .unwrap_or_else(|| json!({"stage":"unknown"}))
    }
    pub fn save_receipt(&mut self, delivery: &str, value: Value) {
        if !self.receipts.contains_key(delivery) {
            self.receipt_order.push_back(delivery.into());
        }
        self.receipts.insert(delivery.into(), value.clone());
        self.put("receipt", delivery, value);
        while self.receipt_order.len() > 256 {
            let at = self
                .receipt_order
                .iter()
                .position(|id| !self.delivery_active(id));
            let Some(at) = at else {
                self.incomplete.insert("receipts".into());
                break;
            };
            let old = self.receipt_order.remove(at).unwrap();
            if self.inputs.values().any(|(d, _)| d == &old)
                && self.receipts.get(&old).is_some_and(|r| {
                    matches!(
                        r["stage"].as_str(),
                        Some("unknown" | "dispatched" | "admitted")
                    )
                })
            {
                self.ledger_complete = false;
                self.incomplete.insert("input_ledger".into());
            }
            self.receipts.remove(&old);
            self.inputs.retain(|_, (delivery, _)| delivery != &old);
        }
        self.changed += 1;
    }
    fn correlate(
        &mut self,
        uuids: &[String],
        stage: &str,
        run: Option<&str>,
        native_result: Option<&str>,
    ) {
        for uuid in uuids {
            let Some((delivery, epoch)) = self.inputs.get(uuid).cloned() else {
                continue;
            };
            if epoch != self.epoch() {
                continue;
            }
            if let Some(mut value) = self.receipts.get(&delivery).cloned() {
                if value["stage"] == "completed" {
                    continue;
                }
                value["stage"] = json!(stage);
                if let Some(run) = run {
                    value["run"] = json!(run);
                }
                if let Some(uuid) = native_result {
                    value["native"]["result_uuid"] = json!(uuid);
                }
                self.save_receipt(&delivery, value);
            }
        }
    }
}
