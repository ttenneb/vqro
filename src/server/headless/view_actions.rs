//! Explicit, bounded caller proof and SESSION action admission; no render I/O.
#[cfg(test)]
mod tests;
use super::*;
use crate::view_action::{Binding, PrepareParams, Prepared};
use serde_json::{json, Value};
use std::sync::atomic::Ordering;

#[derive(Debug)]
pub(crate) struct ProofReady {
    client_id: u64,
    nonce: String,
    job: String,
    binding: Binding,
    previous: Option<Prepared>,
    request_id: String,
    respond_to: std::sync::mpsc::Sender<String>,
    result: Result<Proved, String>,
    deadline: Instant,
    slot: crate::extension_service::ExtensionWorkSlotGuard,
}
#[derive(Debug)]
struct Proved {
    caller: crate::action_invocation::InvocationCaller,
    identity: crate::client_invocation::ClientIdentity,
    ownership: crate::pty_ownership::OwnershipProof,
    peer: crate::action_invocation::InvocationPeer,
}
fn error(id: &str, message: &str) -> String {
    let code = if message.starts_with("stale:") {
        "view_action_stale"
    } else {
        "view_action_rejected"
    };
    json!({"id":id,"error":{"code":code,"message":message}}).to_string()
}
fn current_evidence(
    attached: &crate::client_invocation::ClientAttachEvidence,
) -> Result<crate::client_invocation::ClientAttachEvidence, String> {
    let first = attached
        .peer
        .ancestry
        .first()
        .ok_or("missing attach identity")?;
    let peer = crate::platform::invocation_peer_for_process(first.pid)
        .ok_or("client process is unavailable")?;
    if peer.ancestry.first().map(|value| (value.pid, value.birth)) != Some((first.pid, first.birth))
    {
        return Err("client incarnation changed".into());
    }
    let tty = crate::platform::client_controlling_tty(first.pid)
        .filter(|(_, birth)| *birth == first.birth)
        .map(|(tty, _)| tty);
    if !crate::platform::invocation_peer_is_current(&peer) {
        return Err("client process changed during TTY capture".into());
    }
    Ok(crate::client_invocation::ClientAttachEvidence {
        complete_ancestry: crate::platform::client_ancestry_complete(&peer),
        peer,
        tty,
    })
}
fn audit(
    request: &str,
    client: u64,
    boot: &str,
    binding: &Binding,
    caller: &crate::action_invocation::InvocationCaller,
    outcome: &str,
) -> Value {
    json!({"event":"view.action.authority.v1","request_id":request,"client_id":client,"boot_id":boot,"caller":caller,
        "namespace":binding.selection.namespace,"scope_id":binding.selection.scope_id,"consumer_pane_id":binding.consumer.pane_id,"terminal_scope":binding.consumer.scope,"document_generation":binding.selection.generation,
        "document_revision":binding.selection.revision,"item_id":binding.selection.item_id,"subject":binding.affordance.subject(),
        "option":binding.selection.option,"operation_id":binding.selection.operation_id,"service_id":binding.service_id,
        "service_generation":binding.service_generation,"outcome":outcome})
}
fn submitted_response(
    id: &str,
    binding: &Binding,
    caller: &crate::action_invocation::InvocationCaller,
    provider: Value,
    audit_warning: Option<String>,
) -> String {
    json!({"id":id,"result":{"type":"host_view_action_submitted","operation_id":binding.selection.operation_id,"caller":caller,"provider_response":provider,"audit_warning":audit_warning}}).to_string()
}

impl HeadlessServer {
    pub(super) fn prune_prepared_view_actions(&mut self) {
        let now = Instant::now();
        for client in self.clients.values_mut() {
            let stale = client
                .view_actions
                .prepared
                .as_ref()
                .is_some_and(|prepared| {
                    let selection = &prepared.binding.selection;
                    now >= prepared.expires
                        || self
                            .app
                            .host_document_caches
                            .get(&(selection.namespace.clone(), selection.scope_id.clone()))
                            .and_then(crate::host_document_cache::HostDocumentCache::current)
                            .is_none_or(|document| {
                                (document.generation, document.revision)
                                    != (selection.generation, selection.revision)
                            })
                        || self
                            .app
                            .host_document_services
                            .generation(&selection.namespace)
                            != Some(selection.generation)
                        || self
                            .app
                            .extension_services
                            .action_generation(&prepared.binding.service_id)
                            != Some(prepared.binding.service_generation)
                        || self
                            .app
                            .host_document_request_watermarks
                            .get(&(selection.namespace.clone(), selection.scope_id.clone()))
                            .is_some_and(|version| {
                                *version != (selection.generation, selection.revision)
                            })
                });
            if stale {
                client.view_actions.prepared = None;
            }
        }
    }

