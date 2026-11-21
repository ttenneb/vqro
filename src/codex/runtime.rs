//! Provider-facing orchestration. Native event draining and controls never wait on an admission.
//! The shared SDK item producer is supplied as a callback; no second item reducer lives here.
use super::{Change, Endpoint, SendContext, provider_capabilities};
use crate::provider_client::{Outcome, ProviderError};
use crate::rpc::{Incoming, Peer, RpcError};
use serde_json::{Map, Value, json};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{Mutex as AsyncMutex, Semaphore, oneshot, watch};
use tokio::time::Instant;

pub type Observer = Arc<dyn Fn(usize, &str, Vec<Change>, bool) + Send + Sync>;
const NATIVE_LIMIT: Duration = Duration::from_secs(300);

struct Slot {
    endpoint: Mutex<Endpoint>,
    admission: Arc<AsyncMutex<()>>,
    dirty: AtomicBool,
}

pub struct Runtime {
    pub native: Peer,
    slots: Vec<Slot>,
    sessions: AsyncMutex<()>,
    native_admissions: Arc<Semaphore>,
    changed: watch::Sender<u64>,
    observer: Option<Observer>,
    incarnation: String,
    launcher_principal: Option<String>,
}

enum Fault {
    Reject(RpcError),
    Unknown(RpcError),
}
impl Fault {
    fn invalid(message: &str) -> Self {
        Self::Reject(RpcError::new("invalid", message))
    }
    fn unsupported(message: &str) -> Self {
        Self::Reject(RpcError::new("unsupported", message))
    }
}

impl Runtime {
    pub fn new(
        native: Peer,
        count: usize,
        incarnation: String,
        observer: Option<Observer>,
    ) -> Arc<Self> {
        let instance = incarnation.clone();
        Self::with_host(
            native,
            count,
            incarnation,
            "codex".into(),
            instance,
            observer,
        )
    }

    pub fn with_host(
        native: Peer,
        count: usize,
        incarnation: String,
        provider: String,
        instance: String,
        observer: Option<Observer>,
    ) -> Arc<Self> {
        Self::with_host_principal(
            native,
            count,
            incarnation,
            provider,
            instance,
            observer,
            None,
        )
    }

    /// An explicit single-slot launcher identity, not a host tag or tool-env capability.
    /// The executable checks that its opt-in matches the configured provider identity.
    pub fn with_host_principal(
        native: Peer,
        count: usize,
        incarnation: String,
        provider: String,
        instance: String,
        observer: Option<Observer>,
        launcher_principal: Option<String>,
    ) -> Arc<Self> {
        assert!((1..=200).contains(&count));
        let launcher_principal = launcher_principal.filter(|principal| {
            count == 1
                && principal != crate::principal::UNDECLARED
                && crate::principal::validate_assertable(principal).is_ok()
        });
        let (changed, _) = watch::channel(0);
        let host = format!("codex:{incarnation}");
        let runtime = Arc::new(Self {
            native,
            slots: (0..count)
                .map(|i| {
                    let mut endpoint = Endpoint::new(
                        format!("endpoint-{}", i + 1),
                        incarnation.clone(),
                        host.clone(),
                        format!("{incarnation}:{i}:0"),
                    );
                    endpoint.host_provider = provider.clone();
                    endpoint.host_instance = instance.clone();
                    Slot {
                        endpoint: Mutex::new(endpoint),
                        admission: Arc::new(AsyncMutex::new(())),
                        dirty: AtomicBool::new(true),
                    }
                })
                .collect(),
            sessions: AsyncMutex::new(()),
            native_admissions: Arc::new(Semaphore::new(64)),
            changed,
            observer,
            incarnation,
            launcher_principal,
        });
        for (index, slot) in runtime.slots.iter().enumerate() {
            let endpoint = slot.endpoint.lock().unwrap();
            runtime.emit(
                index,
                &endpoint,
                vec![
                    endpoint.state_entity(),
                    Change::new("session", "current", json!({"id":null,"title":null})),
                ],
                false,
            );
        }
        runtime
    }

    pub fn capabilities(&self) -> Map<String, Value> {
        let mut caps = provider_capabilities();
        // An intermediate build without C3 is deliberately not an observed/conformant adapter.
        // Do not advertise an item channel until a real bounded producer has been attached.
        if self.observer.is_none() {
            caps.remove("items.stream");
            caps["agent"]["modules"]
                .as_object_mut()
                .unwrap()
                .remove("observation");
        }
        caps
    }

