//! Controlled stdin/stdout mode, retaining native user settings rather than SDK/bare defaults.
pub fn controlled(mut argv: Vec<String>) -> Result<Vec<String>, &'static str> {
    if argv.is_empty() || argv[0].is_empty() {
        return Err("explicit native executable required");
    }
    // A positional prompt would run inference at launch. The bridge owns the stream mode and
    // permission transport; session/fork/management launches are a separate explicit operation.
    let values = [
        "--model",
        "--permission-mode",
        "--settings",
        "--setting-sources",
        "--mcp-config",
        "--plugin-dir",
        "--add-dir",
        "--allowedTools",
        "--disallowedTools",
        "--tools",
        "--system-prompt",
        "--append-system-prompt",
        "--max-turns",
        "--max-budget-usd",
        "--effort",
        "--agent",
    ];
    let flags = [
        "--dangerously-skip-permissions",
        "--allow-dangerously-skip-permissions",
        "--strict-mcp-config",
        "--disable-slash-commands",
    ];
    let mut at = 1;
    while at < argv.len() {
        let argument = &argv[at];
        if values.contains(&argument.as_str()) {
            if at + 1 >= argv.len() {
                return Err("native option requires a value");
            }
            at += 2;
        } else if flags.contains(&argument.as_str()) {
            at += 1;
        } else {
            return Err(
                "native launch option/positional prompt is not supported by this controlled profile",
            );
        }
    }
    argv.extend(
        [
            "-p",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--verbose",
            "--replay-user-messages",
            "--permission-prompt-tool",
            "stdio",
        ]
        .map(str::to_owned),
    );
    Ok(argv)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn no_prompt_bare_or_conflicting_transport_at_launch() {
        for args in [
            vec!["claude", "prompt"],
            vec!["claude", "--bare"],
            vec!["claude", "-p", "prompt"],
            vec!["claude", "--output-format", "text"],
        ] {
            assert!(controlled(args.into_iter().map(str::to_owned).collect()).is_err());
        }
        let args = controlled(
            ["claude", "--model", "native-model"]
                .map(str::to_owned)
                .to_vec(),
        )
        .unwrap();
        assert!(args.ends_with(&["--permission-prompt-tool".into(), "stdio".into()]));
        assert!(
            !args
                .iter()
                .any(|s| s == "--bare" || s == "--setting-sources")
        );
    }
}