    fn view_action_client(&self, origin: &api::ApiOrigin) -> Result<u64, String> {
        let id = match origin {
            api::ApiOrigin::NativeEndpoint { client_id, boot_id }
                if boot_id == &self.client_shell_boot_id =>
            {
                *client_id
            }
            api::ApiOrigin::LocalSocketPeer(peer) => {
                let first = peer.ancestry.first().ok_or("no peer incarnation")?;
                let mut matches = self
                    .clients
                    .iter()
                    .filter(|(_, client)| {
                        client.invocation.as_ref().is_some_and(|attached| {
                            attached.peer.account == peer.account
                                && attached.peer.boot_id == peer.boot_id
                                && attached.peer.ancestry.first().is_some_and(|value| {
                                    (value.pid, value.birth) == (first.pid, first.birth)
                                })
                        })
                    })
                    .map(|(id, _)| *id);
                let id = matches
                    .next()
                    .ok_or("use the connected outer TUI client or the existing Tasks CLI")?;
                if matches.next().is_some() {
                    return Err("peer matches multiple client connections".into());
                }
                id
            }
            _ => return Err("client provenance is unavailable; use the Tasks CLI".into()),
        };
        let client = self.clients.get(&id).ok_or("client disconnected")?;
        if client.view_actions.nonce.is_empty()
            || !client.shell_host_views_enabled
            || !client.shell_host_documents_enabled
        {
            return Err("client view actions unavailable; use the Tasks CLI".into());
        }
        Ok(id)
    }

    fn view_action_consumer(
        &self,
        client_id: u64,
        selection: &PrepareParams,
        view_id: &str,
    ) -> Result<crate::protocol::PaneSurfaceViewEntryV2, String> {
        let client = self.clients.get(&client_id).ok_or("client disconnected")?;
        let accepted = client
            .render_state
            .committed_view(&selection.consumer_pane_id)
            .ok_or("stale: this client has no committed placed consumer")?;
        let tab_id = client
            .shell_location
            .as_ref()
            .and_then(crate::server::clients::ClientShellLocation::focused_tab_id)
            .ok_or("stale: exact client location unavailable")?;
        let (workspace_index, pane) = self
            .app
            .parse_pane_id(&selection.consumer_pane_id)
            .ok_or("stale: view consumer closed")?;
        let workspace = &self.app.state.workspaces[workspace_index];
        let tab_index = workspace
            .find_tab_index_for_pane(pane)
            .ok_or("stale: view consumer moved")?;
        if self
            .app
            .public_tab_id(workspace_index, tab_index)
            .as_deref()
            != Some(tab_id)
        {
            return Err("stale: view consumer is outside this client's current tab".into());
        }
        let tab = &workspace.tabs[tab_index];
        let content = tab
            .layout
            .view_at(pane)
            .ok_or("stale: consumer is no longer a placed view")?;
        let scope = tab
            .layout
            .view_scope(pane)
            .into_iter()
            .map(|terminal| self.app.public_pane_id(workspace_index, terminal))
            .collect::<Option<Vec<_>>>()
            .ok_or("stale: terminal scope is unavailable")?;
        if accepted.extension != selection.namespace
            || accepted.view != view_id
            || content.extension != accepted.extension
            || content.view != accepted.view
            || accepted.scope != selection.terminal_scope
            || scope != accepted.scope
            || !scope.contains(&selection.pane_id)
        {
            return Err(
                "stale: placed consumer/view/full terminal scope changed or excludes this root"
                    .into(),
            );
        }
        Ok(accepted.clone())
    }

    fn check_view_action_admission(&self, deadline: Instant) -> Result<(), String> {
        if self.shutting_down
            || self.host_shutdown_requested()
            || self.should_quit.load(Ordering::Acquire)
        {
            return Err("server is stopping; dispatch not started".into());
        }
        if Instant::now() >= deadline {
            return Err("fresh proof expired before admission; dispatch not started".into());
        }
        Ok(())
    }

