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
# One consent scope: required backup/copy operations cannot be individually skipped.
$ConfirmPreference = 'None'

function Get-PythonPackageFiles($Package) {
    # Same code/data inventory as Unix installation and isolated e2e layouts.
    $source = Join-Path $root 'extensions'
    $inventory = Invoke-Python -Arguments @((Join-Path $source 'vqro_packages.py'), $source, '--package', $Package.Name)
    foreach ($relative in @($inventory | ConvertFrom-Json)) {
        Get-Item -LiteralPath (Join-Path $source $relative)
    }
}

function Find-Python {
    if ($env:VQRO_PYTHON) {
        if ($env:VQRO_PYTHON -match '(?i)[\\/]WindowsApps[\\/]') { throw 'VQRO_PYTHON points to a WindowsApps stub.' }
        return @{ Program = $env:VQRO_PYTHON; Arguments = @() }
    }
    foreach ($dir in ($env:PATH -split ';')) {
        $dir = $dir.Trim('"')
        if (-not $dir) { $dir = '.' } # Empty PATH entries mean the current directory, as in the app.
        if ((Split-Path ($dir.TrimEnd('\', '/')) -Leaf) -ieq 'WindowsApps') { continue }
        foreach ($name in @('py.exe', 'python.exe', 'python3.exe')) {
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
function Assert-PlainPath([string]$Path, [string]$Kind = 'Directory') {
    $current = [IO.Path]::GetFullPath($Path)
    $leaf = $true
    while ($current) {
        # Inspect dangling links as well as existing files. Missing paths are allowed.
        $item = $null
        try { $item = Get-Item -LiteralPath $current -Force -ErrorAction Stop }
        catch [System.Management.Automation.ItemNotFoundException] { }
        if ($null -ne $item) {
            $link = $item.PSObject.Properties['LinkType']
            if (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -or ($link -and $link.Value)) {
                throw "Links/reparse points are not supported: $current"
            }
            $expectDirectory = (-not $leaf) -or $Kind -eq 'Directory'
            if ($item.PSIsContainer -ne $expectDirectory) { throw "Unexpected file/directory type: $current" }
            if ([IO.Path]::GetFullPath($item.FullName).TrimEnd('\') -ine $current.TrimEnd('\')) {
                throw "Path resolves outside the expected location: $current"
            }
        } elseif (Test-Path -LiteralPath $current) { throw "Cannot inspect path: $current" }
        $leaf = $false
        $current = Split-Path $current -Parent
    }
}
function Assert-PlainTree([string]$Path) {
    Assert-PlainPath $Path
    if (Test-Path -LiteralPath $Path) {
        # Reject a junction before walking its children; include hidden entries.
        foreach ($item in Get-ChildItem -LiteralPath $Path -Force) {
            if ($item.PSIsContainer) { Assert-PlainTree $item.FullName }
            else { Assert-PlainPath $item.FullName 'File' }
        }
    }
}
# Read-only handle resolution gives one volume-GUID namespace even through SUBST/drive aliases.
# https://learn.microsoft.com/windows/win32/api/fileapi/nf-fileapi-getfinalpathnamebyhandlew
if (-not ('VqroInstallPaths' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.Runtime.InteropServices;
using System.Text;
using Microsoft.Win32.SafeHandles;
public static class VqroInstallPaths {
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, ExactSpelling = true, SetLastError = true)]
    static extern SafeFileHandle CreateFileW(string path, uint access, uint share,
        IntPtr security, uint creation, uint flags, IntPtr template);
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, ExactSpelling = true, SetLastError = true)]
    static extern uint GetFinalPathNameByHandleW(SafeFileHandle file, StringBuilder path,
        uint capacity, uint flags);
    public static string Existing(string path) {
        // No write access; share read/write/delete, OPEN_EXISTING, BACKUP_SEMANTICS (directories).
        using (var handle = CreateFileW(path, 0, 7, IntPtr.Zero, 3, 0x02000000, IntPtr.Zero)) {
            if (handle.IsInvalid) throw new Win32Exception(Marshal.GetLastWin32Error());
            var buffer = new StringBuilder(32768);
            // FILE_NAME_NORMALIZED | VOLUME_NAME_GUID. Unsupported volumes fail closed.
            uint length = GetFinalPathNameByHandleW(handle, buffer, (uint)buffer.Capacity, 1);
            if (length == 0) throw new Win32Exception(Marshal.GetLastWin32Error());
            if (length >= buffer.Capacity) throw new InvalidOperationException("Physical path too long");
            return buffer.ToString();
        }
    }
}
'@
}
function Resolve-PhysicalPath([string]$Path) {
    $current = [IO.Path]::GetFullPath($Path)
    $suffix = @()
    while ($true) {
        $item = $null
        try { $item = Get-Item -LiteralPath $current -Force -ErrorAction Stop }
        catch [System.Management.Automation.ItemNotFoundException] { }
        if ($null -ne $item) { break }
        $leaf = Split-Path $current -Leaf
        if (-not $leaf -or $leaf -match '[. :]$') { throw "Unsupported missing path component: $current" }
        $suffix = @($leaf) + $suffix
        $parent = Split-Path $current -Parent
        if (-not $parent -or $parent -eq $current) { throw "Cannot resolve physical ancestor: $Path" }
        $current = $parent
    }
    $physical = [VqroInstallPaths]::Existing($item.FullName)
    foreach ($leaf in $suffix) { $physical = [IO.Path]::Combine($physical, $leaf) }
    return $physical
}
function Assert-Separate([string]$Left, [string]$Right) {
    $a = (Resolve-PhysicalPath $Left).TrimEnd('\') + '\'
    $b = (Resolve-PhysicalPath $Right).TrimEnd('\') + '\'
    if ($a.StartsWith($b, [StringComparison]::OrdinalIgnoreCase) -or $b.StartsWith($a, [StringComparison]::OrdinalIgnoreCase)) {
        throw "Overlapping directories are not supported: $Left and $Right"
    }
}
function Get-TreeInventory([string]$Path) {
    Assert-PlainTree $Path
    if (Test-Path -LiteralPath $Path) {
        $base = [IO.Path]::GetFullPath($Path).TrimEnd('\') + '\'
        foreach ($item in Get-ChildItem -LiteralPath $Path -Force -Recurse | Sort-Object FullName) {
            $hash = $null
            if (-not $item.PSIsContainer) { $hash = (Get-FileHash -LiteralPath $item.FullName -Algorithm SHA256).Hash }
            [pscustomobject][ordered]@{ Path = $item.FullName.Substring($base.Length); Directory = [bool]$item.PSIsContainer; SHA256 = $hash }
        }
    }
}
function Get-BinaryInventory([string]$Path) {
    foreach ($name in $names) {
        $file = Join-Path $Path "$name.exe"
        Assert-PlainPath $file 'File'
        $present = Test-Path -LiteralPath $file
        $hash = $null
        if ($present) { $hash = (Get-FileHash -LiteralPath $file -Algorithm SHA256).Hash }
        [pscustomobject][ordered]@{ Name = "$name.exe"; Present = [bool]$present; SHA256 = $hash }
    }
}
function Assert-SameInventory($Expected, $Actual) {
    if ((ConvertTo-Json -InputObject @($Expected) -Depth 5 -Compress) -cne (ConvertTo-Json -InputObject @($Actual) -Depth 5 -Compress)) {
        throw 'Backup inventory/hash verification failed; nothing replaced.'
    }
}
$backup = $null
$backupRoot = Join-Path $env:LOCALAPPDATA 'vqro\backups'
Push-Location -LiteralPath $root
try {
    # Guard managed boundaries before build or shutdown. Linked destinations are unsupported.
    Assert-PlainPath $BinDir
    foreach ($name in $names) { Assert-PlainPath (Join-Path $BinDir "$name.exe") 'File' }
    Assert-PlainTree $extensions
    Assert-PlainPath $backupRoot
    Assert-PlainPath $root
    $manifest = Join-Path $root 'Cargo.toml'
    Assert-PlainPath $manifest 'File'
    if (-not (Test-Path -LiteralPath $manifest -PathType Leaf)) { throw 'Checkout Cargo.toml is missing; nothing installed.' }
    $manifestPhysical = Resolve-PhysicalPath $manifest
    Assert-PlainTree (Join-Path $root 'src')
    Assert-PlainTree (Join-Path $root 'tests')
    Assert-PlainTree (Join-Path $root 'extensions')
    Assert-PlainTree (Join-Path $root 'integrations/pi')
    Assert-PlainTree (Join-Path $root 'scripts')
    $checkout = & git rev-parse --show-toplevel
    if ($LASTEXITCODE -ne 0 -or (Resolve-PhysicalPath $checkout) -ine (Resolve-PhysicalPath $root)) {
        throw 'The script directory must belong to the exact checkout root; nothing installed.'
    }
    # Validate required destination types as well as every existing descendant's link status.
    foreach ($dir in @('manifests', 'optional', 'optional/rex', 'tier1', 'clients', 'tests')) {
        Assert-PlainPath (Join-Path $extensions $dir)
    }
    foreach ($file in @('defaults.json', 'tests.json', 'optional/rex/rex.json', 'optional/rex/README.md', 'tier1/vqro-pull.ts', 'clients/text.json')) {
        Assert-PlainPath (Join-Path $extensions $file) 'File'
    }
    foreach ($file in Get-ChildItem -LiteralPath (Join-Path $root 'extensions') -Filter '*.py' -File) {
        Assert-PlainPath (Join-Path $extensions $file.Name) 'File'
    }
    foreach ($file in Get-ChildItem -LiteralPath (Join-Path $root 'extensions/manifests') -Filter '*.json' -File) {
        Assert-PlainPath (Join-Path (Join-Path $extensions 'manifests') $file.Name) 'File'
    }
    foreach ($file in Get-ChildItem -LiteralPath (Join-Path $root 'extensions/clients') -Filter '*.py' -File) {
        Assert-PlainPath (Join-Path (Join-Path $extensions 'clients') $file.Name) 'File'
    }
    foreach ($package in Get-ChildItem -LiteralPath (Join-Path $root 'extensions') -Directory) {
        if (-not (Test-Path -LiteralPath (Join-Path $package.FullName '__init__.py'))) { continue }
        $destination = Join-Path $extensions $package.Name
        Assert-PlainPath $destination
        foreach ($file in @(Get-PythonPackageFiles $package)) {
            Assert-PlainPath (Join-Path $destination $file.Name) 'File'
        }
    }
    Assert-Separate $BinDir $root
    Assert-Separate $extensions $root
    Assert-Separate $backupRoot $root
    Assert-Separate $BinDir $extensions
    Assert-Separate $BinDir $backupRoot
    Assert-Separate $extensions $backupRoot
    $python = Find-Python
    Invoke-Python -Arguments @('-c', 'import sys; assert sys.version_info >= (3, 11), "Python 3.11+ required"')
    Invoke-Python -Arguments @((Join-Path $root 'scripts/install_extension_tests.py'), '--check', $extensions)
    $metadataText = & cargo metadata --manifest-path $manifest --no-deps --format-version=1
    if ($LASTEXITCODE -ne 0) { throw 'cargo metadata failed; nothing installed.' }
    $metadata = $metadataText | ConvertFrom-Json
    $target = [IO.Path]::GetFullPath($metadata.target_directory)
    # Cargo resolves hierarchy, templates and CARGO_BUILD_BUILD_DIR itself. Never guess a fallback.
    # https://doc.rust-lang.org/cargo/commands/cargo-metadata.html#json-format
    $buildProperty = $metadata.PSObject.Properties['build_directory']
    if (-not $buildProperty -or -not $buildProperty.Value) {
        throw 'Cargo metadata must report build_directory to check all write roots; use Cargo 1.96.1 or newer. Nothing installed.'
    }
    $intermediate = [IO.Path]::GetFullPath($buildProperty.Value)
    foreach ($writeRoot in @($target, $intermediate)) {
        Assert-PlainTree $writeRoot
        foreach ($sourceDirectory in @('extensions', 'integrations', 'scripts', 'src', 'tests')) {
            Assert-Separate $writeRoot (Join-Path $root $sourceDirectory)
        }
        foreach ($destination in @($BinDir, $extensions, $backupRoot)) { Assert-Separate $writeRoot $destination }
    }
    $packages = @($metadata.packages | Where-Object { (Resolve-PhysicalPath $_.manifest_path) -ieq $manifestPhysical })
    if ($packages.Count -ne 1) { throw 'Cannot identify exactly one checkout package; nothing installed.' }
    $packageId = $packages[0].id
    # Freeze the checked target dir; use executable paths from this build, never stale guessed leaves.
    $messages = & cargo build --manifest-path $manifest --release --target-dir $target --message-format=json
    if ($LASTEXITCODE -ne 0) { throw 'cargo build failed; nothing installed.' }
    $artifacts = @{}
    $artifactDirectory = $null
    $artifactProfile = $null
    foreach ($line in $messages) {
        $message = $line | ConvertFrom-Json
        if ($message.reason -eq 'compiler-artifact' -and $message.executable -and $names -contains $message.target.name) {
            if ($artifacts.ContainsKey($message.target.name)) { throw "Ambiguous duplicate artifact for $($message.target.name); nothing installed." }
            if ($message.package_id -cne $packageId -or $message.target.kind.Count -ne 1 -or $message.target.kind[0] -ne 'bin' -or $message.profile.test) {
                throw "Unexpected package/target/profile for $($message.target.name); nothing installed."
            }
            $source = [IO.Path]::GetFullPath($message.executable)
            if (-not $source.StartsWith(($target.TrimEnd('\') + '\'), [StringComparison]::OrdinalIgnoreCase)) {
                throw "Artifact outside checked build directory: $source"
            }
            Assert-PlainPath $source 'File'
            if ([IO.Path]::GetExtension($source) -ine '.exe') { throw "Expected a native Windows executable: $source" }
            if (-not (Test-Path -LiteralPath $source -PathType Leaf)) { throw "Missing artifact: $source" }
            $directory = Resolve-PhysicalPath (Split-Path $source -Parent)
            $profile = $message.profile | ConvertTo-Json -Depth 5 -Compress
            if ($null -eq $artifactDirectory) {
                $artifactDirectory = $directory
                $artifactProfile = $profile
            } elseif ($directory -ine $artifactDirectory -or $profile -cne $artifactProfile) {
                throw 'Artifacts do not form one package/profile/target-directory set; nothing installed.'
            }
            $artifacts[$message.target.name] = $source
        }
    }
    foreach ($name in $names) {
        if (-not $artifacts.ContainsKey($name)) { throw "Build did not report $name.exe; nothing installed." }
    }
    $commit = & git rev-parse HEAD
    if ($LASTEXITCODE -ne 0) { throw 'Cannot identify source commit; nothing installed.' }
    $dirty = & git status --porcelain
    if ($LASTEXITCODE -ne 0) { throw 'Cannot determine source status; nothing installed.' }
    $sourceLabel = "$commit"
    if ($dirty) { $sourceLabel += ' (dirty working tree)' }
    # Windows locks executing images. Finish every preflight before stopping or copying.
    $cli = $artifacts['vqro']
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

    # Recheck before backup. A failed or changing backup aborts before any replacement.
    Assert-PlainPath $BinDir
    Assert-PlainTree $extensions
    Assert-PlainPath $backupRoot
    $priorBinaries = @(Get-BinaryInventory $BinDir)
    $priorExtensionsPresent = Test-Path -LiteralPath $extensions
    $priorExtensions = @(Get-TreeInventory $extensions)
    $slot = Join-Path $backupRoot (([DateTime]::UtcNow.ToString('yyyyMMddTHHmmssfffffffZ')) + '-' + [Guid]::NewGuid().ToString('N'))
    Assert-PlainPath $slot
    if ([IO.Path]::GetDirectoryName([IO.Path]::GetFullPath($slot)).TrimEnd('\') -ine [IO.Path]::GetFullPath($backupRoot).TrimEnd('\')) {
        throw 'Generated backup slot must be a plain direct child of the backup root.'
    }
    if (Test-Path -LiteralPath $slot) { throw "Backup slot already exists; retained backup will not be reused: $slot" }
    $backup = $slot
    [IO.Directory]::CreateDirectory($backup) | Out-Null
    $backupBin = Join-Path $backup 'bin'
    [IO.Directory]::CreateDirectory($backupBin) | Out-Null
    foreach ($name in $names) {
        $installed = Join-Path $BinDir "$name.exe"
        if (Test-Path -LiteralPath $installed) { Copy-Item -LiteralPath $installed -Destination $backupBin }
    }
    if (Test-Path -LiteralPath $extensions) {
        Copy-Item -LiteralPath $extensions -Destination (Join-Path $backup 'extensions') -Recurse -Force
    }
    Assert-SameInventory $priorBinaries @(Get-BinaryInventory $backupBin)
    Assert-SameInventory $priorExtensions @(Get-TreeInventory (Join-Path $backup 'extensions'))
    if ($priorExtensionsPresent -ne (Test-Path -LiteralPath (Join-Path $backup 'extensions'))) { throw 'Backup extension root missing.' }
    Assert-SameInventory $priorBinaries @(Get-BinaryInventory $BinDir)
    Assert-SameInventory $priorExtensions @(Get-TreeInventory $extensions)
    if ($priorExtensionsPresent -ne (Test-Path -LiteralPath $extensions)) { throw 'Installed extension root changed during backup.' }
    $inventory = [ordered]@{ BinDir = $BinDir; ExtensionsDir = $extensions; Binaries = $priorBinaries; ExtensionsPresent = [bool]$priorExtensionsPresent; Extensions = $priorExtensions }
    $inventory | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath (Join-Path $backup 'inventory.json') -Encoding UTF8
    @("Binary destination: $BinDir", "Extension destination: $extensions", 'Backup inventory and SHA256 verified before installation.',
        'Close all vqro processes. Check inventory.json before restoring.',
        'For each Binaries entry: Present=true -> copy its backup/bin file over the destination; Present=false -> remove ONLY that named managed destination file if the failed install created it.',
        'Move the failed extensions directory aside. If ExtensionsPresent=true, copy backup/extensions back; otherwise leave extensions absent.',
        'Preserve unrelated files in BinDir. Reject links/reparse points before manual restoration.') |
        Set-Content -LiteralPath (Join-Path $backup 'restore.txt') -Encoding UTF8
    Write-Host "Backup: $backup (retained, including previous backups)."

    [IO.Directory]::CreateDirectory($extensions) | Out-Null
    [IO.Directory]::CreateDirectory($BinDir) | Out-Null
    Get-ChildItem -LiteralPath (Join-Path $root 'extensions') -Filter '*.py' -File |
        ForEach-Object { Copy-Item -LiteralPath $_.FullName -Destination $extensions -Force }
    Copy-Item -LiteralPath (Join-Path $root 'extensions/defaults.json') -Destination $extensions -Force
    $manifests = Join-Path $extensions 'manifests'
    [IO.Directory]::CreateDirectory($manifests) | Out-Null
    Get-ChildItem -LiteralPath (Join-Path $root 'extensions/manifests') -Filter '*.json' -File |
        ForEach-Object { Copy-Item -LiteralPath $_.FullName -Destination $manifests -Force }
    Invoke-Python -Arguments @((Join-Path $root 'scripts/install_extension_tests.py'), $extensions)
    $rex = Join-Path $extensions 'optional/rex'
    [IO.Directory]::CreateDirectory($rex) | Out-Null
    foreach ($file in @('rex.json', 'README.md')) {
        Copy-Item -LiteralPath (Join-Path (Join-Path $root 'extensions/optional/rex') $file) -Destination $rex -Force
    }
    $tier1 = Join-Path $extensions 'tier1'
    [IO.Directory]::CreateDirectory($tier1) | Out-Null
    Copy-Item -LiteralPath (Join-Path $root 'integrations/pi/vqro-pull.ts') -Destination $tier1 -Force
    $clients = Join-Path $extensions 'clients'
    [IO.Directory]::CreateDirectory($clients) | Out-Null
    Get-ChildItem -LiteralPath (Join-Path $root 'extensions/clients') -Filter '*.py' -File |
        ForEach-Object { Copy-Item -LiteralPath $_.FullName -Destination $clients -Force }
    Copy-Item -LiteralPath (Join-Path $root 'extensions/clients/text.json') -Destination $clients -Force
    foreach ($package in Get-ChildItem -LiteralPath (Join-Path $root 'extensions') -Directory) {
        if (-not (Test-Path -LiteralPath (Join-Path $package.FullName '__init__.py'))) { continue }
        $destination = Join-Path $extensions $package.Name
        if ($package.Name -eq 'pi_bridge' -and (Test-Path -LiteralPath $destination)) {
            Remove-Item -LiteralPath $destination -Recurse -Force
        }
        [IO.Directory]::CreateDirectory($destination) | Out-Null
        Get-PythonPackageFiles $package |
            ForEach-Object { Copy-Item -LiteralPath $_.FullName -Destination $destination -Force }
    }
    foreach ($name in $names) {
        $source = $artifacts[$name]
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
    Write-Host "Installed build from source $sourceLabel to $BinDir; extensions to $extensions"
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
        Write-Warning "Restore only after closing all vqro processes: check restore.txt for verified backup completion and inventory.json for every managed file's prior presence/hash. Restore Present=true binaries from backup/bin; remove ONLY named Present=false managed binaries introduced by this install (preserve unrelated files in $BinDir). Move the failed extensions directory aside; restore backup/extensions only if ExtensionsPresent=true, otherwise leave extensions absent. Do not start mixed versions; repair and rerun instead if backup is incomplete."
    }
    throw
} finally { Pop-Location }
