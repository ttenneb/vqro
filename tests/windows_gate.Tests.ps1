# SPDX-License-Identifier: Apache-2.0
# Dependency-free native tests: powershell -NoProfile -File tests/windows_gate.Tests.ps1
$ErrorActionPreference = 'Stop'
. (Join-Path (Split-Path -Parent $PSScriptRoot) 'scripts/check.ps1') -FunctionsOnly
function Assert($Condition, [string]$Message) { if (-not $Condition) { throw $Message } }
$root = Join-Path ([IO.Path]::GetTempPath()) ('vqro-gate-test-' + [Guid]::NewGuid().ToString('N'))
$oldPythonPath = $env:PYTHONPATH
$oldAuth = $env:OPENAI_API_KEY
try {
    foreach ($name in @('WindowsApps', 'venv', 'tools', 'logs', 'injection')) {
        [void][IO.Directory]::CreateDirectory((Join-Path $root $name))
    }
    foreach ($name in @('WindowsApps/python3.exe', 'venv/python.exe', 'tools/py.exe', 'tools/python.exe')) {
        [IO.File]::WriteAllText((Join-Path $root $name), '')
    }
    $apps = Join-Path $root 'WindowsApps'
    $venv = Join-Path $root 'venv'
    $tools = Join-Path $root 'tools'
    $resolved = Resolve-GatePython -Directories @($apps, $venv, $tools) -Override ''
    Assert ($resolved.Program -eq (Join-Path $venv 'python.exe')) 'Directory order must beat later py'
    Assert ($resolved.Arguments.Count -eq 0) 'python.exe must not gain launcher args'
    $resolved = Resolve-GatePython -Directories @($apps, $tools) -Override ''
    Assert ($resolved.Program -eq (Join-Path $tools 'py.exe')) 'py must win in its directory'
    Assert ($resolved.Arguments[0] -eq '-3') 'py must select Python 3'
    $resolved = Resolve-GatePython -Directories @($apps) -Override 'explicit-python.exe'
    Assert ($resolved.Program -eq 'explicit-python.exe') 'Literal override must win'
    $refused = $false
    try { $null = Resolve-GatePython -Directories @($apps) -Override '' } catch { $refused = $true }
    Assert $refused 'Store-only PATH must fail'

    $env:PYTHONPATH = Join-Path $root 'injection'
    $env:OPENAI_API_KEY = 'gate-auth-canary-not-a-real-key'
    $inputs = @{}
    foreach ($entry in Get-ChildItem Env:) { $inputs[$entry.Name] = $entry.Value }
    # Test the allowlist without linking real tool caches in this unit fixture.
    $inputs.Remove('USERPROFILE')
    $inputs.Remove('CARGO_HOME')
    $inputs.Remove('RUSTUP_HOME')
    foreach ($key in @('PYTHONSTARTUP', 'PYTHONHOME', 'PYTHONUSERBASE', 'VQRO_SOCKET_SOURCE', 'HC_CONFIG', 'HERDR_SOCKET')) {
        $inputs[$key] = 'inherited-canary-must-not-survive'
    }
    $child = New-GateEnvironment (Join-Path $root 'run') (Join-Path $root 'target') $inputs
    Assert (-not $child.ContainsKey('PYTHONPATH')) 'Import injection must not reach children'
    Assert (-not $child.ContainsKey('OPENAI_API_KEY')) 'Auth canary must not reach children'
    foreach ($key in @('PYTHONSTARTUP', 'PYTHONHOME', 'PYTHONUSERBASE', 'VQRO_SOCKET_SOURCE', 'HC_CONFIG', 'HERDR_SOCKET')) {
        Assert (-not $child.ContainsKey($key)) ('Inherited override survived: ' + $key)
    }
    Assert ($child.VQRO_SOCKET.StartsWith('\\.\pipe\vqro-check-')) 'vqro endpoint must be explicitly private'
    Assert ($child.HC_SOCKET.StartsWith('\\.\pipe\vqro-check-hc-')) 'Backend endpoint must be explicitly private'
    foreach ($key in @('HOME', 'USERPROFILE', 'APPDATA', 'LOCALAPPDATA', 'TEMP', 'CARGO_HOME')) {
        Assert ($child[$key].StartsWith($root)) ('Profile/temp/config root not private: ' + $key)
    }
    $shell = (Get-Process -Id $PID).Path
    $logs = Join-Path $root 'logs'
    $row = Invoke-GateStep 'warning' $shell @('-NoProfile', '-Command', "[Console]::Error.WriteLine('warning: fixture'); exit 0") $logs $child $root -ZeroWarnings
    Assert ($row.result -eq 'FAIL' -and $row.warnings -eq 1 -and $row.exit_code -eq 0) 'Native stderr warnings must fail zero-warning build'
    $row = Invoke-GateStep 'stderr' $shell @('-NoProfile', '-Command', "[Console]::Error.WriteLine('ordinary stderr'); exit 0") $logs $child $root -ZeroWarnings
    Assert ($row.result -eq 'PASS') 'Ordinary stderr alone is not a capture failure'
    $row = Invoke-GateStep 'native-exit' $shell @('-NoProfile', '-Command', 'exit 7') $logs $child $root
    Assert ($row.result -eq 'FAIL' -and $row.exit_code -eq 7) 'Native exit code must survive capture'
    $row = Invoke-GateStep 'missing' (Join-Path $root 'missing.exe') @() $logs $child $root
    Assert ($row.result -eq 'FAIL' -and $row.evidence_error) 'Missing executable must fail'
    [void][IO.Directory]::CreateDirectory((Join-Path $logs 'write-failure.log'))
    $row = Invoke-GateStep 'write-failure' $shell @('-NoProfile', '-Command', 'exit 0') $logs $child $root
    Assert ($row.result -eq 'FAIL' -and $row.evidence_error) 'Failed log write must be sticky FAIL despite exit 0'
    $originalReader = ${function:Read-GateLog}
    try {
        function Read-GateLog { param($Path); throw 'fixture log read failure' }
        $row = Invoke-GateStep 'read-failure' $shell @('-NoProfile', '-Command', 'exit 0') $logs $child $root
        Assert ($row.result -eq 'FAIL' -and $row.evidence_error) 'Failed log read must be sticky FAIL despite exit 0'
    } finally { ${function:Read-GateLog} = $originalReader }

    # The first Python launch must not import even a hostile inherited startup
    # module. A real interpreter is required; missing Python is FAIL, not SKIP.
    $marker = Join-Path $root 'sitecustomize-ran'
    $site = "import pathlib; pathlib.Path(" + (ConvertTo-Json $marker -Compress) + ").write_text('unsafe')"
    [IO.File]::WriteAllText((Join-Path $env:PYTHONPATH 'sitecustomize.py'), $site)
    $probe = Join-Path $root 'probe.py'
    [IO.File]::WriteAllText($probe, "import os,site,sys`nassert sys.flags.isolated == 1`nassert sys.flags.utf8_mode == 1`nassert not site.ENABLE_USER_SITE`nassert 'OPENAI_API_KEY' not in os.environ`nassert 'PYTHONPATH' not in os.environ`nassert os.environ['HOME'] == os.environ['USERPROFILE'] == sys.argv[1]`n")
    $argvCases = @('space value', 'embedded"quote', 'C:\tail\', '')
    [IO.File]::AppendAllText($probe, ('assert sys.argv[2:] == ' + (ConvertTo-Json $argvCases -Compress) + "`n"))
    $python = Resolve-GatePython
    $row = Invoke-GateStep 'python-canary' $python.Program ($python.Arguments + @('-I', '-X', 'utf8', $probe, $child.HOME) + $argvCases) $logs $child $root
    Assert ($row.result -eq 'PASS') 'First Python process must have isolated startup and no inherited auth/import environment'
    Assert (-not (Test-Path -LiteralPath $marker)) 'sitecustomize canary must never execute'

    # Invalid UTF-8 must be drained as bytes through a pipe-sized flood before
    # decoding fails. Retain both raw streams and execute the command only once.
    $flood = Join-Path $root 'invalid-flood.py'
    $once = Join-Path $root 'flood-attempts'
    [IO.File]::WriteAllText($flood, "import os,pathlib,sys`npathlib.Path(sys.argv[1]).open('a').write('attempt\n')`nsys.stdout.buffer.write(bytes([255]) + b'x' * (2 * 1024 * 1024))`nsys.stdout.buffer.flush()`nsys.stderr.buffer.write(b'y' * (2 * 1024 * 1024))`nsys.stderr.buffer.flush()`n")
    $floodSource = [IO.File]::ReadAllText($flood)
    Assert ($floodSource.Contains('bytes([255])')) 'Generated control must emit one invalid byte, not an escaped ASCII spelling'
    Assert ($floodSource.Contains(".write('attempt\n')")) 'Generated attempt counter must contain a Python newline escape'
    Assert (-not $floodSource.Contains(".write('attempt\\n')")) 'Generated attempt counter must not double-escape the newline'
    $row = Invoke-GateStep 'invalid-flood' $python.Program ($python.Arguments + @('-I', '-X', 'utf8', $flood, $once)) $logs $child $root -TimeoutSeconds 30
    Assert ($row.result -eq 'FAIL' -and $row.exit_code -eq 0 -and $row.evidence_error) 'Invalid UTF-8 must fail after the child exits, never stall its raw drains'
    Assert ((Get-Item -LiteralPath $row.stdout_bytes).Length -eq (2 * 1024 * 1024 + 1)) 'Entire invalid stdout flood must be retained'
    Assert ((Get-Item -LiteralPath $row.stderr_bytes).Length -eq (2 * 1024 * 1024)) 'Concurrent stderr flood must be retained'
    Assert ([IO.File]::ReadAllLines($once).Count -eq 1) 'Capture failure must never replay a command'

    $fault = New-Object 'System.Threading.Tasks.TaskCompletionSource[bool]'
    $fault.SetException((New-Object IO.IOException('fixture raw drain fault')))
    $observed = $false
    try { Wait-GateCapture $null @($fault.Task) ([Diagnostics.Stopwatch]::StartNew()) 30 }
    catch { $observed = $_.ToString().Contains('fixture raw drain fault') }
    Assert $observed 'Drain task faults must be observed before waiting on process lifetime'
    $pending = New-Object 'System.Threading.Tasks.TaskCompletionSource[bool]'
    $eofBounded = $false
    try { Wait-GateCapture ([pscustomobject]@{ HasExited = $true }) @($pending.Task) ([Diagnostics.Stopwatch]::StartNew()) 1 }
    catch { $eofBounded = $_.ToString().Contains('deadline exceeded') }
    Assert $eofBounded 'An exited parent with a held-open pipe must still have bounded completion'

    $sleeper = Join-Path $root 'owned-sleeper.py'
    [IO.File]::WriteAllText($sleeper, "import time`ntime.sleep(60)`n")
    $row = Invoke-GateStep 'owned-timeout' $python.Program ($python.Arguments + @('-I', '-X', 'utf8', $sleeper)) $logs $child $root -TimeoutSeconds 2
    Assert ($row.result -eq 'FAIL' -and $row.evidence_error.Contains('deadline exceeded')) 'An unfinished owned command must time out and FAIL'
    Assert ($row.cleanup.StartsWith('taskkill accepted')) 'Timeout control must clean only its live owned child tree'

    $expected = @('one', 'two')
    $one = [pscustomobject]@{ name = 'one'; result = 'PASS' }
    $two = [pscustomobject]@{ name = 'two'; result = 'PASS' }
    foreach ($case in @(
        @{ rows = @(); normal = $false },
        @{ rows = @(); normal = $true },
        @{ rows = @($one); normal = $true },
        @{ rows = @($one, $two); normal = $false },
        @{ rows = @($one, $one, $two); normal = $true }
    )) {
        $rows = @(Complete-GateSteps $case.rows $expected $case.normal)
        Assert (@($rows | Where-Object { $_.result -ne 'PASS' }).Count -gt 0) 'Empty/prefix/interrupted/duplicate run must fail'
    }
    $rows = @(Complete-GateSteps @($one, $two) $expected $true)
    Assert (@($rows | Where-Object { $_.result -ne 'PASS' }).Count -eq 0) 'Only a complete normally finished run can pass'
    Write-Host 'Windows gate tests PASS'
} finally {
    $env:PYTHONPATH = $oldPythonPath
    $env:OPENAI_API_KEY = $oldAuth
    # Junction-free fixture: never recurse into approved external cache roots.
    Remove-Item -LiteralPath $root -Recurse -Force
}
