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
/// Private launch descriptor: opaque reference only, never a token or authenticated identity.
#[derive(Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Credential {
    pub account: String,
    #[serde(rename = "ref")]
    pub reference: String,
    pub host: String,
}
impl Credential {
    pub fn parse(raw: &str, account: Option<&serde_json::Value>) -> Result<Self, &'static str> {
        let value: Self =
            serde_json::from_str(raw).map_err(|_| "invalid private launch credential")?;
        if value.account.is_empty()
            || value.account.len() > 64
            || value.host.is_empty()
            || value.host.len() > 128
            || value.host.contains('\0')
            || value.reference.len() != 32
            || !value
                .reference
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || account.is_none_or(|row| {
                row["id"] != value.account
                    || row["kind"] != "subscription"
                    || row
                        .get("provider")
                        .is_some_and(|provider| provider != "anthropic")
            })
        {
            return Err(
                "launch credential must match the configured Claude subscription reference",
            );
        }
        Ok(value)
    }
    pub fn verify_host(
        &self,
        runtime_host: &str,
        resources: &serde_json::Value,
    ) -> Result<(), &'static str> {
        if execution_backend(runtime_host, resources)? != self.host {
            return Err(
                "named Claude launch backend differs from credential host; no helper dispatched",
            );
        }
        Ok(())
    }
}

/// Resolve ONLY a core-authenticated runtime host resource, not native child metadata.
pub fn execution_backend<'a>(
    runtime_host: &str,
    resources: &'a serde_json::Value,
) -> Result<&'a str, &'static str> {
    let rows = resources["resources"]
        .as_array()
        .ok_or("runtime host facts unavailable")?;
    if rows.len() != 1
        || rows[0]["id"] != runtime_host
        || rows[0]["available"] != true
        || rows[0]["state"] != "live"
        || !rows[0]["host"].is_null()
    {
        return Err("authenticated backend host resource unavailable");
    }
    rows[0]["provider"]
        .as_str()
        .filter(|name| !name.is_empty() && name.len() <= 128)
        .ok_or("backend provider name unavailable")
}

/// Run before bootstrap, helper preflight or child spawn on the execution host.
pub fn credential_host_supported(named: bool, windows: bool) -> Result<(), &'static str> {
    if named && windows {
        return Err("not supported on Windows yet");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn windows_named_launch_refuses_before_bootstrap_or_effects() {
        assert_eq!(
            super::credential_host_supported(true, true),
            Err("not supported on Windows yet")
        );
        assert_eq!(super::credential_host_supported(false, true), Ok(()));
        assert_eq!(super::credential_host_supported(true, false), Ok(()));
    }

    use super::*;
    #[test]
    fn named_launch_checks_the_authenticated_backend_not_the_agent_host_or_child() {
        let configured = serde_json::json!({"id":"A","kind":"subscription","provider":"anthropic","sources":{"id":"vqro:launcher"}});
        let named = Credential::parse(
            r#"{"account":"A","ref":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","host":"backend-A"}"#,
            Some(&configured),
        )
        .unwrap();
        let host = serde_json::json!({"resources":[{"id":"new-terminal","provider":"backend-A","host":null,"state":"live","available":true}]});
        assert!(named.verify_host("new-terminal", &host).is_ok());
        assert!(
            named
                .verify_host("new-terminal", &serde_json::json!({"resources":[]}))
                .is_err()
        );
        for patch in [
            serde_json::json!({"provider":"backend-B"}),
            serde_json::json!({"host":"different-parent"}),
            serde_json::json!({"id":"old-terminal"}),
            serde_json::json!({"state":"finished"}),
            serde_json::json!({"available":false}),
        ] {
            let mut bad = host.clone();
            for (key, value) in patch.as_object().unwrap() {
                bad["resources"][0][key] = value.clone();
            }
            assert!(named.verify_host("new-terminal", &bad).is_err());
        }
        assert!(
            Credential::parse(
                r#"{"account":"B","ref":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","host":"backend-A"}"#,
                Some(&configured)
            )
            .is_err()
        );
        assert!(
            Credential::parse(
                r#"{"account":"A","ref":"raw-token","host":"backend-A"}"#,
                Some(&configured)
            )
            .is_err()
        );
        assert!(Credential::parse(r#"{"account":"A","ref":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","host":"backend-A","helper":["arbitrary"]}"#, Some(&configured)).is_err());
    }
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
