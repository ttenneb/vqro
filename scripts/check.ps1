# SPDX-License-Identifier: Apache-2.0
# Native Windows baseline, PowerShell 5.1+. Never installs tools.
[CmdletBinding()]
param([string]$TargetDir = $env:CARGO_TARGET_DIR,
      [string]$Receipt = $env:CHECK_RECEIPT,
      [ValidateRange(1, 86400)][int]$StepTimeoutSeconds = 3600,
      [switch]$FunctionsOnly)

function Resolve-GatePython {
    param([string[]]$Directories = ($env:PATH -split [IO.Path]::PathSeparator),
          [string]$Override = $env:VQRO_PYTHON)
    if ($Override) { return [pscustomobject]@{ Program = $Override; Arguments = @() } }
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
    throw 'No Python 3 found (VQRO_PYTHON, then directory-first PATH: py -3, python, python3; WindowsApps skipped). Python 3.11+ required.'
}

function New-GateEnvironment {
    param([string]$Run, [string]$BuildTarget, [hashtable]$ToolEnvironment)
    # Allowlist, not a scrub: no inherited runtime/auth/import/config overrides.
    # PATH and Windows SDK variables are deliberate tool discovery inputs.
    $child = @{}
    foreach ($key in @('PATH', 'PATHEXT', 'SystemRoot', 'WINDIR', 'COMSPEC', 'SystemDrive', 'OS',
        'PROCESSOR_ARCHITECTURE', 'PROCESSOR_ARCHITEW6432', 'NUMBER_OF_PROCESSORS',
        'INCLUDE', 'LIB', 'LIBPATH', 'VCINSTALLDIR', 'VCToolsInstallDir', 'VSINSTALLDIR',
        'VisualStudioVersion', 'WindowsSdkDir', 'WindowsSDKVersion', 'UniversalCRTSdkDir',
        'UCRTVersion', 'VSCMD_ARG_TGT_ARCH', 'VSCMD_ARG_HOST_ARCH', 'ZIG')) {
        if ($ToolEnvironment[$key]) { $child[$key] = $ToolEnvironment[$key] }
    }
    $profile = Join-Path $Run 'profile'
    $child.HOME = $profile
    $child.USERPROFILE = $profile
    $child.APPDATA = Join-Path $profile 'AppData/Roaming'
    $child.LOCALAPPDATA = Join-Path $profile 'AppData/Local'
    $child.TEMP = Join-Path $Run 'tmp'
    $child.TMP = $child.TEMP
    $child.CARGO_HOME = Join-Path $Run 'cargo-home'
    $child.CARGO_TARGET_DIR = $BuildTarget
    $child.CARGO_TERM_COLOR = 'never'
    $child.PYTHONNOUSERSITE = '1'
    $child.PYTHONIOENCODING = 'utf-8'
    $child.GIT_CONFIG_NOSYSTEM = '1'
    $child.GIT_CONFIG_GLOBAL = Join-Path $profile 'empty-gitconfig'
    $child.VQRO_STATE_DIR = Join-Path $Run 'state'
    $child.VQRO_EXTENSIONS_DIR = Join-Path $Run 'empty-extensions'
    $child.VQRO_HERDR = '["cmd.exe","/c","exit","1"]'
    $child.XDG_RUNTIME_DIR = Join-Path $Run 'runtime'
    $child.XDG_STATE_HOME = Join-Path $Run 'xdg-state'
    $child.XDG_CONFIG_HOME = Join-Path $Run 'xdg-config'
    $child.XDG_CACHE_HOME = Join-Path $Run 'xdg-cache'
    $id = [Guid]::NewGuid().ToString('N')
    $child.VQRO_SOCKET = '\\.\pipe\vqro-check-' + $id
    $child.HC_SOCKET = '\\.\pipe\vqro-check-hc-' + $id
    $child.HC_SESSION = 'vqro-check-' + $id
    foreach ($directory in @($profile, $child.APPDATA, $child.LOCALAPPDATA, $child.TEMP,
        $child.CARGO_HOME, $child.VQRO_STATE_DIR, $child.VQRO_EXTENSIONS_DIR,
        $child.XDG_RUNTIME_DIR, $child.XDG_STATE_HOME, $child.XDG_CONFIG_HOME, $child.XDG_CACHE_HOME)) {
        [void][IO.Directory]::CreateDirectory($directory)
    }
    # Share only package/source cache subdirectories, never Cargo config or
    # credentials. Junctions avoid copying caches and need no elevation on NTFS.
    $cargoCache = $ToolEnvironment.CARGO_HOME
    if (-not $cargoCache -and $ToolEnvironment.USERPROFILE) {
        $cargoCache = Join-Path $ToolEnvironment.USERPROFILE '.cargo'
    }
    if ($cargoCache) {
        foreach ($name in @('registry', 'git')) {
            $cache = Join-Path $cargoCache $name
            if (Test-Path -LiteralPath $cache -PathType Container) {
                $null = New-Item -ItemType Junction -Path (Join-Path $child.CARGO_HOME $name) -Value $cache -ErrorAction Stop
            }
        }
    }
    # Rustup holds approved toolchains/settings, not native agent profiles.
    $rustup = $ToolEnvironment.RUSTUP_HOME
    if (-not $rustup -and $ToolEnvironment.USERPROFILE) { $rustup = Join-Path $ToolEnvironment.USERPROFILE '.rustup' }
    if ($rustup) { $child.RUSTUP_HOME = $rustup }
    foreach ($key in @('ZIG_GLOBAL_CACHE_DIR', 'ZIG_LOCAL_CACHE_DIR')) {
        if ($ToolEnvironment[$key]) { $child[$key] = $ToolEnvironment[$key] }
    }
    # Preserve only the known pinned executable, not the real profile as a
    # fallback search root (build.rs normally searches this location itself).
    if (-not $child.ZIG -and $ToolEnvironment.USERPROFILE) {
        $zig = Join-Path $ToolEnvironment.USERPROFILE '.local/zig-0.17.1/zig.exe'
        if (Test-Path -LiteralPath $zig -PathType Leaf) { $child.ZIG = $zig }
    }
    return $child
}

