use crate::api::schema::{
    EmptyParams, ExtensionServiceBindingApproveParams, ExtensionServiceBindingInspectParams,
    ExtensionServiceBindingInvokeParams, ExtensionServiceBindingRevokeParams,
    ExtensionServiceEndpointParams, Method,
};

pub(super) fn run_service_binding_command(args: &[String]) -> std::io::Result<i32> {
    let method = match args.first().map(String::as_str) {
        Some("inspect") => parse_inspect(&args[1..]).map(Method::ExtensionServiceBindingInspect),
        Some("list") if args.len() == 1 => {
            Ok(Method::ExtensionServiceBindingList(EmptyParams::default()))
        }
        Some("approve") => parse_approve(&args[1..]).map(Method::ExtensionServiceBindingApprove),
        Some("revoke") => parse_revoke(&args[1..]).map(Method::ExtensionServiceBindingRevoke),
        Some("call") => parse_call(&args[1..]).map(Method::ExtensionServiceBindingInvoke),
        Some("help" | "--help" | "-h") => return usage(0),
        Some("list") => Err("service-binding list accepts no arguments".into()),
        _ => return usage(2),
    };
    match method {
        Ok(method) => super::runtime::service_binding(method),
        Err(error) => {
            eprintln!("{error}");
            usage(2)
        }
    }
}

fn parse_inspect(args: &[String]) -> Result<ExtensionServiceBindingInspectParams, String> {
    let endpoints = parse_endpoints(args, false)?;
    Ok(ExtensionServiceBindingInspectParams {
        consumer: endpoints.consumer()?,
        provider: endpoints.provider()?,
    })
}

fn parse_approve(args: &[String]) -> Result<ExtensionServiceBindingApproveParams, String> {
    let parsed = parse_endpoints(args, true)?;
    if !parsed.confirmed {
        return Err("service-binding approval requires --confirm".into());
    }
    let consumer = parsed.consumer()?;
    let provider = parsed.provider()?;
    let expected_consent_sha256 = parsed.consent_sha256.ok_or("missing --consent-sha256")?;
    if expected_consent_sha256.len() != 64
        || !expected_consent_sha256
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Err("--consent-sha256 must be 64 lowercase hexadecimal characters".into());
    }
    Ok(ExtensionServiceBindingApproveParams {
        consumer,
        provider,
        expected_registry_revision: required_u64(parsed.registry_revision, "--registry-revision")?,
        expected_binding_revision: parsed.binding_revision,
        expected_consent_sha256,
        confirmed: true,
    })
}

fn parse_call(args: &[String]) -> Result<ExtensionServiceBindingInvokeParams, String> {
    let binding_id = args
        .first()
        .filter(|value| !value.starts_with('-'))
        .cloned()
        .ok_or("missing binding_id")?;
    let mut binding_revision = None;
    let mut consumer_method = None;
    let mut params = None;
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--binding-revision" => set_once_u64(
                &mut binding_revision,
                args,
                &mut index,
                "--binding-revision",
            )?,
            "--consumer-method" => {
                set_once_string(&mut consumer_method, args, &mut index, "--consumer-method")?
            }
            "--params-json" => {
                if params.is_some() {
                    return Err("duplicate --params-json".into());
                }
                let raw = args
                    .get(index + 1)
                    .ok_or("missing value for --params-json")?;
                params = Some(
                    serde_json::from_str(raw)
                        .map_err(|error| format!("invalid --params-json: {error}"))?,
                );
                index += 2;
            }
            option => return Err(format!("unknown option: {option}")),
        }
    }
    Ok(ExtensionServiceBindingInvokeParams {
        binding_id,
        expected_binding_revision: required_u64(binding_revision, "--binding-revision")?,
        consumer_method: consumer_method.ok_or("missing --consumer-method")?,
        params: params.ok_or("missing --params-json")?,
    })
}

