//! vqro's own input inventory, not a native queue or a replay mechanism.
use super::{Endpoint, RECEIPTS, rejected};
use crate::rpc::RpcError;
use serde_json::{Value, json};

pub(super) struct OwnedInput {
    session: String,
    epoch: String,
    run: Option<String>,
    stage: &'static str,
    auth_generation: u64,
}

impl Endpoint {
    pub(super) fn input_ledger(&self) -> Value {
        json!({"v":0,"source":"vqro-ledger","epoch":self.epoch,"complete":true,"pending":self.owned_inputs.len(),
            "unknown":self.owned_inputs.values().filter(|p|p.stage=="unknown").count()})
    }

    pub(super) fn own_inputs_quiescent(&self) -> bool {
        self.owned_inputs.is_empty()
    }

    pub(super) fn reserve_owned_input(
        &mut self,
        delivery: &str,
        run: Option<String>,
    ) -> Result<(), RpcError> {
        if self.owned_inputs.contains_key(delivery) || self.owned_inputs.len() >= RECEIPTS {
            return Err(rejected(
                "busy",
                "own input inventory is unresolved or full",
            ));
        }
        let session = self
            .session
            .clone()
            .ok_or_else(|| rejected("stale", "no current session"))?;
        self.owned_inputs.insert(
            delivery.to_owned(),
            OwnedInput {
                session,
                epoch: self.epoch.clone(),
                run,
                stage: "dispatched",
                auth_generation: self.availability_auth_generation,
            },
        );
        Ok(())
    }

    pub(super) fn finish_owned_input(
        &mut self,
        delivery: &str,
        stage: &'static str,
        native: Option<&Value>,
    ) {
        if stage == "rejected" {
            self.owned_inputs.remove(delivery);
            return;
        }
        let Some(input) = self.owned_inputs.get_mut(delivery) else {
            return;
        };
        input.stage = stage;
        if let Some(run) = native.and_then(|n| n["run"].as_str()) {
            input.run = Some(run.to_owned());
        }
        let retired = (input.auth_generation != self.availability_auth_generation)
            .then(|| input.run.clone().map(|run| (input.session.clone(), run)))
            .flatten();
        let observed = input.run.as_ref().is_some_and(|run| {
            self.observed_input_runs
                .iter()
                .any(|(session, epoch, id, terminal)| {
                    session == &input.session
                        && epoch == &input.epoch
                        && id == run
                        && (*terminal || stage == "admitted")
                })
        });
        if observed {
            self.owned_inputs.remove(delivery);
        }
        if let Some((session, run)) = retired {
            self.retire_availability_turn(&session, &run);
        }
    }

    pub(super) fn old_unbound_own_inputs(&self) -> bool {
        self.owned_inputs
            .values()
            .any(|p| p.auth_generation != self.availability_auth_generation && p.run.is_none())
    }

    pub(super) fn known_owned_input_runs(&self) -> Vec<(String, String)> {
        self.owned_inputs
            .values()
            .filter_map(|p| p.run.clone().map(|run| (p.session.clone(), run)))
            .collect()
    }

    pub(super) fn observe_owned_input_run(&mut self, run: &str, terminal: bool) {
        let Some(session) = self.session.clone() else {
            return;
        };
        let epoch = self.epoch.clone();
        self.owned_inputs.retain(|_, input| {
            !(input.session == session
                && input.epoch == epoch
                && input.run.as_deref() == Some(run)
                && (terminal || input.stage == "admitted"))
        });
        // Notifications can precede the admission reply. Cache bounded evidence only;
        // eviction can leave an input unresolved, never manufacture an empty inventory.
        self.observed_input_runs
            .push_back((session, epoch, run.to_owned(), terminal));
        while self.observed_input_runs.len() > RECEIPTS {
            self.observed_input_runs.pop_front();
        }
    }
}
