param(
    [Parameter(Mandatory = $true)][ValidateSet('Add', 'Remove')][string]$Action,
    [Parameter(Mandatory = $true)][string]$InstallDir,
    [string]$FixtureRoot = ''
)

# PATH never passes through NSIS strings, setx, cmd.exe, or expanded registry reads.
# A fixture is restricted to an independent, disposable HKCU subtree.
Set-StrictMode -Version 2.0
$ErrorActionPreference = 'Stop'
$environmentKey = $null
$stateKey = $null
$mutex = $null
$locked = $false
$exitCode = 0

function Get-PathSnapshot {
    $exists = @($environmentKey.GetValueNames()) -contains 'Path'
    if (-not $exists) {
        return [pscustomobject]@{ Exists = $false; Value = ''; Kind = 'ExpandString' }
    }
    $kind = $environmentKey.GetValueKind('Path').ToString()
    if ($kind -notin @('String', 'ExpandString')) { throw 'The user Path registry value is not a string.' }
    $value = $environmentKey.GetValue('Path', $null, [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
    return [pscustomobject]@{ Exists = $true; Value = [string]$value; Kind = $kind }
}

function Set-PathSnapshot($Expected, $Next) {
    # Detect edits made while this operation was being prepared. Other programs do
    # not share our mutex, so recheck immediately before the registry write.
    $current = Get-PathSnapshot
    if ($current.Exists -ne $Expected.Exists -or $current.Kind -cne $Expected.Kind -or $current.Value -cne $Expected.Value) {
        throw 'The user PATH changed during this operation. Retry without overwriting the other change.'
    }
    if ($Next.Exists) {
        $kind = [Microsoft.Win32.RegistryValueKind][Enum]::Parse([Microsoft.Win32.RegistryValueKind], $Next.Kind)
        $environmentKey.SetValue('Path', [string]$Next.Value, $kind)
    } else {
        $environmentKey.DeleteValue('Path', $false)
    }
    $environmentKey.Flush()
}

function Get-ComparablePath([string]$Entry) {
    $value = $Entry.Trim()
    if ($value.Length -ge 2 -and $value.StartsWith('"') -and $value.EndsWith('"')) {
        $value = $value.Substring(1, $value.Length - 2)
    }
    if (-not $value) { return '' }
    $value = [Environment]::ExpandEnvironmentVariables($value)
    try { $value = [IO.Path]::GetFullPath($value) } catch { return $value }
    $root = [IO.Path]::GetPathRoot($value)
    if ($value.Length -gt $root.Length) { $value = $value.TrimEnd([char[]]'\/') }
    return $value
}

function Get-OwnedEntryPosition([string]$Value, [string]$Entry) {
    $offset = 0
    foreach ($part in $Value.Split([char]';')) {
        if ([string]::Equals($part, $Entry, [StringComparison]::Ordinal)) { return $offset }
        $offset += $part.Length + 1
    }
    return -1
}

function Read-Ownership {
    $raw = $stateKey.GetValue('PathEntry', $null, [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
    if ($null -eq $raw) { return $null }
    $record = ConvertFrom-Json -InputObject ([string]$raw)
    foreach ($name in @('Version', 'InstallDir', 'Entry', 'BeforeExists', 'BeforeValue', 'BeforeKind', 'AfterValue')) {
        if ($null -eq $record.PSObject.Properties[$name]) { throw 'The PATH ownership record is incomplete.' }
    }
    if ($record.Version -ne 1 -or $record.BeforeKind -notin @('String', 'ExpandString') -or
        $record.BeforeExists -isnot [bool] -or $record.BeforeValue -isnot [string] -or
        $record.Entry -isnot [string] -or $record.InstallDir -isnot [string] -or
        $record.AfterValue -isnot [string] -or -not $record.Entry -or $record.Entry.Contains(';')) {
        throw 'The PATH ownership record is invalid.'
    }
    $expectedAfter = if ($record.BeforeValue.Length -eq 0) { $record.Entry } else { $record.BeforeValue + ';' + $record.Entry }
    if ($record.AfterValue -cne $expectedAfter -or $record.Entry -cne $record.InstallDir) {
        throw 'The PATH ownership record does not describe an appended installation entry.'
    }
    return $record
}

function Send-EnvironmentChange {
    if ($FixtureRoot) { return $true }
    Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class ScratchDSLPathNotification {
    [DllImport("user32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    public static extern IntPtr SendMessageTimeout(IntPtr window, uint message,
        UIntPtr wParam, string lParam, uint flags, uint timeout, out UIntPtr result);
}
'@
    $result = [UIntPtr]::Zero
    $sent = [ScratchDSLPathNotification]::SendMessageTimeout([IntPtr]0xffff, 0x001a,
        [UIntPtr]::Zero, 'Environment', 2, 5000, [ref]$result)
    return $sent -ne [IntPtr]::Zero
}

try {
    if ($FixtureRoot -and $FixtureRoot -notmatch '^Software\\ScratchDSLPathTests\\[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$') {
        throw 'FixtureRoot must be Software\ScratchDSLPathTests\<GUID>.'
    }
    if (-not [IO.Path]::IsPathRooted($InstallDir) -or $InstallDir.IndexOfAny([char[]]";`r`n`"") -ge 0) {
        throw 'InstallDir must be an absolute path without semicolons, quotes, or line breaks.'
    }
    $entry = [IO.Path]::GetFullPath($InstallDir)
    $root = [IO.Path]::GetPathRoot($entry)
    if ($entry.Length -gt $root.Length) { $entry = $entry.TrimEnd([char[]]'\/') }
    $environmentPath = if ($FixtureRoot) { "$FixtureRoot\Environment" } else { 'Environment' }
    $statePath = if ($FixtureRoot) { "$FixtureRoot\State" } else { 'Software\ScratchDSL' }
    $sid = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
    $mutex = New-Object Threading.Mutex($false, "Local\ScratchDSL.UserPath.$sid")
    try { $locked = $mutex.WaitOne(10000) } catch [Threading.AbandonedMutexException] { $locked = $true }
    if (-not $locked) { throw 'Another ScratchDSL PATH update is still running.' }
    $environmentKey = [Microsoft.Win32.Registry]::CurrentUser.CreateSubKey($environmentPath)
    $stateKey = [Microsoft.Win32.Registry]::CurrentUser.CreateSubKey($statePath)
    $current = Get-PathSnapshot
    $owner = Read-Ownership
    $changed = $false

    if ($null -ne $owner -and -not [string]::Equals($owner.InstallDir, $entry, [StringComparison]::OrdinalIgnoreCase)) {
        throw 'The PATH entry belongs to a different ScratchDSL installation. Uninstall that installation first.'
    }

    if ($Action -eq 'Add') {
        if ($null -ne $owner -and (Get-OwnedEntryPosition $current.Value $owner.Entry) -ge 0) {
            Write-Host 'The installer-owned PATH entry is already present.'
        } else {
            if ($null -ne $owner) { $stateKey.DeleteValue('PathEntry', $false) }
            $comparable = Get-ComparablePath $entry
            $alreadyPresent = $false
            foreach ($part in $current.Value.Split([char]';')) {
                if ([string]::Equals((Get-ComparablePath $part), $comparable, [StringComparison]::OrdinalIgnoreCase)) {
                    $alreadyPresent = $true
                    break
                }
            }
            if ($alreadyPresent) {
                Write-Host 'An equivalent user PATH entry already exists; no ownership was claimed.'
            } else {
                # Always append our own separator; empty/trailing existing entries
                # remain byte-for-byte intact, including when others append later.
                $after = if ($current.Value.Length -eq 0) { $entry } else { $current.Value + ';' + $entry }
                $owner = [ordered]@{
                    Version = 1; InstallDir = $entry; Entry = $entry
                    BeforeExists = $current.Exists; BeforeValue = $current.Value
                    BeforeKind = $current.Kind; AfterValue = $after
                }
                # Persist intent first. A failed write can then be safely retried;
                # an absent entry is never removed merely because intent exists.
                $stateKey.SetValue('PathEntry', (ConvertTo-Json -InputObject $owner -Compress), [Microsoft.Win32.RegistryValueKind]::String)
                $stateKey.Flush()
                try {
                    Set-PathSnapshot $current ([pscustomobject]@{ Exists = $true; Value = $after; Kind = $current.Kind })
                } catch {
                    # A concurrent edit may have added an equivalent entry after
                    # our snapshot. Never leave ownership over that user's entry.
                    $stateKey.DeleteValue('PathEntry', $false)
                    $stateKey.Flush()
                    throw
                }
                $changed = $true
                Write-Host 'Added the installation directory to the current user PATH.'
            }
        }
    } elseif ($null -eq $owner) {
        Write-Host 'No installer-owned PATH entry exists; the user PATH was left unchanged.'
    } else {
        $position = Get-OwnedEntryPosition $current.Value $owner.Entry
        if ($current.Exists -and $position -ge 0) {
            if ($current.Value -ceq $owner.AfterValue) {
                # Restore the precise original spelling, empty entries and absence.
                $next = [pscustomobject]@{ Exists = $owner.BeforeExists; Value = $owner.BeforeValue; Kind = $current.Kind }
            } else {
                # The user changed PATH since installation. Remove exactly one
                # recorded entry, preserving every other entry and its spelling.
                $start = $position
                $length = $owner.Entry.Length
                if ($position -gt 0 -and $current.Value[$position - 1] -eq ';') {
                    $start--
                    $length++
                } elseif ($position + $length -lt $current.Value.Length -and $current.Value[$position + $length] -eq ';') {
                    $length++
                }
                $next = [pscustomobject]@{ Exists = $true; Value = $current.Value.Remove($start, $length); Kind = $current.Kind }
            }
            Set-PathSnapshot $current $next
            $changed = $true
        }
        $stateKey.DeleteValue('PathEntry', $false)
        $stateKey.Flush()
        Write-Host 'Removed only the installer-owned PATH entry, if still present.'
    }

    if ($changed -and -not (Send-EnvironmentChange)) {
        Write-Warning 'PATH was saved, but the environment-change broadcast timed out. Open a new terminal after signing in again if necessary.'
        $exitCode = 2
    }
} catch {
    $message = 'ScratchDSL PATH update failed: ' + $_.Exception.Message
    [Console]::Error.WriteLine($message)
    if (-not $FixtureRoot) {
        try { [IO.File]::WriteAllText((Join-Path $PSScriptRoot 'path-update.log'), $message) } catch { }
    }
    $exitCode = 1
} finally {
    if ($null -ne $stateKey) { $stateKey.Dispose() }
    if ($null -ne $environmentKey) { $environmentKey.Dispose() }
    if ($locked) { $mutex.ReleaseMutex() }
    if ($null -ne $mutex) { $mutex.Dispose() }
}
exit $exitCode