    fn view_action_binding(
        &self,
        client_id: u64,
        selection: &PrepareParams,
    ) -> Result<Binding, String> {
        selection.validate()?;
        let client = self.clients.get(&client_id).ok_or("client disconnected")?;
        let current = self
            .app
            .host_document_caches
            .get(&(selection.namespace.clone(), selection.scope_id.clone()))
            .and_then(crate::host_document_cache::HostDocumentCache::current)
            .ok_or("stale: document unavailable")?;
        if current.target != crate::extension_contract::HostTarget::Session
            || current.contract != crate::extension_contract::HOST_VIEW_CONTRACT_V1
            || (current.generation, current.revision) != (selection.generation, selection.revision)
            || self
                .app
                .host_document_services
                .generation(&selection.namespace)
                != Some(selection.generation)
            || self
                .app
                .host_document_request_watermarks
                .get(&(selection.namespace.clone(), selection.scope_id.clone()))
                .is_some_and(|version| *version != (selection.generation, selection.revision))
            || self
                .app
                .host_document_services
                .current_revision(&selection.namespace, &selection.scope_id)
                .is_some_and(|version| version != (selection.generation, selection.revision))
        {
            return Err("stale: document identity or publisher changed; review again".into());
        }
        if !client
            .shell_host_documents
            .iter()
            .any(|entry| &entry.document == current)
        {
            return Err("stale: this client has not accepted the current document".into());
        }
        let item = current
            .items
            .iter()
            .find(|item| item.id == selection.item_id)
            .ok_or("stale: item disappeared")?;
        let mut ancestor = item;
        for depth in 0..=crate::extension_contract::MAX_DOCUMENT_ITEMS {
            if depth > 16 || ancestor.flags.disabled {
                return Err("stale: item hierarchy is disabled or unavailable".into());
            }
            match ancestor.parent_id.as_ref() {
                Some(parent) => {
                    ancestor = current
                        .items
                        .iter()
                        .find(|item| &item.id == parent)
                        .ok_or("stale: item parent disappeared")?
                }
                None => break,
            }
        }
        if ancestor
            .view
            .as_ref()
            .and_then(|view| view.pane_id.as_deref())
            != Some(selection.pane_id.as_str())
        {
            return Err("view action source pane does not match its document root".into());
        }
        let view_id = ancestor
            .view
            .as_ref()
            .and_then(|view| view.view_id.as_deref())
            .ok_or("stale: root has no view identity")?;
        let consumer = self.view_action_consumer(client_id, selection, view_id)?;
        let pane_exists = self
            .app
            .state
            .workspaces
            .iter()
            .enumerate()
            .any(|(index, workspace)| {
                workspace.tabs.iter().any(|tab| {
                    tab.panes.keys().any(|pane| {
                        self.app.public_pane_id(index, *pane).as_deref()
                            == Some(selection.pane_id.as_str())
                    })
                })
            });
        if !pane_exists {
            return Err("stale: source pane closed".into());
        }
        let raw = item
            .view
            .as_ref()
            .and_then(|view| view.affordance.as_ref())
            .ok_or("item is read-only")?;
        let affordance = crate::view_action::Affordance::parse(raw)?;
        affordance.select(selection.option.as_deref())?;
        let service_id = self
            .app
            .host_document_services
            .action_service(&selection.namespace)
            .ok_or("document publisher has no bound action provider")?
            .to_owned();
        let service_generation = self
            .app
            .extension_services
            .action_generation(&service_id)
            .ok_or("action provider disabled, replaced or missing action_invoke grant")?;
        Ok(Binding {
            selection: selection.clone(),
            service_id,
            service_generation,
            consumer,
            affordance,
        })
    }

