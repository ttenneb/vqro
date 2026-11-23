//! One backend-owned Claude child, native effects, and public provider SDK plumbing.
use super::{contract, state::State, wire::ClaudeCodec};
use crate::native_process::{Error as NativeError, Process};
use crate::provider_client::items::{ItemCursor, Next};
use crate::provider_client::{
    EffectRequest, Outcome, Provider, ProviderError, ProviderStream, Request,
};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, AtomicUsize, Ordering},
};
use std::time::Duration;
use tokio::sync::Notify;
use tokio::time::Instant;

pub struct Shared {
    pub state: Mutex<State>,
    process: Mutex<Option<Process>>,
    pub changed: Notify,
    outgoing: Notify,
    effects: tokio::sync::Mutex<()>,
    generation: AtomicU64,
    viewers: AtomicUsize,
    viewers_changed: Notify,
    stdout_done: std::sync::atomic::AtomicBool,
    background_probe: std::sync::atomic::AtomicBool,
    spawned: std::sync::atomic::AtomicBool,
    pub name: String,
    pub instance: String,
}
fn rejected(kind: &str, message: &str) -> Result<Outcome, ProviderError> {
    Ok(Outcome::Rejected {
        kind: kind.into(),
        message: message.into(),
    })
}
fn native_failure(error: NativeError) -> Result<Outcome, ProviderError> {
    if error.may_have_acted {
        Err(ProviderError::new("backend", error.to_string()))
    } else {
        rejected(error.kind, &error.message)
    }
}
fn uuid() -> Result<String, ProviderError> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes)
        .map_err(|_| ProviderError::new("backend", "cannot allocate native client identifier"))?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    ))
}
impl Shared {
    pub fn new(boot: String, name: String, instance: String) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(State::new(boot)),
            process: Mutex::new(None),
            changed: Notify::new(),
            outgoing: Notify::new(),
            effects: tokio::sync::Mutex::new(()),
            generation: AtomicU64::new(1),
            viewers: AtomicUsize::new(0),
            viewers_changed: Notify::new(),
            stdout_done: std::sync::atomic::AtomicBool::new(false),
            background_probe: std::sync::atomic::AtomicBool::new(false),
            spawned: std::sync::atomic::AtomicBool::new(false),
            name,
            instance,
        })
    }
    pub fn process(&self) -> Option<Process> {
        self.process.lock().unwrap().clone()
    }
    pub fn facts(&self) -> Value {
        self.state.lock().unwrap().facts(&self.name, &self.instance)
    }
    pub fn wake(&self) {
        self.changed.notify_waiters();
        self.outgoing.notify_one();
    }
    pub async fn spawn_native(
        self: &Arc<Self>,
        argv: &[String],
        cwd: &std::path::Path,
        env: &HashMap<String, String>,
    ) -> Result<Process, NativeError> {
        if self.spawned.swap(true, Ordering::SeqCst) {
            return Err(NativeError {
                kind: "conflict",
                message: "one owned native slot is already reserved".into(),
                may_have_acted: false,
            });
        }
        let weak = Arc::downgrade(self);
        let generation = self.generation.load(Ordering::SeqCst);
        let mut env = env.clone();
        env.remove("CLAUDE_CODE_RESUME_INTERRUPTED_TURN");
        env.remove("CLAUDE_CODE_RESUME_REASON");
        let process = Process::spawn(
            argv,
            cwd,
            &env,
            Arc::new(ClaudeCodec),
            Arc::new(move |event| {
                if let Some(shared) = weak.upgrade() {
                    if shared.generation.load(Ordering::SeqCst) != generation {
                        return;
                    }
                    shared.state.lock().unwrap().observe(&event);
                    shared.wake();
                }
            }),
        )
        .await?;
        *self.process.lock().unwrap() = Some(process.clone());
        Ok(process)
    }
    pub async fn initialize(self: Arc<Self>) {
        let Some(native) = self.process() else {
            return;
        };
        // No prompt, model request, transcript scan, custom system prompt or settings-source override.
        let result = native
            .call(
                "initialize",
                json!({"supportedDialogKinds":[]}),
                Duration::from_secs(15),
            )
            .await;
        let probe = {
            let mut state = self.state.lock().unwrap();
            match result {
                Ok(reply) if reply.success => state.initialized(&reply.data),
                _ => {
                    state.incomplete.insert("initialize".into());
                    state.ready = false;
                    state.refresh();
                }
            }
            state.literal_supported() && !self.background_probe.swap(true, Ordering::SeqCst)
        };
        self.wake();
        if probe {
            // The pinned native protocol emits a COMPLETE background level behind repeated
            // initialize, even when empty. Until that FRAME arrives, background remains unknown.
            // This is metadata/control only, never a user prompt or input replay.
            let _ = native
                .call(
                    "initialize",
                    json!({"supportedDialogKinds":[]}),
                    Duration::from_secs(15),
                )
                .await;
            self.wake();
        }
    }
    /// Unsupported native questions are cancelled once where that protocol has a cancellation response.
    /// A failed/unknown write is reported, never retried on the next wake or reconnect.
    pub async fn drain_outgoing(self: Arc<Self>) {
        loop {
            let changed = self.outgoing.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let response = self.state.lock().unwrap().outgoing.pop_front();
            if let Some(response) = response {
                let Some(native) = self.process() else {
                    break;
                };
                if native
                    .notify_guarded(
                        "control_response",
                        response.frame,
                        Instant::now() + Duration::from_secs(3),
                        response.token,
                    )
                    .await
                    .is_err()
                {
                    let mut state = self.state.lock().unwrap();
                    state.incomplete.insert("requests".into());
                    state.refresh();
                    drop(state);
                    self.changed.notify_waiters();
                }
            } else {
                changed.await;
            }
        }
    }
    pub async fn invoke(
        self: &Arc<Self>,
        provider: &Provider,
        effect: &EffectRequest,
    ) -> Result<Outcome, ProviderError> {
        {
            let state = self.state.lock().unwrap();
            if effect.r#ref.as_deref() != Some("agent") || effect.incarnation != state.incarnation()
            {
                return rejected("stale", "agent resource incarnation changed");
            }
            if let Err(e) = contract::common(&effect.args, &state) {
                return rejected(e.kind, e.message);
            }
        }
        if effect.op == "agent.receipt" {
            let delivery = match contract::id(&effect.args["delivery"]) {
                Ok(id) => id,
                Err(e) => return rejected(e.kind, e.message),
            };
            return Ok(Outcome::Observed(Some(
                self.state.lock().unwrap().receipt(delivery),
            )));
        }
        let _gate = match tokio::time::timeout_at(effect.deadline, self.effects.lock()).await {
            Ok(gate) => gate,
            Err(_) => return rejected("timeout", "deadline before native dispatch"),
        };
        let Some(native) = self.process() else {
            return rejected("unavailable", "native process is not present");
        };
        // Revalidate after queueing behind another effect, before any native write or request claim.
        {
            let state = self.state.lock().unwrap();
            if let Err(e) = contract::common(&effect.args, &state) {
                return rejected(e.kind, e.message);
            }
        }
        match effect.op.as_str() {
            "agent.send" => {
                let owner = provider
                    .call("workspace.get", json!({"key":"doc/principals/owner"}))
                    .await
                    .unwrap_or(Value::Null);
                let uuid = match uuid() {
                    Ok(id) => id,
                    Err(_) => return rejected("backend", "native client identifier unavailable"),
                };
                let prepared = {
                    let mut state = self.state.lock().unwrap();
                    let prepared = match contract::send(
                        &effect.args,
                        &state,
                        &effect.attribution,
                        &owner,
                        uuid,
                    ) {
                        Ok(p) => p,
                        Err(e) => return rejected(e.kind, e.message),
                    };
                    state.availability.begin_turn();
                    state.inputs.insert(
                        prepared.uuid.clone(),
                        (prepared.delivery.clone(), prepared.epoch.clone()),
                    );
                    state.save_receipt(&prepared.delivery, prepared.receipt.clone());
                    prepared
                };
                self.wake();
                let result = native
                    .notify("user", prepared.native, effect.deadline)
                    .await;
                let mut state = self.state.lock().unwrap();
                let mut receipt = state.receipt(&prepared.delivery);
                match &result {
                    Ok(()) => {
                        if receipt["stage"] == "unknown" {
                            receipt["stage"] = json!("dispatched");
                        }
                    }
                    Err(e) if !e.may_have_acted => {
                        receipt["stage"] = json!("rejected");
                        state.inputs.remove(&prepared.uuid);
                    }
                    Err(_) => {}
                }
                // Native echo/result can win the write-completion race. Never downgrade it or rewrite
                // the internally frozen origin/may_transform snapshot after ACK or an epoch change.
                state.save_receipt(&prepared.delivery, receipt.clone());
                drop(state);
                self.wake();
                match result {
                    Ok(()) => Ok(Outcome::Dispatched(Some(json!({"receipt":receipt})))),
                    Err(error) => native_failure(error),
                }
            }
            "agent.respond" => {
                if effect.args.get("epoch").is_none() {
                    return rejected("invalid", "request observation epoch is required");
                }
                let request = match contract::id(&effect.args["request"]) {
                    Ok(id) => id.to_owned(),
                    Err(e) => return rejected(e.kind, e.message),
                };
                let Some(revision) = effect.args["revision"].as_u64() else {
                    return rejected("invalid", "request revision is required");
                };
                let (response, token) = {
                    let mut state = self.state.lock().unwrap();
                    let response =
                        match state
                            .requests
                            .answer(&request, revision, &effect.args["response"])
                        {
                            Ok(r) => r,
                            Err(e) => return rejected(e.kind, e.message),
                        };
                    let token = state
                        .requests
                        .answer_token(&request)
                        .expect("claimed native request has token");
                    (response, token)
                };
                let result = native
                    .notify_guarded("control_response", response, effect.deadline, token)
                    .await;
                let mut state = self.state.lock().unwrap();
                match &result {
                    Err(e) if !e.may_have_acted => state.requests.not_written(&request),
                    result => {
                        if let Some(change) = state.requests.dispatched(&request, result.is_ok()) {
                            state.put("request", &change.id, change.value);
                        }
                    }
                }
                state.refresh();
                drop(state);
                self.wake();
                match result {
                    Ok(()) => Ok(Outcome::Dispatched(None)),
                    Err(error) => native_failure(error),
                }
            }
            "agent.interrupt" => {
                let remaining = effect.deadline.saturating_duration_since(Instant::now());
                let result = native.call("interrupt", json!({}), remaining).await;
                match result {
                    Ok(reply) if reply.success => {
                        let mut data = json!({"pending":"unknown"});
                        for field in ["still_queued", "cancelled"] {
                            if let Some(value) = reply.data.get(field) {
                                if crate::limits::encoded_bytes(value) <= 8192 {
                                    data[field] = value.clone();
                                }
                            }
                        }
                        Ok(Outcome::Accepted(Some(data)))
                    }
                    Ok(_) => Err(ProviderError::new(
                        "backend",
                        "native interrupt returned an error; outcome not upgraded to no-effect",
                    )),
                    Err(error) => native_failure(error),
                }
            }
            _ => rejected("unsupported", "agent operation is not declared"),
        }
    }
    pub fn open(self: &Arc<Self>, open: crate::provider_client::OpenRequest) {
        let valid = {
            let state = self.state.lock().unwrap();
            open.stream.r#ref == "agent"
                && open.stream.incarnation == state.incarnation()
                && open.items.is_some()
        };
        if !valid {
            open.respond(Err(ProviderError::new(
                "unsupported",
                "one agent item channel only",
            )));
            return;
        }
        let cursor = self
            .state
            .lock()
            .unwrap()
            .journal
            .open(open.items.as_ref().and_then(|i| i.get("after")));
        if let Some(stream) = open.respond_items(cursor.reset(), cursor.epoch()) {
            self.viewers.fetch_add(1, Ordering::SeqCst);
            let shared = self.clone();
            tokio::spawn(async move {
                shared.viewer(stream, cursor).await;
            });
        }
    }
    async fn viewer(self: Arc<Self>, mut stream: ProviderStream, mut cursor: ItemCursor) {
        struct Guard(Arc<Shared>);
        impl Drop for Guard {
            fn drop(&mut self) {
                self.0.viewers.fetch_sub(1, Ordering::SeqCst);
                self.0.viewers_changed.notify_waiters();
            }
        }
        let _guard = Guard(self.clone());
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let next = {
                let mut state = self.state.lock().unwrap();
                cursor.next(&mut state.journal, 8192)
            };
            match next {
                Next::Data(data) => {
                    if stream.send(&data).await.is_err() {
                        break;
                    }
                }
                Next::Gap(reason) => {
                    let _ = stream.gap(reason).await;
                    break;
                }
                Next::Idle => {
                    if self.stdout_done.load(Ordering::SeqCst) {
                        let kind = if self
                            .state
                            .lock()
                            .unwrap()
                            .incomplete
                            .contains("native_tail")
                        {
                            "native_tail_unconfirmed"
                        } else {
                            "native_reader_closed"
                        };
                        let _ = stream.close(json!({"reason":kind})).await;
                        break;
                    }
                    tokio::select! {_=changed=>{},command=stream.next()=>match command{
                        None|Some(crate::provider_client::StreamCommand::Close)=>break,
                        _=>{let _=stream.error(&ProviderError::new("unsupported","agent item stream is read-only")).await;break;}
                    }}
                }
            }
        }
    }
    pub async fn publish(self: Arc<Self>, provider: Provider) {
        let mut published = 0;
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let (revision, facts) = {
                let state = self.state.lock().unwrap();
                (state.changed, state.facts(&self.name, &self.instance))
            };
            if revision != published {
                if provider.upsert(facts).await.is_err() {
                    break;
                }
                published = revision;
            }
            changed.await;
        }
    }
    pub async fn finish(self: &Arc<Self>, provider: &Provider) {
        if let Some(native) = self.process() {
            if !matches!(
                tokio::time::timeout(Duration::from_secs(2), native.drained()).await,
                Ok(crate::native_process::Drain::Eof)
            ) {
                self.state
                    .lock()
                    .unwrap()
                    .incomplete
                    .insert("native_tail".into());
            }
        }
        self.stdout_done.store(true, Ordering::SeqCst);
        self.wake();
        let wait = async {
            loop {
                let changed = self.viewers_changed.notified();
                tokio::pin!(changed);
                changed.as_mut().enable();
                if self.viewers.load(Ordering::SeqCst) == 0 {
                    break;
                }
                changed.await;
            }
        };
        if tokio::time::timeout(Duration::from_secs(2), wait)
            .await
            .is_err()
        {
            self.state
                .lock()
                .unwrap()
                .incomplete
                .insert("item_stream_tail".into());
        }
        {
            let mut state = self.state.lock().unwrap();
            if state.exit.is_some() {
                state.end_ready = true;
            } else {
                state.incomplete.insert("native_exit".into());
            }
            state.refresh();
        }
        self.wake();
        // Only now may either publisher announce ended, after the native and item-stream tail cut.
        // The final upsert is queued after completed item-stream sends by the SDK writer.
        let _ = provider.upsert(self.facts()).await;
    }
    pub async fn serve_request(self: &Arc<Self>, provider: &Provider, request: Request) {
        match request {
            Request::Effect(effect) => {
                let shared = self.clone();
                let provider = provider.clone();
                tokio::spawn(async move {
                    let result = shared.invoke(&provider, &effect).await;
                    effect.respond(result);
                });
            }
            Request::Open(open) => self.open(open),
        }
    }
}
