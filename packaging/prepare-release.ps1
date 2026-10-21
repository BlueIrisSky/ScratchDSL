param([string]$SevenZip = '')

Set-StrictMode -Version 2.0
$ErrorActionPreference = 'Stop'
if ($env:PATHEXT -notmatch '(?i)(^|;)\.EXE(;|$)') { $env:PATHEXT = "$env:PATHEXT;.EXE" }
$repo = Split-Path $PSScriptRoot -Parent
$dist = Join-Path $repo 'dist'
$manifest = Get-Content (Join-Path $repo 'vscode/package.json') -Raw -Encoding UTF8 | ConvertFrom-Json
$version = $manifest.version
if ($manifest.license -cne 'MIT') { throw 'The extension license must be MIT' }
if ($version -notmatch '^\d+\.\d+\.\d+$') { throw "Unsupported release version: $version" }
$compilerManifest = Get-Content (Join-Path $repo 'compiler/Cargo.toml') -Raw -Encoding UTF8
if ($compilerManifest -notmatch ('(?m)^version\s*=\s*"' + [regex]::Escape($version) + '"')) {
    throw 'Compiler and extension versions must agree'
}
if ($compilerManifest -notmatch '(?m)^license\s*=\s*"MIT"') { throw 'The compiler license must be MIT' }

$compiler = Join-Path $dist 'sbdsl.exe'
$installer = Join-Path $dist 'Install.exe'
$notices = Join-Path $dist 'THIRD_PARTY_NOTICES.txt'
$license = Join-Path $dist 'LICENSE'
$rootLicense = Join-Path $repo 'LICENSE'
$vsix = Join-Path $dist "scratchdsl-$version-win32-x64.vsix"
$portableName = "scratchdsl-$version-windows-x64.zip"
$portable = Join-Path $dist $portableName
foreach ($file in @($compiler, $installer, $notices, $license, $rootLicense, $vsix)) {
    if (-not (Test-Path -LiteralPath $file -PathType Leaf) -or (Get-Item -LiteralPath $file).Length -eq 0) {
        throw "Missing release file: $file"
    }
}
$reportedVersion = & $compiler --version
if ($LASTEXITCODE -ne 0 -or $reportedVersion -cne "sbdsl $version") { throw 'CLI version mismatch' }
if ((Get-Item -LiteralPath $installer).VersionInfo.ProductVersion -cne $version) {
    throw 'Installer version mismatch; rebuild it with packaging/package.ps1'
}

# PE machine 0x8664 is AMD64; this VSIX and installer are Windows x64 only.
$pe = [IO.BinaryReader]::new([IO.File]::OpenRead($compiler))
try {
    if ($pe.ReadUInt16() -ne 0x5a4d) { throw 'CLI is not a Windows executable' }
    $pe.BaseStream.Position = 0x3c
    $header = $pe.ReadInt32()
    $pe.BaseStream.Position = $header
    if ($pe.ReadUInt32() -ne 0x4550 -or $pe.ReadUInt16() -ne 0x8664) { throw 'CLI is not Windows x64' }
} finally { $pe.Dispose() }

if (-not $SevenZip) {
    $command = Get-Command 7z.exe -ErrorAction SilentlyContinue
    if ($command) { $SevenZip = $command.Source }
    else { $SevenZip = Join-Path $env:ProgramFiles '7-Zip/7z.exe' }
}
if (-not (Test-Path -LiteralPath $SevenZip -PathType Leaf)) {
    throw '7-Zip is required to verify the installer; pass -SevenZip <path>'
}

