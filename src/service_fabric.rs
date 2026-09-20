//! Durable authority for exact package-to-package service bindings.
//!
//! This module owns consent and persistence only. Runtime invocation leases are
//! deliberately separate and are never serialized.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc,
};
use std::time::Instant;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::extension_package::{
    ServiceContractDescriptorV1, ValidatedPackageV1, MAX_INFORMATION_CATEGORIES,
    MAX_SERVICE_CALL_BYTES,
};

pub(crate) const SERVICE_BINDING_REGISTRY_V1: &str = "vqro.service-binding-registry.v1";
pub(crate) const MAX_SERVICE_BINDINGS: usize = 4_096;
pub(crate) const MAX_PENDING_SERVICE_FABRIC_INVOCATIONS: usize = 64;
pub(crate) const SERVICE_READ_METHOD: &str = "service.read.call";
pub(crate) const SERVICE_READ_CAPABILITY: &str = "service.read";
pub(crate) const SERVICE_READ_CALL_V1: &str = "vqro.service-read-call.v1";
pub(crate) const ERROR_BINDING_UNAVAILABLE: &str = "service_binding_unavailable";
pub(crate) const ERROR_BINDING_REVOKED: &str = "service_binding_revoked";
pub(crate) const ERROR_METHOD_DENIED: &str = "service_method_denied";
pub(crate) const ERROR_REQUEST_INVALID: &str = "service_request_invalid";
pub(crate) const ERROR_CALL_BUSY: &str = "service_call_busy";
pub(crate) const ERROR_CALL_TIMEOUT: &str = "service_call_timeout";
pub(crate) const ERROR_CALL_CANCELED: &str = "service_call_canceled";
pub(crate) const ERROR_STALE_LEASE: &str = "stale_service_call_lease";
pub(crate) const ERROR_PROVIDER_FAILED: &str = "service_provider_failed";
pub(crate) const ERROR_RESPONSE_INVALID: &str = "service_response_invalid";
pub(crate) const STABLE_R1_ERROR_CODES: [&str; 10] = [
    ERROR_BINDING_UNAVAILABLE,
    ERROR_BINDING_REVOKED,
    ERROR_METHOD_DENIED,
    ERROR_REQUEST_INVALID,
    ERROR_CALL_BUSY,
    ERROR_CALL_TIMEOUT,
    ERROR_CALL_CANCELED,
    ERROR_STALE_LEASE,
    ERROR_PROVIDER_FAILED,
    ERROR_RESPONSE_INVALID,
];
static NEXT_SERVICE_CALL_ATTEMPT: AtomicU64 = AtomicU64::new(1);
static NEXT_SERVICE_FABRIC_OUTER_ATTEMPT: AtomicU64 = AtomicU64::new(1);
pub(crate) const SERVICE_FABRIC_INVOCATION_TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(2);

pub(crate) fn stable_r1_error_code(code: &str) -> &str {
    if STABLE_R1_ERROR_CODES.contains(&code) {
        code
    } else {
        ERROR_PROVIDER_FAILED
    }
}

pub(crate) fn next_service_fabric_outer_attempt_id() -> Result<u64, String> {
    let sequence = NEXT_SERVICE_FABRIC_OUTER_ATTEMPT.fetch_add(1, Ordering::Relaxed);
    if sequence == 0 || sequence >= (1_u64 << 63) {
        return Err("service fabric outer attempt identity is exhausted".into());
    }
    Ok((1_u64 << 63) | sequence)
}

