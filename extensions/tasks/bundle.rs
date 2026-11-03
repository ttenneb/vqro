//! Compiled distribution declaration owned by Tasks, not host workflow policy.
use crate::bundled::{DefaultBundle, DefaultPlugin};

/// Tasks owns its separately composed Pi consumer resource as well.
pub(crate) const PROFILE_RESOURCE: (&str, &str) = ("tasks-pi.ts", include_str!("pi.ts"));

pub(crate) const BUNDLE: DefaultBundle = DefaultBundle {
    prefix: "tasks",
    assets: &[
        ("vqro-extension.toml", include_str!("vqro-extension.toml")),
        ("provider.py", include_str!("provider.py")),
        ("view.py", include_str!("view.py")),
        ("document.py", include_str!("document.py")),
        ("decision_queue.py", include_str!("decision_queue.py")),
        ("messages.py", include_str!("messages.py")),
        ("graph.py", include_str!("graph.py")),
        ("ids.py", include_str!("ids.py")),
        ("cli.py", include_str!("cli.py")),
        ("local_platform.py", include_str!("local_platform.py")),
        ("windows_api.py", include_str!("windows_api.py")),
    ],
    plugin: Some(DefaultPlugin {
        id: "vqro.tasks",
        manifest: "vqro-extension.toml",
        legacy_directory: "tasks",
    }),
};

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    fn owned_generation(root: &std::path::Path, registered: &str) -> bool {
        crate::bundled::owned_generation(
            root,
            registered,
            super::BUNDLE.prefix,
            super::BUNDLE.plugin.unwrap().legacy_directory,
        )
    }

    #[test]
    fn owned_generation_matches_only_our_addresses() {
        let root = PathBuf::from("/cfg/bundled");
        assert!(owned_generation(
            &root,
            "/cfg/bundled/tasks-9547e32d79df4a85"
        ));
        assert!(owned_generation(&root, "/cfg/bundled/tasks"));
        assert!(!owned_generation(&root, "/cfg/bundled/my-dev-checkout"));
        assert!(!owned_generation(&root, "/cfg/bundled/tasks-nothex"));
        assert!(!owned_generation(
            &root,
            "/cfg/bundled/tasks-9547e32d79df4a8"
        ));
        assert!(!owned_generation(
            &root,
            "/elsewhere/tasks-9547e32d79df4a85"
        ));
    }
}