function ConvertTo-GateArgument {
    param([AllowEmptyString()][string]$Argument)
    # Windows CRT argv quoting; ProcessStartInfo.ArgumentList is unavailable on
    # .NET Framework/PowerShell 5.1. Protect quotes and trailing backslashes.
    $escaped = [regex]::Replace($Argument, '(\\*)"', '$1$1\"')
    $escaped = [regex]::Replace($escaped, '(\\+)$', '$1$1')
    return '"' + $escaped + '"'
}

function Read-GateLog {
    param([string]$Path)
    return [IO.File]::ReadAllText($Path, (New-Object Text.UTF8Encoding($false, $true)))
}

function Wait-GateCapture {
    param($Process, [object[]]$Tasks, [Diagnostics.Stopwatch]$Watch, [int]$TimeoutSeconds)
    while ($true) {
        foreach ($task in $Tasks) {
            if ($task.IsFaulted) { throw $task.Exception }
            if ($task.IsCanceled) { throw 'Raw pipe drain canceled' }
        }
        $done = @($Tasks | Where-Object { -not $_.IsCompleted }).Count -eq 0
        if ($Process.HasExited -and $done) { return }
        if ($Watch.Elapsed.TotalSeconds -ge $TimeoutSeconds) {
            throw ('Owned command/capture deadline exceeded ({0}s)' -f $TimeoutSeconds)
        }
        # Both child lifetime and EOF are bounded; descendants can hold pipe
        # handles after the direct child exits. Never wait unbounded for EOF.
        if ($Process.HasExited) { [Threading.Thread]::Sleep(100) }
        else { $null = $Process.WaitForExit(100) }
    }
}

