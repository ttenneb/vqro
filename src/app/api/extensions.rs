use crate::api::schema::{
    ExtensionConfirmedParams, ExtensionInfo, ExtensionListParams, ExtensionReorderParams,
    ExtensionRepositoryOverrideInfo, ExtensionRepositoryOverrideParams, ExtensionSetEnabledParams,
    ExtensionUpdateParams, ResponseResult,
};
use crate::app::App;
use crate::extensions::{CuratedExtension, ExtensionPackage};

use super::responses::{encode_error, encode_success};

impl App {
    fn extension_info(&self, extension: &CuratedExtension) -> ExtensionInfo {
        let state = self
            .extension_state
            .entries
            .get(&extension.repository)
            .expect("extension state is normalized against its bundled index");
        let (package_kind, integration_target) = match &extension.package {
            ExtensionPackage::Bundled { .. } => ("bundled", None),
            ExtensionPackage::AgentIntegration { target } => {
                ("agent_integration", Some(target.clone()))
            }
            ExtensionPackage::GithubPlugin { .. } => ("github_plugin", None),
        };
        ExtensionInfo {
            id: extension.id.clone(),
            repository: extension.repository.clone(),
            name: extension.name.clone(),
            summary: extension.summary.clone(),
            details: extension.details.clone(),
            categories: extension.categories.clone(),
            package_kind: package_kind.into(),
            integration_target,
            available_version: extension.version.clone(),
            active_version: state.active_version.clone(),
            installed: state.installed,
            enabled: state.enabled,
            load_order: self
                .extension_state
                .load_order
                .iter()
                .position(|repository| repository == &extension.repository)
                .unwrap_or(usize::MAX),
            pending_version: state.pending_version.clone(),
            rollback_version: state.rollback_version.clone(),
            activation_error: state.activation_error.clone(),
            repository_overrides: state
                .repository_overrides
                .iter()
                .map(|(repository_id, enabled)| ExtensionRepositoryOverrideInfo {
                    repository_id: repository_id.clone(),
                    enabled: *enabled,
                })
                .collect(),
        }
    }

