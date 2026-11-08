//! Bounded derived header facts. Package documents remain the semantic authority.
use std::collections::HashMap;

use crate::{extension_contract::HostDocumentV1, layout::PaneId};

pub(crate) const MAX_HEADER_SCALARS: usize = 160;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HeaderText(pub String);

impl HeaderText {
    pub(crate) fn new(text: &str) -> Self {
        Self(
            text.chars()
                .filter(|ch| !ch.is_control())
                .take(MAX_HEADER_SCALARS)
                .collect(),
        )
    }
}

pub(crate) struct HeaderSource {
    pub source_id: String,
    pub incarnation: (u64, u64),
    pub roots: HashMap<(String, String), HeaderText>,
}

impl HeaderSource {
    pub(crate) fn accepted(source_id: &str, document: &HostDocumentV1) -> Self {
        let mut roots = HashMap::new();
        for item in &document.items {
            let Some(view) = item.view.as_ref().filter(|_| !item.flags.disabled) else {
                continue;
            };
            let (Some(view), Some(pane)) = (&view.view_id, &view.pane_id) else {
                continue;
            };
            // Bound before joining: a large valid document field never becomes a
            // large retained header or a per-frame scan.
            let title = HeaderText::new(&item.label);
            let detail = HeaderText::new(item.detail.as_deref().unwrap_or_default());
            let text = if detail.0.is_empty() {
                title
            } else {
                HeaderText::new(&format!("{} · {}", title.0, detail.0))
            };
            roots.insert((view.clone(), pane.clone()), text);
        }
        Self {
            source_id: source_id.into(),
            incarnation: (document.generation, document.revision),
            roots,
        }
    }
}

#[derive(Default)]
pub(crate) struct ViewHeaders {
    pub sessions: HashMap<String, HeaderSource>,
    pub tabs: HashMap<(String, String), HeaderSource>,
    pub panes: HashMap<PaneId, HeaderText>,
}

impl ViewHeaders {
    pub(crate) fn project(
        &self,
        namespace: &str,
        tab: &str,
        view: &str,
        scope: &[String],
    ) -> HeaderText {
        let specific = self.tabs.get(&(namespace.into(), tab.into()));
        let session = self.sessions.get(namespace);
        let mut parts = Vec::new();
        let mut scalars = 0;
        for pane in scope {
            let key = (view.to_owned(), pane.clone());
            let text = specific
                .and_then(|source| source.roots.get(&key))
                .or_else(|| session.and_then(|source| source.roots.get(&key)));
            if let Some(text) = text {
                parts.push(text.0.as_str());
                scalars += text.0.chars().count();
                if scalars >= MAX_HEADER_SCALARS {
                    break;
                }
            }
        }
        if parts.is_empty() {
            HeaderText::new(view)
        } else {
            HeaderText::new(&parts.join(" / "))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn document() -> HostDocumentV1 {
        serde_json::from_value(serde_json::json!({"contract":"host.view.v1","namespace":"sample.views","generation":7,"revision":1,"surface":"navigation","target":{"kind":"session"},"items":[
            {"id":"root","role":"section","label":"Tasks","detail":"3/4 done · 1 waiting","view":{"pane_id":"w1:p1","view_id":"tasks","state":"open"}}
        ]})).unwrap()
    }

    #[test]
    fn accepted_headers_are_bounded_single_line_package_facts_not_rows() {
        let mut doc = document();
        let mut cache = ViewHeaders::default();
        cache.sessions.insert(
            "sample.views".into(),
            HeaderSource::accepted("session", &doc),
        );
        assert_eq!(
            cache
                .project("sample.views", "w1:t1", "tasks", &["w1:p1".into()])
                .0,
            "Tasks · 3/4 done · 1 waiting"
        );
        doc.items[0].detail = Some(format!("\n\t{}", "x".repeat(4096)));
        let source = HeaderSource::accepted("session", &doc);
        let header = &source.roots[&("tasks".into(), "w1:p1".into())];
        assert_eq!(header.0.chars().count(), MAX_HEADER_SCALARS);
        assert!(!header.0.chars().any(char::is_control));
    }

    #[test]
    fn scope_source_and_withdrawal_do_not_reuse_another_terminals_summary() {
        let doc = document();
        let mut cache = ViewHeaders::default();
        cache.sessions.insert(
            "sample.views".into(),
            HeaderSource::accepted("session", &doc),
        );
        assert_eq!(
            cache
                .project("sample.views", "w1:t1", "tasks", &["w1:p2".into()])
                .0,
            "tasks"
        );
        cache.sessions.clear();
        assert_eq!(
            cache
                .project("sample.views", "w1:t1", "tasks", &["w1:p1".into()])
                .0,
            "tasks"
        );
    }
}