    pub fn facts(&self) -> Vec<Value> {
        self.slots
            .iter()
            .map(|slot| self.published_facts(slot.endpoint.lock().unwrap().facts()))
            .collect()
    }

    pub fn fact(&self, reference: &str) -> Option<Value> {
        let index = self.index(reference)?;
        Some(self.published_facts(self.slots[index].endpoint.lock().unwrap().facts()))
    }

    /// Single publisher drains semantic facts dirtiness; native item traffic never republishes all
    /// endpoints. Capture and clear under the endpoint lock so a subsequent change cannot be lost.
    pub fn take_dirty_facts(&self) -> Vec<Value> {
        self.slots
            .iter()
            .filter_map(|slot| {
                let endpoint = slot.endpoint.lock().unwrap();
                if !slot.dirty.swap(false, Ordering::AcqRel) {
                    return None;
                }
                Some(self.published_facts(endpoint.facts()))
            })
            .collect()
    }

    fn published_facts(&self, mut facts: Value) -> Value {
        if let Some(principal) = &self.launcher_principal {
            facts["data"]["agent"]["principal"] = json!(principal);
        }
        if self.observer.is_none() {
            facts["data"]["agent"]["caps"]
                .as_object_mut()
                .unwrap()
                .remove("items.stream");
            facts["data"]["agent"]["caps"]["agent"]["modules"]
                .as_object_mut()
                .unwrap()
                .remove("observation");
            facts["data"]["agent"]["incomplete"] = json!(["items"]);
        }
        facts
    }

    pub fn changes(&self) -> watch::Receiver<u64> {
        self.changed.subscribe()
    }

    fn emit(&self, index: usize, endpoint: &Endpoint, changes: Vec<Change>, reset: bool) {
        if reset
            || changes
                .iter()
                .any(|c| matches!(c.kind.as_str(), "state" | "session" | "request"))
        {
            self.slots[index].dirty.store(true, Ordering::Release);
        }
        if let Some(observer) = &self.observer {
            observer(index, &endpoint.epoch, changes, reset);
        }
        self.changed.send_modify(|n| *n = n.saturating_add(1));
    }

    fn index(&self, reference: &str) -> Option<usize> {
        reference
            .strip_prefix("endpoint-")?
            .parse::<usize>()
            .ok()?
            .checked_sub(1)
            .filter(|i| *i < self.slots.len())
    }

    fn thread_index(&self, thread: &str) -> Option<usize> {
        self.slots
            .iter()
            .position(|s| s.endpoint.lock().unwrap().session.as_deref() == Some(thread))
    }

    /// Called by the native reader independently of provider connection lifetime.
    pub fn incoming(&self, incoming: Incoming) {
        match incoming {
            Incoming::Notification { method, params } => {
                if crate::limits::json_depth(&params, 64).is_err() {
                    self.native.close();
                    return;
                }
                if let Some(index) = params["threadId"]
                    .as_str()
                    .and_then(|id| self.thread_index(id))
                {
                    let mut endpoint = self.slots[index].endpoint.lock().unwrap();
                    let changes = endpoint.notification(&method, &params);
                    if !changes.is_empty() {
                        self.emit(index, &endpoint, changes, false);
                    }
                }
            }
            Incoming::Request { id, method, params } => {
                if crate::limits::json_depth(&params, 64).is_err() {
                    self.native.close();
                    return;
                }
                if let Some(index) = params["threadId"]
                    .as_str()
                    .and_then(|id| self.thread_index(id))
                {
                    let mut endpoint = self.slots[index].endpoint.lock().unwrap();
                    match endpoint.request(id.clone(), &method, &params) {
                        Ok(change) => self.emit(index, &endpoint, vec![change], false),
                        // A reused outstanding native id is a protocol fault, not another answer
                        // to the original request. Closing preserves uncertainty and never approves.
                        Err(error) if error.kind() == "stale" => self.native.close(),
                        Err(_) => {
                            self.cancel_unknown(id);
                            let notice = endpoint.request_cancellation_notice();
                            self.emit(index, &endpoint, vec![notice], false);
                        }
                    }
                } else {
                    self.cancel_unknown(id);
                    // A host-wide/unbound request has no honest endpoint/session association.
                    // Report that shared-host diagnostic to its endpoints, never invent a question.
                    for (index, slot) in self.slots.iter().enumerate() {
                        let mut endpoint = slot.endpoint.lock().unwrap();
                        let notice = endpoint.request_cancellation_notice();
                        self.emit(index, &endpoint, vec![notice], false);
                    }
                }
            }
        }
    }