Add-Type -AssemblyName System.IO.Compression.FileSystem
function Get-EntryHash($Archive, [string]$Name) {
    $entry = $Archive.GetEntry($Name)
    if ($null -eq $entry -or $entry.Length -eq 0) { throw "Missing archive entry: $Name" }
    $stream = $entry.Open()
    $hash = [Security.Cryptography.SHA256]::Create()
    try { return [BitConverter]::ToString($hash.ComputeHash($stream)).Replace('-', '').ToLowerInvariant() }
    finally { $stream.Dispose(); $hash.Dispose() }
}
function Read-Entry($Archive, [string]$Name) {
    $entry = $Archive.GetEntry($Name)
    if ($null -eq $entry -or $entry.Length -eq 0) { throw "Missing archive entry: $Name" }
    $reader = [IO.StreamReader]::new($entry.Open(), [Text.Encoding]::UTF8)
    try { return $reader.ReadToEnd() } finally { $reader.Dispose() }
}
$compilerHash = (Get-FileHash -LiteralPath $compiler -Algorithm SHA256).Hash.ToLowerInvariant()
$noticesHash = (Get-FileHash -LiteralPath $notices -Algorithm SHA256).Hash.ToLowerInvariant()
$licenseHash = (Get-FileHash -LiteralPath $rootLicense -Algorithm SHA256).Hash.ToLowerInvariant()
if ((Get-FileHash -LiteralPath $license -Algorithm SHA256).Hash.ToLowerInvariant() -cne $licenseHash) {
    throw 'Standalone LICENSE differs from the project LICENSE'
}
$archive = [IO.Compression.ZipFile]::OpenRead($vsix)
try {
    $packagedManifest = Read-Entry $archive 'extension/package.json' | ConvertFrom-Json
    $vsixManifest = [xml](Read-Entry $archive 'extension.vsixmanifest')
    if ($packagedManifest.version -cne $version -or
        $vsixManifest.PackageManifest.Metadata.Identity.Version -cne $version -or
        $vsixManifest.PackageManifest.Metadata.Identity.TargetPlatform -cne 'win32-x64') {
        throw 'VSIX version or target platform mismatch'
    }
    if ($packagedManifest.license -cne 'MIT') { throw 'VSIX package license must be MIT' }
    $preRelease = $vsixManifest.PackageManifest.Metadata.Properties.Property |
        Where-Object { $_.Id -ceq 'Microsoft.VisualStudio.Code.PreRelease' }
    if ($null -eq $preRelease -or $preRelease.Value -cne 'true') { throw 'VSIX must be marked as a pre-release' }
    # vsce normalizes the generated LICENSE filename when writing the VSIX.
    if ((Get-EntryHash $archive 'extension/LICENSE.txt') -cne $licenseHash) { throw 'VSIX LICENSE differs from the project LICENSE' }
    if ((Get-EntryHash $archive 'extension/server/sbdsl.exe') -cne $compilerHash) { throw 'VSIX server differs from CLI' }
    if ((Get-EntryHash $archive 'extension/server/THIRD_PARTY_NOTICES.txt') -cne $noticesHash) { throw 'VSIX Rust notices differ from CLI notices' }
    foreach ($entry in @('extension/THIRD_PARTY_NOTICES.txt', 'extension/dist/extension.js',
        'extension/syntaxes/sbdsl.tmLanguage.json', 'extension/syntaxes/buildsb.tmLanguage.json')) {
        Get-EntryHash $archive $entry | Out-Null
    }
    if ($archive.Entries.FullName -match '^extension/(src|test|node_modules)/') { throw 'VSIX contains development files' }
} finally { $archive.Dispose() }