    fn curated_extension(&self, repository: &str) -> Result<CuratedExtension, &'static str> {
        self.extension_index
            .extensions
            .iter()
            .find(|extension| extension.repository == repository)
            .cloned()
            .ok_or("extension_not_found")
    }

    fn save_extension_state(&self) -> Result<(), String> {
        if self.policy.persist_plugin_registry {
            crate::extensions::save_state_to_path(
                &crate::extensions::state_path(),
                &self.extension_state,
            )?;
        }
        Ok(())
    }

    fn apply_bundled_extension_state(&mut self) {
        self.disabled_extensions = crate::extensions::disabled_bundled_providers(
            &self.extension_index,
            &self.extension_state,
        );
        self.rebuild_extension_catalogs();
    }

    fn integration_target(&self, label: &str) -> Option<crate::api::schema::IntegrationTarget> {
        crate::integration::integration_recommendations()
            .into_iter()
            .find(|recommendation| {
                crate::integration::integration_target_label(recommendation.target) == label
            })
            .map(|recommendation| recommendation.target)
    }

    fn activate_curated_extension(&mut self, extension: &CuratedExtension) -> Result<(), String> {
        match &extension.package {
            ExtensionPackage::Bundled { .. } => {
                self.apply_bundled_extension_state();
                Ok(())
            }
            ExtensionPackage::AgentIntegration { target } => {
                let target = self.integration_target(target).ok_or_else(|| {
                    format!("agent integration '{target}' is not available through this endpoint")
                })?;
                crate::integration::install_target(target)
                    .map(|_| ())
                    .map_err(|error| error.to_string())?;
                self.state.integration_recommendations =
                    crate::integration::integration_recommendations();
                Ok(())
            }
            ExtensionPackage::GithubPlugin { .. } => {
                Err("GitHub package installation is not available through extension v1".into())
            }
        }
    }

    fn deactivate_curated_extension(&mut self, extension: &CuratedExtension) -> Result<(), String> {
        match &extension.package {
            ExtensionPackage::Bundled { .. } => {
                self.apply_bundled_extension_state();
                Ok(())
            }
            ExtensionPackage::AgentIntegration { target } => {
                let target = self.integration_target(target).ok_or_else(|| {
                    format!("agent integration '{target}' is not available through this endpoint")
                })?;
                crate::integration::uninstall_target(target)
                    .map(|_| ())
                    .map_err(|error| error.to_string())?;
                self.state.integration_recommendations =
                    crate::integration::integration_recommendations();
                Ok(())
            }
            ExtensionPackage::GithubPlugin { .. } => Ok(()),
        }
    }

    pub(super) fn handle_extension_list(&self, id: String, params: ExtensionListParams) -> String {
        let mut extensions = self
            .extension_index
            .search(
                params.query.as_deref().unwrap_or_default(),
                params.category.as_deref(),
            )
            .into_iter()
            .map(|extension| self.extension_info(extension))
            .filter(|extension| {
                params
                    .installed
                    .is_none_or(|installed| extension.installed == installed)
            })
            .collect::<Vec<_>>();
        extensions.sort_by(|left, right| {
            left.load_order
                .cmp(&right.load_order)
                .then_with(|| left.name.cmp(&right.name))
        });
        encode_success(id, ResponseResult::ExtensionList { extensions })
    }

    pub(super) fn handle_extension_install(
        &mut self,
        id: String,
        params: ExtensionConfirmedParams,
    ) -> String {
        if !params.confirmed {
            return encode_error(
                id,
                "confirmation_required",
                "extension installation requires explicit confirmation",
            );
        }
        let extension = match self.curated_extension(&params.repository) {
            Ok(extension) => extension,
            Err(code) => return encode_error(id, code, "extension is not in the curated index"),
        };
        self.extension_state.install(&extension);
        let activation = self.activate_curated_extension(&extension);
        if let Err(error) = &activation {
            let _ = self
                .extension_state
                .finish_activation(&extension.repository, Err(error.clone()));
        } else {
            let _ = self
                .extension_state
                .finish_activation(&extension.repository, Ok(()));
        }
        if let Err(error) = self.save_extension_state() {
            return encode_error(id, "extension_state_save_failed", error);
        }
        encode_success(
            id,
            ResponseResult::ExtensionChanged {
                extension: self.extension_info(&extension),
            },
        )
    }

    pub(super) fn handle_extension_enable(
        &mut self,
        id: String,
        params: ExtensionSetEnabledParams,
    ) -> String {
        let extension = match self.curated_extension(&params.repository) {
            Ok(extension) => extension,
            Err(code) => return encode_error(id, code, "extension is not in the curated index"),
        };
        if let Err(error) = self
            .extension_state
            .set_enabled(&extension.repository, params.enabled)
        {
            return encode_error(id, "extension_state_invalid", error);
        }
        let activation = if params.enabled {
            self.activate_curated_extension(&extension)
        } else {
            self.deactivate_curated_extension(&extension)
        };
        if let Err(error) = activation {
            let _ = self
                .extension_state
                .finish_activation(&extension.repository, Err(error));
        }
        if let Err(error) = self.save_extension_state() {
            return encode_error(id, "extension_state_save_failed", error);
        }
        encode_success(
            id,
            ResponseResult::ExtensionChanged {
                extension: self.extension_info(&extension),
            },
        )
    }

    pub(super) fn handle_extension_remove(
        &mut self,
        id: String,
        params: ExtensionConfirmedParams,
    ) -> String {
        if !params.confirmed {
            return encode_error(
                id,
                "confirmation_required",
                "extension removal requires explicit confirmation",
            );
        }
        let extension = match self.curated_extension(&params.repository) {
            Ok(extension) => extension,
            Err(code) => return encode_error(id, code, "extension is not in the curated index"),
        };
        if let Err(error) = self.extension_state.remove(&extension.repository) {
            return encode_error(id, "extension_state_invalid", error);
        }
        if let Err(error) = self.deactivate_curated_extension(&extension) {
            tracing::warn!(extension = %extension.id, %error, "extension deactivation failed during removal");
        }
        if let Err(error) = self.save_extension_state() {
            return encode_error(id, "extension_state_save_failed", error);
        }
        encode_success(
            id,
            ResponseResult::ExtensionChanged {
                extension: self.extension_info(&extension),
            },
        )
    }

    pub(super) fn handle_extension_data_delete(
        &mut self,
        id: String,
        params: ExtensionConfirmedParams,
    ) -> String {
        if !params.confirmed {
            return encode_error(
                id,
                "confirmation_required",
                "extension data deletion requires explicit confirmation",
            );
        }
        let extension = match self.curated_extension(&params.repository) {
            Ok(extension) => extension,
            Err(code) => return encode_error(id, code, "extension is not in the curated index"),
        };
        if self.extension_state.entries[&extension.repository].installed {
            return encode_error(
                id,
                "extension_still_installed",
                "remove the extension before deleting its data",
            );
        }
        if let Err(error) = crate::extensions::delete_extension_data(&extension.id) {
            return encode_error(id, "extension_data_delete_failed", error);
        }
        encode_success(
            id,
            ResponseResult::ExtensionDataDeleted {
                repository: extension.repository,
            },
        )
    }

    pub(super) fn handle_extension_reorder(
        &mut self,
        id: String,
        params: ExtensionReorderParams,
    ) -> String {
        let extension = match self.curated_extension(&params.repository) {
            Ok(extension) => extension,
            Err(code) => return encode_error(id, code, "extension is not in the curated index"),
        };
        if let Err(error) = self
            .extension_state
            .move_to(&extension.repository, params.index)
        {
            return encode_error(id, "extension_state_invalid", error);
        }
        if let Err(error) = self.save_extension_state() {
            return encode_error(id, "extension_state_save_failed", error);
        }
        encode_success(
            id,
            ResponseResult::ExtensionChanged {
                extension: self.extension_info(&extension),
            },
        )
    }

    pub(super) fn handle_extension_repository_override_set(
        &mut self,
        id: String,
        params: ExtensionRepositoryOverrideParams,
    ) -> String {
        let extension = match self.curated_extension(&params.repository) {
            Ok(extension) => extension,
            Err(code) => return encode_error(id, code, "extension is not in the curated index"),
        };
        if let Err(error) = self.extension_state.set_repository_override(
            &extension.repository,
            params.repository_id,
            params.enabled,
        ) {
            return encode_error(id, "extension_state_invalid", error);
        }
        if let Err(error) = self.save_extension_state() {
            return encode_error(id, "extension_state_save_failed", error);
        }
        encode_success(
            id,
            ResponseResult::ExtensionChanged {
                extension: self.extension_info(&extension),
            },
        )
    }

    pub(super) fn handle_extension_update(
        &mut self,
        id: String,
        params: ExtensionUpdateParams,
    ) -> String {
        let extension = match self.curated_extension(&params.repository) {
            Ok(extension) => extension,
            Err(code) => return encode_error(id, code, "extension is not in the curated index"),
        };
        if let Err(error) = self
            .extension_state
            .stage_update(&extension.repository, params.version)
        {
            return encode_error(id, "extension_update_invalid", error);
        }
        if params.approved {
            if let Err(error) = self.extension_state.approve_update(&extension.repository) {
                return encode_error(id, "extension_update_invalid", error);
            }
            if let Err(error) = self.activate_curated_extension(&extension) {
                let _ = self.extension_state.rollback(&extension.repository);
                let _ = self
                    .extension_state
                    .finish_activation(&extension.repository, Err(error));
            }
        }
        if let Err(error) = self.save_extension_state() {
            return encode_error(id, "extension_state_save_failed", error);
        }
        encode_success(
            id,
            ResponseResult::ExtensionChanged {
                extension: self.extension_info(&extension),
            },
        )
    }

    pub(super) fn handle_extension_rollback(
        &mut self,
        id: String,
        params: ExtensionConfirmedParams,
    ) -> String {
        if !params.confirmed {
            return encode_error(
                id,
                "confirmation_required",
                "extension rollback requires explicit confirmation",
            );
        }
        let extension = match self.curated_extension(&params.repository) {
            Ok(extension) => extension,
            Err(code) => return encode_error(id, code, "extension is not in the curated index"),
        };
        if let Err(error) = self.extension_state.rollback(&extension.repository) {
            return encode_error(id, "extension_rollback_unavailable", error);
        }
        if let Err(error) = self.activate_curated_extension(&extension) {
            let _ = self
                .extension_state
                .finish_activation(&extension.repository, Err(error));
        }
        if let Err(error) = self.save_extension_state() {
            return encode_error(id, "extension_state_save_failed", error);
        }
        encode_success(
            id,
            ResponseResult::ExtensionChanged {
                extension: self.extension_info(&extension),
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::schema::{Method, Request, SuccessResponse};

    fn test_app() -> App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        App::new(
            &crate::config::Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        )
    }

    fn result(response: &str) -> ResponseResult {
        serde_json::from_str::<SuccessResponse>(response)
            .expect("success response")
            .result
    }

    #[test]
    fn extension_list_exposes_exact_default_installed_set() {
        let mut app = test_app();
        let response = app.handle_api_request(Request {
            id: "extensions".into(),
            method: Method::ExtensionList(ExtensionListParams::default()),
        });
        let ResponseResult::ExtensionList { extensions } = result(&response) else {
            panic!("extension list response");
        };
        let installed = extensions
            .iter()
            .filter(|extension| extension.installed && extension.enabled)
            .map(|extension| extension.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            installed,
            [
                "Collections",
                "Spaces",
                "Delegation",
                "Agent Status",
                "Pi",
                "Claude Code",
                "Codex"
            ]
        );
        assert!(extensions
            .iter()
            .find(|extension| extension.name == "OMP")
            .is_some_and(|extension| !extension.installed));
    }

    #[test]
    fn bundled_removal_requires_confirmation_and_is_reinstallable() {
        let mut app = test_app();
        let repository = "ttenneb/vqro-extension-collections".to_string();
        let rejected: serde_json::Value = serde_json::from_str(&app.handle_api_request(Request {
            id: "remove".into(),
            method: Method::ExtensionRemove(ExtensionConfirmedParams {
                repository: repository.clone(),
                confirmed: false,
            }),
        }))
        .unwrap();
        assert_eq!(rejected["error"]["code"], "confirmation_required");

        let removed = app.handle_api_request(Request {
            id: "remove".into(),
            method: Method::ExtensionRemove(ExtensionConfirmedParams {
                repository: repository.clone(),
                confirmed: true,
            }),
        });
        assert!(matches!(
            result(&removed),
            ResponseResult::ExtensionChanged {
                extension: ExtensionInfo {
                    installed: false,
                    enabled: false,
                    ..
                }
            }
        ));
        assert!(app.disabled_extensions.contains("vqro.collections"));

        let installed = app.handle_api_request(Request {
            id: "install".into(),
            method: Method::ExtensionInstall(ExtensionConfirmedParams {
                repository,
                confirmed: true,
            }),
        });
        assert!(matches!(
            result(&installed),
            ResponseResult::ExtensionChanged {
                extension: ExtensionInfo {
                    installed: true,
                    enabled: true,
                    ..
                }
            }
        ));
        assert!(!app.disabled_extensions.contains("vqro.collections"));
    }
}