    fn cancel_unknown(&self, id: Value) {
        self.native.respond(
            id,
            Err(RpcError {
                code: -32800,
                message: "request is not representable by this adapter".into(),
                data: None,
            }),
        );
    }

    pub fn host_exited(&self, reason: &str) {
        for (index, slot) in self.slots.iter().enumerate() {
            let mut endpoint = slot.endpoint.lock().unwrap();
            let changes = endpoint.host_exited(reason);
            self.emit(index, &endpoint, changes, false);
        }
    }

    pub async fn invoke(
        self: &Arc<Self>,
        reference: &str,
        incarnation: &str,
        op: &str,
        args: Value,
        deadline: Instant,
    ) -> Result<Outcome, ProviderError> {
        self.invoke_with_context(
            reference,
            incarnation,
            op,
            args,
            deadline,
            SendContext::default(),
        )
        .await
    }

    /// Trusted adapter evidence is separate from the caller's wire arguments.
    pub async fn invoke_with_context(
        self: &Arc<Self>,
        reference: &str,
        incarnation: &str,
        op: &str,
        args: Value,
        deadline: Instant,
        context: SendContext,
    ) -> Result<Outcome, ProviderError> {
        let Some(index) = self.index(reference) else {
            return Ok(Outcome::Rejected {
                kind: "not_found".into(),
                message: "unknown endpoint".into(),
            });
        };
        if incarnation != self.incarnation {
            return Ok(Outcome::Rejected {
                kind: "stale".into(),
                message: "endpoint incarnation changed".into(),
            });
        }
        if Instant::now() >= deadline {
            return Ok(Outcome::Rejected {
                kind: "timeout".into(),
                message: "deadline expired before native dispatch".into(),
            });
        }
        let result = self.operation(index, op, args, deadline, context).await;
        match result {
            Ok(outcome) => Ok(outcome),
            Err(Fault::Reject(error)) => Ok(Outcome::Rejected {
                kind: error.kind().into(),
                message: error.message,
            }),
            Err(Fault::Unknown(error)) => Err(ProviderError::new(
                "backend",
                format!(
                    "native outcome unknown: {}",
                    error.message.chars().take(512).collect::<String>()
                ),
            )),
        }
    }

    /// Read-only bounded discovery. Unsupported/malformed catalogs never become synthetic choices.
    pub async fn initialize_models(
        self: &Arc<Self>,
        deadline: Instant,
    ) -> Result<(), ProviderError> {
        let mut choices = Vec::new();
        let mut cursor = Value::Null;
        let mut seen = std::collections::BTreeSet::new();
        for _ in 0..super::configuration::PAGES {
            let page = self
                .native
                .call_with_timeout(
                    "model/list",
                    json!({"limit":64,"cursor":cursor,"includeHidden":false}),
                    remaining(deadline),
                )
                .await
                .map_err(|_| {
                    ProviderError::new("unavailable", "native model catalog unavailable")
                })?;
            let next = super::configuration::page(&page, &mut choices).map_err(|_| {
                ProviderError::new("backend", "native model catalog invalid or exceeds bound")
            })?;
            match next {
                Some(next) if seen.insert(next.clone()) => cursor = json!(next),
                Some(_) => {
                    return Err(ProviderError::new(
                        "backend",
                        "native model catalog repeats a cursor",
                    ));
                }
                None => {
                    for (index, slot) in self.slots.iter().enumerate() {
                        let mut endpoint = slot.endpoint.lock().unwrap();
                        endpoint.install_models(choices.clone());
                        self.emit(index, &endpoint, Vec::new(), false);
                    }
                    return Ok(());
                }
            }
        }
        Err(ProviderError::new(
            "backend",
            "native model catalog exceeds page bound",
        ))
    }

