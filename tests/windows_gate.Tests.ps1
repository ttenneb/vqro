# SPDX-License-Identifier: Apache-2.0
# Dependency-free native tests: powershell -NoProfile -File tests/windows_gate.Tests.ps1
$ErrorActionPreference = 'Stop'
. (Join-Path (Split-Path -Parent $PSScriptRoot) 'scripts/check.ps1') -FunctionsOnly
function Assert($Condition, [string]$Message) {
    if (-not $Condition) { throw $Message }
}
$root = Join-Path ([IO.Path]::GetTempPath()) ('vqro-gate-test-' + [Guid]::NewGuid().ToString('N'))
try {
    foreach ($name in @('WindowsApps', 'venv', 'tools', 'logs')) {
        [void][IO.Directory]::CreateDirectory((Join-Path $root $name))
    }
    foreach ($name in @('WindowsApps/python3.exe', 'venv/python.exe', 'tools/py.exe', 'tools/python.exe')) {
        [IO.File]::WriteAllText((Join-Path $root $name), '')
    }
    $apps = Join-Path $root 'WindowsApps'
    $venv = Join-Path $root 'venv'
    $tools = Join-Path $root 'tools'
    $resolved = Resolve-GatePython -Directories @($apps, $venv, $tools) -Override ''
    Assert ($resolved.Program -eq (Join-Path $venv 'python.exe')) 'Directory order must beat a later py launcher'
    Assert ($resolved.Arguments.Count -eq 0) 'python.exe must not gain launcher arguments'
    $resolved = Resolve-GatePython -Directories @($apps, $tools) -Override ''
    Assert ($resolved.Program -eq (Join-Path $tools 'py.exe')) 'py must win inside its directory'
    Assert ($resolved.Arguments[0] -eq '-3') 'py must select Python 3'
    $resolved = Resolve-GatePython -Directories @($apps) -Override 'explicit-python.exe'
    Assert ($resolved.Program -eq 'explicit-python.exe') 'Literal VQRO_PYTHON must win'
    $refused = $false
    try { $null = Resolve-GatePython -Directories @($apps) -Override '' } catch { $refused = $true }
    Assert $refused 'Store-only PATH must be refused'

    $shell = (Get-Process -Id $PID).Path
    $logs = Join-Path $root 'logs'
    $row = Invoke-GateStep 'warning' $shell @('-NoProfile', '-Command', "Write-Output 'warning: fixture'; exit 0") $logs -ZeroWarnings
    Assert ($row.result -eq 'FAIL' -and $row.warnings -eq 1 -and $row.exit_code -eq 0) 'Warnings must fail an otherwise successful build'
    $row = Invoke-GateStep 'native-exit' $shell @('-NoProfile', '-Command', 'exit 7') $logs
    Assert ($row.result -eq 'FAIL' -and $row.exit_code -eq 7) 'Native exit code must survive output pipeline'
    $row = Invoke-GateStep 'success' $shell @('-NoProfile', '-Command', "Write-Output 'ok'; exit 0") $logs -ZeroWarnings
    Assert ($row.result -eq 'PASS' -and $row.warnings -eq 0) 'Successful command must pass'
    $row = Invoke-GateStep 'missing' (Join-Path $root 'missing.exe') @() $logs
    Assert ($row.result -eq 'FAIL') 'Missing executable must fail, not inherit an earlier zero exit'
    Write-Host 'Windows gate tests PASS'
} finally {
    Remove-Item -LiteralPath $root -Recurse -Force
}