pub(crate) struct PendingServiceFabricInvocation {
    pub(crate) request_id: String,
    pub(crate) binding_id: String,
    pub(crate) binding_revision: u64,
    pub(crate) consumer_package_id: String,
    pub(crate) consumer_service_id: String,
    pub(crate) consumer_artifact_sha256: String,
    pub(crate) consumer_artifact_generation: u64,
    pub(crate) consumer_runtime_generation: u64,
    pub(crate) provider_package_id: String,
    pub(crate) provider_service_id: String,
    pub(crate) provider_artifact_sha256: String,
    pub(crate) provider_artifact_generation: u64,
    pub(crate) provider_runtime_generation: u64,
    pub(crate) canceled: Arc<AtomicBool>,
    pub(crate) respond_to: std::sync::mpsc::Sender<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ServiceBindingRegistryV1 {
    pub contract: String,
    pub revision: u64,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub bindings: BTreeMap<String, ServiceBindingV1>,
}

impl Default for ServiceBindingRegistryV1 {
    fn default() -> Self {
        Self {
            contract: SERVICE_BINDING_REGISTRY_V1.into(),
            revision: 0,
            bindings: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ServiceEndpointV1 {
    pub package_id: String,
    pub package_version: String,
    pub package_sha256: String,
    pub manifest_sha256: String,
    pub service_id: String,
    pub endpoint_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ServiceContractIdentityV1 {
    pub contract_id: String,
    pub contract_version: String,
    pub descriptor_sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ServiceBindingMethodV1 {
    pub id: String,
    pub request_information: Vec<String>,
    pub response_information: Vec<String>,
    pub max_request_bytes: u64,
    pub max_response_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ServiceBindingConsentV1 {
    pub consumer: ServiceEndpointV1,
    pub provider: ServiceEndpointV1,
    pub contract: ServiceContractIdentityV1,
    pub methods: Vec<ServiceBindingMethodV1>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ServiceBindingStateV1 {
    Approved,
    Revoked,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ServiceBindingV1 {
    pub binding_id: String,
    pub revision: u64,
    pub state: ServiceBindingStateV1,
    pub consent_sha256: String,
    pub consent: ServiceBindingConsentV1,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EffectiveServiceBindingStatusV1 {
    Ready,
    Suspended,
    Revoked,
}

impl ServiceBindingRegistryV1 {
    pub(crate) fn validate(&self) -> Result<(), String> {
        if self.contract != SERVICE_BINDING_REGISTRY_V1 {
            return Err("unsupported service binding registry contract".into());
        }
        if self.bindings.len() > MAX_SERVICE_BINDINGS {
            return Err("service binding registry exceeds its binding limit".into());
        }
        for (binding_id, binding) in &self.bindings {
            validate_binding(binding)?;
            if binding_id != &binding.binding_id || binding.revision > self.revision {
                return Err("service binding registry key or revision is inconsistent".into());
            }
        }
        Ok(())
    }

    pub(crate) fn approve(
        &mut self,
        consent: ServiceBindingConsentV1,
        expected_registry_revision: u64,
        expected_binding_revision: Option<u64>,
        expected_consent_sha256: &str,
        confirmed: bool,
    ) -> Result<&ServiceBindingV1, String> {
        self.validate()?;
        if !confirmed {
            return Err("service binding approval requires explicit confirmation".into());
        }
        if expected_registry_revision != self.revision {
            return Err(format!(
                "service binding registry revision conflict: {}",
                self.revision
            ));
        }
        validate_consent(&consent)?;
        let binding_id = binding_id(&consent.consumer)?;
        let actual_consent_sha256 = consent_sha256(&consent)?;
        validate_digest(expected_consent_sha256, "service binding consent")?;
        if actual_consent_sha256 != expected_consent_sha256 {
            return Err("service binding consent digest mismatch".into());
        }
        match (self.bindings.get(&binding_id), expected_binding_revision) {
            (None, None) => {}
            (Some(current), Some(expected)) if current.revision == expected => {}
            (Some(current), _) => {
                return Err(format!(
                    "service binding revision conflict: {}",
                    current.revision
                ));
            }
            (None, Some(_)) => return Err("service binding does not exist".into()),
        }
        self.revision = self
            .revision
            .checked_add(1)
            .ok_or("service binding registry revision exhausted")?;
        let binding = ServiceBindingV1 {
            binding_id: binding_id.clone(),
            revision: self.revision,
            state: ServiceBindingStateV1::Approved,
            consent_sha256: actual_consent_sha256,
            consent,
        };
        self.bindings.insert(binding_id.clone(), binding);
        Ok(&self.bindings[&binding_id])
    }

    pub(crate) fn revoke(
        &mut self,
        binding_id: &str,
        expected_registry_revision: u64,
        expected_binding_revision: u64,
        confirmed: bool,
    ) -> Result<&ServiceBindingV1, String> {
        self.validate()?;
        if !confirmed {
            return Err("service binding revocation requires explicit confirmation".into());
        }
        if expected_registry_revision != self.revision {
            return Err(format!(
                "service binding registry revision conflict: {}",
                self.revision
            ));
        }
        let current = self
            .bindings
            .get(binding_id)
            .ok_or_else(|| "service binding does not exist".to_string())?;
        if current.revision != expected_binding_revision {
            return Err(format!(
                "service binding revision conflict: {}",
                current.revision
            ));
        }
        self.revision = self
            .revision
            .checked_add(1)
            .ok_or("service binding registry revision exhausted")?;
        let binding = self.bindings.get_mut(binding_id).expect("binding exists");
        binding.revision = self.revision;
        binding.state = ServiceBindingStateV1::Revoked;
        Ok(binding)
    }
}

pub(crate) fn consent_for_binding(
    consumer: &ValidatedPackageV1,
    consumer_service_id: &str,
    import_id: &str,
    provider: &ValidatedPackageV1,
    provider_service_id: &str,
    export_id: &str,
) -> Result<ServiceBindingConsentV1, String> {
    if consumer.manifest.id == provider.manifest.id {
        return Err("R1 service bindings cannot call the same package".into());
    }
    if consumer.manifest.manifest_version != 3 || provider.manifest.manifest_version != 3 {
        return Err("service bindings require manifest-v3 packages".into());
    }
    let consumer_service = consumer
        .manifest
        .services
        .iter()
        .find(|service| service.id == consumer_service_id)
        .ok_or_else(|| "consumer service does not exist".to_string())?;
    let import = consumer_service
        .imports
        .iter()
        .find(|import| import.id == import_id)
        .ok_or_else(|| "consumer import does not exist".to_string())?;
    let provider_service = provider
        .manifest
        .services
        .iter()
        .find(|service| service.id == provider_service_id)
        .ok_or_else(|| "provider service does not exist".to_string())?;
    let export = provider_service
        .exports
        .iter()
        .find(|export| export.id == export_id)
        .ok_or_else(|| "provider export does not exist".to_string())?;
    if import.descriptor_sha256 != export.descriptor_sha256 {
        return Err("consumer and provider contract descriptors do not match".into());
    }
    let consumer_descriptor = consumer
        .contract_descriptors
        .get(&import.descriptor_sha256)
        .ok_or_else(|| "consumer contract descriptor is absent".to_string())?;
    let provider_descriptor = provider
        .contract_descriptors
        .get(&export.descriptor_sha256)
        .ok_or_else(|| "provider contract descriptor is absent".to_string())?;
    if consumer_descriptor != provider_descriptor {
        return Err("consumer and provider contract identities do not match".into());
    }
    consent_from_descriptor(
        consumer,
        consumer_service_id,
        import_id,
        provider,
        provider_service_id,
        export_id,
        &import.descriptor_sha256,
        consumer_descriptor,
    )
}

fn consent_from_descriptor(
    consumer: &ValidatedPackageV1,
    consumer_service_id: &str,
    import_id: &str,
    provider: &ValidatedPackageV1,
    provider_service_id: &str,
    export_id: &str,
    descriptor_sha256: &str,
    descriptor: &ServiceContractDescriptorV1,
) -> Result<ServiceBindingConsentV1, String> {
    let consent = ServiceBindingConsentV1 {
        consumer: endpoint(consumer, consumer_service_id, import_id),
        provider: endpoint(provider, provider_service_id, export_id),
        contract: ServiceContractIdentityV1 {
            contract_id: descriptor.contract_id.clone(),
            contract_version: descriptor.contract_version.clone(),
            descriptor_sha256: descriptor_sha256.into(),
        },
        methods: descriptor
            .methods
            .iter()
            .map(|method| ServiceBindingMethodV1 {
                id: method.id.clone(),
                request_information: method.request_information.clone(),
                response_information: method.response_information.clone(),
                max_request_bytes: method.max_request_bytes,
                max_response_bytes: method.max_response_bytes,
            })
            .collect(),
    };
    validate_consent(&consent)?;
    Ok(consent)
}

fn endpoint(
    package: &ValidatedPackageV1,
    service_id: &str,
    endpoint_id: &str,
) -> ServiceEndpointV1 {
    ServiceEndpointV1 {
        package_id: package.manifest.id.clone(),
        package_version: package.manifest.version.clone(),
        package_sha256: package.package_sha256.clone(),
        manifest_sha256: package.manifest_sha256.clone(),
        service_id: service_id.into(),
        endpoint_id: endpoint_id.into(),
    }
}

pub(crate) fn consent_sha256(consent: &ServiceBindingConsentV1) -> Result<String, String> {
    validate_consent(consent)?;
    let bytes = serde_json::to_vec(consent)
        .map_err(|error| format!("encode service binding consent: {error}"))?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

pub(crate) fn binding_id(consumer: &ServiceEndpointV1) -> Result<String, String> {
    validate_endpoint(consumer)?;
    let logical = format!(
        "{}\0{}\0{}",
        consumer.package_id, consumer.service_id, consumer.endpoint_id
    );
    Ok(format!("binding_{:x}", Sha256::digest(logical.as_bytes())))
}

pub(crate) fn effective_status(
    binding: &ServiceBindingV1,
    consumer_artifact_active: bool,
    provider_artifact_active: bool,
    consumer_worker_ready: bool,
    provider_worker_ready: bool,
) -> EffectiveServiceBindingStatusV1 {
    if binding.state == ServiceBindingStateV1::Revoked {
        return EffectiveServiceBindingStatusV1::Revoked;
    }
    if consumer_artifact_active
        && provider_artifact_active
        && consumer_worker_ready
        && provider_worker_ready
    {
        EffectiveServiceBindingStatusV1::Ready
    } else {
        EffectiveServiceBindingStatusV1::Suspended
    }
}

fn validate_binding(binding: &ServiceBindingV1) -> Result<(), String> {
    if binding.revision == 0 || binding.binding_id != binding_id(&binding.consent.consumer)? {
        return Err("service binding identity or revision is invalid".into());
    }
    validate_digest(&binding.consent_sha256, "service binding consent")?;
    if binding.consent_sha256 != consent_sha256(&binding.consent)? {
        return Err("service binding consent digest is invalid".into());
    }
    validate_consent(&binding.consent)
}

fn validate_consent(consent: &ServiceBindingConsentV1) -> Result<(), String> {
    validate_endpoint(&consent.consumer)?;
    validate_endpoint(&consent.provider)?;
    if consent.consumer.package_id == consent.provider.package_id {
        return Err("R1 service bindings cannot call the same package".into());
    }
    crate::extension_package::validate_package_id(&consent.contract.contract_id)?;
    validate_digest(
        &consent.contract.descriptor_sha256,
        "service contract descriptor",
    )?;
    if crate::update::Version::parse(&consent.contract.contract_version).is_none() {
        return Err("service contract version is invalid".into());
    }
    if consent.methods.is_empty() || consent.methods.len() > 32 {
        return Err("service binding method count is invalid".into());
    }
    let mut methods = BTreeSet::new();
    for method in &consent.methods {
        validate_bounded_id(&method.id, "service method")?;
        if !methods.insert(method.id.as_str()) {
            return Err("service binding contains duplicate methods".into());
        }
        validate_categories(&method.request_information)?;
        validate_categories(&method.response_information)?;
        if method.max_request_bytes == 0
            || method.max_response_bytes == 0
            || method.max_request_bytes > MAX_SERVICE_CALL_BYTES
            || method.max_response_bytes > MAX_SERVICE_CALL_BYTES
        {
            return Err("service binding method byte bound is invalid".into());
        }
    }
    Ok(())
}

fn validate_endpoint(endpoint: &ServiceEndpointV1) -> Result<(), String> {
    crate::extension_package::validate_package_id(&endpoint.package_id)?;
    if crate::update::Version::parse(&endpoint.package_version).is_none() {
        return Err("service endpoint package version is invalid".into());
    }
    validate_digest(&endpoint.package_sha256, "service endpoint package")?;
    validate_digest(&endpoint.manifest_sha256, "service endpoint manifest")?;
    validate_bounded_id(&endpoint.service_id, "service id")?;
    validate_bounded_id(&endpoint.endpoint_id, "service endpoint id")
}

fn validate_categories(values: &[String]) -> Result<(), String> {
    if values.len() > MAX_INFORMATION_CATEGORIES {
        return Err("service binding has too many information categories".into());
    }
    let mut categories = BTreeSet::new();
    for value in values {
        crate::extension_package::validate_package_id(value)?;
        if !categories.insert(value) {
            return Err("service binding contains duplicate information categories".into());
        }
    }
    Ok(())
}

fn validate_bounded_id(value: &str, label: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(format!("{label} is invalid"));
    }
    Ok(())
}

fn validate_digest(value: &str, label: &str) -> Result<(), String> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(format!("{label} is not lowercase SHA-256"));
    }
    Ok(())
}

#[derive(Clone)]
pub(crate) struct ServiceReadInvocationAuthorization {
    binding: ServiceBindingV1,
    binding_registry_revision: u64,
    consumer_artifact_generation: u64,
    consumer_runtime_generation: u64,
    provider: crate::extension_candidate::ServiceReadProviderEndpoint,
    revoked: Arc<AtomicBool>,
}

impl ServiceReadInvocationAuthorization {
    #[allow(dead_code)] // Minted by the experimental fabric composition path.
    pub(crate) fn new(
        binding: ServiceBindingV1,
        binding_registry_revision: u64,
        consumer_artifact_sha256: &str,
        consumer_artifact_generation: u64,
        consumer_runtime_generation: u64,
        provider: crate::extension_candidate::ServiceReadProviderEndpoint,
        revoked: Arc<AtomicBool>,
    ) -> Result<Self, String> {
        validate_binding(&binding)?;
        if binding.state != ServiceBindingStateV1::Approved
            || binding_registry_revision == 0
            || consumer_artifact_sha256 != binding.consent.consumer.package_sha256
            || consumer_artifact_generation == 0
            || consumer_runtime_generation == 0
            || provider.artifact_generation() == 0
            || provider.runtime_generation() == 0
            || provider.package_id() != binding.consent.provider.package_id
            || provider.service_id() != binding.consent.provider.service_id
            || provider.artifact_sha256() != binding.consent.provider.package_sha256
        {
            return Err("service-read authorization does not match its exact binding".into());
        }
        Ok(Self {
            binding,
            binding_registry_revision,
            consumer_artifact_generation,
            consumer_runtime_generation,
            provider,
            revoked,
        })
    }

    pub(crate) fn bind(
        self,
        outer_request_id: String,
        identity: crate::extension_runtime::RuntimeIdentityV1,
        deadline: Instant,
        outer_canceled: Arc<AtomicBool>,
    ) -> Result<BoundServiceReadInvocation, crate::extension_runtime::RuntimeError> {
        if outer_request_id.is_empty()
            || Instant::now() >= deadline
            || outer_canceled.load(Ordering::Acquire)
            || self.revoked.load(Ordering::Acquire)
            || identity.package_id != self.binding.consent.consumer.package_id
            || identity.service_id != self.binding.consent.consumer.service_id
            || identity.generation != self.consumer_runtime_generation
            || self.binding_registry_revision == 0
            || self.consumer_artifact_generation == 0
        {
            return Err(crate::extension_runtime::RuntimeError::Configuration(
                "service-read authorization is stale or does not match the consumer runtime".into(),
            ));
        }
        Ok(BoundServiceReadInvocation {
            identity,
            outer_request_id,
            import_id: self.binding.consent.consumer.endpoint_id.clone(),
            contract: self.binding.consent.contract,
            methods: self
                .binding
                .consent
                .methods
                .into_iter()
                .map(|method| (method.id.clone(), method))
                .collect(),
            binding_id: self.binding.binding_id,
            binding_revision: self.binding.revision,
            binding_registry_revision: self.binding_registry_revision,
            consumer_artifact_generation: self.consumer_artifact_generation,
            consumer_runtime_generation: self.consumer_runtime_generation,
            provider: self.provider,
            revoked: self.revoked,
            deadline,
            outer_canceled,
            used: AtomicBool::new(false),
        })
    }
}

pub(crate) struct BoundServiceReadInvocation {
    identity: crate::extension_runtime::RuntimeIdentityV1,
    outer_request_id: String,
    import_id: String,
    contract: ServiceContractIdentityV1,
    methods: BTreeMap<String, ServiceBindingMethodV1>,
    binding_id: String,
    binding_revision: u64,
    binding_registry_revision: u64,
    consumer_artifact_generation: u64,
    consumer_runtime_generation: u64,
    provider: crate::extension_candidate::ServiceReadProviderEndpoint,
    revoked: Arc<AtomicBool>,
    deadline: Instant,
    outer_canceled: Arc<AtomicBool>,
    used: AtomicBool,
}

impl BoundServiceReadInvocation {
    pub(crate) fn identity(&self) -> crate::extension_runtime::RuntimeIdentityV1 {
        self.identity.clone()
    }

    pub(crate) fn namespace(&self) -> String {
        self.identity.namespace.clone()
    }

    pub(crate) fn allowed_methods(&self) -> BTreeMap<String, String> {
        BTreeMap::from([(SERVICE_READ_METHOD.into(), SERVICE_READ_CAPABILITY.into())])
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ServiceReadCallParamsV1 {
    contract: String,
    import_id: String,
    method: String,
    payload: serde_json::Value,
}

impl crate::extension_runtime::RuntimeHostCallHandler for BoundServiceReadInvocation {
    fn handle_host_call(
        &self,
        call: crate::extension_runtime::RuntimeHostCallV1,
    ) -> crate::extension_runtime::RuntimeHostCallResponseV1 {
        use crate::extension_runtime::{RuntimeErrorBodyV1, RuntimeHostCallResponseV1};
        let respond = |result: Option<serde_json::Value>, code: Option<&str>, message: &str| {
            RuntimeHostCallResponseV1 {
                kind: "host_response".into(),
                call_id: call.call_id.clone(),
                identity: self.identity.clone(),
                result,
                error: code.map(|code| RuntimeErrorBodyV1 {
                    code: code.into(),
                    message: message.into(),
                }),
            }
        };
        if self
            .used
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return respond(
                None,
                Some("service_call_busy"),
                "service-read lease was already used",
            );
        }
        if call.method != SERVICE_READ_METHOD
            || call.capability != SERVICE_READ_CAPABILITY
            || call.identity != self.identity
            || call.depth != crate::extension_runtime::MAX_HOST_CALL_DEPTH
            || self.outer_canceled.load(Ordering::Acquire)
            || self.revoked.load(Ordering::Acquire)
            || Instant::now() >= self.deadline
        {
            return respond(
                None,
                Some("stale_service_call_lease"),
                "service-read lease is stale or invalid",
            );
        }
        let params: ServiceReadCallParamsV1 = match serde_json::from_value(call.params) {
            Ok(params) => params,
            Err(_) => {
                return respond(
                    None,
                    Some("service_request_invalid"),
                    "service-read request envelope is invalid",
                );
            }
        };
        if params.contract != SERVICE_READ_CALL_V1 || params.import_id != self.import_id {
            return respond(
                None,
                Some("service_binding_unavailable"),
                "service-read request does not match the approved import",
            );
        }
        let Some(method) = self.methods.get(&params.method) else {
            return respond(
                None,
                Some("service_method_denied"),
                "service-read method is not approved",
            );
        };
        let request_bytes = match serde_json::to_vec(&params.payload) {
            Ok(bytes)
                if u64::try_from(bytes.len()).unwrap_or(u64::MAX) <= method.max_request_bytes =>
            {
                bytes.len()
            }
            _ => {
                return respond(
                    None,
                    Some("service_request_invalid"),
                    "service-read request exceeds its approved bound",
                );
            }
        };
        let attempt_id = NEXT_SERVICE_CALL_ATTEMPT.fetch_add(1, Ordering::Relaxed);
        if attempt_id == 0 {
            return respond(
                None,
                Some("service_provider_failed"),
                "service-read attempt identity is exhausted",
            );
        }
        tracing::debug!(
            binding_id = %self.binding_id,
            binding_revision = self.binding_revision,
            binding_registry_revision = self.binding_registry_revision,
            contract_id = %self.contract.contract_id,
            contract_version = %self.contract.contract_version,
            descriptor_sha256 = %self.contract.descriptor_sha256,
            method = %params.method,
            attempt_id,
            consumer_artifact_generation = self.consumer_artifact_generation,
            consumer_runtime_generation = self.consumer_runtime_generation,
            provider_artifact_generation = self.provider.artifact_generation(),
            provider_runtime_generation = self.provider.runtime_generation(),
            request_bytes,
            outer_request_id = %self.outer_request_id,
            "service-read call admitted"
        );
        let attempt_canceled = Arc::new(AtomicBool::new(false));
        let result = self.provider.call(
            attempt_id,
            params.method,
            params.payload,
            self.deadline,
            Arc::clone(&attempt_canceled),
            self.outer_canceled.as_ref(),
            self.revoked.as_ref(),
        );
        if !self.provider.is_current() {
            return respond(
                None,
                Some("stale_service_call_lease"),
                "service provider was retired",
            );
        }
        if self.outer_canceled.load(Ordering::Acquire) {
            return respond(
                None,
                Some("service_call_canceled"),
                "service-read call was canceled",
            );
        }
        if self.revoked.load(Ordering::Acquire) {
            return respond(
                None,
                Some("stale_service_call_lease"),
                "service binding was revoked",
            );
        }
        if Instant::now() >= self.deadline {
            return respond(
                None,
                Some("service_call_timeout"),
                "service-read call timed out",
            );
        }
        match result {
            Ok(value) => match serde_json::to_vec(&value) {
                Ok(bytes)
                    if u64::try_from(bytes.len()).unwrap_or(u64::MAX)
                        <= method.max_response_bytes =>
                {
                    tracing::debug!(
                        binding_id = %self.binding_id,
                        attempt_id,
                        response_bytes = bytes.len(),
                        "service-read call completed"
                    );
                    respond(Some(value), None, "")
                }
                _ => respond(
                    None,
                    Some("service_response_invalid"),
                    "service provider response exceeds its approved bound",
                ),
            },
            Err(error) => {
                let (code, message) = if error.contains("queue is full") {
                    ("service_call_busy", "service provider is busy")
                } else if error.contains("timed out") {
                    ("service_call_timeout", "service-read call timed out")
                } else if error.contains("canceled") || error.contains("retired") {
                    ("service_call_canceled", "service-read call was canceled")
                } else {
                    ("service_provider_failed", "service provider failed")
                };
                respond(None, Some(code), message)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extension_package::{
        PackageRuntimeV1, ServiceContractExportV1, ServiceContractImportV1,
        ServiceContractMethodV1, ValidatedPackageManifestV1, ValidatedPackageServiceV1,
    };

    #[test]
    fn stable_r1_error_taxonomy_is_closed_and_normalizes_unknown_failures() {
        assert_eq!(
            STABLE_R1_ERROR_CODES,
            [
                "service_binding_unavailable",
                "service_binding_revoked",
                "service_method_denied",
                "service_request_invalid",
                "service_call_busy",
                "service_call_timeout",
                "service_call_canceled",
                "stale_service_call_lease",
                "service_provider_failed",
                "service_response_invalid",
            ]
        );
        for code in STABLE_R1_ERROR_CODES {
            assert_eq!(stable_r1_error_code(code), code);
        }
        assert_eq!(
            stable_r1_error_code("internal_detail"),
            ERROR_PROVIDER_FAILED
        );
    }

    fn package(id: &str, sha: char, import: bool) -> ValidatedPackageV1 {
        let descriptor_sha256 = "a".repeat(64);
        let descriptor = ServiceContractDescriptorV1 {
            contract: crate::extension_package::SERVICE_CONTRACT_V1.into(),
            contract_id: "example.echo".into(),
            contract_version: "1.0.0".into(),
            methods: vec![ServiceContractMethodV1 {
                id: "echo".into(),
                request_schema: serde_json::json!({}),
                response_schema: serde_json::json!({}),
                request_information: vec!["user.supplied.text".into()],
                response_information: vec!["provider.generated.text".into()],
                max_request_bytes: 256,
                max_response_bytes: 512,
            }],
        };
        ValidatedPackageV1 {
            contract: crate::extension_package::PACKAGE_CONTRACT_V1.into(),
            package_sha256: sha.to_string().repeat(64),
            manifest_sha256: sha
                .to_ascii_uppercase()
                .to_string()
                .repeat(64)
                .to_ascii_lowercase(),
            manifest: ValidatedPackageManifestV1 {
                manifest_version: 3,
                package_contract: crate::extension_package::PACKAGE_CONTRACT_V1.into(),
                id: id.into(),
                version: "1.0.0".into(),
                min_vqro_version: "0.1.0".into(),
                namespaces: vec![id.into()],
                services: vec![ValidatedPackageServiceV1 {
                    id: "main".into(),
                    provides: Vec::new(),
                    capabilities: Vec::new(),
                    exports: (!import)
                        .then(|| ServiceContractExportV1 {
                            id: "echo".into(),
                            descriptor: "contracts/echo.json".into(),
                            descriptor_sha256: descriptor_sha256.clone(),
                        })
                        .into_iter()
                        .collect(),
                    imports: import
                        .then(|| ServiceContractImportV1 {
                            id: "echo-source".into(),
                            descriptor: "contracts/echo.json".into(),
                            descriptor_sha256: descriptor_sha256.clone(),
                            required: true,
                        })
                        .into_iter()
                        .collect(),
                    runtime: PackageRuntimeV1::Component {
                        component: "services/main.wasm".into(),
                        world: "vqro:extension/service@1.0.0".into(),
                    },
                }],
            },
            contract_descriptors: BTreeMap::from([(descriptor_sha256, descriptor)]),
            files: Vec::new(),
        }
    }

    #[test]
    fn exact_binding_approval_is_cas_guarded_and_revocable() {
        let consumer = package("example.consumer", '1', true);
        let provider = package("example.provider", '2', false);
        let consent =
            consent_for_binding(&consumer, "main", "echo-source", &provider, "main", "echo")
                .unwrap();
        let digest = consent_sha256(&consent).unwrap();
        let mut registry = ServiceBindingRegistryV1::default();
        assert!(registry
            .approve(consent.clone(), 0, None, &digest, false)
            .is_err());
        let binding = registry
            .approve(consent, 0, None, &digest, true)
            .unwrap()
            .clone();
        assert_eq!(binding.revision, 1);
        assert_eq!(
            effective_status(&binding, true, true, true, true),
            EffectiveServiceBindingStatusV1::Ready
        );
        assert_eq!(
            effective_status(&binding, true, true, false, true),
            EffectiveServiceBindingStatusV1::Suspended
        );
        let revoked = registry
            .revoke(&binding.binding_id, 1, 1, true)
            .unwrap()
            .clone();
        assert_eq!(revoked.state, ServiceBindingStateV1::Revoked);
        assert_eq!(
            effective_status(&revoked, true, true, true, true),
            EffectiveServiceBindingStatusV1::Revoked
        );
    }

    fn approved_binding() -> (ServiceBindingV1, ValidatedPackageV1, ValidatedPackageV1) {
        let consumer = package("example.consumer", '1', true);
        let provider = package("example.provider", '2', false);
        let consent =
            consent_for_binding(&consumer, "main", "echo-source", &provider, "main", "echo")
                .unwrap();
        let digest = consent_sha256(&consent).unwrap();
        let mut registry = ServiceBindingRegistryV1::default();
        let binding = registry
            .approve(consent, 0, None, &digest, true)
            .unwrap()
            .clone();
        (binding, consumer, provider)
    }

    #[test]
    fn service_read_lease_selects_provider_once_and_bounds_payloads() {
        let (binding, consumer, provider) = approved_binding();
        let provider_endpoint = crate::extension_candidate::ServiceReadProviderEndpoint::for_test(
            &provider.manifest.id,
            "main",
            &provider.package_sha256,
            13,
            17,
            |method, value| {
                if method != "echo" {
                    return Err("unexpected method".into());
                }
                Ok(serde_json::json!({"echoed": value}))
            },
        );
        let revoked = Arc::new(AtomicBool::new(false));
        let authorization = ServiceReadInvocationAuthorization::new(
            binding,
            1,
            &consumer.package_sha256,
            11,
            7,
            provider_endpoint,
            Arc::clone(&revoked),
        )
        .unwrap();
        let identity = crate::extension_runtime::RuntimeIdentityV1 {
            package_id: consumer.manifest.id.clone(),
            namespace: consumer.manifest.id.clone(),
            service_id: "main".into(),
            generation: 7,
        };
        let deadline = Instant::now() + std::time::Duration::from_secs(1);
        let bound = authorization
            .bind(
                "outer-1".into(),
                identity.clone(),
                deadline,
                Arc::new(AtomicBool::new(false)),
            )
            .unwrap();
        let router = crate::extension_runtime::RuntimeHostCallRouter::new(
            bound.identity(),
            bound.namespace(),
            bound.allowed_methods(),
            bound,
        );
        let call = crate::extension_runtime::RuntimeHostCallV1 {
            kind: "host_call".into(),
            call_id: "call-1".into(),
            identity: identity.clone(),
            depth: crate::extension_runtime::MAX_HOST_CALL_DEPTH,
            capability: SERVICE_READ_CAPABILITY.into(),
            method: SERVICE_READ_METHOD.into(),
            namespace: identity.namespace.clone(),
            params: serde_json::json!({
                "contract": SERVICE_READ_CALL_V1,
                "import_id": "echo-source",
                "method": "echo",
                "payload": {"text": "hello"}
            }),
        };
        assert_eq!(
            router.handle(call.clone()).result.unwrap(),
            serde_json::json!({"echoed": {"text": "hello"}})
        );
        assert_eq!(router.handle(call).error.unwrap().code, "service_call_busy");
    }

    #[test]
    fn service_read_timeout_and_response_limits_fail_closed() {
        fn router(
            binding: ServiceBindingV1,
            consumer: &ValidatedPackageV1,
            endpoint: crate::extension_candidate::ServiceReadProviderEndpoint,
            deadline: Instant,
        ) -> (
            crate::extension_runtime::RuntimeHostCallRouter,
            crate::extension_runtime::RuntimeIdentityV1,
        ) {
            let authorization = ServiceReadInvocationAuthorization::new(
                binding,
                1,
                &consumer.package_sha256,
                11,
                7,
                endpoint,
                Arc::new(AtomicBool::new(false)),
            )
            .unwrap();
            let identity = crate::extension_runtime::RuntimeIdentityV1 {
                package_id: consumer.manifest.id.clone(),
                namespace: consumer.manifest.id.clone(),
                service_id: "main".into(),
                generation: 7,
            };
            let bound = authorization
                .bind(
                    "outer-bounds".into(),
                    identity.clone(),
                    deadline,
                    Arc::new(AtomicBool::new(false)),
                )
                .unwrap();
            (
                crate::extension_runtime::RuntimeHostCallRouter::new(
                    bound.identity(),
                    bound.namespace(),
                    bound.allowed_methods(),
                    bound,
                ),
                identity,
            )
        }
        fn call(
            identity: crate::extension_runtime::RuntimeIdentityV1,
            payload: serde_json::Value,
        ) -> crate::extension_runtime::RuntimeHostCallV1 {
            crate::extension_runtime::RuntimeHostCallV1 {
                kind: "host_call".into(),
                call_id: "call-bounds".into(),
                identity: identity.clone(),
                depth: crate::extension_runtime::MAX_HOST_CALL_DEPTH,
                capability: SERVICE_READ_CAPABILITY.into(),
                method: SERVICE_READ_METHOD.into(),
                namespace: identity.namespace,
                params: serde_json::json!({
                    "contract": SERVICE_READ_CALL_V1,
                    "import_id": "echo-source",
                    "method": "echo",
                    "payload": payload
                }),
            }
        }

        let (binding, consumer, provider) = approved_binding();
        let slow = crate::extension_candidate::ServiceReadProviderEndpoint::for_test(
            &provider.manifest.id,
            "main",
            &provider.package_sha256,
            13,
            17,
            |_method, value| {
                std::thread::sleep(std::time::Duration::from_millis(500));
                Ok(value)
            },
        );
        let started = Instant::now();
        let (slow_router, identity) = router(
            binding.clone(),
            &consumer,
            slow,
            Instant::now() + std::time::Duration::from_millis(50),
        );
        assert_eq!(
            slow_router
                .handle(call(identity, serde_json::json!({})))
                .error
                .unwrap()
                .code,
            "service_call_timeout"
        );
        assert!(started.elapsed() < std::time::Duration::from_millis(300));

        let oversized = crate::extension_candidate::ServiceReadProviderEndpoint::for_test(
            &provider.manifest.id,
            "main",
            &provider.package_sha256,
            13,
            17,
            |_method, _value| Ok(serde_json::json!({"text": "x".repeat(600)})),
        );
        let (oversized_router, identity) = router(
            binding,
            &consumer,
            oversized,
            Instant::now() + std::time::Duration::from_secs(1),
        );
        assert_eq!(
            oversized_router
                .handle(call(identity, serde_json::json!({})))
                .error
                .unwrap()
                .code,
            "service_response_invalid"
        );
    }

    #[test]
    fn revocation_cancels_an_inflight_service_read_without_app_completion() {
        let (binding, consumer, provider) = approved_binding();
        let endpoint = crate::extension_candidate::ServiceReadProviderEndpoint::for_test(
            &provider.manifest.id,
            "main",
            &provider.package_sha256,
            13,
            17,
            |_method, value| {
                std::thread::sleep(std::time::Duration::from_secs(1));
                Ok(value)
            },
        );
        let revoked = Arc::new(AtomicBool::new(false));
        let authorization = ServiceReadInvocationAuthorization::new(
            binding,
            1,
            &consumer.package_sha256,
            11,
            7,
            endpoint,
            Arc::clone(&revoked),
        )
        .unwrap();
        let identity = crate::extension_runtime::RuntimeIdentityV1 {
            package_id: consumer.manifest.id.clone(),
            namespace: consumer.manifest.id,
            service_id: "main".into(),
            generation: 7,
        };
        let bound = authorization
            .bind(
                "outer-revoke".into(),
                identity.clone(),
                Instant::now() + std::time::Duration::from_secs(2),
                Arc::new(AtomicBool::new(false)),
            )
            .unwrap();
        let router = crate::extension_runtime::RuntimeHostCallRouter::new(
            bound.identity(),
            bound.namespace(),
            bound.allowed_methods(),
            bound,
        );
        let join = std::thread::spawn(move || {
            router.handle(crate::extension_runtime::RuntimeHostCallV1 {
                kind: "host_call".into(),
                call_id: "call-revoke".into(),
                identity: identity.clone(),
                depth: crate::extension_runtime::MAX_HOST_CALL_DEPTH,
                capability: SERVICE_READ_CAPABILITY.into(),
                method: SERVICE_READ_METHOD.into(),
                namespace: identity.namespace,
                params: serde_json::json!({
                    "contract": SERVICE_READ_CALL_V1,
                    "import_id": "echo-source",
                    "method": "echo",
                    "payload": {}
                }),
            })
        });
        std::thread::sleep(std::time::Duration::from_millis(30));
        revoked.store(true, Ordering::Release);
        let response = join.join().unwrap();
        assert_eq!(response.error.unwrap().code, "stale_service_call_lease");
    }

    #[test]
    fn consumer_retirement_cancels_an_inflight_service_read() {
        let (binding, consumer, provider) = approved_binding();
        let endpoint = crate::extension_candidate::ServiceReadProviderEndpoint::for_test(
            &provider.manifest.id,
            "main",
            &provider.package_sha256,
            13,
            17,
            |_method, value| {
                std::thread::sleep(std::time::Duration::from_secs(1));
                Ok(value)
            },
        );
        let authorization = ServiceReadInvocationAuthorization::new(
            binding,
            1,
            &consumer.package_sha256,
            11,
            7,
            endpoint,
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        let identity = crate::extension_runtime::RuntimeIdentityV1 {
            package_id: consumer.manifest.id.clone(),
            namespace: consumer.manifest.id,
            service_id: "main".into(),
            generation: 7,
        };
        let consumer_retired = Arc::new(AtomicBool::new(false));
        let bound = authorization
            .bind(
                "outer-consumer-retire".into(),
                identity.clone(),
                Instant::now() + std::time::Duration::from_secs(2),
                Arc::clone(&consumer_retired),
            )
            .unwrap();
        let router = crate::extension_runtime::RuntimeHostCallRouter::new(
            bound.identity(),
            bound.namespace(),
            bound.allowed_methods(),
            bound,
        );
        let join = std::thread::spawn(move || {
            router.handle(crate::extension_runtime::RuntimeHostCallV1 {
                kind: "host_call".into(),
                call_id: "call-consumer-retire".into(),
                identity: identity.clone(),
                depth: crate::extension_runtime::MAX_HOST_CALL_DEPTH,
                capability: SERVICE_READ_CAPABILITY.into(),
                method: SERVICE_READ_METHOD.into(),
                namespace: identity.namespace,
                params: serde_json::json!({
                    "contract": SERVICE_READ_CALL_V1,
                    "import_id": "echo-source",
                    "method": "echo",
                    "payload": {}
                }),
            })
        });
        std::thread::sleep(std::time::Duration::from_millis(30));
        consumer_retired.store(true, Ordering::Release);
        assert_eq!(
            join.join().unwrap().error.unwrap().code,
            "service_call_canceled"
        );
    }

    #[test]
    fn provider_retirement_after_dispatch_rejects_the_completion() {
        let (started_tx, started_rx) = std::sync::mpsc::sync_channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
        let endpoint = crate::extension_candidate::ServiceReadProviderEndpoint::for_test(
            "example.provider",
            "main",
            &"2".repeat(64),
            13,
            17,
            move |_method, value| {
                let _ = started_tx.send(());
                let _ = release_rx.recv();
                Ok(value)
            },
        );
        let caller = endpoint.clone();
        let join = std::thread::spawn(move || {
            caller.call(
                1,
                "echo".into(),
                serde_json::json!({}),
                Instant::now() + std::time::Duration::from_secs(1),
                Arc::new(AtomicBool::new(false)),
                &AtomicBool::new(false),
                &AtomicBool::new(false),
            )
        });
        started_rx.recv().unwrap();
        endpoint.retire_for_test();
        release_tx.send(()).unwrap();
        assert!(join.join().unwrap().unwrap_err().contains("cancel"));
    }

    #[test]
    fn provider_queue_saturation_fails_immediately_without_an_unbounded_waiter() {
        let (started_tx, started_rx) = std::sync::mpsc::sync_channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
        let endpoint =
            crate::extension_candidate::ServiceReadProviderEndpoint::for_test_with_capacity(
                "example.provider",
                "main",
                &"2".repeat(64),
                13,
                17,
                0,
                move |_method, value| {
                    let _ = started_tx.send(());
                    let _ = release_rx.recv();
                    Ok(value)
                },
            );
        let first = endpoint.clone();
        let join = std::thread::spawn(move || {
            first.call(
                1,
                "echo".into(),
                serde_json::json!({}),
                Instant::now() + std::time::Duration::from_secs(1),
                Arc::new(AtomicBool::new(false)),
                &AtomicBool::new(false),
                &AtomicBool::new(false),
            )
        });
        started_rx.recv().unwrap();
        let started = Instant::now();
        let error = endpoint
            .call(
                2,
                "echo".into(),
                serde_json::json!({}),
                Instant::now() + std::time::Duration::from_secs(1),
                Arc::new(AtomicBool::new(false)),
                &AtomicBool::new(false),
                &AtomicBool::new(false),
            )
            .unwrap_err();
        assert_eq!(error, "service provider queue is full");
        assert!(started.elapsed() < std::time::Duration::from_millis(100));
        release_tx.send(()).unwrap();
        assert!(join.join().unwrap().is_ok());
    }

    #[test]
    fn service_read_revocation_and_wrong_artifact_fail_closed() {
        let (binding, consumer, provider) = approved_binding();
        let wrong_endpoint = crate::extension_candidate::ServiceReadProviderEndpoint::for_test(
            &provider.manifest.id,
            "main",
            &"3".repeat(64),
            13,
            17,
            |_method, value| Ok(value),
        );
        assert!(ServiceReadInvocationAuthorization::new(
            binding.clone(),
            1,
            &consumer.package_sha256,
            11,
            7,
            wrong_endpoint,
            Arc::new(AtomicBool::new(false)),
        )
        .is_err());

        let endpoint = crate::extension_candidate::ServiceReadProviderEndpoint::for_test(
            &provider.manifest.id,
            "main",
            &provider.package_sha256,
            13,
            17,
            |_method, value| Ok(value),
        );
        let revoked = Arc::new(AtomicBool::new(true));
        let authorization = ServiceReadInvocationAuthorization::new(
            binding,
            1,
            &consumer.package_sha256,
            11,
            7,
            endpoint,
            revoked,
        )
        .unwrap();
        assert!(authorization
            .bind(
                "outer-2".into(),
                crate::extension_runtime::RuntimeIdentityV1 {
                    package_id: consumer.manifest.id.clone(),
                    namespace: consumer.manifest.id,
                    service_id: "main".into(),
                    generation: 7,
                },
                Instant::now() + std::time::Duration::from_secs(1),
                Arc::new(AtomicBool::new(false)),
            )
            .is_err());
    }

    #[test]
    fn self_binding_is_rejected_before_runtime_authority_exists() {
        let consumer = package("example.same", '1', true);
        let provider = package("example.same", '2', false);
        assert!(
            consent_for_binding(&consumer, "main", "echo-source", &provider, "main", "echo")
                .unwrap_err()
                .contains("same package")
        );
    }

    #[test]
    fn contract_name_without_exact_descriptor_identity_never_binds() {
        let consumer = package("example.consumer", '1', true);
        let mut provider = package("example.provider", '2', false);
        provider.manifest.services[0].exports[0].descriptor_sha256 = "b".repeat(64);
        let descriptor = provider
            .contract_descriptors
            .remove(&"a".repeat(64))
            .unwrap();
        provider
            .contract_descriptors
            .insert("b".repeat(64), descriptor);
        assert!(
            consent_for_binding(&consumer, "main", "echo-source", &provider, "main", "echo")
                .unwrap_err()
                .contains("do not match")
        );
    }

    #[test]
    fn replacement_requires_new_exact_artifact_consent() {
        let consumer = package("example.consumer", '1', true);
        let provider = package("example.provider", '2', false);
        let consent =
            consent_for_binding(&consumer, "main", "echo-source", &provider, "main", "echo")
                .unwrap();
        let digest = consent_sha256(&consent).unwrap();
        let mut registry = ServiceBindingRegistryV1::default();
        let binding = registry
            .approve(consent, 0, None, &digest, true)
            .unwrap()
            .clone();
        let replaced_provider = package("example.provider", '3', false);
        assert_eq!(
            effective_status(&binding, true, false, true, true),
            EffectiveServiceBindingStatusV1::Suspended
        );
        let replacement = consent_for_binding(
            &consumer,
            "main",
            "echo-source",
            &replaced_provider,
            "main",
            "echo",
        )
        .unwrap();
        let replacement_digest = consent_sha256(&replacement).unwrap();
        assert_ne!(digest, replacement_digest);
        let rebound = registry
            .approve(
                replacement,
                1,
                Some(binding.revision),
                &replacement_digest,
                true,
            )
            .unwrap();
        assert_eq!(rebound.revision, 2);
    }
}