    async fn operation(
        self: &Arc<Self>,
        index: usize,
        op: &str,
        args: Value,
        deadline: Instant,
        context: SendContext,
    ) -> Result<Outcome, Fault> {
        self.slots[index]
            .endpoint
            .lock()
            .unwrap()
            .validate(&args)
            .map_err(Fault::Reject)?;
        match op {
            "agent.send" => self.send(index, args, deadline, context).await,
            "agent.configure" => self.configure(index, args, deadline).await,
            "agent.receipt" => {
                let delivery = super::id(&args, "delivery").map_err(Fault::Reject)?;
                Ok(Outcome::Observed(Some(
                    self.slots[index]
                        .endpoint
                        .lock()
                        .unwrap()
                        .receipt(&delivery),
                )))
            }
            "agent.interrupt" => {
                let params = self.slots[index]
                    .endpoint
                    .lock()
                    .unwrap()
                    .interrupt_params(&args)
                    .map_err(Fault::Reject)?;
                let Some(params) = params else {
                    return Ok(Outcome::Observed(Some(json!({"noop": true}))));
                };
                self.native
                    .call_with_timeout("turn/interrupt", params, remaining(deadline))
                    .await
                    .map_err(Fault::Unknown)?;
                // A successful native interrupt request is not itself endpoint or turn-end evidence.
                Ok(Outcome::Accepted(None))
            }
            "agent.respond" => {
                let answer = {
                    let mut endpoint = self.slots[index].endpoint.lock().unwrap();
                    let answer = endpoint.prepare_answer(&args).map_err(Fault::Reject)?;
                    self.emit(index, &endpoint, vec![answer.change.clone()], false);
                    answer
                };
                self.native
                    .respond_flushed(answer.native_id, answer.result)
                    .await
                    .map_err(Fault::Unknown)?;
                Ok(Outcome::Dispatched(None))
            }
            "agent.session" => self.session(index, &args, deadline).await,
            _ => Err(Fault::unsupported(
                "operation is not declared for this endpoint",
            )),
        }
    }

    async fn send(
        self: &Arc<Self>,
        index: usize,
        args: Value,
        deadline: Instant,
        context: SendContext,
    ) -> Result<Outcome, Fault> {
        let gate = self.slots[index]
            .admission
            .clone()
            .try_lock_owned()
            .map_err(|_| {
                Fault::Reject(RpcError::new(
                    "overloaded",
                    "one native admission/session preflight is already in flight",
                ))
            })?;
        let permit = self
            .native_admissions
            .clone()
            .try_acquire_owned()
            .map_err(|_| {
                Fault::Reject(RpcError::new(
                    "overloaded",
                    "native admission capacity full; not sent",
                ))
            })?;
        let call = self.slots[index]
            .endpoint
            .lock()
            .unwrap()
            .prepare_send_with_context(&args, context)
            .map_err(Fault::Reject)?;
        if Instant::now() >= deadline {
            return Err(Fault::Reject(RpcError::new(
                "timeout",
                "deadline expired before native write",
            )));
        }
        let delivery = call.delivery.clone();
        let (tx, mut rx) = oneshot::channel();
        let this = self.clone();
        tokio::spawn(async move {
            // Keep session/admission serialization even after the original provider reply deadline.
            let (_gate, _permit) = (gate, permit);
            if Instant::now() >= deadline {
                let _ = tx.send(Err(Fault::Reject(RpcError::new(
                    "timeout",
                    "deadline expired before native write",
                ))));
                return;
            }
            {
                let mut endpoint = this.slots[index].endpoint.lock().unwrap();
                let change = endpoint.record_receipt(&call.delivery, "dispatched", None, &args);
                this.emit(index, &endpoint, vec![change], false);
            }
            let result = this
                .native
                .call_with_timeout(call.method, call.params, NATIVE_LIMIT)
                .await
                .map_err(|error| native_admission_error(call.method, error));
            let (stage, native) = match &result {
                Ok(value) => {
                    let run = value["turnId"]
                        .as_str()
                        .or_else(|| value["turn"]["id"].as_str())
                        .filter(|s| !s.is_empty() && s.len() <= 128);
                    ("admitted", run.map(|r| json!({"run": r})))
                }
                Err(Fault::Reject(_)) => ("rejected", None),
                Err(Fault::Unknown(_)) => ("unknown", None),
            };
            let receipt = {
                let mut endpoint = this.slots[index].endpoint.lock().unwrap();
                let change = endpoint.record_receipt(&call.delivery, stage, native, &args);
                let receipt = change.value.clone();
                this.emit(index, &endpoint, vec![change], false);
                receipt
            };
            let result = result.map(|_| receipt);
            let _ = tx.send(result);
        });
        // Leave scheduling margin for the provider to flush its one reply before the core deadline.
        let reply_at = deadline
            .checked_sub(Duration::from_millis(10))
            .unwrap_or(deadline);
        tokio::select! {
            biased;
            result = &mut rx => match result {
                Ok(Ok(receipt)) => Ok(Outcome::Accepted(Some(json!({"receipt":receipt})))),
                Ok(Err(error)) => Err(error),
                Err(_) => Err(Fault::Unknown(RpcError::new("backend", "admission worker ended; do not replay"))),
            },
            _ = tokio::time::sleep_until(reply_at) => {
                // Native callback remains alive and updates the bounded receipt lookup later.
                let receipt = self.slots[index].endpoint.lock().unwrap().receipt(&delivery);
                match receipt["stage"].as_str() {
                    Some("admitted") => Ok(Outcome::Accepted(Some(json!({"receipt":receipt})))),
                    Some("unknown") => Err(Fault::Unknown(RpcError::new("backend","native admission outcome unknown"))),
                    Some("rejected") => Err(Fault::Reject(RpcError::new("stale","native run guard rejected before input"))),
                    _ => Ok(Outcome::Dispatched(Some(json!({"receipt":receipt})))),
                }
            }
        }
    }