    pub(super) fn start_view_action(&mut self, msg: api::ApiRequestMessage) {
        let request_id = msg.request.id.clone();
        let respond = msg.respond_to.clone();
        if let Err(message) = self.start_view_action_inner(msg) {
            let _ = respond.send(error(&request_id, &message));
        }
    }
    fn start_view_action_inner(&mut self, msg: api::ApiRequestMessage) -> Result<(), String> {
        self.prune_prepared_view_actions();
        let client_id = self.view_action_client(&msg.origin)?;
        let (selection, previous) = match msg.request.method {
            api::schema::Method::HostViewActionPrepare(selection) => {
                if self
                    .clients
                    .values()
                    .filter(|client| {
                        client.view_actions.prepared.is_some() || client.view_actions.busy.is_some()
                    })
                    .count()
                    >= 64
                {
                    return Err("view-action preparation capacity reached".into());
                }
                (selection, None)
            }
            api::schema::Method::HostViewActionSubmit(submit) => {
                let client = self
                    .clients
                    .get_mut(&client_id)
                    .ok_or("client disconnected")?;
                let prepared = client
                    .view_actions
                    .prepared
                    .as_ref()
                    .ok_or("preparation expired, consumed or belongs to another client")?;
                prepared.check(&submit, Instant::now())?;
                if submit.cancel {
                    client.view_actions.prepared = None;
                    let _ = msg.respond_to.send(json!({"id":msg.request.id,"result":{"type":"host_view_action_canceled","canceled":true}}).to_string());
                    return Ok(());
                }
                let prepared = client
                    .view_actions
                    .prepared
                    .take()
                    .ok_or("preparation consumed")?;
                (prepared.binding.selection.clone(), Some(prepared))
            }
            _ => return Err("not a view-action request".into()),
        };
        let binding = self.view_action_binding(client_id, &selection)?;
        if previous
            .as_ref()
            .is_some_and(|prepared| prepared.binding != binding)
        {
            return Err("stale: prepared choice/provider changed".into());
        }
        let slot = crate::extension_service::ExtensionWorkSlotGuard::acquire()?;
        let job = crate::view_action::token()?;
        let client = self
            .clients
            .get_mut(&client_id)
            .ok_or("client disconnected")?;
        if client.view_actions.busy.is_some() {
            return Err("client action is already in flight".into());
        }
        let attached = client
            .invocation
            .clone()
            .ok_or("client has no kernel attach evidence")?;
        let nonce = client.view_actions.nonce.clone();
        let canceled = client.view_actions.canceled.clone();
        client.view_actions.prepared = None;
        client.view_actions.busy = Some(job.clone());
        let owned = self.app.client_peer_in_owned_pane(&attached.peer);
        let boot = self.client_shell_boot_id.clone();
        let events = self.server_event_tx.clone();
        let error_job = job.clone();
        let deadline = Instant::now() + Duration::from_secs(3);
        let spawned = crate::thread_spawn::spawn_named("vqro-view-action-proof", move || {
            let result = (|| {
                if canceled.load(Ordering::Acquire) {
                    return Err("client disconnected".into());
                }
                let current = current_evidence(&attached)?;
                let observed = crate::pty_ownership::observation::observe(deadline)?;
                let identity = crate::client_invocation::classify_external(
                    &current, &attached, owned, &observed,
                )?;
                if previous
                    .as_ref()
                    .is_some_and(|prepared| prepared.identity != identity)
                {
                    return Err("prepared caller incarnation/TTY changed".into());
                }
                let caller = crate::action_invocation::InvocationCaller::ClientHumanUnverified {
                    account: current.peer.account.clone(),
                    client: identity.clone(),
                };
                crate::logging::checked_action_audit(&audit(
                    &msg.request.id,
                    client_id,
                    &boot,
                    &binding,
                    &caller,
                    "dispatch_not_started",
                ))
                .map_err(|error| format!("checked audit failed; no dispatch: {error}"))?;
                observed.recheck_after_audit(deadline)?;
                let after = current_evidence(&attached)?;
                if crate::client_invocation::classify_external(&after, &attached, owned, &observed)?
                    != identity
                    || canceled.load(Ordering::Acquire)
                    || Instant::now() >= deadline
                {
                    return Err("client/ownership proof changed or expired after audit".into());
                }
                Ok(Proved {
                    caller,
                    identity,
                    ownership: observed.proof,
                    peer: after.peer,
                })
            })();
            let ready = ProofReady {
                client_id,
                nonce,
                job,
                binding,
                previous,
                request_id: msg.request.id,
                respond_to: msg.respond_to,
                result,
                deadline,
                slot,
            };
            if let Err(failure) =
                events.blocking_send(ServerEvent::ViewActionProofReady(Box::new(ready)))
            {
                if let ServerEvent::ViewActionProofReady(ready) = failure.0 {
                    let _ = ready.respond_to.send(error(
                        &ready.request_id,
                        "server retired before action admission",
                    ));
                }
            }
        });
        if let Err(error) = spawned {
            if let Some(client) = self.clients.get_mut(&client_id) {
                if client.view_actions.busy.as_deref() == Some(&error_job) {
                    client.view_actions.busy = None;
                }
            }
            return Err(format!("action proof worker unavailable: {error}"));
        }
        Ok(())
    }

