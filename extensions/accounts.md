# Runtime accounts (slice 1)

`accounts.show` opens a metadata-only registry and native sign-in actions. Account records live in
`doc/settings/accounts = {"accounts": [...]}`. The **Add account…** prompt accepts a JSON object, for example:

```json
{"id":"work-pi","label":"Work Pi","runtime":"pi","provider":"openai-codex","kind":"subscription","profile":"/profiles/pi-work"}
```

`runtime` is `pi`, `codex` or `claude`. IDs are unique safe identifiers (up to64 characters), labels are printable
(up to128 characters), and optional `provider` is the configured provider identifier. `profile` must be an absolute
literal **terminal-host** path: POSIX or Windows syntax is accepted, but `~`, environment and shell expansion are
not performed. The native runtime creates/manages the profile when you sign in; vqro never creates, scans, reads,
copies, migrates or deletes profile contents. Never put a token/API key/credential in registry fields or argv.
Only metadata fields are accepted; secret-shaped extra fields are rejected.

With named accounts present, `pi.new`, `codex.new`, `tier1.pi` and `tier1.claude` ask for an account before spawning.
An explicit `account:<id>` selects it; `account:"normal"` means **Use my normal login**, keeping the launcher's
existing native profile behavior. With no matching records, existing launch commands behave as before. Offered
choices carry a registry revision; a changed registry requires choosing again. No prompt/input is sent at launch,
and no failed/uncertain launch is replayed. IDs identify configured profiles, not verified provider users; do not
reuse an ID for a different profile. Removing an entry removes only metadata, not native logins or running agents.

The launcher maps a named profile to `PI_CODING_AGENT_DIR`, `CODEX_HOME` or `CLAUDE_CONFIG_DIR`. A Pi account's
provider is passed through native `--provider`; a conflicting runtime provider is refused. Profile/account identity
is captured at launch, never inferred from profile content or updated by a native login/logout. Ambient native
credential/configuration precedence still belongs to the runtime: this metadata is not proof of authenticated
identity, subscription entitlement, billing mode or isolated capacity.

**Sign in…** makes one backend-owned terminal and opens it with `tabs.show` (resource-picker fallback). Codex uses
its configured executable prefix followed by `login`; Claude uses `auth login`. Pi has no native login CLI flag:
it opens the native interactive Pi terminal on the selected profile; enter `/login` there. vqro never types a login
command, handles a browser token, reads a credential, checks sign-in success or sends a model prompt. The runtime
owns browser/device flow, token storage and refresh. For multiple configured executable variants, the programmatic
`accounts.sign_in` command accepts `launcher:<runtime name>`; the UI uses the first configured variant. Verify you
intend that executable before signing in. The normal-login action honors existing explicit runtime profile selectors.

`kind` is declared metadata (`subscription | api_key`), not observed auth. Profile-backed API-key entries can select
an already configured native login, but importing/storing keys and API-key sign-in are explicitly unsupported in
this slice. The keychain slice will add a separate host-local opaque-handle path; no API key is passed through
workspace entries or core spawn environment here. Implicit normal launches omit an account fact rather than guess
their kind.

Rust Pi/Codex bridges publish the bounded non-secret configured `data.agent.account` with `sources: "vqro:launcher"`
per field. It contains no profile path; internal `VQRO_ACCOUNT` metadata is removed from native child environments.
Tier-1 terminals retain equivalent configured metadata in their launcher declaration, not a fabricated structured
agent endpoint. Unknown/malformed metadata is rejected before a native child is started.

No account switching of an existing conversation, automatic capacity balancing, overflow/replay, usage fetching,
provider-specific OAuth implementation, credential storage or authenticated verification is added. Disable the
accounts UI to remove its commands/menu; existing processes/native stores are untouched, and launchers can still
interpret explicitly configured account metadata. This extension is replaceable using public workspace commands;
core has no accounts registry or authentication policy.