    async fn configure(
        self: &Arc<Self>,
        index: usize,
        args: Value,
        deadline: Instant,
    ) -> Result<Outcome, Fault> {
        let gate = self.slots[index]
            .admission
            .clone()
            .try_lock_owned()
            .map_err(|_| {
                Fault::Reject(RpcError::new(
                    "busy",
                    "native admission or configuration is unresolved",
                ))
            })?;
        let permit = self
            .native_admissions
            .clone()
            .try_acquire_owned()
            .map_err(|_| {
                Fault::Reject(RpcError::new(
                    "overloaded",
                    "native configuration capacity full",
                ))
            })?;
        let (requested, params) = self.slots[index]
            .endpoint
            .lock()
            .unwrap()
            .prepare_model(&args)
            .map_err(Fault::Reject)?;
        let session = params["threadId"].as_str().unwrap().to_owned();
        if Instant::now() >= deadline {
            return Err(Fault::Reject(RpcError::new(
                "timeout",
                "deadline before native model write",
            )));
        }
        let (tx, mut rx) = oneshot::channel();
        let this = self.clone();
        tokio::spawn(async move {
            let (_gate, _permit) = (gate, permit);
            if Instant::now() >= deadline {
                let _ = tx.send(Err(Fault::Reject(RpcError::new(
                    "timeout",
                    "deadline before native model write",
                ))));
                return;
            }
            {
                let mut endpoint = this.slots[index].endpoint.lock().unwrap();
                endpoint.begin_model();
                this.emit(index, &endpoint, Vec::new(), false);
            }
            // Native resumes this same thread with a model override, never config/value/write.
            let result = this
                .native
                .call_with_timeout("thread/resume", params, NATIVE_LIMIT)
                .await;
            let outcome = {
                let mut endpoint = this.slots[index].endpoint.lock().unwrap();
                match result {
                    Ok(response) => match endpoint.finish_model(&response, &session, &requested) {
                        Ok((matched, changes)) => {
                            let value = endpoint.model_option();
                            this.emit(index, &endpoint, changes, false);
                            Ok(if matched {
                                Outcome::Observed(Some(value))
                            } else {
                                Outcome::Accepted(Some(value))
                            })
                        }
                        Err(_) => {
                            let changes = endpoint.model_unknown();
                            this.emit(index, &endpoint, changes, false);
                            Err(Fault::Unknown(RpcError::new(
                                "backend",
                                "native model readback missing or changed thread; do not replay",
                            )))
                        }
                    },
                    Err(_) => {
                        let changes = endpoint.model_unknown();
                        this.emit(index, &endpoint, changes, false);
                        Err(Fault::Unknown(RpcError::new(
                            "backend",
                            "native model outcome unknown; do not replay",
                        )))
                    }
                }
            };
            let _ = tx.send(outcome);
        });
        let reply_at = deadline
            .checked_sub(Duration::from_millis(10))
            .unwrap_or(deadline);
        tokio::select! {
            biased;
            result=&mut rx=>result.unwrap_or_else(|_|Err(Fault::Unknown(RpcError::new("backend","native configuration worker ended; do not replay")))),
            _=tokio::time::sleep_until(reply_at)=>Ok(Outcome::Dispatched(None)),
        }
    }

