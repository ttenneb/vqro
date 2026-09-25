use crate::api::schema::{
    ErrorResponse, ExtensionDoctorParams, Method, Request, ResponseResult, SuccessResponse,
};
use crate::extension_health::{ExtensionDoctorDiagnosticV1, ExtensionDoctorStatusV1};

pub(super) fn run_extension_command(args: &[String]) -> std::io::Result<i32> {
    match args.first().map(String::as_str) {
        Some("doctor") => run_doctor(&args[1..]),
        Some("help" | "--help" | "-h") => usage(0),
        _ => usage(2),
    }
}

fn run_doctor(args: &[String]) -> std::io::Result<i32> {
    let json = match args {
        [] => false,
        [flag] if flag == "--json" => true,
        [flag] if matches!(flag.as_str(), "help" | "--help" | "-h") => return doctor_usage(0),
        _ => return doctor_usage(2),
    };
    let response = super::send_request(&Request {
        id: "cli:extension:doctor".into(),
        method: Method::ExtensionDoctor(ExtensionDoctorParams::default()),
    })?;
    if response.get("error").is_some() {
        let error: ErrorResponse =
            serde_json::from_value(response).map_err(std::io::Error::other)?;
        if json {
            println!(
                "{}",
                serde_json::to_string(&error).map_err(std::io::Error::other)?
            );
        } else {
            eprintln!("{}", unavailable_message(&error.error.code));
        }
        return Ok(2);
    }
    let success: SuccessResponse =
        serde_json::from_value(response).map_err(std::io::Error::other)?;
    let ResponseResult::ExtensionDoctorDiagnostic { diagnostic } = success.result else {
        return Err(std::io::Error::other(
            "server returned an unexpected Extension Doctor response",
        ));
    };
    diagnostic.validate().map_err(std::io::Error::other)?;
    if json {
        println!(
            "{}",
            serde_json::to_string(&diagnostic).map_err(std::io::Error::other)?
        );
    } else {
        print_human(&diagnostic);
    }
    Ok(match diagnostic.status {
        ExtensionDoctorStatusV1::Healthy => 0,
        ExtensionDoctorStatusV1::Degraded | ExtensionDoctorStatusV1::Unhealthy => 1,
    })
}

fn unavailable_message(code: &str) -> String {
    match code {
        crate::service_fabric::ERROR_BINDING_UNAVAILABLE =>
            "Extension Doctor unavailable: an enabled Health provider and approved connection are required.".into(),
        crate::service_fabric::ERROR_BINDING_REVOKED =>
            "Extension Doctor unavailable: the connection was revoked; inspect and approve a connection.".into(),
        _ if crate::service_fabric::STABLE_R1_ERROR_CODES.contains(&code) => {
            format!("Extension Doctor unavailable ({code}).")
        }
        _ => "Extension Doctor unavailable (service_provider_failed).".into(),
    }
}

fn print_human(diagnostic: &ExtensionDoctorDiagnosticV1) {
    let status = match diagnostic.status {
        ExtensionDoctorStatusV1::Healthy => "healthy",
        ExtensionDoctorStatusV1::Degraded => "degraded",
        ExtensionDoctorStatusV1::Unhealthy => "unhealthy",
    };
    println!("Extension health: {status}");
    for finding in &diagnostic.findings {
        let severity = match finding.severity {
            crate::extension_health::ExtensionDoctorSeverityV1::Warning => "warning",
            crate::extension_health::ExtensionDoctorSeverityV1::Error => "error",
        };
        println!("{severity}: {}", finding.message);
    }
}

fn usage(code: i32) -> std::io::Result<i32> {
    eprintln!("vqro extension commands: doctor");
    Ok(code)
}

fn doctor_usage(code: i32) -> std::io::Result<i32> {
    eprintln!("usage: vqro extension doctor [--json]");
    Ok(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unavailable_messages_are_product_owned_and_bounded() {
        assert_eq!(
            unavailable_message("service_binding_unavailable"),
            "Extension Doctor unavailable: an enabled Health provider and approved connection are required."
        );
        assert_eq!(
            unavailable_message("service_binding_revoked"),
            "Extension Doctor unavailable: the connection was revoked; inspect and approve a connection."
        );
        assert_eq!(
            unavailable_message("service_call_timeout"),
            "Extension Doctor unavailable (service_call_timeout)."
        );
        assert_eq!(
            unavailable_message("credential=/secret"),
            "Extension Doctor unavailable (service_provider_failed)."
        );
    }
}
