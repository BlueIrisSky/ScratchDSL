param([switch]$SkipBuild, [string]$Npm = "npm.cmd")
$ErrorActionPreference = "Stop"
$env:PATHEXT = '.COM;.EXE;.BAT;.CMD;.VBS;.VBE;.JS;.JSE;.WSF;.WSH;.MSC'
$repo = Split-Path $PSScriptRoot -Parent
Set-Location $repo

# The same release executable powers both CLI and LSP. This also refreshes all
# Rust dependency notices after Cargo.lock changes.
& "$PSScriptRoot/package.ps1" -CompilerOnly -SkipBuild:$SkipBuild
if ($LASTEXITCODE -ne 0) { throw "Compiler packaging failed" }
$plugin = Join-Path $repo 'vscode'
New-Item -ItemType Directory -Force "$plugin/server" | Out-Null
Copy-Item "$repo/dist/sbdsl.exe" "$plugin/server/sbdsl.exe"
Copy-Item "$repo/dist/THIRD_PARTY_NOTICES.txt" "$plugin/server/THIRD_PARTY_NOTICES.txt"
Copy-Item "$repo/dist/LICENSE" "$plugin/LICENSE"
Push-Location $plugin
try {
    & $Npm ci --no-audit --no-fund
    if ($LASTEXITCODE -ne 0) { throw "Plugin dependency installation failed" }
    & $Npm test
    if ($LASTEXITCODE -ne 0) { throw "Plugin checks failed" }
    $manifest = Get-Content package.json -Raw -Encoding UTF8 | ConvertFrom-Json
    $output = Join-Path $repo "dist/scratchdsl-$($manifest.version)-win32-x64.vsix"
    & $Npm exec --offline -- vsce package --target win32-x64 --pre-release --no-dependencies -o $output
    if ($LASTEXITCODE -ne 0) { throw "VSIX packaging failed" }
    Write-Host "Created $output"
} finally { Pop-Location }
