# Native Windows development install; not a release qualification gate.
[CmdletBinding(SupportsShouldProcess = $true)]
param(
    [string]$BinDir = (Join-Path $env:LOCALAPPDATA 'vqro\bin'),
    [switch]$AddToPath
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version 2.0
$root = Split-Path $PSScriptRoot -Parent
$extensions = Join-Path $env:LOCALAPPDATA 'vqro\extensions'
$names = @('vqro-herdr', 'vqro-tmux', 'vqro-rex', 'vqro-bridge-pi', 'vqro-bridge-codex', 'vqro-claude-hook', 'vqro')
$BinDir = [IO.Path]::GetFullPath($BinDir)
# A true plan-only path: no build, interpreter invocation, shutdown, mkdir or PATH edit.
if ($WhatIfPreference) {
    Write-Host "Would build cargo release in $root; verify all seven executables and bundled tests."
    Write-Host "Would discover Python (VQRO_PYTHON, py -3, python, python3; no WindowsApps)."
    Write-Host 'Would request protocol shutdown and wait up to 15 seconds for all vqro processes to exit; otherwise abort.'
    Write-Host 'Would back up existing executables and extensions to a timestamped LOCALAPPDATA/vqro/backups folder first (backups retained).'
    Write-Host "Would install extensions, manifests, tests, optional rex, tier1 plugin, clients and packages to $extensions."
    Write-Host "Would stage and rename executables into $BinDir; verify and print SHA256/version/commit."
    if ($AddToPath) { Write-Host "Would add $BinDir to user PATH if absent." }
    Write-Host 'Would leave the core stopped; vqro tui starts it.'
    return
}
if (-not $PSCmdlet.ShouldProcess($BinDir, 'Build and install vqro and bundled extensions')) { return }

function Find-Python {
    if ($env:VQRO_PYTHON) {
        if ($env:VQRO_PYTHON -match '(?i)[\\/]WindowsApps[\\/]') { throw 'VQRO_PYTHON points to a WindowsApps stub.' }
        return @{ Program = $env:VQRO_PYTHON; Arguments = @() }
    }
    foreach ($name in @('py.exe', 'python.exe', 'python3.exe')) {
        foreach ($dir in ($env:PATH -split ';')) {
            $dir = $dir.Trim('"')
            if (-not $dir -or (Split-Path $dir -Leaf) -ieq 'WindowsApps') { continue }
            $candidate = Join-Path $dir $name
            if (Test-Path -LiteralPath $candidate -PathType Leaf) {
                $arguments = @()
                if ($name -eq 'py.exe') { $arguments = @('-3') }
                return @{ Program = $candidate; Arguments = $arguments }
            }
        }
    }
    throw 'No Python 3 found: tried VQRO_PYTHON, py -3, python, python3 (WindowsApps skipped). Install Python 3.11+ or set VQRO_PYTHON.'
}
function Invoke-Python([string[]]$Arguments) {
    $prefix = @($python.Arguments)
    & $python.Program @prefix @Arguments
    if ($LASTEXITCODE -ne 0) { throw "Python failed (exit $LASTEXITCODE); installation aborted." }
}
function Get-VqroProcesses {
    # Do not kill anything: even another state directory or an open UI must be closed explicitly.
    @(Get-Process -Name 'vqro*' -ErrorAction SilentlyContinue)
}
$backup = $null
Push-Location $root
try {
    & cargo build --release
    if ($LASTEXITCODE -ne 0) { throw 'cargo build failed; nothing installed.' }
    $target = 'target'
    if ($env:CARGO_TARGET_DIR) { $target = $env:CARGO_TARGET_DIR }
    $release = Join-Path $target 'release'
    foreach ($name in $names) {
        $source = Join-Path $release "$name.exe"
        if (-not (Test-Path -LiteralPath $source -PathType Leaf)) { throw "$source missing; nothing installed." }
    }
    $python = Find-Python
    Invoke-Python -Arguments @('-c', 'import sys; assert sys.version_info >= (3, 11), "Python 3.11+ required"')
    Invoke-Python -Arguments @('scripts/install_extension_tests.py', '--check', $extensions)

    # Windows locks executing images. Finish every preflight before stopping or copying.
    $cli = Join-Path $release 'vqro.exe'
    $status = & $cli status
    if ($LASTEXITCODE -ne 0) { throw 'Cannot determine core status; nothing installed.' }
    if ($status -match '^running') {
        Write-Host 'Stopping the core through vqro stop. Close open UIs so they cannot restart it.'
        & $cli stop
        if ($LASTEXITCODE -ne 0) { throw 'Protocol shutdown failed; nothing installed.' }
    }
    $deadline = (Get-Date).AddSeconds(15)
    while (@(Get-VqroProcesses).Count -gt 0 -and (Get-Date) -lt $deadline) { Start-Sleep -Milliseconds 250 }
    $remaining = @(Get-VqroProcesses)
    if ($remaining.Count -gt 0) {
        throw "vqro processes still running (PIDs: $($remaining.Id -join ', ')). Close UIs/other cores/bridges and retry; nothing installed. No processes were forcibly killed."
    }

    # Back up before the first replacement; a failed backup aborts without installing.
    $backupRoot = Join-Path $env:LOCALAPPDATA 'vqro\backups'
    $backup = Join-Path $backupRoot (([DateTime]::UtcNow.ToString('yyyyMMddTHHmmssfffffffZ')) + '-' + [Guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $backup | Out-Null
    $backupBin = Join-Path $backup 'bin'
    New-Item -ItemType Directory -Path $backupBin | Out-Null
    foreach ($name in $names) {
        $installed = Join-Path $BinDir "$name.exe"
        if (Test-Path -LiteralPath $installed) { Copy-Item -LiteralPath $installed -Destination $backupBin }
    }
    if (Test-Path -LiteralPath $extensions) {
        Copy-Item -LiteralPath $extensions -Destination (Join-Path $backup 'extensions') -Recurse
    }
    @("Binary destination: $BinDir", "Extension destination: $extensions", 'Backup completed before installation.') |
        Set-Content -LiteralPath (Join-Path $backup 'restore.txt') -Encoding UTF8
    Write-Host "Backup: $backup (retained, including previous backups)."

    New-Item -ItemType Directory -Force -Path $extensions, $BinDir | Out-Null
    Copy-Item extensions/*.py, extensions/defaults.json -Destination $extensions -Force
    $manifests = Join-Path $extensions 'manifests'
    New-Item -ItemType Directory -Force -Path $manifests | Out-Null
    Copy-Item extensions/manifests/*.json -Destination $manifests -Force
    Invoke-Python -Arguments @('scripts/install_extension_tests.py', $extensions)
    $rex = Join-Path $extensions 'optional/rex'
    New-Item -ItemType Directory -Force -Path $rex | Out-Null
    Copy-Item extensions/optional/rex/rex.json, extensions/optional/rex/README.md -Destination $rex -Force
    $tier1 = Join-Path $extensions 'tier1'
    New-Item -ItemType Directory -Force -Path $tier1 | Out-Null
    Copy-Item integrations/pi/vqro-pull.ts -Destination $tier1 -Force
    $clients = Join-Path $extensions 'clients'
    New-Item -ItemType Directory -Force -Path $clients | Out-Null
    Copy-Item extensions/clients/*.py, extensions/clients/text.json -Destination $clients -Force
    foreach ($package in Get-ChildItem extensions -Directory) {
        if (-not (Test-Path -LiteralPath (Join-Path $package.FullName '__init__.py'))) { continue }
        $destination = Join-Path $extensions $package.Name
        if ($package.Name -eq 'pi_bridge' -and (Test-Path -LiteralPath $destination)) {
            Remove-Item -LiteralPath $destination -Recurse -Force
        }
        New-Item -ItemType Directory -Force -Path $destination | Out-Null
        Copy-Item (Join-Path $package.FullName '*.py') -Destination $destination -Force
    }
    foreach ($name in $names) {
        $source = Join-Path $release "$name.exe"
        $destination = Join-Path $BinDir "$name.exe"
        $temporary = Join-Path $BinDir ('.' + $name + '.' + [Guid]::NewGuid().ToString('N') + '.new')
        try {
            Copy-Item -LiteralPath $source -Destination $temporary
            $hash = (Get-FileHash -LiteralPath $source -Algorithm SHA256).Hash
            if ((Get-FileHash -LiteralPath $temporary -Algorithm SHA256).Hash -ne $hash) { throw "Staged $name hash mismatch" }
            Move-Item -LiteralPath $temporary -Destination $destination -Force
            if ((Get-FileHash -LiteralPath $destination -Algorithm SHA256).Hash -ne $hash) { throw "Installed $name hash mismatch" }
            Write-Host "$name.exe SHA256 $hash"
        } finally {
            if (Test-Path -LiteralPath $temporary) { Remove-Item -LiteralPath $temporary -Force }
        }
    }
    & (Join-Path $BinDir 'vqro.exe') --version
    if ($LASTEXITCODE -ne 0) { throw 'Installed vqro --version failed.' }
    $commit = & git rev-parse HEAD
    Write-Host "Installed commit $commit to $BinDir; extensions to $extensions"
    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    $onPath = @($userPath -split ';' | Where-Object { $_.TrimEnd('\') -ieq $BinDir.TrimEnd('\') }).Count -gt 0
    if (-not $onPath) {
        if ($AddToPath) {
            [Environment]::SetEnvironmentVariable('Path', ($BinDir + ';' + $userPath), 'User')
            Write-Host 'Added to user PATH. Open a new terminal to use it.'
        } else {
            $quoted = $BinDir.Replace("'", "''")
            Write-Host 'User PATH unchanged. To add the install directory, run:'
            Write-Host ("[Environment]::SetEnvironmentVariable('Path', '" + $quoted + ";' + [Environment]::GetEnvironmentVariable('Path', 'User'), 'User')")
        }
    }
    Write-Host 'Core not started. Run vqro tui to start it (use the full installed path until PATH is refreshed).'
} catch {
    if ($backup) {
        Write-Warning "Install failed. Backup (possibly incomplete if backup itself failed): $backup"
        Write-Warning "Restore only after closing all vqro processes: check restore.txt for backup completion, copy backup/bin/*.exe to $BinDir; move the failed extensions directory aside and copy backup/extensions to $extensions if present. If no prior install existed, there is nothing to restore. Do not start mixed versions; repair and rerun instead if backup is incomplete."
    }
    throw
} finally { Pop-Location }
