# Runtime accounts and capacity policy

`accounts.show` opens a metadata-only registry and native sign-in actions. Account records live in
`doc/settings/accounts = {"accounts": [...]}`. The **Add account…** prompt accepts a JSON object, for example:

```json
{"id":"work-pi","label":"Work Pi","runtime":"pi","provider":"openai-codex","kind":"subscription","profile":"/profiles/pi-work"}
```

`runtime` is `pi`, `codex` or `claude`. IDs are unique safe identifiers (up to64 characters), labels are printable
(up to128 characters), and optional `provider` is the configured provider identifier. Omit it when unspecified;
explicit JSON null is rejected consistently before registry/UI/bridge projection. `profile` must be an absolute
literal **terminal-host** path: POSIX or Windows syntax is accepted, but `~`, environment and shell expansion are
not performed. The native runtime creates/manages the profile when you sign in; vqro never creates, scans, reads,
copies, migrates or deletes profile contents. Never put a token/API key/credential in registry fields or argv.
Only metadata fields are accepted; secret-shaped extra fields are rejected.

With named accounts present, `pi.new`, `codex.new`, `tier1.pi` and `tier1.claude` default to asking for an account before spawning; the optional policy below changes only implicit NEW-agent choices.
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
intend that executable before signing in. The normal-login action shares the launcher's profile mapping, including explicit HOME/Windows-home/XDG selectors.
Sign-in accepts only recognized inert native options (and a recognized Node/Bun script prefix); prompt-bearing,
noninteractive, resume or unknown argv shapes refuse before spawn rather than risk running a model request.
Codex validates its full argv: only an empty suffix, `--stdio`, or `--listen stdio://` after `app-server` is removed.
Recognized global selectors must precede `app-server`; unproved suffix placements, missing values, `--`, repeated
subcommands and unknown remainders refuse rather than silently opening a differently configured login.

`kind` is declared metadata (`subscription | api_key`), not observed auth. Profile-backed API-key entries can select
an already configured native login, but importing/storing keys and API-key sign-in are explicitly unsupported in
this slice. The keychain slice will add a separate host-local opaque-handle path; no API key is passed through
workspace entries or core spawn environment here. Implicit normal launches omit an account fact rather than guess
their kind.

Rust Pi/Codex bridges publish the bounded non-secret configured `data.agent.account` with `sources: "vqro:launcher"`
per field. It contains no profile path; internal `VQRO_ACCOUNT` metadata is removed from native child environments.
Tier-1 terminals retain equivalent configured metadata in their launcher declaration, not a fabricated structured
agent endpoint. The tier1 wrapper strips internal `VQRO_ACCOUNT` before native exec while retaining the selected
native profile selector, so nested normal launches cannot inherit stale configured-account facts. Unknown/malformed metadata is rejected before a native child is started.

## Capacity strategies (slice 3)

Accounts offers `doc/settings/accounts-policy` (ordinary undoable workspace configuration):

```json
{"new_agents":"ask","blocked":"notify","api_key_overflow":false}
```

- `new_agents`: `ask` (default), `most_remaining_capacity`, or `round_robin`. Explicit account/normal-login
  choices always win. These choices never change an existing agent. Without matching named profiles the default
  `ask` preserves existing native launch behavior; an explicitly selected capacity/rotation mode instead asks
  before using an uninspected normal login. Unknown/malformed policies fall back to ask + notify, overflow off.
- `most_remaining_capacity` ranks **reported percentage headroom**, taking the smallest remaining percentage
  across each account's reported windows. A row must be account-scoped to the launcher's configured ID, sourced,
  observed within60 seconds, not future-dated and not expired. Counts without a denominator, unknown windows,
  stale/expired/unsourced reports never become spare capacity. A native known-blocked live account is excluded.
  No positive comparable result means ask; reset times never manufacture a fresh100%. This is a comparison of
  reported fractions, **not** equal token allocations, entitlement, authenticated identity or permission to run.
- `round_robin` rotates eligible configured profiles in registry order, excluding known live blocked accounts.
  Unknown capacity is not called available; this mode is rotation, not a capacity inference. The transactional
  `doc/accounts/round-robin/<runtime>` cursor records an attempted selection. A failed/unknown launch consumes
  its turn, with no effect retry to repair the cursor. A CAS conflict stops before spawning.
- `blocked`: native Q5 `availability.state=blocked`, with declared `quota|rate_limited`, is handled as `notify`
  (default), `offer_restart`, or `auto_restart`. Auth/unknown errors and coarse status text are not quota signals.
  Offers are explicit **ONE new empty agent/session** actions on another configured account/provider, with
  unknown capacity labelled. Automatic handling requires a known configured source and positive fresh reported
  headroom on an alternative, and a single unambiguous inert runtime executable. Otherwise it reports no action.
  Automatic targets use most reported headroom, independently of the new-agent picker setting.
- `api_key_overflow` is off by default: implicit rotation/capacity choices and blocked-agent alternatives exclude
  API-key profiles. Turning it on permits already-configured API-key profiles in automatic selection/overflow;
  explicit account choices are unaffected. It does not import/store/unlock keys or send a model request. Native
  setup stays separate; declared kind is not verified billing identity.

A restart does **not** stop, switch, resume, migrate or feed the blocked conversation. No source prompt, transcript,
queue, session ID, cwd or model choice is copied. The launch uses its ordinary project/runtime defaults with
`fresh:true`; launchers preserve that flag through pickers and reject resume/prompt-bearing/unknown native argv
before spawn. This does not inspect native history/auth/profile contents. Observe the native result; launch intent
is not a promise of authenticated identity or a successfully initialized native session.

Fresh-work actions claim a bounded128KiB **protected operational** `accounts` domain before invoking a launcher
once. Claims are outside workspace undo/history, survive extension restart and fence resource incarnation/native
session epoch. Unknown/adopted-but-unconfirmed claim or launch outcomes never cause an automatic resend. Known
new child/host IDs, or the target account after an uncertain outcome, suppress restart chains. The journal keeps
at most64 source-epoch records and64 child/host/uncertain markers; full/unsupported state fails closed, without eviction
that could replay an old attempt. Handling is conservative: at most one fresh attempt per source incarnation,
native epoch (even after switching away and back); further work can always be an explicit ordinary new-agent action. Pending work is
bounded64; overload is reported, never silently turned into extra launches. No polling or new quota/auth request
is made; fresh native resource reports and policy changes drive re-evaluation.

Missing `operational.*` refuses offered/automatic fresh-restart effects; there is no private-file or undoable-claim
fallback. Normal new-agent selection and notifications still work. The source is rechecked after claim admission;
a stale check consumes the claim rather than retrying an effect. Native launch/readback errors are reported without
copying raw native error bodies into workspace state.

Disabling the accounts extension removes its UI and stops its background blocked-agent handling; existing
processes/native stores remain untouched. Launchers still honor saved new-agent selection policy and explicit
account metadata. No switching/replay of an existing conversation, usage fetching, provider-specific OAuth,
credential storage or authenticated verification is added. This remains replaceable extension policy over public
workspace/resource/command/operational primitives; core has no accounts registry or capacity policy.
