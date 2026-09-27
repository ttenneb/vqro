# installed by vqro
# managed by vqro; reinstalling or updating the integration overwrites this file.
# add custom hooks beside this file instead of editing it.
# VQRO_INTEGRATION_ID=codex
# VQRO_INTEGRATION_VERSION=8

param([string]$Action = "")

if ($Action -ne "session") { exit 0 }
if ($env:VQRO_ENV -ne "1") { exit 0 }
if ([string]::IsNullOrWhiteSpace($env:VQRO_PANE_ID)) { exit 0 }

$inputText = [Console]::In.ReadToEnd()
try {
    $payload = if ([string]::IsNullOrWhiteSpace($inputText)) { $null } else { $inputText | ConvertFrom-Json }
} catch {
    exit 0
}

if ($payload.hook_event_name -and $payload.hook_event_name -ne "SessionStart") { exit 0 }

$sessionId = $payload.session_id
if ([string]::IsNullOrWhiteSpace($sessionId)) { exit 0 }
if ([string]::IsNullOrWhiteSpace($payload.transcript_path)) { exit 0 }
if (-not [string]::IsNullOrWhiteSpace($env:CODEX_THREAD_ID) -and $env:CODEX_THREAD_ID -ne $sessionId) { exit 0 }

$seq = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
$vqro = if ([string]::IsNullOrWhiteSpace($env:VQRO_BIN_PATH)) { "vqro" } else { $env:VQRO_BIN_PATH }
try {
    $args = @(
        "pane",
        "report-agent-session",
        $env:VQRO_PANE_ID,
        "--source",
        "vqro:codex",
        "--agent",
        "codex",
        "--seq",
        "$seq",
        "--agent-session-id",
        "$sessionId"
    )
    if ($payload.hook_event_name -eq "SessionStart" -and $payload.source -is [string] -and -not [string]::IsNullOrWhiteSpace($payload.source)) {
        $args += @("--session-start-source", "$($payload.source)")
    }
    & $vqro @args 2>$null | Out-Null
} catch {
}
