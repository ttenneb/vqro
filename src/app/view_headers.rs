//! Event adapters for cached generic collapsed headers. Never called by render.
use std::collections::{HashMap, HashSet};

use super::App;
use crate::{extension_contract::HostTarget, view_headers::HeaderSource};

#[cfg(test)]
mod tests;

impl App {
    pub(crate) fn sync_view_headers(&mut self) {
        let mut consumers = Vec::new();
        let mut namespaces = HashSet::new();
        let mut tabs = HashSet::new();
        for (wi, workspace) in self.state.workspaces.iter().enumerate() {
            for (ti, tab) in workspace.tabs.iter().enumerate() {
                let Some(tab_id) = self.public_tab_id(wi, ti) else {
                    continue;
                };
                tabs.insert(tab_id.clone());
                for pane in tab.layout.view_panes() {
                    let Some(view) = tab.layout.view_at(pane) else {
                        continue;
                    };
                    namespaces.insert(view.extension.clone());
                    let scope = tab
                        .layout
                        .view_scope(pane)
                        .into_iter()
                        .filter_map(|terminal| self.public_pane_id(wi, terminal))
                        .collect::<Vec<_>>();
                    consumers.push((pane, tab_id.clone(), view, scope));
                }
            }
        }
        let mut sessions = HashSet::new();
        let mut specifics = HashSet::new();
        for ((namespace, source_id), cache) in &self.host_document_caches {
            if !namespaces.contains(namespace) {
                continue;
            }
            let Some(document) = cache
                .current()
                .filter(|document| document.namespace == *namespace)
            else {
                continue;
            };
            let stamp = (document.generation, document.revision);
            if document.target == HostTarget::Session {
                if sessions.contains(namespace)
                    && self
                        .state
                        .view_headers
                        .sessions
                        .get(namespace)
                        .is_some_and(|source| {
                            (source.incarnation, source.source_id.as_str())
                                > (stamp, source_id.as_str())
                        })
                {
                    continue;
                }
                sessions.insert(namespace.clone());
                let same = self
                    .state
                    .view_headers
                    .sessions
                    .get(namespace)
                    .is_some_and(|source| {
                        source.source_id == *source_id && source.incarnation == stamp
                    });
                if !same {
                    self.state.view_headers.sessions.insert(
                        namespace.clone(),
                        HeaderSource::accepted(source_id, document),
                    );
                }
            } else if tabs.contains(source_id) {
                let key = (namespace.clone(), source_id.clone());
                specifics.insert(key.clone());
                let same = self
                    .state
                    .view_headers
                    .tabs
                    .get(&key)
                    .is_some_and(|source| source.incarnation == stamp);
                if !same {
                    self.state
                        .view_headers
                        .tabs
                        .insert(key, HeaderSource::accepted(source_id, document));
                }
            }
        }
        self.state
            .view_headers
            .sessions
            .retain(|namespace, _| sessions.contains(namespace));
        self.state
            .view_headers
            .tabs
            .retain(|key, _| specifics.contains(key));
        let mut headers = HashMap::new();
        for (pane, tab, view, scope) in consumers {
            headers.insert(
                pane,
                self.state
                    .view_headers
                    .project(&view.extension, &tab, &view.view, &scope),
            );
        }
        self.state.view_headers.panes = headers;
    }
}
