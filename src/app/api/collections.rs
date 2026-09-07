use ratatui::layout::Direction;

use super::{responses, App};
use crate::api::schema::{
    CollectionAddParams, CollectionCloseDisposition, CollectionCloseParams, CollectionCreateParams,
    CollectionInfo, CollectionListParams, CollectionMemberInfo, CollectionMemberTarget,
    CollectionPromoteParams, CollectionReorderParams, CollectionSelectParams, CollectionTarget,
    ResponseResult,
};
use crate::layout::{CollectionId, LayoutLeaf};

fn direction(value: crate::api::schema::SplitDirection) -> Direction {
    match value {
        crate::api::schema::SplitDirection::Right => Direction::Horizontal,
        crate::api::schema::SplitDirection::Down => Direction::Vertical,
    }
}

fn collection_id_text(id: CollectionId) -> String {
    format!("collection_{}", id.raw())
}

impl App {
    fn emit_collection_event(&mut self, action: &str, collection: CollectionInfo) {
        self.emit_event(crate::api::schema::EventEnvelope {
            event: crate::api::schema::EventKind::CollectionUpdated,
            data: crate::api::schema::EventData::CollectionUpdated {
                action: action.into(),
                collection,
            },
        });
    }

    fn find_collection(&self, id: CollectionId) -> Option<(usize, usize)> {
        self.state
            .workspaces
            .iter()
            .enumerate()
            .find_map(|(ws_idx, ws)| {
                ws.tabs
                    .iter()
                    .position(|tab| tab.collection(id).is_some())
                    .map(|tab_idx| (ws_idx, tab_idx))
            })
    }

    pub(crate) fn collection_info(
        &self,
        ws_idx: usize,
        tab_idx: usize,
        id: CollectionId,
    ) -> Option<CollectionInfo> {
        let ws = self.state.workspaces.get(ws_idx)?;
        let tab = ws.tabs.get(tab_idx)?;
        let collection = tab.collection(id)?;
        let collection_id = collection_id_text(id);
        let selected = collection.selected();
        Some(CollectionInfo {
            collection_id,
            workspace_id: self.public_workspace_id(ws_idx),
            tab_id: self.public_tab_id(ws_idx, tab_idx)?,
            label: collection.label.clone(),
            focused: tab.layout.focused_leaf() == LayoutLeaf::Collection(id),
            selected_pane_id: selected.and_then(|pane| self.public_pane_id(ws_idx, pane)),
            members: collection
                .members()
                .iter()
                .enumerate()
                .filter_map(|(index, pane)| {
                    Some(CollectionMemberInfo {
                        pane_id: self.public_pane_id(ws_idx, *pane)?,
                        index,
                        archived: collection.is_archived(*pane),
                        selected: selected == Some(*pane),
                    })
                })
                .collect(),
        })
    }

    pub(crate) fn handle_collection_list(
        &self,
        request_id: String,
        params: CollectionListParams,
    ) -> String {
        let workspace_filter = params
            .workspace_id
            .as_deref()
            .and_then(|id| self.parse_workspace_id(id));
        let tab_filter = params
            .tab_id
            .as_deref()
            .and_then(|id| self.parse_tab_id(id));
        if params.workspace_id.is_some() && workspace_filter.is_none()
            || params.tab_id.is_some() && tab_filter.is_none()
        {
            return responses::encode_error(
                request_id,
                "not_found",
                "workspace or tab was not found",
            );
        }
        let mut collections = Vec::new();
        for (ws_idx, ws) in self.state.workspaces.iter().enumerate() {
            for (tab_idx, tab) in ws.tabs.iter().enumerate() {
                if workspace_filter.is_some_and(|wanted| wanted != ws_idx)
                    || tab_filter.is_some_and(|wanted| wanted != (ws_idx, tab_idx))
                {
                    continue;
                }
                for id in tab.layout.collection_ids() {
                    if let Some(info) = self.collection_info(ws_idx, tab_idx, id) {
                        collections.push(info);
                    }
                }
            }
        }
        responses::encode_success(request_id, ResponseResult::CollectionList { collections })
    }

    pub(crate) fn handle_collection_get(
        &self,
        request_id: String,
        target: CollectionTarget,
    ) -> String {
        let Ok(id) = CollectionId::parse(&target.collection_id) else {
            return responses::encode_error(request_id, "invalid_id", "invalid collection id");
        };
        let Some((ws_idx, tab_idx)) = self.find_collection(id) else {
            return responses::encode_error(request_id, "not_found", "collection was not found");
        };
        responses::encode_success(
            request_id,
            ResponseResult::CollectionInfo {
                collection: self
                    .collection_info(ws_idx, tab_idx, id)
                    .expect("located collection"),
            },
        )
    }

