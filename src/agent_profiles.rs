//! Neutral seam for the temporarily admitted compiled launch-profile adapter.
//! Preparation writes only at plain startup. Selection and notices remain fresh
//! read-only projections at their existing runtime seams; never cache success.
use portable_pty::CommandBuilder;
use std::io;

#[derive(Debug)]
pub(crate) struct ProfileLaunch {
    pub(crate) argv: Vec<String>,
    pub(crate) notice: Option<&'static str>,
}

pub(crate) struct AgentLaunchProfile {
    pub(crate) prepare: fn(&crate::config::Config) -> io::Result<()>,
    pub(crate) apply_pane_env: fn(&crate::config::Config, &mut CommandBuilder),
    pub(crate) launch: fn(&crate::config::Config, &str) -> io::Result<Option<ProfileLaunch>>,
}

pub(crate) fn prepare(config: &crate::config::Config) -> io::Result<()> {
    for profile in crate::default_extensions::LAUNCH_PROFILES {
        (profile.prepare)(config)?;
    }
    Ok(())
}

pub(crate) fn apply_pane_env(command: &mut CommandBuilder) {
    let config = crate::config::Config::load().config;
    for profile in crate::default_extensions::LAUNCH_PROFILES {
        (profile.apply_pane_env)(&config, command);
    }
}

/// One config snapshot binds argv, environment and notice at the launch boundary.
/// The trusted profile owns agent matching; no installed manifest supplies this.
pub(crate) fn launch(
    config: &crate::config::Config,
    executable: &str,
) -> io::Result<ProfileLaunch> {
    for profile in crate::default_extensions::LAUNCH_PROFILES {
        if let Some(launch) = (profile.launch)(config, executable)? {
            return Ok(launch);
        }
    }
    Ok(ProfileLaunch {
        argv: vec![executable.into()],
        notice: None,
    })
}
