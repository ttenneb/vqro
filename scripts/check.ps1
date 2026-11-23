# SPDX-License-Identifier: Apache-2.0
# Native Windows baseline gate. PowerShell 5.1+, Cargo, pinned Zig and Python 3.11+.
# No tool installation: missing offline licence prerequisites fail, not skip.
[CmdletBinding()]
param(
    [string]$TargetDir = $env:CARGO_TARGET_DIR,
    [string]$Receipt = $env:CHECK_RECEIPT,
    [switch]$FunctionsOnly
)

function Resolve-GatePython {
    param([string[]]$Directories = ($env:PATH -split [IO.Path]::PathSeparator),
          [string]$Override = $env:VQRO_PYTHON)
    # Match src/program.rs: literal override; then directory-first PATH order,
    # py -3, python, python3 within each directory; never inspect Store aliases.
    if ($Override) {
        return [pscustomobject]@{ Program = $Override; Arguments = @() }
    }
    foreach ($directory in $Directories) {
        if (-not $directory) { continue }
        $directory = $directory.TrimEnd('\', '/')
        if ([IO.Path]::GetFileName($directory) -ieq 'WindowsApps') { continue }
        foreach ($name in @('py.exe', 'python.exe', 'python3.exe')) {
            $candidate = Join-Path $directory $name
            if (Test-Path -LiteralPath $candidate -PathType Leaf) {
                $leading = @()
                if ($name -eq 'py.exe') { $leading = @('-3') }
                return [pscustomobject]@{ Program = $candidate; Arguments = $leading }
            }
        }
    }
    throw 'No Python 3 found (VQRO_PYTHON, then PATH: py -3, python, python3; WindowsApps skipped). Install Python 3.11+ yourself.'
}

function Invoke-GateStep {
    param([string]$Name, [string]$Program, [string[]]$Arguments,
          [string]$LogDirectory, [switch]$ZeroWarnings)
    $log = Join-Path $LogDirectory ($Name + '.log')
    $watch = [Diagnostics.Stopwatch]::StartNew()
    $exitCode = 1
    try {
        # Windows PowerShell treats native stderr as ErrorRecords. Keep it in
        # the log, and judge the native exit code, not PowerShell's $? pipeline.
        $null = Get-Command $Program -CommandType Application -ErrorAction Stop
        $ErrorActionPreference = 'Continue'
        $LASTEXITCODE = 0
        & $Program @Arguments 2>&1 | ForEach-Object { $_.ToString() } |
            Out-File -LiteralPath $log -Encoding utf8
        $exitCode = $LASTEXITCODE
    } catch {
        $_.ToString() | Out-File -LiteralPath $log -Encoding utf8 -Append
        $exitCode = 1
    }
    $warnings = @(Select-String -LiteralPath $log -Pattern '^warning(?:\[|:)' -ErrorAction SilentlyContinue).Count
    $result = 'PASS'
    if ($exitCode -ne 0 -or ($ZeroWarnings -and $warnings -ne 0)) { $result = 'FAIL' }
    $watch.Stop()
    Write-Host ('{0,-26} {1} {2:n1}s ({3} warnings)' -f $Name, $result, $watch.Elapsed.TotalSeconds, $warnings)
    return [pscustomobject]@{ name = $Name; result = $result; seconds = $watch.Elapsed.TotalSeconds;
        log = $log; exit_code = $exitCode; warnings = $warnings }
}

if ($FunctionsOnly) { return }
if ($env:OS -ne 'Windows_NT') { throw 'check.ps1 is the native Windows baseline gate; use check.sh on Unix.' }
$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
$stamp = [DateTime]::UtcNow.ToString('yyyyMMdd-HHmmss') + '-' + [Guid]::NewGuid().ToString('N')
$run = Join-Path ([IO.Path]::GetTempPath()) ('vqro-check-' + $stamp)
$logs = Join-Path $run 'logs'
[void][IO.Directory]::CreateDirectory($logs)
if (-not $TargetDir) { $TargetDir = Join-Path $repo 'target' }
$TargetDir = [IO.Path]::GetFullPath($TargetDir)
if (-not $Receipt) { $Receipt = Join-Path $TargetDir 'check-windows-receipt.json' }
$Receipt = [IO.Path]::GetFullPath($Receipt)
$immutable = Join-Path $logs 'check-receipt.json'
$steps = @()
$savedEnvironment = @{}
foreach ($entry in Get-ChildItem Env:) { $savedEnvironment[$entry.Name] = $entry.Value }
$oldLocation = Get-Location
$oldOutputEncoding = $OutputEncoding
$oldConsoleEncoding = [Console]::OutputEncoding
$commit = 'unknown'
$failed = @()
try {
    # Resolve before clearing inherited vqro configuration; an explicit interpreter
    # is intentional. Preserve native Windows toolchain/environment discovery.
    $python = Resolve-GatePython
    foreach ($entry in Get-ChildItem Env:) {
        if ($entry.Name -match '^(HERDR_|VQRO_|HC_|CHECK_)') {
            [Environment]::SetEnvironmentVariable($entry.Name, $null, 'Process')
        }
    }
    $env:CARGO_TARGET_DIR = $TargetDir
    $env:CARGO_TERM_COLOR = 'never'
    $env:PYTHONUTF8 = '1'
    $env:PYTHONIOENCODING = 'utf-8'
    $env:VQRO_PYTHON = $python.Program
    # py -3 is resolved by the app itself; do not turn it into an invalid
    # one-program override for child processes.
    if ($python.Arguments.Count) { Remove-Item Env:VQRO_PYTHON }
    $env:VQRO_STATE_DIR = Join-Path $run 'state'
    $env:VQRO_EXTENSIONS_DIR = Join-Path $run 'empty-extensions'
    $env:VQRO_HERDR = '["cmd.exe","/c","exit","1"]'
    $env:XDG_RUNTIME_DIR = Join-Path $run 'runtime'
    $env:XDG_STATE_HOME = Join-Path $run 'xdg-state'
    $env:TEMP = Join-Path $run 'tmp'
    $env:TMP = $env:TEMP
    $env:HC_SOCKET = '\\.\pipe\vqro-check-hc-' + $stamp
    $env:HC_SESSION = 'vqro-check-' + $stamp
    foreach ($directory in @($env:VQRO_STATE_DIR, $env:VQRO_EXTENSIONS_DIR, $env:XDG_RUNTIME_DIR, $env:XDG_STATE_HOME, $env:TEMP)) {
        [void][IO.Directory]::CreateDirectory($directory)
    }
    $OutputEncoding = New-Object Text.UTF8Encoding($false)
    [Console]::OutputEncoding = $OutputEncoding
    Set-Location -LiteralPath $repo
    $commitOutput = & git rev-parse HEAD 2>$null
    if ($LASTEXITCODE -eq 0) { $commit = [string]$commitOutput }
    Write-Host ('Windows baseline check at {0}; logs {1}' -f $commit, $logs)
    $steps += Invoke-GateStep 'python-version' $python.Program ($python.Arguments + @('-c', 'import sys; print(sys.version); sys.exit(sys.version_info < (3, 11))')) $logs
    if ($steps[-1].result -ne 'PASS') { throw 'Python version check failed' }
    foreach ($check in @(
        @('licenses', 'scripts/license_check.py', '--check'),
        @('vendor-libghostty', 'scripts/check_vendor.py'),
        @('manifest-deps', 'scripts/check_manifest_deps.py')
    )) {
        $steps += Invoke-GateStep $check[0] $python.Program ($python.Arguments + @($check | Select-Object -Skip 1)) $logs
    }
    $steps += Invoke-GateStep 'build-zero-warnings' 'cargo' @('build', '--locked', '--all-targets') $logs -ZeroWarnings
    $steps += Invoke-GateStep 'cargo-test' 'cargo' @('test', '--locked') $logs
    $steps += Invoke-GateStep 'python-unittest' $python.Program ($python.Arguments + @('-m', 'unittest', 'discover', '-s', 'tests', '-p', 'test_*.py')) $logs
} catch {
    $failureLog = Join-Path $logs 'gate-error.log'
    $_.ToString() | Out-File -LiteralPath $failureLog -Encoding utf8
    $steps += [pscustomobject]@{ name = 'gate-error'; result = 'FAIL'; seconds = 0; log = $failureLog; exit_code = 1; warnings = 0 }
} finally {
    $failed = @($steps | Where-Object { $_.result -ne 'PASS' } | ForEach-Object { $_.name })
    $result = 'pass'
    if ($failed.Count) { $result = 'fail' }
    $record = [ordered]@{ kind = 'vqro.check-receipt'; version = 1; mode = 'windows-baseline';
        commit = $commit; finished_unix = [DateTimeOffset]::UtcNow.ToUnixTimeSeconds(); result = $result;
        failed = $failed; skipped = @(); steps = $steps; logs = $logs; run = $run;
        leftover_scope = 'not-scanned'; leftover_pids = @() }
    try {
        $json = $record | ConvertTo-Json -Depth 8
        [void][IO.Directory]::CreateDirectory((Split-Path -Parent $Receipt))
        $utf8 = New-Object Text.UTF8Encoding($false)
        [IO.File]::WriteAllText($immutable, $json, $utf8)
        [IO.File]::WriteAllText($Receipt, $json, $utf8)
        Write-Host ('{0}: receipt {1}; immutable {2}' -f $result.ToUpperInvariant(), $Receipt, $immutable)
    } finally {
        Set-Location $oldLocation
        foreach ($entry in Get-ChildItem Env:) {
            if (-not $savedEnvironment.ContainsKey($entry.Name)) {
                [Environment]::SetEnvironmentVariable($entry.Name, $null, 'Process')
            }
        }
        foreach ($key in $savedEnvironment.Keys) {
            [Environment]::SetEnvironmentVariable($key, $savedEnvironment[$key], 'Process')
        }
        $OutputEncoding = $oldOutputEncoding
        [Console]::OutputEncoding = $oldConsoleEncoding
    }
}
if ($failed.Count) { exit 1 }
exit 0
