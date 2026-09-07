//! Central product identity and mutable-runtime namespace.
//!
//! Keep upstream compatibility names out of this module. Everything here names
//! state owned by this binary so vqro can coexist with an installed Herdr.

pub(crate) const NAME: &str = "vqro";
pub(crate) const DISPLAY_NAME: &str = "vqro";
pub(crate) const APP_DIR_NAME: &str = "vqro";
pub(crate) const DEV_APP_DIR_NAME: &str = "vqro-dev";
pub(crate) const API_SOCKET_FILE: &str = "vqro.sock";
pub(crate) const CLIENT_SOCKET_FILE: &str = "vqro-client.sock";
pub(crate) const LOG_FILE: &str = "vqro.log";
pub(crate) const CLIENT_LOG_FILE: &str = "vqro-client.log";
pub(crate) const SERVER_LOG_FILE: &str = "vqro-server.log";

pub(crate) const ENV_MARKER: &str = "VQRO_ENV";
pub(crate) const CONFIG_PATH_ENV: &str = "VQRO_CONFIG_PATH";
pub(crate) const SESSION_ENV: &str = "VQRO_SESSION";
pub(crate) const SOCKET_PATH_ENV: &str = "VQRO_SOCKET_PATH";
pub(crate) const CLIENT_SOCKET_PATH_ENV: &str = "VQRO_CLIENT_SOCKET_PATH";
pub(crate) const LOG_ENV: &str = "VQRO_LOG";
pub(crate) const PANE_ID_ENV: &str = "VQRO_PANE_ID";
pub(crate) const TAB_ID_ENV: &str = "VQRO_TAB_ID";
pub(crate) const WORKSPACE_ID_ENV: &str = "VQRO_WORKSPACE_ID";
pub(crate) const BIN_PATH_ENV: &str = "VQRO_BIN_PATH";

/// No vqro release feed exists during the alpha identity bootstrap.
pub(crate) const SELF_UPDATE_ENABLED: bool = false;

pub(crate) fn app_dir_name(debug: bool) -> &'static str {
    if debug {
        DEV_APP_DIR_NAME
    } else {
        APP_DIR_NAME
    }
}

pub(crate) fn command(args: &str) -> String {
    if args.is_empty() {
        NAME.to_string()
    } else {
        format!("{NAME} {args}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mutable_namespace_does_not_collide_with_upstream() {
        for value in [
            APP_DIR_NAME,
            DEV_APP_DIR_NAME,
            API_SOCKET_FILE,
            CLIENT_SOCKET_FILE,
            LOG_FILE,
            CLIENT_LOG_FILE,
            SERVER_LOG_FILE,
        ] {
            assert!(!value.starts_with("herdr"), "{value}");
        }
        for value in [
            ENV_MARKER,
            CONFIG_PATH_ENV,
            SESSION_ENV,
            SOCKET_PATH_ENV,
            CLIENT_SOCKET_PATH_ENV,
            LOG_ENV,
            PANE_ID_ENV,
            TAB_ID_ENV,
            WORKSPACE_ID_ENV,
            BIN_PATH_ENV,
        ] {
            assert!(value.starts_with("VQRO_"), "{value}");
        }
    }

    #[test]
    fn inherited_self_update_is_quarantined() {
        const { assert!(!SELF_UPDATE_ENABLED) };
    }

    #[test]
    fn release_and_debug_directories_are_distinct() {
        assert_eq!(app_dir_name(false), "vqro");
        assert_eq!(app_dir_name(true), "vqro-dev");
        assert_ne!(app_dir_name(false), app_dir_name(true));
    }
}
