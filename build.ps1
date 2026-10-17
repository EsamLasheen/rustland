param([switch]$Test)
# cargo writes progress to stderr; failures are detected through exit codes below
$ErrorActionPreference = 'Continue'
$projectRoot = $PSScriptRoot
$targetRoot = if ($env:CARGO_TARGET_DIR) { [IO.Path]::GetFullPath($env:CARGO_TARGET_DIR) } else { Join-Path $projectRoot 'target' }
$cargoTool = Get-Command cargo -ErrorAction Stop
if ((& rustc -vV) -match 'host: .*windows-gnu') {
    if (-not (Get-Command gcc -ErrorAction SilentlyContinue)) {
        $toolRoot = Get-ChildItem "$env:LOCALAPPDATA\Microsoft\WinGet\Packages" -Directory -Filter 'BrechtSanders.WinLibs*' -ErrorAction SilentlyContinue | Select-Object -First 1
        if ($toolRoot) { $env:Path = (Join-Path $toolRoot.FullName 'mingw64\bin') + ';' + $env:Path }
    }
}
Push-Location $projectRoot
try {
    if ($Test) {
        & $cargoTool.Source test --workspace --locked
        if ($LASTEXITCODE -ne 0) { throw 'Tests failed.' }
    }
    & $cargoTool.Source build --release --locked
    if ($LASTEXITCODE -ne 0) { throw 'Build failed.' }
    $releaseDir = Join-Path $projectRoot 'dist'
    New-Item -ItemType Directory -Force $releaseDir | Out-Null
    Copy-Item -LiteralPath (Join-Path $targetRoot 'release\monarch-next.exe') -Destination (Join-Path $releaseDir 'MonarchNext.exe') -Force
    # the same executable installs itself when its file name contains "setup"
    Move-Item -LiteralPath (Join-Path $releaseDir 'MonarchNext.exe') -Destination (Join-Path $releaseDir 'MonarchNext-Setup.exe') -Force
    $hash = Get-FileHash -Algorithm SHA256 -LiteralPath (Join-Path $releaseDir 'MonarchNext-Setup.exe')
    "$($hash.Hash)  MonarchNext-Setup.exe" | Set-Content (Join-Path $releaseDir 'SHA256SUMS.txt')
    $hash | Format-Table
} finally { Pop-Location }