    async fn session(
        &self,
        index: usize,
        args: &Value,
        deadline: Instant,
    ) -> Result<Outcome, Fault> {
        let action = super::id(args, "action").map_err(Fault::Reject)?;
        if action == "list" {
            let page = super::optional_id(args, "page").map_err(Fault::Reject)?;
            let result = self
                .native
                .call_with_timeout(
                    "thread/list",
                    json!({"limit":64,"cursor":page}),
                    remaining(deadline),
                )
                .await
                .map_err(Fault::Unknown)?;
            let data = result["data"]
                .as_array()
                .filter(|a| a.len() <= 64)
                .ok_or_else(|| {
                    Fault::Unknown(RpcError::new("backend", "invalid native session page"))
                })?;
            let mut sessions = Vec::new();
            for thread in data {
                let id = super::id(thread, "id").map_err(Fault::Unknown)?;
                sessions.push(json!({"id":id,"title":bounded_string(thread.get("name").or_else(||thread.get("title")),256),"cwd":bounded_string(thread.get("cwd"),4096),"updated_ms":thread["updatedAt"].as_i64().and_then(|s|s.checked_mul(1000))}));
            }
            let value = json!({"sessions":sessions,"next":result["nextCursor"]});
            if !super::small(&value, 256 * 1024) {
                return Err(Fault::Unknown(RpcError::new(
                    "backend",
                    "native session page exceeds bound",
                )));
            }
            return Ok(Outcome::Observed(Some(value)));
        }
        if !matches!(action.as_str(), "new" | "resume") {
            return Err(Fault::unsupported("session action is not declared"));
        }
        for name in ["roots", "config"] {
            if args.get(name).is_some_and(|v| !v.is_null()) {
                return Err(Fault::unsupported(
                    "native session roots/config are not declared",
                ));
            }
        }
        let _sessions = self.sessions.try_lock().map_err(|_| {
            Fault::Reject(RpcError::new(
                "overloaded",
                "session mutation in flight on this app-server",
            ))
        })?;
        let _gate = self.slots[index]
            .admission
            .clone()
            .try_lock_owned()
            .map_err(|_| {
                Fault::Reject(RpcError::new(
                    "busy",
                    "native admission is still unresolved",
                ))
            })?;
        self.slots[index]
            .endpoint
            .lock()
            .unwrap()
            .may_switch(args)
            .map_err(Fault::Reject)?;
        let (method, mut params) = if action == "new" {
            let cwd = args.get("cwd").filter(|v| !v.is_null());
            if cwd.is_some_and(|v| v.as_str().is_none_or(|s| s.len() > 4096)) {
                return Err(Fault::invalid("cwd must be a bounded host path string"));
            }
            ("thread/start", json!({"cwd":cwd}))
        } else {
            let session = super::id(args, "session").map_err(Fault::Reject)?;
            if self.slots.iter().enumerate().any(|(i, s)| {
                i != index && s.endpoint.lock().unwrap().session.as_deref() == Some(&session)
            }) {
                return Err(Fault::Reject(RpcError::new(
                    "conflict",
                    "native thread is already bound to another endpoint",
                )));
            }
            ("thread/resume", json!({"threadId":session}))
        };
        if let Some(model) = self.slots[index]
            .endpoint
            .lock()
            .unwrap()
            .model_override()
            .map(str::to_owned)
        {
            params["model"] = json!(model);
        }
        let result = self
            .native
            .call_with_timeout(method, params, remaining(deadline))
            .await
            .map_err(Fault::Unknown)?;
        let mut endpoint = self.slots[index].endpoint.lock().unwrap();
        let next_epoch = unique_epoch(&self.incarnation);
        let changes = endpoint
            .set_session_response(&result, next_epoch)
            .map_err(Fault::Unknown)?;
        let session = endpoint.session_info();
        self.emit(index, &endpoint, changes, true);
        Ok(Outcome::Observed(Some(json!({"session":session}))))
    }
}

fn bounded_string(value: Option<&Value>, limit: usize) -> Option<String> {
    value
        .and_then(Value::as_str)
        .map(|s| s.chars().take(limit).collect())
}
fn remaining(deadline: Instant) -> Duration {
    deadline
        .saturating_duration_since(Instant::now())
        .min(Duration::from_secs(30))
}
fn unique_epoch(prefix: &str) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQUENCE: AtomicU64 = AtomicU64::new(1);
    format!("{prefix}:e{}", SEQUENCE.fetch_add(1, Ordering::Relaxed))
}
fn native_admission_error(method: &str, error: RpcError) -> Fault {
    // These exact no-input guard rejections are evidenced in the pinned native turn_steer path.
    // Other native errors, including timeouts/internal failures, remain uncertain.
    if method == "turn/steer"
        && error.code == -32600
        && (error.message.starts_with("expected active turn id")
            || error.message == "no active turn to steer")
    {
        Fault::Reject(RpcError::new("stale", error.message))
    } else {
        Fault::Unknown(error)
    }
}