    pub(super) fn finish_view_action_proof(&mut self, ready: ProofReady) {
        let respond = ready.respond_to.clone();
        let id = ready.request_id.clone();
        if let Err(message) = self.finish_view_action_inner(ready) {
            let _ = respond.send(error(&id, &message));
        }
    }
    fn finish_view_action_inner(&mut self, ready: ProofReady) -> Result<(), String> {
        let client = self
            .clients
            .get_mut(&ready.client_id)
            .ok_or("client disconnected before admission")?;
        if client.view_actions.nonce != ready.nonce
            || client.view_actions.busy.as_deref() != Some(ready.job.as_str())
        {
            return Err("client connection/preparation changed".into());
        }
        client.view_actions.busy = None;
        self.check_view_action_admission(ready.deadline)?;
        let proved = ready.result?;
        if ready
            .previous
            .as_ref()
            .is_some_and(|prepared| Instant::now() >= prepared.expires)
        {
            return Err("preparation expired during proof; review again".into());
        }
        if self.app.client_peer_in_owned_pane(&proved.peer) {
            return Err("client moved into an owned pane".into());
        }
        let current = self.view_action_binding(ready.client_id, &ready.binding.selection)?;
        if current != ready.binding {
            return Err("stale: action/provider/document changed after proof".into());
        }
        let local = crate::pty_ownership::publisher::current_status()
            .ok_or("local ownership unavailable")?;
        if !proved.ownership.matches_local(&local) {
            return Err("local ownership epoch changed before admission".into());
        }
        self.check_view_action_admission(ready.deadline)?;
        let client = self
            .clients
            .get_mut(&ready.client_id)
            .ok_or("client disconnected")?;
        if ready.previous.is_none() {
            let prepared = Prepared::new(current, proved.identity)?;
            let response = json!({"id":ready.request_id,"result":{"type":"host_view_action_prepared","token":prepared.token,"confirmation_token":prepared.confirmation,"requires_confirmation":prepared.confirmation.is_some(),"expires_in_ms":60000,"operation_id":prepared.binding.selection.operation_id}}).to_string();
            client.view_actions.prepared = Some(prepared);
            let _ = ready.respond_to.send(response);
            return Ok(());
        }
        // A consumed token remains busy until the provider's bounded response.
        // Teardown cancels queued/pre-dispatch work; it cannot undo an effect.
        client.view_actions.busy = Some(ready.job.clone());
        let canceled = client.view_actions.canceled.clone();
        let (tx, rx) = std::sync::mpsc::channel::<String>();
        let respond = ready.respond_to.clone();
        let request_id = ready.request_id.clone();
        let binding = ready.binding.clone();
        let caller = proved.caller.clone();
        let boot = self.client_shell_boot_id.clone();
        let client_id = ready.client_id;
        let nonce = ready.nonce.clone();
        let job = ready.job.clone();
        let events = self.server_event_tx.clone();
        let slot = ready.slot;
        let relay = crate::thread_spawn::spawn_named("vqro-view-action-result", move || {
            let _slot = slot;
            let provider = rx.recv_timeout(Duration::from_secs(6)).ok().and_then(|value| serde_json::from_str::<Value>(&value).ok()).unwrap_or_else(||json!({"error":{"code":"outcome_unknown","message":"provider result unavailable; inspect the original operation"}}));
            let outcome = if provider.get("result").is_some() {
                "provider_result"
            } else if provider["error"]["code"] == "provider_error" {
                "provider_reply"
            } else if provider["error"]["code"] == "action_rejected" {
                "dispatch_not_started"
            } else {
                "outcome_unknown"
            };
            let mut record = audit(&request_id, client_id, &boot, &binding, &caller, outcome);
            record["provider_response"] = provider.clone();
            let audit_warning = crate::logging::checked_action_audit(&record)
                .err()
                .map(|error| error.to_string());
            // Never turn a known provider reply into a rejection/unknown merely
            // because the later checked audit failed. Preserve exact correlation.
            let response =
                submitted_response(&request_id, &binding, &caller, provider, audit_warning);
            let _ = respond.send(response);
            let _ = events.blocking_send(ServerEvent::ViewActionFinished {
                client_id,
                nonce,
                job,
            });
        });
        if let Err(error) = relay {
            client.view_actions.busy = None;
            return Err(format!(
                "result worker unavailable; dispatch not started: {error}"
            ));
        }
        let mut payload = json!({"action":"view_action","subject":current.affordance.subject(),"operation_id":current.selection.operation_id});
        if let Some(option) = current.selection.option {
            payload["option"] = json!(option);
        }
        // Last age/stop and local epoch fences immediately precede the queue.
        if let Err(message) = self.check_view_action_admission(ready.deadline) {
            let _ =
                tx.send(json!({"error":{"code":"action_rejected","message":message}}).to_string());
            return Ok(());
        }
        if !crate::pty_ownership::publisher::current_status()
            .is_some_and(|status| proved.ownership.matches_local(&status))
        {
            let _ = tx.send(json!({"error":{"code":"action_rejected","message":"local ownership changed at final admission"}}).to_string());
            return Ok(());
        }
        self.app.extension_services.request_client_action(
            ready.request_id,
            api::schema::ExtensionActionInvokeParams {
                service_id: current.service_id,
                payload,
            },
            proved.caller,
            tx,
            current.service_generation,
            canceled,
        );
        Ok(())
    }
}
