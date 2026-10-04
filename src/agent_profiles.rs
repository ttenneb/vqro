//! Neutral seam for the temporarily admitted compiled launch-profile adapter.
//! Preparation writes only at plain startup. Selection and notices remain fresh
//! read-only projections at their existing runtime seams; never cache success.
use portable_pty::CommandBuilder;
use std::io;

pub(crate) struct AgentLaunchProfile {
    pub(crate) prepare: fn() -> io::Result<()>,
    pub(crate) apply_pane_env: fn(&mut CommandBuilder),
    pub(crate) notice: fn(&str) -> Option<&'static str>,
}

pub(crate) fn prepare() -> io::Result<()> {
    for profile in crate::default_extensions::LAUNCH_PROFILES {
        (profile.prepare)()?;
    }
    Ok(())
}

pub(crate) fn apply_pane_env(command: &mut CommandBuilder) {
    for profile in crate::default_extensions::LAUNCH_PROFILES {
        (profile.apply_pane_env)(command);
    }
}

pub(crate) fn notice(executable: &str) -> Option<&'static str> {
    crate::default_extensions::LAUNCH_PROFILES
        .iter()
        .find_map(|profile| (profile.notice)(executable))
}