    pub(crate) fn handle_collection_create(
        &mut self,
        request_id: String,
        params: CollectionCreateParams,
    ) -> String {
        let Some((ws_idx, pane_id)) = self.parse_current_public_pane_id(&params.target_pane_id)
        else {
            return responses::encode_error(request_id, "not_found", "target pane was not found");
        };
        let Some(tab_idx) = self.state.workspaces[ws_idx].find_tab_index_for_pane(pane_id) else {
            return responses::encode_error(request_id, "not_found", "target tab was not found");
        };
        let created = self.state.workspaces[ws_idx].tabs[tab_idx].create_collection_near(
            LayoutLeaf::Pane(pane_id),
            direction(params.direction),
            params.ratio.unwrap_or(0.5),
            params.label,
        );
        let Ok(collection_id) = created else {
            return responses::encode_error(
                request_id,
                "invalid_placement",
                "collection could not be created at that target",
            );
        };
        if params.focus {
            let _ = self.state.workspaces[ws_idx].tabs[tab_idx]
                .layout
                .focus_leaf(LayoutLeaf::Collection(collection_id));
        }
        self.state.mark_session_dirty();
        let collection = self
            .collection_info(ws_idx, tab_idx, collection_id)
            .expect("created collection");
        self.emit_collection_event("created", collection.clone());
        responses::encode_success(request_id, ResponseResult::CollectionInfo { collection })
    }

    pub(crate) fn handle_collection_add(
        &mut self,
        request_id: String,
        params: CollectionAddParams,
    ) -> String {
        let Ok(collection_id) = CollectionId::parse(&params.collection_id) else {
            return responses::encode_error(request_id, "invalid_id", "invalid collection id");
        };
        let Some((ws_idx, tab_idx)) = self.find_collection(collection_id) else {
            return responses::encode_error(request_id, "not_found", "collection was not found");
        };
        let Some((pane_ws_idx, pane_id)) = self.parse_current_public_pane_id(&params.pane_id)
        else {
            return responses::encode_error(request_id, "not_found", "pane was not found");
        };
        if pane_ws_idx != ws_idx
            || self.state.workspaces[ws_idx].find_tab_index_for_pane(pane_id) != Some(tab_idx)
        {
            return responses::encode_error(
                request_id,
                "cross_tab_unsupported",
                "the first port only collects panes from the same tab",
            );
        }
        if self.state.workspaces[ws_idx].tabs[tab_idx]
            .collect_tiled_pane(pane_id, collection_id)
            .is_err()
        {
            return responses::encode_error(
                request_id,
                "invalid_placement",
                "pane could not be added to the collection",
            );
        }
        self.state.mark_session_dirty();
        let collection = self
            .collection_info(ws_idx, tab_idx, collection_id)
            .expect("updated collection");
        self.emit_collection_event("member_added", collection.clone());
        responses::encode_success(request_id, ResponseResult::CollectionInfo { collection })
    }

    pub(crate) fn handle_collection_select(
        &mut self,
        request_id: String,
        params: CollectionSelectParams,
    ) -> String {
        self.mutate_collection_member(
            request_id,
            &params.collection_id,
            &params.pane_id,
            |tab, collection, pane| {
                tab.select_collection_member(collection, pane)?;
                if params.focus {
                    tab.layout.focus_leaf(LayoutLeaf::Collection(collection));
                }
                Ok(())
            },
        )
    }

    pub(crate) fn handle_collection_reorder(
        &mut self,
        request_id: String,
        params: CollectionReorderParams,
    ) -> String {
        self.mutate_collection_member(
            request_id,
            &params.collection_id,
            &params.pane_id,
            |tab, collection, pane| tab.reorder_collection_member(collection, pane, params.index),
        )
    }

    pub(crate) fn handle_collection_archive(
        &mut self,
        request_id: String,
        params: CollectionMemberTarget,
        archived: bool,
    ) -> String {
        self.mutate_collection_member(
            request_id,
            &params.collection_id,
            &params.pane_id,
            |tab, collection, pane| tab.set_collection_member_archived(collection, pane, archived),
        )
    }

    fn mutate_collection_member<F>(
        &mut self,
        request_id: String,
        collection_raw: &str,
        pane_raw: &str,
        mutate: F,
    ) -> String
    where
        F: FnOnce(
            &mut crate::workspace::Tab,
            CollectionId,
            crate::layout::PaneId,
        ) -> Result<(), crate::workspace::CollectionMutationError>,
    {
        let Ok(collection_id) = CollectionId::parse(collection_raw) else {
            return responses::encode_error(request_id, "invalid_id", "invalid collection id");
        };
        let Some((ws_idx, tab_idx)) = self.find_collection(collection_id) else {
            return responses::encode_error(request_id, "not_found", "collection was not found");
        };
        let Some((pane_ws_idx, pane_id)) = self.parse_current_public_pane_id(pane_raw) else {
            return responses::encode_error(request_id, "not_found", "pane was not found");
        };
        if pane_ws_idx != ws_idx
            || mutate(
                &mut self.state.workspaces[ws_idx].tabs[tab_idx],
                collection_id,
                pane_id,
            )
            .is_err()
        {
            return responses::encode_error(
                request_id,
                "invalid_placement",
                "pane is not a member of the collection",
            );
        }
        self.state.mark_session_dirty();
        let collection = self
            .collection_info(ws_idx, tab_idx, collection_id)
            .expect("updated collection");
        self.emit_collection_event("member_updated", collection.clone());
        responses::encode_success(request_id, ResponseResult::CollectionInfo { collection })
    }

