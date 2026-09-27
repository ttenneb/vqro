//! Bounded product contracts for the ordinary Extension Doctor composition.
//!
//! The host supplies facts only. The Health provider owns policy and the Doctor
//! consumer owns the stable diagnostic presentation.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub(crate) const DOCTOR_PACKAGE_ID: &str = "vqro.extension-doctor";
pub(crate) const DOCTOR_SERVICE_ID: &str = "doctor";
pub(crate) const DOCTOR_IMPORT_ID: &str = "health-policy";
pub(crate) const DOCTOR_METHOD: &str = "doctor";
pub(crate) const HEALTH_CONTRACT_ID: &str = "vqro.extension-health";
pub(crate) const HEALTH_CONTRACT_VERSION: &str = "1.0.0";
pub(crate) const HEALTH_DESCRIPTOR_SHA256: &str =
    "23aec77a84334a070d1de2e918db9ac0a084c9af2332b99b5824cde0156326e0";
pub(crate) const HEALTH_METHOD: &str = "evaluate";
pub(crate) const OBSERVATION_CONTRACT_V1: &str = "vqro.extension-health-observation.v1";
pub(crate) const DIAGNOSTIC_CONTRACT_V1: &str = "vqro.extension-doctor-diagnostic.v1";
pub(crate) const MAX_OBSERVATION_BYTES: usize = 16 * 1024;
pub(crate) const MAX_SERVICE_COUNT: u64 = (crate::extension_package::MAX_INSTALLED_PACKAGES
    * crate::extension_package::MAX_SERVICES) as u64;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExtensionHealthPackageCountsV1 {
    pub disabled: u64,
    pub enabled_ready: u64,
    pub enabled_unavailable: u64,
    pub total: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExtensionHealthServiceCountsV1 {
    pub ready: u64,
    pub unavailable: u64,
    pub total: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExtensionHealthBindingCountsV1 {
    pub ready: u64,
    pub suspended: u64,
    pub revoked: u64,
    pub total: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExtensionHealthDoctorEdgeV1 {
    pub binding_id: String,
    pub binding_revision: u64,
    pub consumer_package_sha256: String,
    pub consumer_manifest_sha256: String,
    pub provider_package_sha256: String,
    pub provider_manifest_sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExtensionHealthObservationV1 {
    pub contract: String,
    pub registry_revision: u64,
    pub packages: ExtensionHealthPackageCountsV1,
    pub services: ExtensionHealthServiceCountsV1,
    pub bindings: ExtensionHealthBindingCountsV1,
    pub doctor_edge: ExtensionHealthDoctorEdgeV1,
}

impl ExtensionHealthObservationV1 {
    pub(crate) fn validate(&self) -> Result<(), &'static str> {
        if self.contract != OBSERVATION_CONTRACT_V1 {
            return Err("unsupported extension health observation contract");
        }
        validate_sum(
            self.packages.total,
            &[
                self.packages.disabled,
                self.packages.enabled_ready,
                self.packages.enabled_unavailable,
            ],
            crate::extension_package::MAX_INSTALLED_PACKAGES as u64,
        )?;
        validate_sum(
            self.services.total,
            &[self.services.ready, self.services.unavailable],
            MAX_SERVICE_COUNT,
        )?;
        validate_sum(
            self.bindings.total,
            &[
                self.bindings.ready,
                self.bindings.suspended,
                self.bindings.revoked,
            ],
            crate::service_fabric::MAX_SERVICE_BINDINGS as u64,
        )?;
        if self.doctor_edge.binding_id.is_empty()
            || self.doctor_edge.binding_id.len() > 128
            || self.doctor_edge.binding_revision == 0
        {
            return Err("invalid Extension Doctor binding identity");
        }
        for digest in [
            &self.doctor_edge.consumer_package_sha256,
            &self.doctor_edge.consumer_manifest_sha256,
            &self.doctor_edge.provider_package_sha256,
            &self.doctor_edge.provider_manifest_sha256,
        ] {
            if !is_sha256(digest) {
                return Err("invalid Extension Doctor artifact digest");
            }
        }
        let bytes = serde_json::to_vec(self).map_err(|_| "cannot encode extension observation")?;
        if bytes.len() > MAX_OBSERVATION_BYTES {
            return Err("extension health observation exceeds its byte limit");
        }
        Ok(())
    }
}

fn validate_sum(total: u64, parts: &[u64], maximum: u64) -> Result<(), &'static str> {
    if total > maximum {
        return Err("extension health count exceeds its limit");
    }
    let sum = parts.iter().try_fold(0_u64, |sum, value| {
        if *value > maximum {
            None
        } else {
            sum.checked_add(*value)
        }
    });
    if sum != Some(total) {
        return Err("extension health counts are inconsistent");
    }
    Ok(())
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionDoctorStatusV1 {
    Healthy,
    Degraded,
    Unhealthy,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionDoctorSeverityV1 {
    Warning,
    Error,
}

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionDoctorFindingCodeV1 {
    BindingRevoked,
    BindingSuspended,
    PackageDisabled,
    PackageUnavailable,
    ServiceUnavailable,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExtensionDoctorFindingV1 {
    pub code: ExtensionDoctorFindingCodeV1,
    pub severity: ExtensionDoctorSeverityV1,
    pub count: u64,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExtensionDoctorDiagnosticV1 {
    pub contract: String,
    pub status: ExtensionDoctorStatusV1,
    pub findings: Vec<ExtensionDoctorFindingV1>,
}

impl ExtensionDoctorDiagnosticV1 {
    pub(crate) fn validate(&self) -> Result<(), &'static str> {
        if self.contract != DIAGNOSTIC_CONTRACT_V1 || self.findings.len() > 5 {
            return Err("invalid Extension Doctor diagnostic contract");
        }
        let mut previous = None;
        let mut has_warning = false;
        let mut has_error = false;
        for finding in &self.findings {
            if finding.count == 0 || finding.count > MAX_SERVICE_COUNT {
                return Err("invalid Extension Doctor finding count");
            }
            if previous.is_some_and(|code| code >= finding.code) {
                return Err("Extension Doctor findings are not unique and ordered");
            }
            if finding.message != finding_message(finding.code, finding.count) {
                return Err("Extension Doctor finding message is not canonical");
            }
            match finding.severity {
                ExtensionDoctorSeverityV1::Warning => has_warning = true,
                ExtensionDoctorSeverityV1::Error => has_error = true,
            }
            previous = Some(finding.code);
        }
        let expected = if has_error {
            ExtensionDoctorStatusV1::Unhealthy
        } else if has_warning {
            ExtensionDoctorStatusV1::Degraded
        } else {
            ExtensionDoctorStatusV1::Healthy
        };
        if self.status != expected {
            return Err("Extension Doctor status is inconsistent with findings");
        }
        Ok(())
    }
}

pub(crate) fn finding_message(code: ExtensionDoctorFindingCodeV1, count: u64) -> String {
    let suffix = match code {
        ExtensionDoctorFindingCodeV1::BindingRevoked => "extension service bindings are revoked.",
        ExtensionDoctorFindingCodeV1::BindingSuspended => {
            "extension service bindings are suspended."
        }
        ExtensionDoctorFindingCodeV1::PackageDisabled => "extension packages are disabled.",
        ExtensionDoctorFindingCodeV1::PackageUnavailable => {
            "enabled extension packages are unavailable."
        }
        ExtensionDoctorFindingCodeV1::ServiceUnavailable => "extension services are unavailable.",
    };
    format!("{count} {suffix}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest() -> String {
        "a".repeat(64)
    }

    fn observation() -> ExtensionHealthObservationV1 {
        ExtensionHealthObservationV1 {
            contract: OBSERVATION_CONTRACT_V1.into(),
            registry_revision: 7,
            packages: ExtensionHealthPackageCountsV1 {
                disabled: 1,
                enabled_ready: 1,
                enabled_unavailable: 1,
                total: 3,
            },
            services: ExtensionHealthServiceCountsV1 {
                ready: 1,
                unavailable: 1,
                total: 2,
            },
            bindings: ExtensionHealthBindingCountsV1 {
                ready: 1,
                suspended: 1,
                revoked: 1,
                total: 3,
            },
            doctor_edge: ExtensionHealthDoctorEdgeV1 {
                binding_id: "sb_doctor".into(),
                binding_revision: 1,
                consumer_package_sha256: digest(),
                consumer_manifest_sha256: digest(),
                provider_package_sha256: digest(),
                provider_manifest_sha256: digest(),
            },
        }
    }

    #[test]
    fn checked_in_local_package_contract_has_frozen_identity() {
        let bytes = include_bytes!("../docs/next/api/vqro-extension-health-contract-v1.json");
        let (descriptor, digest) =
            crate::extension_package::parse_service_contract_descriptor(bytes).unwrap();
        assert_eq!(descriptor.contract_id, HEALTH_CONTRACT_ID);
        assert_eq!(descriptor.contract_version, HEALTH_CONTRACT_VERSION);
        assert_eq!(descriptor.methods.len(), 1);
        assert_eq!(descriptor.methods[0].id, HEALTH_METHOD);
        assert_eq!(digest, HEALTH_DESCRIPTOR_SHA256);
    }

    #[test]
    fn observation_is_bounded_and_rejects_arithmetic_inconsistency() {
        let mut value = observation();
        assert!(value.validate().is_ok());
        value.packages.total = u64::MAX;
        assert_eq!(
            value.validate(),
            Err("extension health count exceeds its limit")
        );
        value = observation();
        value.bindings.total = 2;
        assert_eq!(
            value.validate(),
            Err("extension health counts are inconsistent")
        );
    }

    #[test]
    fn diagnostic_rejects_noncanonical_presentation() {
        let mut diagnostic = ExtensionDoctorDiagnosticV1 {
            contract: DIAGNOSTIC_CONTRACT_V1.into(),
            status: ExtensionDoctorStatusV1::Degraded,
            findings: vec![ExtensionDoctorFindingV1 {
                code: ExtensionDoctorFindingCodeV1::PackageDisabled,
                severity: ExtensionDoctorSeverityV1::Warning,
                count: 2,
                message: finding_message(ExtensionDoctorFindingCodeV1::PackageDisabled, 2),
            }],
        };
        assert!(diagnostic.validate().is_ok());
        diagnostic.findings[0].message = "/secret/raw error".into();
        assert_eq!(
            diagnostic.validate(),
            Err("Extension Doctor finding message is not canonical")
        );
    }
}
