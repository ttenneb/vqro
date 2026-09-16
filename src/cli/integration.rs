use crate::api::schema::IntegrationTarget;

pub(super) fn run_integration_command(args: &[String]) -> std::io::Result<i32> {
    let Some(subcommand) = args.first().map(|arg| arg.as_str()) else {
        print_integration_help();
        return Ok(2);
    };

    match subcommand {
        "install" => integration_install(&args[1..]),
        "uninstall" => integration_uninstall(&args[1..]),
        "status" => integration_status(&args[1..]),
        "help" | "--help" | "-h" => {
            print_integration_help();
            Ok(0)
        }
        _ => {
            print_integration_help();
            Ok(2)
        }
    }
}

fn integration_status(args: &[String]) -> std::io::Result<i32> {
    let outdated_only = match args {
        [] => false,
        [flag] if flag == "--outdated-only" => true,
        _ => {
            eprintln!("usage: vqro integration status [--outdated-only]");
            return Ok(2);
        }
    };

    if outdated_only {
        crate::integration::print_outdated_update_notice();
        return Ok(0);
    }

    for status in crate::integration::installed_integration_statuses()
        .into_iter()
        .filter(|status| crate::integration::integration_mutation_enabled(status.target))
    {
        let target = crate::integration::integration_target_label(status.target);
        let state = describe_integration_state(
            status.state,
            status.installed_version,
            status.expected_version,
        );
        println!("{target}: {state} ({})", status.path.display());
    }

    Ok(0)
}

fn describe_integration_state(
    state: crate::integration::IntegrationStatusKind,
    installed_version: Option<u32>,
    expected_version: u32,
) -> String {
    let version = match installed_version {
        Some(version) => format!("v{version}"),
        None => "legacy".to_string(),
    };
    match state {
        crate::integration::IntegrationStatusKind::NotInstalled => "not installed".to_string(),
        crate::integration::IntegrationStatusKind::Current => format!("current ({version})"),
        crate::integration::IntegrationStatusKind::Outdated
            if installed_version.is_some_and(|installed| installed >= expected_version) =>
        {
            format!("needs repair ({version})")
        }
        crate::integration::IntegrationStatusKind::Outdated => {
            format!("outdated ({version} < v{expected_version})")
        }
    }
}

fn integration_install(args: &[String]) -> std::io::Result<i32> {
    let Some(target) = parse_integration_target(args, "install")? else {
        return Ok(2);
    };
    let IntegrationCommandTarget::Builtin(target) = target else {
        eprintln!("vqro integration installation is disabled for non-namespaced integrations");
        return Ok(1);
    };
    if !crate::integration::integration_mutation_enabled(target) {
        eprintln!("vqro integration installation is disabled for non-namespaced integrations");
        return Ok(1);
    }
    for message in crate::integration::install_target(target)? {
        println!("{message}");
    }
    Ok(0)
}

fn integration_uninstall(args: &[String]) -> std::io::Result<i32> {
    let Some(target) = parse_integration_target(args, "uninstall")? else {
        return Ok(2);
    };
    let IntegrationCommandTarget::Builtin(target) = target else {
        eprintln!("vqro integration removal is disabled for non-namespaced integrations");
        return Ok(1);
    };
    if !crate::integration::integration_mutation_enabled(target) {
        eprintln!("vqro integration removal is disabled for non-namespaced integrations");
        return Ok(1);
    }
    for message in crate::integration::uninstall_target(target)? {
        println!("{message}");
    }
    Ok(0)
}

/// Integration target accepted by the CLI. Letta is deliberately kept out of
/// the frozen client endpoint `IntegrationTarget` enum and is handled as an
/// experimental CLI-only target until the agent registry replaces it.
enum IntegrationCommandTarget {
    Builtin(IntegrationTarget),
    Letta,
}

