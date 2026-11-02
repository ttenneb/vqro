use std::time::{Duration, Instant};

use super::{App, SESSION_SAVE_DEBOUNCE};

enum SessionSaveJob {
    Clear,
    Save {
        snapshot: Box<crate::persist::SessionSnapshot>,
        history: Option<crate::persist::SessionHistorySnapshot>,
    },
}

impl App {
    pub(super) fn schedule_session_save(&mut self) {
        if self.policy.persist_session && !self.session_persistence_blocked {
            self.pane_exit_checkpoint_pending = false;
            self.session_save_deadline = Some(Instant::now() + SESSION_SAVE_DEBOUNCE);
        }
    }

    pub(crate) fn sync_session_save_schedule(&mut self) {
        if self.state.session_dirty {
            self.state.session_dirty = false;
            self.schedule_session_save();
        }
    }

    fn reap_finished_session_save(&mut self) {
        if self
            .session_save_thread
            .as_ref()
            .is_some_and(std::thread::JoinHandle::is_finished)
        {
            if let Some(thread) = self.session_save_thread.take() {
                let _ = thread.join();
            }
            if let Err(error) = self.maybe_cutover_collections_authority() {
                tracing::warn!(%error, "deferred Collections authority cutover failed");
            }
        }
    }

    pub(crate) fn capture_session_snapshot(&self) -> crate::persist::SessionSnapshot {
        let mut snapshot = crate::persist::capture(
            &self.state.workspaces,
            &self.state.repositories,
            &self.state.space_order,
            &self.state.terminals,
            &self.terminal_runtimes,
            self.state.active,
            self.state.selected,
        );
        snapshot.delegations = self
            .state
            .delegations
            .preorder()
            .into_iter()
            .filter_map(|id| self.state.delegations.get(id).cloned())
            .collect();
        snapshot.collections_authority = self.collections_authority.clone();
        // A previously durable high-water never decreases, even if authority
        // is currently legacy/unavailable. Session v8 can always represent it.
        snapshot.collections_action_sequence = self.next_collections_action_id;
        snapshot.version = crate::persist::SNAPSHOT_VERSION;
        snapshot
    }

    fn capture_session_save_job(&self) -> SessionSaveJob {
        if self.state.workspaces.is_empty() {
            SessionSaveJob::Clear
        } else {
            let history = self.persist_pane_history.then(|| {
                crate::persist::capture_history(&self.state.workspaces, &self.terminal_runtimes)
            });
            SessionSaveJob::Save {
                snapshot: Box::new(self.capture_session_snapshot()),
                history,
            }
        }
    }

    pub(crate) fn start_background_session_save(&mut self) {
        if !self.policy.persist_session || self.session_persistence_blocked {
            self.session_save_deadline = None;
            return;
        }
        if self.collections_mutations_blocked {
            self.session_save_deadline = Some(Instant::now() + Duration::from_millis(250));
            return;
        }

        self.reap_finished_session_save();
        if self.session_save_thread.is_some() {
            self.session_save_deadline = Some(Instant::now() + Duration::from_millis(250));
            return;
        }

        let job = self.capture_session_save_job();
        self.pane_exit_checkpoint_pending = false;
        self.session_save_deadline = None;
        match std::thread::Builder::new()
            .name("herdr-session-save".into())
            .spawn(move || run_session_save_job(job))
        {
            Ok(thread) => self.session_save_thread = Some(thread),
            Err(err) => {
                tracing::warn!(err = %err, "failed to spawn session save thread; saving inline");
                run_session_save_job(self.capture_session_save_job());
            }
        }
    }

    pub(crate) fn save_session_now(&mut self) {
        if let Some(thread) = self.session_save_thread.take() {
            let _ = thread.join();
        }

        if !self.policy.persist_session || self.session_persistence_blocked {
            self.session_save_deadline = None;
            return;
        }
        if self.collections_mutations_blocked {
            self.session_save_deadline = Some(Instant::now() + Duration::from_millis(250));
            return;
        }

        run_session_save_job(self.capture_session_save_job());
        self.pane_exit_checkpoint_pending = false;
        self.session_save_deadline = None;
    }

    pub(crate) fn checkpoint_session_before_pane_exit(&mut self) {
        if !self.policy.persist_session
            || self.session_persistence_blocked
            || (self.pane_exit_checkpoint_pending && !self.state.session_dirty)
        {
            return;
        }
        self.save_session_now();
        self.pane_exit_checkpoint_pending = true;
        self.state.session_dirty = false;
    }

    pub(crate) fn finish_checkpointed_pane_exit(&mut self) {
        if self.pane_exit_checkpoint_pending {
            self.state.session_dirty = false;
            self.session_save_deadline = Some(Instant::now() + SESSION_SAVE_DEBOUNCE);
        }
    }

    /// Only durable completion events are meaningful after shutdown admission.
    /// Never apply queued PTY deaths, agent exits or new package plans here.
    pub(crate) fn handle_shutdown_persistence_event(&mut self, event: crate::events::AppEvent) {
        match event {
            crate::events::AppEvent::CollectionsCheckpointPersisted(result) => {
                self.finish_collections_checkpoint(*result)
            }
            crate::events::AppEvent::CollectionsCreateCheckpointPersisted(result) => {
                self.finish_collections_create_checkpoint(*result)
            }
            _ => return,
        }
        if !self.collections_mutations_blocked && !self.session_persistence_blocked {
            self.pane_exit_checkpoint_pending = false;
        }
    }

    pub(crate) async fn settle_persistence_before_shutdown(
        &mut self,
        timeout: Duration,
    ) -> Result<(), &'static str> {
        let canceled_plans = self.cancel_prepublication_collections_for_shutdown();
        let awaiting_checkpoint = self.collections_mutations_blocked;
        let deadline = tokio::time::Instant::now() + timeout;
        while self.collections_mutations_blocked {
            if tokio::time::Instant::now() >= deadline {
                break;
            }
            match tokio::time::timeout_at(deadline, self.event_rx.recv()).await {
                Ok(Some(event)) => self.handle_shutdown_persistence_event(event),
                _ => break,
            }
        }
        if self.collections_mutations_blocked {
            // Publication may already have happened. Retain journal/evidence and
            // both fences for startup recovery; never race a forced ordinary save.
            self.session_persistence_blocked = true;
            self.extension_state_persistence_blocked = true;
            return Err("shutdown persistence completion unavailable; final ordinary save blocked, durable checkpoint recovery required");
        }
        if awaiting_checkpoint && self.session_persistence_blocked {
            return Err("shutdown checkpoint failed; final ordinary save blocked, durable recovery required");
        }
        if canceled_plans || awaiting_checkpoint {
            // An admitted mutation supersedes an older pre-exit checkpoint.
            // Capture current resume/cwd facts after publication reconciliation,
            // even when the checkpoint finisher cleared session_dirty.
            self.pane_exit_checkpoint_pending = false;
        }
        Ok(())
    }

    pub(crate) fn save_session_on_shutdown(&mut self) {
        if self.pane_exit_checkpoint_pending && !self.state.session_dirty {
            self.session_save_deadline = None;
            return;
        }
        self.save_session_now();
    }
}

fn run_session_save_job(job: SessionSaveJob) {
    match job {
        SessionSaveJob::Clear => crate::persist::clear(),
        SessionSaveJob::Save { snapshot, history } => {
            crate::persist::save(&snapshot, history.as_ref());
        }
    }
}
