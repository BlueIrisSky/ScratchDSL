# Development-only registry fixtures. This script never targets HKCU\Environment.
# Run from the repository root:
# powershell.exe -NoProfile -ExecutionPolicy Bypass -File packaging/windows/tests/path.ps1
param([string]$Helper = '')

Set-StrictMode -Version 2.0
$ErrorActionPreference = 'Stop'
# WSL-launched PowerShell can inherit PATHEXT without .EXE. This process-only
# adjustment ensures child helper calls execute synchronously as native tools.
if ($env:PATHEXT -notmatch '(?i)(^|;)\.EXE(;|$)') { $env:PATHEXT = "$env:PATHEXT;.EXE" }
$repo = Split-Path (Split-Path (Split-Path $PSScriptRoot -Parent) -Parent) -Parent
if (-not $Helper) { $Helper = Join-Path $repo 'packaging\windows\Update-UserPath.ps1' }
$Helper = [IO.Path]::GetFullPath($Helper)
if (-not (Test-Path -LiteralPath $Helper -PathType Leaf)) { throw "Missing helper: $Helper" }
$powershellExe = Join-Path $PSHOME 'powershell.exe'
$registry = [Microsoft.Win32.Registry]::CurrentUser
$fixtureBase = 'Software\ScratchDSLPathTests'
$existingBase = $registry.OpenSubKey($fixtureBase)
$baseExisted = $null -ne $existingBase
if ($existingBase) { $existingBase.Dispose() }
$completed = [Collections.Generic.List[string]]::new()
$fixtureRoots = [Collections.Generic.List[string]]::new()

function Assert-Equal($Actual, $Expected, [string]$Message) {
    if ($Actual -cne $Expected) { throw "${Message}: expected [$Expected], got [$Actual]" }
}

