use crate::api::schema::{
    LayoutCollapseDescription, LayoutCollapseTarget, LayoutExportParams, LayoutPathStep,
    LayoutSplitChild, LayoutTreeNode, Method, PaneTarget, Request, ResponseResult, SuccessResponse,
};

pub(super) fn run(args: &[String], collapsed: bool) -> std::io::Result<i32> {
    let (pane, containing_box) = match parse(args) {
        Ok(parsed) => parsed,
        Err(message) => {
            eprintln!("{message}");
            return Ok(2);
        }
    };
    let pane = super::super::normalize_pane_id(&pane);
    let method = if containing_box {
        let response = super::super::send_request(&Request {
            id: "cli:collapse:describe".into(),
            method: Method::LayoutCollapseDescribe(LayoutExportParams {
                tab_id: None,
                pane_id: Some(pane.clone()),
            }),
        })?;
        if response.get("error").is_some() {
            return super::super::print_response(&response);
        }
        let success: SuccessResponse =
            serde_json::from_value(response).map_err(std::io::Error::other)?;
        let ResponseResult::LayoutCollapseDescription { collapse } = success.result else {
            return Err(std::io::Error::other(
                "server omitted collapse target description",
            ));
        };
        let target = match box_target(collapse, &pane) {
            Ok(target) => target,
            Err(message) => {
                eprintln!("{message}");
                return Ok(2);
            }
        };
        if collapsed {
            Method::LayoutCollapse(target)
        } else {
            Method::LayoutExpand(target)
        }
    } else {
        let target = PaneTarget { pane_id: pane };
        if collapsed {
            Method::PaneCollapse(target)
        } else {
            Method::PaneExpand(target)
        }
    };
    let response = super::super::send_request(&Request {
        id: "cli:pane:collapse-state".into(),
        method,
    })?;
    super::super::print_response(&response)
}

fn parse(args: &[String]) -> Result<(String, bool), String> {
    let mut pane = None;
    let mut containing_box = false;
    for arg in args {
        if arg == "--box" && !containing_box {
            containing_box = true;
        } else if arg.starts_with('-') || pane.is_some() {
            return Err("usage: vqro pane collapse|expand <pane_id> [--box]".into());
        } else {
            pane = Some(arg.clone());
        }
    }
    pane.filter(|pane| !pane.is_empty())
        .map(|pane| (pane, containing_box))
        .ok_or_else(|| "usage: vqro pane collapse|expand <pane_id> [--box]".into())
}

fn box_target(
    description: LayoutCollapseDescription,
    pane: &str,
) -> Result<LayoutCollapseTarget, String> {
    fn find(node: &LayoutTreeNode, pane: &str, path: &mut Vec<LayoutPathStep>) -> bool {
        match node {
            LayoutTreeNode::Pane { pane_id, .. } | LayoutTreeNode::View { pane_id, .. } => {
                pane_id == pane
            }
            LayoutTreeNode::Split { first, second, .. } => {
                path.push(LayoutPathStep::Split(LayoutSplitChild::First));
                if find(first, pane, path) {
                    return true;
                }
                path.pop();
                path.push(LayoutPathStep::Split(LayoutSplitChild::Second));
                if find(second, pane, path) {
                    return true;
                }
                path.pop();
                false
            }
            LayoutTreeNode::Stack { children, .. } => {
                for (index, child) in children.iter().enumerate() {
                    path.push(LayoutPathStep::StackChild(index));
                    if find(child, pane, path) {
                        return true;
                    }
                    path.pop();
                }
                false
            }
            LayoutTreeNode::Unknown => false,
        }
    }
    let mut path = Vec::new();
    if !find(&description.layout.root, pane, &mut path) {
        return Err("pane is absent from the current collapse description".into());
    }
    if path.pop().is_none() {
        return Err("pane has no containing box".into());
    }
    Ok(LayoutCollapseTarget {
        boot_id: description.boot_id,
        tab_id: description.layout.tab_id,
        tree_fingerprint: description.tree_fingerprint,
        path,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn collapse_parser_requires_exact_pane_and_optional_box_without_ambiguous_flags() {
        assert_eq!(parse(&["w1:p2".into()]).unwrap(), ("w1:p2".into(), false));
        assert_eq!(
            parse(&["--box".into(), "w1:p2".into()]).unwrap(),
            ("w1:p2".into(), true)
        );
        for args in [
            vec![],
            vec!["--box"],
            vec!["p1", "p2"],
            vec!["p1", "--box", "--box"],
            vec!["p1", "--unknown"],
        ] {
            assert!(parse(&args.into_iter().map(String::from).collect::<Vec<_>>()).is_err());
        }
    }
    #[test]
    fn containing_box_target_preserves_stack_grammar_and_full_hidden_membership() {
        let description: LayoutCollapseDescription = serde_json::from_value(serde_json::json!({
            "kind":"layout.collapse.v1","boot_id":"boot","tree_fingerprint":"a".repeat(64),
            "layout":{"kind":"layout.tree.v1","workspace_id":"w1","tab_id":"w1:t1","zoomed":false,"focused":{"type":"pane","pane_id":"w1:p1"},"root":{
                "type":"stack","box_id":"container_1","selected":1,"children":[
                    {"type":"split","direction":"down","ratio":0.5,"collapsed":true,"first":{"type":"pane","pane_id":"w1:p1"},"second":{"type":"view","pane_id":"w1:p2","extension":"sample","view":"tasks"}},
                    {"type":"pane","pane_id":"w1:p3"}
                ]}}
        })).unwrap();
        let target = box_target(description, "w1:p2").unwrap();
        assert_eq!(target.path, vec![LayoutPathStep::StackChild(0)]);
        assert_eq!(target.boot_id, "boot");
    }
}
