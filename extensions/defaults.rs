//! Distribution catalog, not installed-package discovery or capability admission.
//! The one compiled launch-profile entry is temporary and owner-approved; its
//! removal requires the approved replacement described in the architecture note.
#[path = "vqro-pi/profile.rs"]
mod launch_profile;
#[path = "tasks/bundle.rs"]
mod tasks;

pub(crate) static BUNDLES: &[crate::bundled::DefaultBundle] =
    &[tasks::BUNDLE, launch_profile::BUNDLE];
pub(crate) static LAUNCH_PROFILES: &[crate::agent_profiles::AgentLaunchProfile] =
    &[crate::agent_profiles::AgentLaunchProfile {
        prepare: launch_profile::ensure_configured_profile,
        apply_pane_env: launch_profile::apply_pane_env,
        launch: launch_profile::launch,
    }];