fn parse_revoke(args: &[String]) -> Result<ExtensionServiceBindingRevokeParams, String> {
    let binding_id = args
        .first()
        .filter(|value| !value.starts_with('-'))
        .cloned()
        .ok_or("missing binding_id")?;
    let mut registry_revision = None;
    let mut binding_revision = None;
    let mut confirmed = false;
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--registry-revision" => set_once_u64(
                &mut registry_revision,
                args,
                &mut index,
                "--registry-revision",
            )?,
            "--binding-revision" => set_once_u64(
                &mut binding_revision,
                args,
                &mut index,
                "--binding-revision",
            )?,
            "--confirm" if !confirmed => {
                confirmed = true;
                index += 1;
            }
            "--confirm" => return Err("duplicate --confirm".into()),
            option => return Err(format!("unknown option: {option}")),
        }
    }
    if !confirmed {
        return Err("service-binding revocation requires --confirm".into());
    }
    Ok(ExtensionServiceBindingRevokeParams {
        binding_id,
        expected_registry_revision: required_u64(registry_revision, "--registry-revision")?,
        expected_binding_revision: required_u64(binding_revision, "--binding-revision")?,
        confirmed: true,
    })
}

#[derive(Default)]
struct EndpointOptions {
    consumer_package: Option<String>,
    consumer_service: Option<String>,
    import_id: Option<String>,
    provider_package: Option<String>,
    provider_service: Option<String>,
    export_id: Option<String>,
    registry_revision: Option<u64>,
    binding_revision: Option<u64>,
    consent_sha256: Option<String>,
    confirmed: bool,
}

impl EndpointOptions {
    fn consumer(&self) -> Result<ExtensionServiceEndpointParams, String> {
        Ok(ExtensionServiceEndpointParams {
            package_id: required(&self.consumer_package, "--consumer-package")?,
            service_id: required(&self.consumer_service, "--consumer-service")?,
            endpoint_id: required(&self.import_id, "--import")?,
        })
    }

    fn provider(&self) -> Result<ExtensionServiceEndpointParams, String> {
        Ok(ExtensionServiceEndpointParams {
            package_id: required(&self.provider_package, "--provider-package")?,
            service_id: required(&self.provider_service, "--provider-service")?,
            endpoint_id: required(&self.export_id, "--export")?,
        })
    }
}

fn parse_endpoints(args: &[String], mutation: bool) -> Result<EndpointOptions, String> {
    let mut parsed = EndpointOptions::default();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--consumer-package" => set_once_string(
                &mut parsed.consumer_package,
                args,
                &mut index,
                "--consumer-package",
            )?,
            "--consumer-service" => set_once_string(
                &mut parsed.consumer_service,
                args,
                &mut index,
                "--consumer-service",
            )?,
            "--import" => set_once_string(&mut parsed.import_id, args, &mut index, "--import")?,
            "--provider-package" => set_once_string(
                &mut parsed.provider_package,
                args,
                &mut index,
                "--provider-package",
            )?,
            "--provider-service" => set_once_string(
                &mut parsed.provider_service,
                args,
                &mut index,
                "--provider-service",
            )?,
            "--export" => set_once_string(&mut parsed.export_id, args, &mut index, "--export")?,
            "--registry-revision" if mutation => set_once_u64(
                &mut parsed.registry_revision,
                args,
                &mut index,
                "--registry-revision",
            )?,
            "--binding-revision" if mutation => set_once_u64(
                &mut parsed.binding_revision,
                args,
                &mut index,
                "--binding-revision",
            )?,
            "--consent-sha256" if mutation => set_once_string(
                &mut parsed.consent_sha256,
                args,
                &mut index,
                "--consent-sha256",
            )?,
            "--confirm" if mutation && !parsed.confirmed => {
                parsed.confirmed = true;
                index += 1;
            }
            "--confirm" if mutation => return Err("duplicate --confirm".into()),
            option => return Err(format!("unknown option: {option}")),
        }
    }
    Ok(parsed)
}

fn set_once_string(
    slot: &mut Option<String>,
    args: &[String],
    index: &mut usize,
    option: &str,
) -> Result<(), String> {
    if slot.is_some() {
        return Err(format!("duplicate {option}"));
    }
    *slot = Some(
        args.get(*index + 1)
            .filter(|value| !value.starts_with("--"))
            .cloned()
            .ok_or_else(|| format!("missing value for {option}"))?,
    );
    *index += 2;
    Ok(())
}

