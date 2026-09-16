const API_SCHEMA_JSON: &str = include_str!("../../docs/next/api/vqro-api.schema.json");

use crate::api::schema::{EmptyParams, Method, Request};

pub(super) fn run_api_command(args: &[String]) -> std::io::Result<i32> {
    let Some(subcommand) = args.first().map(String::as_str) else {
        print_api_help();
        return Ok(2);
    };

    match subcommand {
        "describe" => api_describe(&args[1..]),
        "schema" => api_schema(&args[1..]),
        "snapshot" => api_snapshot(&args[1..]),
        "help" | "--help" | "-h" => {
            print_api_help();
            Ok(0)
        }
        _ => {
            print_api_help();
            Ok(2)
        }
    }
}

fn api_schema(args: &[String]) -> std::io::Result<i32> {
    match args {
        [] => {
            print!("{}", schema_summary_text()?);
        }
        [flag] if flag == "--json" => {
            print!("{API_SCHEMA_JSON}");
        }
        [flag, path] if flag == "--output" => {
            write_schema_file(std::path::Path::new(path))?;
            println!("wrote API schema to {path}");
        }
        [flag] if flag == "--output" => {
            eprintln!("missing value for --output");
            return Ok(2);
        }
        [flag] if matches!(flag.as_str(), "help" | "--help" | "-h") => {
            print_api_schema_help();
        }
        [other] if other.starts_with('-') => {
            eprintln!("unknown option: {other}");
            return Ok(2);
        }
        _ => {
            print_api_schema_help();
            return Ok(2);
        }
    }
    Ok(0)
}

fn api_describe(args: &[String]) -> std::io::Result<i32> {
    let descriptor = crate::native_api::descriptor();
    match args {
        [] => println!(
            "contract: {}\nmajor_version: {}\nschema_id: {}\nschema_sha256: {}",
            descriptor.contract,
            descriptor.major_version,
            descriptor.schema_id,
            descriptor.schema_sha256
        ),
        [flag] if flag == "--json" => println!(
            "{}",
            serde_json::to_string_pretty(&descriptor).map_err(std::io::Error::other)?
        ),
        _ => {
            eprintln!("usage: vqro api describe [--json]");
            return Ok(2);
        }
    }
    Ok(0)
}

fn api_snapshot(args: &[String]) -> std::io::Result<i32> {
    if !args.is_empty() {
        eprintln!("usage: vqro api snapshot");
        return Ok(2);
    }

    super::print_response(&super::send_request(&Request {
        id: "cli:api:snapshot".into(),
        method: Method::SessionSnapshot(EmptyParams::default()),
    })?)
}

fn write_schema_file(path: &std::path::Path) -> std::io::Result<()> {
    std::fs::write(path, API_SCHEMA_JSON)
}

fn schema_summary_text() -> std::io::Result<String> {
    let value: serde_json::Value = serde_json::from_str(API_SCHEMA_JSON)?;
    let contract = value
        .get("contract")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| std::io::Error::other("API schema is missing contract"))?;
    let major_version = value
        .get("major_version")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| std::io::Error::other("API schema is missing major_version"))?;
    let mut schemas: Vec<&str> = value
        .get("schemas")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| std::io::Error::other("API schema is missing schemas"))?
        .keys()
        .map(String::as_str)
        .collect();
    schemas.sort();

    Ok(format!(
        "vqro API schema\ncontract: {}\nmajor_version: {}\nschemas: {}\n\nUse `vqro api schema --json` to print the full schema.\nUse `vqro api schema --output PATH` to write it to a file.\n",
        contract,
        major_version,
        schemas.join(", ")
    ))
}

fn print_api_help() {
    eprintln!("vqro api commands:");
    eprintln!("  vqro api describe [--json]");
    eprintln!("  vqro api snapshot");
    eprintln!("  vqro api schema [--json | --output PATH]");
}

fn print_api_schema_help() {
    eprintln!("usage: vqro api schema [--json | --output PATH]");
}

#[cfg(test)]
mod tests {
    #[test]
    fn schema_summary_text_stays_human_sized() {
        let text = super::schema_summary_text().unwrap();
        assert!(text.contains("vqro API schema"));
        assert!(text.contains("Use `vqro api schema --json`"));
        assert!(text.len() < 400);
    }
}