function Stop-GateOwnedTree {
    param([Diagnostics.Process]$Process, [hashtable]$ChildEnvironment, [string]$WorkingDirectory)
    # Never replay a numeric PID after observing its owned process has exited.
    if ($Process.HasExited) { return 'Parent already exited; descendant cleanup unconfirmed (no PID replay)' }
    $killer = New-Object Diagnostics.Process
    try {
        $start = New-Object Diagnostics.ProcessStartInfo
        $start.FileName = Join-Path $ChildEnvironment.SystemRoot 'System32/taskkill.exe'
        $start.Arguments = '/PID ' + $Process.Id + ' /T /F'
        $start.UseShellExecute = $false
        $start.CreateNoWindow = $true
        $start.WorkingDirectory = $WorkingDirectory
        $start.EnvironmentVariables.Clear()
        foreach ($key in $ChildEnvironment.Keys) { $start.EnvironmentVariables[$key] = $ChildEnvironment[$key] }
        $killer.StartInfo = $start
        if ($Process.HasExited) { return 'Parent already exited; descendant cleanup unconfirmed (no PID replay)' }
        if (-not $killer.Start()) { throw 'Owned-tree cleanup helper did not start' }
        if (-not $killer.WaitForExit(10000)) {
            $killer.Kill() # Only the exact cleanup helper process we started.
            $null = $killer.WaitForExit(2000)
            return 'taskkill timed out; owned-tree cleanup unconfirmed'
        }
        if ($killer.ExitCode -ne 0) { return ('taskkill failed ({0}); cleanup unconfirmed' -f $killer.ExitCode) }
        if (-not $Process.WaitForExit(2000)) { return 'Owned parent did not exit after taskkill; cleanup unconfirmed' }
        return 'taskkill accepted for the live owned parent/tree; no global leftover scan'
    } catch { return ('Owned-tree cleanup unconfirmed: ' + $_.ToString()) }
    finally { $killer.Dispose() }
}

function Invoke-GateStep {
    param([string]$Name, [string]$Program, [string[]]$Arguments,
          [string]$LogDirectory, [hashtable]$ChildEnvironment, [string]$WorkingDirectory,
          [switch]$ZeroWarnings, [ValidateRange(1, 86400)][int]$TimeoutSeconds = 3600)
    $log = Join-Path $LogDirectory ($Name + '.log')
    $watch = [Diagnostics.Stopwatch]::StartNew()
    $exitCode = 1
    $warnings = 0
    $evidenceError = $null
    $cleanup = $null
    $started = $false
    $drainsFinished = $false
    $stdoutFile = $null
    $stderrFile = $null
    $stdoutPath = $log + '.stdout.bin'
    $stderrPath = $log + '.stderr.bin'
    $process = New-Object Diagnostics.Process
    try {
        $start = New-Object Diagnostics.ProcessStartInfo
        $start.FileName = (Get-Command $Program -CommandType Application -ErrorAction Stop).Source
        $start.Arguments = (($Arguments | ForEach-Object { ConvertTo-GateArgument $_ }) -join ' ')
        $start.UseShellExecute = $false
        $start.CreateNoWindow = $true
        $start.RedirectStandardOutput = $true
        $start.RedirectStandardError = $true
        $start.WorkingDirectory = $WorkingDirectory
        $start.EnvironmentVariables.Clear()
        foreach ($key in $ChildEnvironment.Keys) { $start.EnvironmentVariables[$key] = $ChildEnvironment[$key] }
        $process.StartInfo = $start
        # Open destinations before launch: a failed capture file must not leave
        # a child running with undrained pipes.
        $stdoutFile = [IO.File]::Create($stdoutPath)
        $stderrFile = [IO.File]::Create($stderrPath)
        $started = $process.Start()
        if (-not $started) { throw 'Native process did not start' }
        # Drain raw bytes. Invalid UTF-8 cannot fault a decoder mid-stream and
        # stop pipe drainage. Observe I/O faults while the child is still live.
        $stdout = $process.StandardOutput.BaseStream.CopyToAsync($stdoutFile)
        $stderr = $process.StandardError.BaseStream.CopyToAsync($stderrFile)
        Wait-GateCapture $process @($stdout, $stderr) $watch $TimeoutSeconds
        $null = $stdout.GetAwaiter().GetResult()
        $null = $stderr.GetAwaiter().GetResult()
        $drainsFinished = $true
        $exitCode = $process.ExitCode
        $stdoutFile.Dispose(); $stdoutFile = $null
        $stderrFile.Dispose(); $stderrFile = $null
        # Decode only after both raw drains and the owned process completed.
        # Retain .bin evidence even when strict decoding fails.
        $text = (Read-GateLog $stdoutPath) + "`n" + (Read-GateLog $stderrPath)
        [IO.File]::WriteAllText($log, $text, (New-Object Text.UTF8Encoding($false)))
        # A write/read/decoder failure is sticky FAIL, never zero warnings/PASS.
        $captured = Read-GateLog $log
        $warnings = [regex]::Matches($captured, '(?m)^warning(?:\[|:)').Count
    } catch {
        $evidenceError = $_.ToString()
        try { [IO.File]::AppendAllText($log, $evidenceError, (New-Object Text.UTF8Encoding($false))) } catch { }
    } finally {
        if ($started -and ($evidenceError -or -not $process.HasExited)) {
            if ($process.HasExited -and $drainsFinished) {
                $cleanup = 'Owned command and raw drains completed; decode/log failure, no kill needed'
            } else { $cleanup = Stop-GateOwnedTree $process $ChildEnvironment $WorkingDirectory }
        }
        if ($stdoutFile) { $stdoutFile.Dispose() }
        if ($stderrFile) { $stderrFile.Dispose() }
        $process.Dispose()
        $watch.Stop()
    }
    $result = 'PASS'
    if ($evidenceError -or $exitCode -ne 0 -or ($ZeroWarnings -and $warnings -ne 0)) { $result = 'FAIL' }
    Write-Host ('{0,-26} {1} {2:n1}s ({3} warnings)' -f $Name, $result, $watch.Elapsed.TotalSeconds, $warnings)
    return [pscustomobject]@{ name = $Name; result = $result; seconds = $watch.Elapsed.TotalSeconds;
        log = $log; exit_code = $exitCode; warnings = $warnings; evidence_error = $evidenceError;
        stdout_bytes = $stdoutPath; stderr_bytes = $stderrPath; cleanup = $cleanup }
}

