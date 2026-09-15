use std::str::FromStr;

use super::{responses, App};
use crate::api::schema::{
    DelegationCreateParams, DelegationInfo, DelegationReorderParams, DelegationReparentParams,
    DelegationSiblingPosition, DelegationTarget, DelegationTreeEntry, ResponseResult,
};
use crate::delegation::{DelegationId, SiblingPosition};

impl App {
    fn emit_delegation_event(&mut self, action: &str, delegation: DelegationInfo) {
        self.emit_event(crate::api::schema::EventEnvelope {
            event: crate::api::schema::EventKind::DelegationUpdated,
            data: crate::api::schema::EventData::DelegationUpdated {
                action: action.into(),
                delegation,
            },
        });
    }

    fn delegation_info(&self, id: DelegationId) -> Option<DelegationInfo> {
        let record = self.state.delegations.get(id)?;
        let pane_id = record.pane_id.and_then(|pane| {
            self.find_pane(pane)
                .and_then(|(ws_idx, _)| self.public_pane_id(ws_idx, pane))
        });
        Some(DelegationInfo {
            delegation_id: id.to_string(),
            pane_id,
            parent_id: record.parent_id.map(|parent| parent.to_string()),
            purpose: record.purpose.clone(),
            sibling_rank: record.sibling_rank,
            tombstone: record.tombstone,
        })
    }