$work = Join-Path $repo ('target/release-package/' + [guid]::NewGuid().ToString('N'))
$unpacked = Join-Path $work 'installer'
$staging = Join-Path $work 'portable'
New-Item -ItemType Directory -Force $unpacked, $staging | Out-Null
try {
    # Extracting NSIS does not execute the installer or modify the user's PATH.
    & $SevenZip x -y "-o$unpacked" $installer | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 'Cannot extract installer for verification' }
    $embeddedCompiler = @(Get-ChildItem $unpacked -Recurse -File -Filter sbdsl.exe)
    $embeddedNotices = @(Get-ChildItem $unpacked -Recurse -File -Filter THIRD_PARTY_NOTICES.txt)
    $embeddedLicense = @(Get-ChildItem $unpacked -Recurse -File -Filter LICENSE)
    if ($embeddedCompiler.Count -ne 1 -or $embeddedNotices.Count -ne 1 -or $embeddedLicense.Count -ne 1) {
        throw 'Installer payload is incomplete or ambiguous'
    }
    if ((Get-FileHash $embeddedCompiler[0].FullName -Algorithm SHA256).Hash.ToLowerInvariant() -cne $compilerHash) {
        throw 'Installer compiler differs from CLI'
    }
    if ((Get-FileHash $embeddedNotices[0].FullName -Algorithm SHA256).Hash.ToLowerInvariant() -cne $noticesHash) {
        throw 'Installer dependency notices differ from CLI notices'
    }
    if ((Get-FileHash $embeddedLicense[0].FullName -Algorithm SHA256).Hash.ToLowerInvariant() -cne $licenseHash) {
        throw 'Installer LICENSE differs from the project LICENSE'
    }
    Copy-Item -LiteralPath $compiler, $notices, $license, (Join-Path $repo 'README.md') -Destination $staging
    Copy-Item -LiteralPath (Join-Path $repo 'docs') -Destination $staging -Recurse
    foreach ($example in @('hello/main.sbdsl', 'hello/sbdsl.buildsb', 'extensions/main.sbdsl',
        'extensions/sbdsl.buildsb', 'extensions/\u81ea\u5b9a\u4e49\u62d3\u5c55.js')) {
        # Keep this script ASCII-compatible with Windows PowerShell 5.1.
        $relative = [regex]::Replace($example, '\\u([0-9a-f]{4})', { param($match) [char][Convert]::ToInt32($match.Groups[1].Value, 16) })
        $destination = Join-Path $staging "examples/$relative"
        New-Item -ItemType Directory -Force (Split-Path $destination -Parent) | Out-Null
        Copy-Item -LiteralPath (Join-Path $repo "examples/$relative") -Destination $destination
    }
    if (Test-Path -LiteralPath $portable) { Remove-Item -LiteralPath $portable }
    [IO.Compression.ZipFile]::CreateFromDirectory($staging, $portable, [IO.Compression.CompressionLevel]::Optimal, $false)
} finally { Remove-Item -LiteralPath $work -Recurse -Force }

$commit = & git -C $repo rev-parse HEAD
if ($LASTEXITCODE -ne 0) { throw 'Cannot read source commit' }
$artifacts = @('Install.exe', 'sbdsl.exe', $portableName, (Split-Path $vsix -Leaf), 'LICENSE', 'THIRD_PARTY_NOTICES.txt')
$checksums = foreach ($name in $artifacts) {
    $file = Get-Item -LiteralPath (Join-Path $dist $name)
    [ordered]@{ name = $name; bytes = $file.Length; sha256 = (Get-FileHash $file.FullName -Algorithm SHA256).Hash.ToLowerInvariant() }
}
$release = [ordered]@{ version = $version; gitCommit = $commit; platform = 'windows-x64'; assets = @($checksums) }
[IO.File]::WriteAllText((Join-Path $dist 'release-manifest.json'), ($release | ConvertTo-Json -Depth 5) + "`n", [Text.UTF8Encoding]::new($false))
$checksumLines = @($checksums | ForEach-Object { "$($_.sha256)  $($_.name)" })
$checksumLines += (Get-FileHash (Join-Path $dist 'release-manifest.json') -Algorithm SHA256).Hash.ToLowerInvariant() + '  release-manifest.json'
[IO.File]::WriteAllText((Join-Path $dist 'SHA256SUMS.txt'), ($checksumLines -join "`n") + "`n", [Text.UTF8Encoding]::new($false))
Write-Host "Verified Windows x64 release $version ($commit)"
Write-Host "Compiler payload SHA256 (CLI, installer, portable archive, VSIX): $compilerHash"