function Complete-GateSteps {
    param([object[]]$Steps, [string[]]$Expected, [bool]$NormalCompletion)
    $rows = @($Steps)
    foreach ($name in $Expected) {
        if (@($rows | Where-Object { $_.name -eq $name }).Count -ne 1) {
            $rows += [pscustomobject]@{ name = $name; result = 'FAIL'; seconds = 0;
                log = $null; exit_code = $null; warnings = 0; evidence_error = 'Expected exactly one completed terminal step' }
        }
    }
    if (-not $NormalCompletion) {
        $rows += [pscustomobject]@{ name = 'normal-completion'; result = 'FAIL'; seconds = 0;
            log = $null; exit_code = $null; warnings = 0; evidence_error = 'Gate interrupted or did not reach normal completion' }
    }
    return $rows
}

if ($FunctionsOnly) { return }
if ($env:OS -ne 'Windows_NT') { throw 'check.ps1 is native Windows only; use check.sh on Unix.' }
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
$retained = Join-Path $logs 'check-receipt.json'
$expected = @('python-version', 'licenses', 'vendor-libghostty', 'manifest-deps', 'build-zero-warnings', 'cargo-test', 'python-unittest')
$steps = @()
$normalCompletion = $false
$commit = 'unknown'
$revision = $null
$toolEnvironment = @{}
foreach ($entry in Get-ChildItem Env:) { $toolEnvironment[$entry.Name] = $entry.Value }
try {
    $python = Resolve-GatePython
    $child = New-GateEnvironment $run $TargetDir $toolEnvironment
    # VQRO_PYTHON is one executable, not a command line. Preserve the selected
    # literal interpreter for application children, but never encode py -3 here.
    if (-not $python.Arguments.Count) { $child.VQRO_PYTHON = $python.Program }
    # -I ignores PYTHON* injection and user site even for the very first probe.
    $pythonArgs = $python.Arguments + @('-I', '-X', 'utf8')
    $revision = Invoke-GateStep 'source-revision' 'git' @('rev-parse', 'HEAD') $logs $child $repo -TimeoutSeconds $StepTimeoutSeconds
    if ($revision.result -ne 'PASS') { throw 'Could not record source revision' }
    $commit = (Read-GateLog $revision.log).Trim()
    Write-Host ('Windows baseline check at {0}; logs {1}' -f $commit, $logs)
    $steps += Invoke-GateStep 'python-version' $python.Program ($pythonArgs + @('-c', 'import sys; print(sys.version); sys.exit(sys.version_info < (3, 11))')) $logs $child $repo -TimeoutSeconds $StepTimeoutSeconds
    if ($steps[-1].result -ne 'PASS') { throw 'Python version check failed' }
    foreach ($check in @(
        @('licenses', 'scripts/license_check.py', '--check'),
        @('vendor-libghostty', 'scripts/check_vendor.py'),
        @('manifest-deps', 'scripts/check_manifest_deps.py')
    )) {
        $steps += Invoke-GateStep $check[0] $python.Program ($pythonArgs + @($check | Select-Object -Skip 1)) $logs $child $repo -TimeoutSeconds $StepTimeoutSeconds
    }
    $steps += Invoke-GateStep 'build-zero-warnings' 'cargo' @('build', '--locked', '--all-targets') $logs $child $repo -ZeroWarnings -TimeoutSeconds $StepTimeoutSeconds
    if ($steps[-1].result -eq 'PASS') {
        $steps += Invoke-GateStep 'cargo-test' 'cargo' @('test', '--locked') $logs $child $repo -TimeoutSeconds $StepTimeoutSeconds
    } else {
        $steps += [pscustomobject]@{ name = 'cargo-test'; result = 'FAIL'; seconds = 0; log = $null;
            exit_code = $null; warnings = 0; evidence_error = 'Blocked by failed build; no implicit rebuild' }
    }
    # Never drop full discovery: unported Windows fixtures are real FAILs until
    # the separately reviewed portability fixes land.
    $steps += Invoke-GateStep 'python-unittest' $python.Program ($pythonArgs + @('-m', 'unittest', 'discover', '-s', 'tests', '-p', 'test_*.py')) $logs $child $repo -TimeoutSeconds $StepTimeoutSeconds
    $normalCompletion = $true
} catch {
    $steps += [pscustomobject]@{ name = 'gate-error'; result = 'FAIL'; seconds = 0; log = $null;
        exit_code = $null; warnings = 0; evidence_error = $_.ToString() }
} finally {
    $steps = @(Complete-GateSteps $steps $expected $normalCompletion)
    $failed = @($steps | Where-Object { $_.result -ne 'PASS' } | ForEach-Object { $_.name })
    $result = 'pass'
    if ($failed.Count) { $result = 'fail' }
    $record = [ordered]@{ kind = 'vqro.check-receipt'; version = 1; mode = 'windows-baseline';
        commit = $commit; finished_unix = [DateTimeOffset]::UtcNow.ToUnixTimeSeconds(); result = $result;
        failed = $failed; skipped = @(); steps = $steps; expected = $expected; normal_completion = $normalCompletion;
        logs = $logs; run = $run; source_revision = $revision; step_timeout_seconds = $StepTimeoutSeconds;
        leftover_scope = 'not-scanned'; leftover_pids = @() }
    $json = $record | ConvertTo-Json -Depth 8
    [void][IO.Directory]::CreateDirectory((Split-Path -Parent $Receipt))
    $utf8 = New-Object Text.UTF8Encoding($false)
    # Failure here terminates nonzero: an old latest receipt is not this run.
    [IO.File]::WriteAllText($retained, $json, $utf8)
    [IO.File]::WriteAllText($Receipt, $json, $utf8)
    Write-Host ('{0}: fresh retained receipt {1}; latest {2}' -f $result.ToUpperInvariant(), $retained, $Receipt)
}
if ($failed.Count) { exit 1 }
exit 0