    fn parse_delegation_target(&self, raw: &str) -> Result<DelegationId, &'static str> {
        let id = DelegationId::from_str(raw).map_err(|_| "invalid delegation id")?;
        self.state
            .delegations
            .get(id)
            .map(|_| id)
            .ok_or("delegation was not found")
    }

    pub(crate) fn handle_delegation_create(
        &mut self,
        request_id: String,
        params: DelegationCreateParams,
    ) -> String {
        let pane_id = match params.pane_id.as_deref() {
            Some(raw) => match self.parse_current_public_pane_id(raw) {
                Some((_, pane)) => Some(pane),
                None => {
                    return responses::encode_error(request_id, "not_found", "pane was not found")
                }
            },
            None => None,
        };
        let parent_id = match params.parent_id.as_deref() {
            Some(raw) => match self.parse_delegation_target(raw) {
                Ok(id) => Some(id),
                Err(message) => return responses::encode_error(request_id, "not_found", message),
            },
            None => None,
        };
        if let Err(error) =
            crate::admission::relationship_create(&self.state.delegations, parent_id)
        {
            return responses::encode_error(request_id, "resource_limit", error.to_string());
        }
        let Ok(id) = self
            .state
            .delegations
            .create(pane_id, parent_id, params.purpose)
        else {
            return responses::encode_error(
                request_id,
                "invalid_delegation",
                "delegation could not be created",
            );
        };
        self.state.mark_session_dirty();
        let delegation = self.delegation_info(id).expect("created delegation");
        self.emit_delegation_event("created", delegation.clone());
        responses::encode_success(request_id, ResponseResult::DelegationInfo { delegation })
    }

    pub(crate) fn handle_delegation_get(
        &self,
        request_id: String,
        target: DelegationTarget,
    ) -> String {
        let Ok(id) = self.parse_delegation_target(&target.delegation_id) else {
            return responses::encode_error(request_id, "not_found", "delegation was not found");
        };
        responses::encode_success(
            request_id,
            ResponseResult::DelegationInfo {
                delegation: self.delegation_info(id).expect("located delegation"),
            },
        )
    }

    pub(crate) fn delegation_tree(&self) -> Vec<DelegationTreeEntry> {
        let included = self.state.delegations.records().keys().copied().collect();
        self.state
            .delegations
            .preorder_projection(&included)
            .into_iter()
            .filter_map(|entry| {
                Some(DelegationTreeEntry {
                    delegation: self.delegation_info(entry.id)?,
                    depth: entry.depth,
                })
            })
            .collect()
    }

    pub(crate) fn handle_delegation_tree(&self, request_id: String) -> String {
        responses::encode_success(
            request_id,
            ResponseResult::DelegationTree {
                delegations: self.delegation_tree(),
            },
        )
    }

    pub(crate) fn handle_delegation_root(
        &self,
        request_id: String,
        target: DelegationTarget,
    ) -> String {
        let Ok(id) = self.parse_delegation_target(&target.delegation_id) else {
            return responses::encode_error(request_id, "not_found", "delegation was not found");
        };
        let Ok(root) = self.state.delegations.root(id) else {
            return responses::encode_error(
                request_id,
                "invalid_delegation",
                "delegation tree is corrupt",
            );
        };
        responses::encode_success(
            request_id,
            ResponseResult::DelegationInfo {
                delegation: self.delegation_info(root).expect("root exists"),
            },
        )
    }

    pub(crate) fn handle_delegation_descendants(
        &self,
        request_id: String,
        target: DelegationTarget,
    ) -> String {
        let Ok(id) = self.parse_delegation_target(&target.delegation_id) else {
            return responses::encode_error(request_id, "not_found", "delegation was not found");
        };
        let Ok(ids) = self.state.delegations.descendants(id) else {
            return responses::encode_error(
                request_id,
                "invalid_delegation",
                "delegation tree is corrupt",
            );
        };
        let delegations = ids
            .into_iter()
            .filter_map(|entry_id| self.delegation_info(entry_id))
            .collect();
        responses::encode_success(request_id, ResponseResult::DelegationList { delegations })
    }

    pub(crate) fn handle_delegation_reparent(
        &mut self,
        request_id: String,
        params: DelegationReparentParams,
    ) -> String {
        let Ok(id) = self.parse_delegation_target(&params.delegation_id) else {
            return responses::encode_error(request_id, "not_found", "delegation was not found");
        };
        let parent = match params.parent_id.as_deref() {
            Some(raw) => match self.parse_delegation_target(raw) {
                Ok(parent_id) => Some(parent_id),
                Err(_) => {
                    return responses::encode_error(
                        request_id,
                        "not_found",
                        "parent delegation was not found",
                    )
                }
            },
            None => None,
        };
        if let Err(error) =
            crate::admission::relationship_parent(&self.state.delegations, Some(id), parent)
        {
            return responses::encode_error(request_id, "resource_limit", error.to_string());
        }
        if self.state.delegations.reparent(id, parent).is_err() {
            return responses::encode_error(
                request_id,
                "invalid_delegation",
                "reparenting would create an invalid tree",
            );
        }
        self.state.mark_session_dirty();
        let delegation = self.delegation_info(id).expect("delegation exists");
        self.emit_delegation_event("reparented", delegation.clone());
        responses::encode_success(request_id, ResponseResult::DelegationInfo { delegation })
    }

    pub(crate) fn handle_delegation_reorder(
        &mut self,
        request_id: String,
        params: DelegationReorderParams,
    ) -> String {
        let Ok(id) = self.parse_delegation_target(&params.delegation_id) else {
            return responses::encode_error(request_id, "not_found", "delegation was not found");
        };
        let position = match params.position {
            DelegationSiblingPosition::First => SiblingPosition::First,
            DelegationSiblingPosition::Last => SiblingPosition::Last,
            DelegationSiblingPosition::Before { delegation_id } => {
                match DelegationId::from_str(&delegation_id) {
                    Ok(anchor) => SiblingPosition::Before(anchor),
                    Err(_) => {
                        return responses::encode_error(
                            request_id,
                            "invalid_id",
                            "invalid sibling delegation id",
                        )
                    }
                }
            }
            DelegationSiblingPosition::After { delegation_id } => {
                match DelegationId::from_str(&delegation_id) {
                    Ok(anchor) => SiblingPosition::After(anchor),
                    Err(_) => {
                        return responses::encode_error(
                            request_id,
                            "invalid_id",
                            "invalid sibling delegation id",
                        )
                    }
                }
            }
        };
        if self.state.delegations.reorder(id, position).is_err() {
            return responses::encode_error(
                request_id,
                "invalid_delegation",
                "delegation can only be reordered among siblings",
            );
        }
        self.state.mark_session_dirty();
        let delegation = self.delegation_info(id).expect("delegation exists");
        self.emit_delegation_event("reordered", delegation.clone());
        responses::encode_success(request_id, ResponseResult::DelegationInfo { delegation })
    }
}
