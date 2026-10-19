param([string]$Makensis = "", [switch]$CompilerOnly, [switch]$SkipBuild)
$ErrorActionPreference = "Stop"
# Some minimal shell environments omit .EXE; affect only this process.
if ($env:PATHEXT -notmatch '(?i)(^|;)\.EXE(;|$)') { $env:PATHEXT = "$env:PATHEXT;.EXE" }
Set-Location (Split-Path $PSScriptRoot -Parent)
if (-not (Test-Path -LiteralPath LICENSE -PathType Leaf)) { throw "Project LICENSE is missing" }
$cargoCommand = Get-Command cargo.exe -ErrorAction SilentlyContinue
$cargoExe = if ($cargoCommand) { $cargoCommand.Source } else { Join-Path $env:USERPROFILE '.cargo\bin\cargo.exe' }
if (-not (Test-Path $cargoExe)) { throw "Rust Cargo was not found" }
if (-not $SkipBuild) {
    & $cargoExe test --locked
    if ($LASTEXITCODE -ne 0) { throw "Tests failed" }
    & $cargoExe build --release --locked
    if ($LASTEXITCODE -ne 0) { throw "Release build failed" }
}
if (-not (Test-Path target/release/sbdsl.exe)) { throw "Release compiler is missing" }
$manifest = Get-Content compiler/Cargo.toml -Raw -Encoding UTF8
if ($manifest -notmatch '(?m)^version\s*=\s*"(\d+\.\d+\.\d+)"') { throw "Cannot read compiler version" }
$version = $Matches[1]
$reportedVersion = & ./target/release/sbdsl.exe --version
if ($LASTEXITCODE -ne 0 -or $reportedVersion -cne "sbdsl $version") {
    throw "Release compiler version does not match compiler/Cargo.toml"
}
New-Item -ItemType Directory -Force dist | Out-Null
Copy-Item target/release/sbdsl.exe dist/sbdsl.exe
Copy-Item -LiteralPath LICENSE -Destination dist/LICENSE

# Keep dependency licenses alongside the offline installer.
$metadata = & $cargoExe metadata --locked --format-version 1 | ConvertFrom-Json
if ($LASTEXITCODE -ne 0) { throw "Cannot read dependency metadata" }
$notices = [System.Collections.Generic.List[string]]::new()
foreach ($package in ($metadata.packages | Where-Object { $_.source } | Sort-Object name, version)) {
    $notices.Add("===== $($package.name) $($package.version) ($($package.license)) =====")
    $root = Split-Path $package.manifest_path -Parent
    $licenses = Get-ChildItem $root -File | Where-Object { $_.Name -match '^(LICENSE|COPYING|NOTICE|COPYRIGHT)' } | Sort-Object Name
    foreach ($license in $licenses) {
        $notices.Add("--- $($license.Name) ---")
        $notices.Add([IO.File]::ReadAllText($license.FullName))
    }
    if ($package.name -eq 'rquickjs-sys') {
        Get-ChildItem $root -Recurse -File -Filter LICENSE | Where-Object { $_.DirectoryName -ne $root } | Sort-Object FullName | ForEach-Object {
            $notices.Add([IO.File]::ReadAllText($_.FullName))
        }
    }
}
[IO.File]::WriteAllText((Join-Path $PWD 'dist/THIRD_PARTY_NOTICES.txt'), ($notices -join "`r`n"))
if ($CompilerOnly) { Write-Host "Created dist/sbdsl.exe"; exit 0 }
if (-not $Makensis) {
    $found = Get-Command makensis.exe -ErrorAction SilentlyContinue
    if ($found) { $Makensis = $found.Source }
    else {
        foreach ($candidate in @("${env:ProgramFiles(x86)}\NSIS\makensis.exe", "$env:ProgramFiles\NSIS\makensis.exe")) {
            if (Test-Path $candidate) { $Makensis = $candidate; break }
        }
    }
}
if (-not $Makensis) { throw "sbdsl.exe is ready in dist. Install NSIS or pass -Makensis <path> to produce Install.exe." }
$installerScript = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot 'windows\ScratchDSL.nsi')).Path
& $Makensis /V3 /INPUTCHARSET UTF8 "/DAPP_VERSION=$version" $installerScript
if ($LASTEXITCODE -ne 0) { throw "Installer compilation failed" }
Write-Host "Created dist/Install.exe and dist/sbdsl.exe"