    pub(crate) fn handle_collection_promote(
        &mut self,
        request_id: String,
        params: CollectionPromoteParams,
    ) -> String {
        let Some((ws_idx, pane_id)) = self.parse_current_public_pane_id(&params.pane_id) else {
            return responses::encode_error(request_id, "not_found", "pane was not found");
        };
        let Some((target_ws_idx, target)) =
            self.parse_current_public_pane_id(&params.target_pane_id)
        else {
            return responses::encode_error(request_id, "not_found", "target pane was not found");
        };
        if ws_idx != target_ws_idx {
            return responses::encode_error(
                request_id,
                "cross_workspace_unsupported",
                "promotion target must be in the same workspace",
            );
        }
        let Some(tab_idx) = self.state.workspaces[ws_idx].find_tab_index_for_pane(pane_id) else {
            return responses::encode_error(request_id, "not_found", "pane tab was not found");
        };
        if self.state.workspaces[ws_idx].find_tab_index_for_pane(target) != Some(tab_idx) {
            return responses::encode_error(
                request_id,
                "cross_tab_unsupported",
                "promotion target must be in the same tab",
            );
        }
        let Some(crate::layout::PanePlacement::Collection(collection_id)) =
            self.state.workspaces[ws_idx].tabs[tab_idx].pane_placement(pane_id)
        else {
            return responses::encode_error(
                request_id,
                "invalid_placement",
                "pane is not collected",
            );
        };
        if self.state.workspaces[ws_idx].tabs[tab_idx]
            .promote_collection_member_near(
                pane_id,
                collection_id,
                target,
                direction(params.direction),
                params.ratio.unwrap_or(0.5),
            )
            .is_err()
        {
            return responses::encode_error(
                request_id,
                "invalid_placement",
                "pane could not be promoted",
            );
        }
        if !params.focus {
            self.state.workspaces[ws_idx].tabs[tab_idx]
                .layout
                .focus_pane(target);
        }
        self.state.mark_session_dirty();
        let collection = self
            .collection_info(ws_idx, tab_idx, collection_id)
            .expect("updated collection");
        self.emit_collection_event("member_promoted", collection.clone());
        responses::encode_success(request_id, ResponseResult::CollectionInfo { collection })
    }

    pub(crate) fn handle_collection_close(
        &mut self,
        request_id: String,
        params: CollectionCloseParams,
    ) -> String {
        let Ok(collection_id) = CollectionId::parse(&params.collection_id) else {
            return responses::encode_error(request_id, "invalid_id", "invalid collection id");
        };
        let Some((ws_idx, tab_idx)) = self.find_collection(collection_id) else {
            return responses::encode_error(request_id, "not_found", "collection was not found");
        };
        let members = self.state.workspaces[ws_idx].tabs[tab_idx]
            .collection(collection_id)
            .expect("located collection")
            .members()
            .to_vec();
        if !members.is_empty()
            || params.disposition == Some(CollectionCloseDisposition::CascadeClose)
        {
            return responses::encode_error(
                request_id,
                "collection_not_empty",
                "archive or promote members before closing the collection",
            );
        }
        if self.state.workspaces[ws_idx].tabs[tab_idx]
            .remove_empty_collection(collection_id)
            .is_err()
        {
            return responses::encode_error(
                request_id,
                "invalid_placement",
                "collection cannot be removed as the last layout leaf",
            );
        }
        self.state.mark_session_dirty();
        responses::encode_success(request_id, ResponseResult::Ok {})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::schema::{Method, Request, SplitDirection};

    fn app_with_workspace() -> App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![crate::workspace::Workspace::test_new("collections")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.ensure_test_terminals();
        app
    }

    #[test]
    fn collection_create_and_list_use_stable_ids() {
        let mut app = app_with_workspace();
        let root = app.state.workspaces[0].tabs[0].root_pane;
        let pane_id = app.public_pane_id(0, root).expect("public pane id");
        let response = app.handle_api_request(Request {
            id: "create".into(),
            method: Method::CollectionCreate(CollectionCreateParams {
                target_pane_id: pane_id,
                direction: SplitDirection::Right,
                ratio: Some(0.4),
                label: Some("helpers".into()),
                focus: true,
            }),
        });
        let created: crate::api::schema::SuccessResponse =
            serde_json::from_str(&response).expect("response");
        let ResponseResult::CollectionInfo { collection } = created.result else {
            panic!("collection response")
        };
        assert!(collection.collection_id.starts_with("collection_"));
        assert_eq!(collection.label.as_deref(), Some("helpers"));
        assert!(collection.focused);

        let listed = app.handle_api_request(Request {
            id: "list".into(),
            method: Method::CollectionList(CollectionListParams::default()),
        });
        let listed: crate::api::schema::SuccessResponse =
            serde_json::from_str(&listed).expect("response");
        let ResponseResult::CollectionList { collections } = listed.result else {
            panic!("list response")
        };
        assert_eq!(collections.len(), 1);
        assert_eq!(collections[0].collection_id, collection.collection_id);
    }
}