function Read-Snapshot([string]$KeyPath) {
    $key = $registry.OpenSubKey($KeyPath)
    try {
        if ($null -eq $key -or @($key.GetValueNames()) -notcontains 'Path') {
            return [pscustomobject]@{ Exists = $false; Value = ''; Kind = 'Missing' }
        }
        return [pscustomobject]@{
            Exists = $true
            Value = $key.GetValue('Path', $null, [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
            Kind = $key.GetValueKind('Path').ToString()
        }
    } finally { if ($key) { $key.Dispose() } }
}

function Assert-Snapshot($Actual, $Expected, [string]$Message) {
    Assert-Equal $Actual.Exists $Expected.Exists "$Message existence"
    Assert-Equal $Actual.Value $Expected.Value "$Message raw text"
    Assert-Equal $Actual.Kind $Expected.Kind "$Message registry kind"
}

function Set-FixturePath($Fixture, [string]$Value, [Microsoft.Win32.RegistryValueKind]$Kind) {
    $key = $registry.CreateSubKey($Fixture.Environment)
    try { $key.SetValue('Path', $Value, $Kind) } finally { $key.Dispose() }
}

function Read-Owner($Fixture) {
    $key = $registry.OpenSubKey($Fixture.State)
    try {
        if ($null -eq $key) { return $null }
        $raw = $key.GetValue('PathEntry', $null, [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
        if ($null -eq $raw) { return $null }
        return ConvertFrom-Json -InputObject ([string]$raw)
    } finally { if ($key) { $key.Dispose() } }
}

function Invoke-Helper($Fixture, [string]$Action, [string]$InstallDir = '', [int]$ExpectedExit = 0) {
    if (-not $InstallDir) { $InstallDir = $Fixture.InstallDir }
    # A failing native helper is an assertion result, not a PowerShell error trap.
    $savedPreference = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try {
        $global:LASTEXITCODE = -999
        $output = & $powershellExe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File $Helper `
            -Action $Action -InstallDir $InstallDir -FixtureRoot $Fixture.Root 2>&1
        $code = $LASTEXITCODE
    } finally { $ErrorActionPreference = $savedPreference }
    if ($code -ne $ExpectedExit) {
        throw "Helper $Action exited $code instead of ${ExpectedExit}: $($output -join ' ')"
    }
}

function Invoke-Fixture([string]$Name, [scriptblock]$Test) {
    $root = "$fixtureBase\$([Guid]::NewGuid().ToString())"
    $fixtureRoots.Add($root)
    # Construct non-ASCII path characters without depending on Windows PS 5.1's
    # source-file encoding. The directory need not exist for registry fixtures.
    $chinese = [string][char]0x4e2d + [char]0x6587
    $fixture = [pscustomobject]@{
        Root = $root; Environment = "$root\Environment"; State = "$root\State"
        InstallDir = [IO.Path]::GetFullPath((Join-Path $repo "target\PATH fixture $chinese $([Guid]::NewGuid())"))
    }
    try {
        & $Test $fixture
        $completed.Add($Name)
        Write-Host "PASS $Name"
    } finally { $registry.DeleteSubKeyTree($root, $false) }
}

$realBefore = Read-Snapshot 'Environment'
try {
    Invoke-Fixture 'long REG_EXPAND_SZ, exact ownership, duplicate Add and Remove' {
        param($f)
        $raw = '%USERPROFILE%\Raw Variable;' + (('C:\OtherEntry0123456789;' * 500)) + ';'
        if ($raw.Length -le 8192) { throw 'Long PATH fixture is too short' }
        Set-FixturePath $f $raw ([Microsoft.Win32.RegistryValueKind]::ExpandString)
        $before = Read-Snapshot $f.Environment
        Invoke-Helper $f 'Add'
        $after = Read-Snapshot $f.Environment
        Assert-Equal $after.Value ($raw + ';' + $f.InstallDir) 'Long PATH append'
        Assert-Equal $after.Kind 'ExpandString' 'Long PATH kind'
        $owner = Read-Owner $f
        if ($null -eq $owner) { throw 'Missing ownership record' }
        Assert-Equal $owner.BeforeValue $raw 'Ownership BeforeValue'
        Assert-Equal $owner.BeforeKind 'ExpandString' 'Ownership BeforeKind'
        Assert-Equal $owner.BeforeExists $true 'Ownership BeforeExists'
        Assert-Equal $owner.AfterValue $after.Value 'Ownership AfterValue'
        Assert-Equal $owner.Entry $f.InstallDir 'Ownership Entry'
        $ownerBefore = ConvertTo-Json $owner -Compress
        Invoke-Helper $f 'Add'
        Assert-Snapshot (Read-Snapshot $f.Environment) $after 'Duplicate Add'
        Assert-Equal (ConvertTo-Json (Read-Owner $f) -Compress) $ownerBefore 'Duplicate ownership'
        Invoke-Helper $f 'Remove'
        Assert-Snapshot (Read-Snapshot $f.Environment) $before 'Long PATH roundtrip'
        Assert-Equal (Read-Owner $f) $null 'Ownership removed'
        Invoke-Helper $f 'Remove'
        Assert-Snapshot (Read-Snapshot $f.Environment) $before 'Duplicate Remove'
    }

    Invoke-Fixture 'REG_SZ preserves percent text, empty entries and trailing separator' {
        param($f)
        Set-FixturePath $f 'C:\Alpha;;%USERPROFILE%\Literal;' ([Microsoft.Win32.RegistryValueKind]::String)
        $before = Read-Snapshot $f.Environment
        Invoke-Helper $f 'Add'
        Assert-Equal (Read-Snapshot $f.Environment).Kind 'String' 'REG_SZ Add kind'
        Invoke-Helper $f 'Remove'
        Assert-Snapshot (Read-Snapshot $f.Environment) $before 'REG_SZ roundtrip'
    }

    Invoke-Fixture 'pre-existing equivalent entry is never owned or removed' {
        param($f)
        $equivalent = '"' + $f.InstallDir.ToUpperInvariant() + '\"'
        Set-FixturePath $f ("C:\Before;$equivalent;C:\After") ([Microsoft.Win32.RegistryValueKind]::String)
        $before = Read-Snapshot $f.Environment
        Invoke-Helper $f 'Add'
        Assert-Snapshot (Read-Snapshot $f.Environment) $before 'Pre-existing Add'
        Assert-Equal (Read-Owner $f) $null 'No ownership of pre-existing entry'
        Invoke-Helper $f 'Remove'
        Assert-Snapshot (Read-Snapshot $f.Environment) $before 'Pre-existing Remove'
    }

    Invoke-Fixture 'environment-variable spelling counts as a pre-existing path' {
        param($f)
        $installDir = Join-Path $env:USERPROFILE 'ScratchDSL Existing Fixture'
        Set-FixturePath $f '%USERPROFILE%\ScratchDSL Existing Fixture;C:\After' ([Microsoft.Win32.RegistryValueKind]::ExpandString)
        $before = Read-Snapshot $f.Environment
        Invoke-Helper $f 'Add' $installDir
        Assert-Equal (Read-Owner $f) $null 'No ownership of expanded-equivalent entry'
        Invoke-Helper $f 'Remove' $installDir
        Assert-Snapshot (Read-Snapshot $f.Environment) $before 'Expanded-equivalent roundtrip'
    }

    Invoke-Fixture 'concurrent edits and current kind survive removal' {
        param($f)
        $raw = 'C:\Original;;%USERPROFILE%\Keep;'
        Set-FixturePath $f $raw ([Microsoft.Win32.RegistryValueKind]::String)
        Invoke-Helper $f 'Add'
        $after = (Read-Snapshot $f.Environment).Value
        $suffix = ';' + $f.InstallDir + '-other;%TEMP%\Added Later;'
        $changed = 'C:\Added Before;' + $after + $suffix
        Set-FixturePath $f $changed ([Microsoft.Win32.RegistryValueKind]::ExpandString)
        Invoke-Helper $f 'Remove'
        $expected = [pscustomobject]@{
            Exists = $true; Value = 'C:\Added Before;' + $raw + $suffix; Kind = 'ExpandString'
        }
        Assert-Snapshot (Read-Snapshot $f.Environment) $expected 'Concurrent PATH edits'
        Assert-Equal (Read-Owner $f) $null 'Concurrent edit cleanup ownership'
    }

    Invoke-Fixture 'originally missing Path remains missing after removal' {
        param($f)
        $before = Read-Snapshot $f.Environment
        Assert-Equal $before.Exists $false 'Missing fixture precondition'
        Invoke-Helper $f 'Add'
        Assert-Equal (Read-Owner $f).BeforeExists $false 'Missing original ownership'
        Invoke-Helper $f 'Remove'
        Assert-Snapshot (Read-Snapshot $f.Environment) $before 'Missing Path roundtrip'
    }

    Invoke-Fixture 'existing empty REG_SZ remains an existing empty value' {
        param($f)
        Set-FixturePath $f '' ([Microsoft.Win32.RegistryValueKind]::String)
        $before = Read-Snapshot $f.Environment
        Invoke-Helper $f 'Add'
        Invoke-Helper $f 'Remove'
        Assert-Snapshot (Read-Snapshot $f.Environment) $before 'Empty Path roundtrip'
    }

    Invoke-Fixture 'already removed owned entry does not affect unrelated data' {
        param($f)
        Set-FixturePath $f 'C:\Original' ([Microsoft.Win32.RegistryValueKind]::String)
        Invoke-Helper $f 'Add'
        Set-FixturePath $f 'C:\New;%TEMP%\Unrelated' ([Microsoft.Win32.RegistryValueKind]::ExpandString)
        $changed = Read-Snapshot $f.Environment
        Invoke-Helper $f 'Remove'
        Assert-Snapshot (Read-Snapshot $f.Environment) $changed 'Missing owned entry'
        Assert-Equal (Read-Owner $f) $null 'Missing owned entry ownership cleanup'
    }

    Invoke-Fixture 'another installation cannot overwrite ownership' {
        param($f)
        Set-FixturePath $f 'C:\Original' ([Microsoft.Win32.RegistryValueKind]::String)
        Invoke-Helper $f 'Add'
        $before = Read-Snapshot $f.Environment
        $ownerBefore = ConvertTo-Json (Read-Owner $f) -Compress
        Invoke-Helper $f 'Add' ($f.InstallDir + '-different') 1
        Assert-Snapshot (Read-Snapshot $f.Environment) $before 'Foreign owner Add rejection'
        Assert-Equal (ConvertTo-Json (Read-Owner $f) -Compress) $ownerBefore 'Foreign owner record preserved'
        Invoke-Helper $f 'Remove'
    }

    Invoke-Fixture 'non-string Path is rejected without a write' {
        param($f)
        $key = $registry.CreateSubKey($f.Environment)
        try { $key.SetValue('Path', 42, [Microsoft.Win32.RegistryValueKind]::DWord) } finally { $key.Dispose() }
        Invoke-Helper $f 'Add' '' 1
        $after = Read-Snapshot $f.Environment
        Assert-Equal $after.Kind 'DWord' 'Unsupported kind preserved'
        Assert-Equal $after.Value 42 'Unsupported value preserved'
        Assert-Equal (Read-Owner $f) $null 'No ownership for unsupported value'
    }
} finally {
    foreach ($root in $fixtureRoots) { $registry.DeleteSubKeyTree($root, $false) }
    if (-not $baseExisted) {
        $base = $registry.OpenSubKey($fixtureBase)
        if ($base) {
            $empty = $base.SubKeyCount -eq 0 -and $base.ValueCount -eq 0
            $base.Dispose()
            if ($empty) { $registry.DeleteSubKey($fixtureBase, $false) }
        }
    }
    Assert-Snapshot (Read-Snapshot 'Environment') $realBefore 'Real user PATH must remain untouched'
}
Write-Host "All $($completed.Count) isolated PATH fixtures passed; the real user PATH was unchanged."
# Expected-failure fixtures leave a native exit code behind. Report the suite's
# successful result to callers, including GitHub Actions' PowerShell wrapper.
$global:LASTEXITCODE = 0
