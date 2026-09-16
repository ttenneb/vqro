use crate::api::schema::{
    IntegrationInfo, IntegrationInstallResult, IntegrationState, IntegrationUninstallResult,
    ResponseResult,
};
use crate::app::App;

use super::responses::{encode_error, encode_success};

const INTEGRATION_MUTATION_DISABLED: &str =
    "vqro integration changes are disabled until integrations use the vqro namespace";

fn integration_mutation_disabled(id: String) -> String {
    encode_error(id, "feature_disabled", INTEGRATION_MUTATION_DISABLED)
}

impl App {
    fn integration_is_marketplace_managed(
        &self,
        path: &std::path::Path,
        target: crate::api::schema::IntegrationTarget,
    ) -> Result<bool, String> {
        if crate::extensions::pending_extension_lifecycle(path)?.is_some() {
            return Err(
                "a marketplace integration recovery is still pending; retry shortly".into(),
            );
        }
        let state = crate::extensions::load_state_from_path(path, &self.extension_index)?;
        Ok(self.extension_index.extensions.iter().any(|extension| {
            matches!(
                &extension.package,
                crate::extensions::ExtensionPackage::AgentIntegration { target: extension_target }
                    if extension_target.supported() == Some(target)
            ) && state
                .entries
                .get(&extension.repository)
                .is_some_and(|entry| entry.installed)
        }))
    }

    pub(super) fn handle_integration_list(&self, id: String) -> String {
        let integrations = crate::integration::integration_recommendations()
            .into_iter()
            .map(|recommendation| IntegrationInfo {
                target: recommendation.target,
                label: recommendation.label.to_owned(),
                command: recommendation.command.to_owned(),
                available: recommendation.available,
                state: match recommendation.state {
                    crate::integration::IntegrationStatusKind::NotInstalled => {
                        IntegrationState::NotInstalled
                    }
                    crate::integration::IntegrationStatusKind::Current => IntegrationState::Current,
                    crate::integration::IntegrationStatusKind::Outdated => {
                        IntegrationState::Outdated
                    }
                },
            })
            .collect();
        encode_success(id, ResponseResult::IntegrationList { integrations })
    }

    pub(super) fn handle_integration_install(
        &mut self,
        id: String,
        params: crate::api::schema::IntegrationInstallParams,
    ) -> String {
        let target = params.target;
        if !crate::integration::integration_mutation_enabled(target) {
            return integration_mutation_disabled(id);
        }
        let state_path = match crate::extensions::state_storage_path() {
            Ok(path) => path,
            Err(error) => return encode_error(id, "extension_state_lock_failed", error),
        };
        let _extension_lock = match crate::extensions::try_lock_state_file(&state_path) {
            Ok(Some(lock)) => lock,
            Ok(None) => {
                return encode_error(
                    id,
                    "extension_state_lock_failed",
                    "extension state is busy; retry shortly",
                );
            }
            Err(error) => return encode_error(id, "extension_state_lock_failed", error),
        };
        match self.integration_is_marketplace_managed(&state_path, target) {
            Ok(true) => {
                return encode_error(
                    id,
                    "integration_managed_by_extension",
                    "use the Extensions marketplace to change this managed integration",
                );
            }
            Ok(false) => {}
            Err(error) => return encode_error(id, "extension_state_unreadable", error),
        }
        let messages = match crate::integration::install_target(target) {
            Ok(messages) => messages,
            Err(err) => return encode_error(id, "integration_install_failed", err.to_string()),
        };
        self.state.integration_recommendations = crate::integration::integration_recommendations();

        encode_success(
            id,
            ResponseResult::IntegrationInstall {
                target,
                details: IntegrationInstallResult { messages },
            },
        )
    }

    pub(super) fn handle_integration_uninstall(
        &mut self,
        id: String,
        params: crate::api::schema::IntegrationUninstallParams,
    ) -> String {
        let target = params.target;
        if !crate::integration::integration_mutation_enabled(target) {
            return integration_mutation_disabled(id);
        }
        let state_path = match crate::extensions::state_storage_path() {
            Ok(path) => path,
            Err(error) => return encode_error(id, "extension_state_lock_failed", error),
        };
        let _extension_lock = match crate::extensions::try_lock_state_file(&state_path) {
            Ok(Some(lock)) => lock,
            Ok(None) => {
                return encode_error(
                    id,
                    "extension_state_lock_failed",
                    "extension state is busy; retry shortly",
                );
            }
            Err(error) => return encode_error(id, "extension_state_lock_failed", error),
        };
        match self.integration_is_marketplace_managed(&state_path, target) {
            Ok(true) => {
                return encode_error(
                    id,
                    "integration_managed_by_extension",
                    "use the Extensions marketplace to change this managed integration",
                );
            }
            Ok(false) => {}
            Err(error) => return encode_error(id, "extension_state_unreadable", error),
        }
        let messages = match crate::integration::uninstall_target(target) {
            Ok(messages) => messages,
            Err(err) => return encode_error(id, "integration_uninstall_failed", err.to_string()),
        };
        self.state.integration_recommendations = crate::integration::integration_recommendations();

        encode_success(
            id,
            ResponseResult::IntegrationUninstall {
                target,
                details: IntegrationUninstallResult { messages },
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_namespaced_integration_mutations_are_disabled_with_the_cli_guard() {
        let response: serde_json::Value =
            serde_json::from_str(&integration_mutation_disabled("request".into()))
                .expect("error response");
        assert_eq!(response["id"], "request");
        assert_eq!(response["error"]["code"], "feature_disabled");
        assert_eq!(response["error"]["message"], INTEGRATION_MUTATION_DISABLED);
    }
}