fn parse_integration_target(
    args: &[String],
    action: &str,
) -> std::io::Result<Option<IntegrationCommandTarget>> {
    let Some(target) = args.first().map(|arg| arg.as_str()) else {
        eprintln!(
            "usage: vqro integration {action} <pi|omp|claude|codex|copilot|devin|droid|kimi|opencode|kilo|hermes|qodercli|qwen|letta|cursor|mastracode|grok>"        );
        return Ok(None);
    };
    if args.len() != 1 {
        eprintln!(
            "usage: vqro integration {action} <pi|omp|claude|codex|copilot|devin|droid|kimi|opencode|kilo|hermes|qodercli|qwen|letta|cursor|mastracode|grok>"        );
        return Ok(None);
    }

    let parsed = match target {
        "pi" => IntegrationCommandTarget::Builtin(IntegrationTarget::Pi),
        "omp" => IntegrationCommandTarget::Builtin(IntegrationTarget::Omp),
        "claude" => IntegrationCommandTarget::Builtin(IntegrationTarget::Claude),
        "codex" => IntegrationCommandTarget::Builtin(IntegrationTarget::Codex),
        "copilot" => IntegrationCommandTarget::Builtin(IntegrationTarget::Copilot),
        "devin" => IntegrationCommandTarget::Builtin(IntegrationTarget::Devin),
        "droid" => IntegrationCommandTarget::Builtin(IntegrationTarget::Droid),
        "kimi" => IntegrationCommandTarget::Builtin(IntegrationTarget::Kimi),
        "opencode" => IntegrationCommandTarget::Builtin(IntegrationTarget::Opencode),
        "kilo" => IntegrationCommandTarget::Builtin(IntegrationTarget::Kilo),
        "hermes" => IntegrationCommandTarget::Builtin(IntegrationTarget::Hermes),
        "qodercli" => IntegrationCommandTarget::Builtin(IntegrationTarget::Qodercli),
        "qwen" => IntegrationCommandTarget::Builtin(IntegrationTarget::Qwen),
        "letta" => IntegrationCommandTarget::Letta,
        "cursor" => IntegrationCommandTarget::Builtin(IntegrationTarget::Cursor),
        "mastracode" => IntegrationCommandTarget::Builtin(IntegrationTarget::Mastracode),
        "antigravity-cli" | "antigravity_cli" => {
            IntegrationCommandTarget::Builtin(IntegrationTarget::AntigravityCli)
        }
        "grok" => IntegrationCommandTarget::Builtin(IntegrationTarget::Grok),
        _ => {
            eprintln!("unknown integration target: {target}");
            eprintln!(
                "currently supported: pi, omp, claude, codex, copilot, devin, droid, kimi, opencode, kilo, hermes, qodercli, qwen, letta, cursor, mastracode, antigravity-cli, grok"
            );
            return Ok(None);
        }
    };

    Ok(Some(parsed))
}

fn print_integration_help() {
    eprintln!("vqro integration commands:");
    eprintln!("  vqro integration install pi");
    eprintln!("  vqro integration install omp");
    eprintln!("  vqro integration install claude");
    eprintln!("  vqro integration install codex");
    eprintln!("  vqro integration install copilot");
    eprintln!("  vqro integration install devin");
    eprintln!("  vqro integration install droid");
    eprintln!("  vqro integration install kimi");
    eprintln!("  vqro integration install opencode");
    eprintln!("  vqro integration install kilo");
    eprintln!("  vqro integration install hermes");
    eprintln!("  vqro integration install qodercli");
    eprintln!("  vqro integration install qwen");
    eprintln!("  vqro integration install letta");
    eprintln!("  vqro integration install cursor");
    eprintln!("  vqro integration install mastracode");
    eprintln!("  vqro integration install antigravity-cli");
    eprintln!("  vqro integration install grok");
    eprintln!("  vqro integration uninstall pi");
    eprintln!("  vqro integration uninstall omp");
    eprintln!("  vqro integration uninstall claude");
    eprintln!("  vqro integration uninstall codex");
    eprintln!("  vqro integration uninstall copilot");
    eprintln!("  vqro integration uninstall devin");
    eprintln!("  vqro integration uninstall droid");
    eprintln!("  vqro integration uninstall kimi");
    eprintln!("  vqro integration uninstall opencode");
    eprintln!("  vqro integration uninstall kilo");
    eprintln!("  vqro integration uninstall hermes");
    eprintln!("  vqro integration uninstall qodercli");
    eprintln!("  vqro integration uninstall qwen");
    eprintln!("  vqro integration uninstall letta");
    eprintln!("  vqro integration uninstall cursor");
    eprintln!("  vqro integration uninstall mastracode");
    eprintln!("  vqro integration uninstall antigravity-cli");
    eprintln!("  vqro integration uninstall grok");
    eprintln!("  vqro integration status [--outdated-only]");
}