fn set_once_u64(
    slot: &mut Option<u64>,
    args: &[String],
    index: &mut usize,
    option: &str,
) -> Result<(), String> {
    if slot.is_some() {
        return Err(format!("duplicate {option}"));
    }
    let value = args
        .get(*index + 1)
        .filter(|value| !value.starts_with("--"))
        .ok_or_else(|| format!("missing value for {option}"))?
        .parse()
        .map_err(|_| format!("invalid value for {option}"))?;
    *slot = Some(value);
    *index += 2;
    Ok(())
}

fn required(value: &Option<String>, option: &str) -> Result<String, String> {
    value.clone().ok_or_else(|| format!("missing {option}"))
}

fn required_u64(value: Option<u64>, option: &str) -> Result<u64, String> {
    value.ok_or_else(|| format!("missing {option}"))
}

fn usage(code: i32) -> std::io::Result<i32> {
    eprintln!("vqro service-binding commands: inspect, list, approve, revoke, call");
    Ok(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    fn endpoints() -> Vec<String> {
        strings(&[
            "--consumer-package",
            "consumer.pkg",
            "--consumer-service",
            "main",
            "--import",
            "attention",
            "--provider-package",
            "provider.pkg",
            "--provider-service",
            "status",
            "--export",
            "activity",
        ])
    }

    #[test]
    fn inspect_parses_exact_endpoint_coordinates() {
        let parsed = parse_inspect(&endpoints()).unwrap();
        assert_eq!(parsed.consumer.package_id, "consumer.pkg");
        assert_eq!(parsed.consumer.endpoint_id, "attention");
        assert_eq!(parsed.provider.service_id, "status");
        assert_eq!(parsed.provider.endpoint_id, "activity");
    }

    #[test]
    fn approve_requires_confirmation_cas_and_consent_digest() {
        let mut args = endpoints();
        args.extend(strings(&[
            "--registry-revision",
            "7",
            "--binding-revision",
            "5",
            "--consent-sha256",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "--confirm",
        ]));
        let parsed = parse_approve(&args).unwrap();
        assert_eq!(parsed.expected_registry_revision, 7);
        assert_eq!(parsed.expected_binding_revision, Some(5));
        assert!(parsed.confirmed);

        let mut missing_confirm = args.clone();
        missing_confirm.pop();
        assert!(parse_approve(&missing_confirm).is_err());
        let missing_cas = endpoints();
        assert!(parse_approve(&missing_cas).is_err());

        let mut invalid_digest = args;
        let digest = invalid_digest
            .iter()
            .position(|value| value == "--consent-sha256")
            .unwrap();
        invalid_digest[digest + 1] = "A".repeat(64);
        assert!(parse_approve(&invalid_digest).is_err());
    }

    #[test]
    fn revoke_requires_confirmation_and_both_cas_revisions() {
        let parsed = parse_revoke(&strings(&[
            "binding-1",
            "--registry-revision",
            "9",
            "--binding-revision",
            "8",
            "--confirm",
        ]))
        .unwrap();
        assert_eq!(parsed.binding_id, "binding-1");
        assert_eq!(parsed.expected_registry_revision, 9);
        assert_eq!(parsed.expected_binding_revision, 8);
        assert!(parse_revoke(&strings(&[
            "binding-1",
            "--registry-revision",
            "9",
            "--binding-revision",
            "8",
        ]))
        .is_err());
    }

    #[test]
    fn call_requires_exact_binding_revision_method_and_json() {
        let parsed = parse_call(&strings(&[
            "binding-1",
            "--binding-revision",
            "4",
            "--consumer-method",
            "compose",
            "--params-json",
            r#"{"topic":"status"}"#,
        ]))
        .unwrap();
        assert_eq!(parsed.binding_id, "binding-1");
        assert_eq!(parsed.expected_binding_revision, 4);
        assert_eq!(parsed.consumer_method, "compose");
        assert_eq!(parsed.params["topic"], "status");
        assert!(parse_call(&strings(&[
            "binding-1",
            "--binding-revision",
            "4",
            "--consumer-method",
            "compose",
        ]))
        .is_err());
    }

    #[test]
    fn parsers_reject_duplicates_and_mutation_flags_on_inspect() {
        let mut duplicate = endpoints();
        duplicate.extend(strings(&["--import", "other"]));
        assert!(parse_inspect(&duplicate).is_err());
        let mut mutation = endpoints();
        mutation.push("--confirm".into());
        assert!(parse_inspect(&mutation).is_err());
    }
}
