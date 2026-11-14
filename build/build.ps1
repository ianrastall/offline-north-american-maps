<#
.SYNOPSIS
    Checks, tests and publishes Offline Maps as a win-x64 installer.

.DESCRIPTION
    The normal release command. It fails if a check, a test or a packaging prerequisite
    (map page assets, the WebView2 offline runtime installer, Inno Setup) is missing.
    Outputs go to dist\.

    Prerequisites, once per checkout:
      cd proof/renderer; npm ci; node fetch-assets.mjs   (MapLibre, sprites, glyphs; pinned hashes)
      third_party/webview2/MicrosoftEdgeWebView2RuntimeInstallerX64.exe
        (the signed Evergreen Standalone x64 installer, https://go.microsoft.com/fwlink/?linkid=2124701)

.EXAMPLE
    pwsh build/build.ps1                # checks, tests, publish and installer
    pwsh build/build.ps1 -SkipChecks    # publish and installer only
#>
param(
    [switch]$SkipChecks,
    [string]$InnoCompiler
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$target = Join-Path $root 'target'
$dist = Join-Path $root 'dist'
$appDir = Join-Path $dist 'OfflineMaps'
$version = ([xml](Get-Content (Join-Path $root 'Directory.Build.props'))).Project.PropertyGroup.Version | Select-Object -First 1
$cargoVersion = (Select-String -Path (Join-Path $root 'Cargo.toml') -Pattern '^version = "(.+)"').Matches[0].Groups[1].Value
if ($cargoVersion -ne $version) { throw "Cargo.toml has version $cargoVersion but Directory.Build.props has $version." }

$runtime = Join-Path $root 'third_party/webview2/MicrosoftEdgeWebView2RuntimeInstallerX64.exe'
if (-not (Test-Path $runtime)) {
    throw 'The WebView2 offline runtime installer is missing from third_party/webview2 (see the prerequisites in this script).'
}
$signature = Get-AuthenticodeSignature $runtime
if ($signature.Status -ne 'Valid' -or $signature.SignerCertificate.Subject -notmatch 'CN=Microsoft Corporation') {
    throw 'The WebView2 runtime installer does not carry a valid Microsoft signature.'
}
foreach ($asset in 'proof/renderer/node_modules/maplibre-gl/dist/maplibre-gl.mjs', 'proof/renderer/node_modules/maplibre-contour/dist/index.mjs', 'data/renderer-assets/glyphs/noto_sans_regular/0-255.pbf', 'data/renderer-assets/sprites/base.json') {
    if (-not (Test-Path (Join-Path $root $asset))) { throw "Map page asset $asset is missing; run npm ci and node fetch-assets.mjs in proof/renderer." }
}
if (-not $InnoCompiler) {
    $InnoCompiler = @(
        (Join-Path $env:LOCALAPPDATA 'Programs/Inno Setup 6/ISCC.exe'),
        (Join-Path ${env:ProgramFiles(x86)} 'Inno Setup 6/ISCC.exe'),
        (Join-Path $env:ProgramFiles 'Inno Setup 6/ISCC.exe')
    ) | Where-Object { Test-Path -LiteralPath $_ } | Select-Object -First 1
}
if (-not $InnoCompiler -or -not (Test-Path -LiteralPath $InnoCompiler)) {
    throw 'Install Inno Setup 6 or pass -InnoCompiler <ISCC.exe>. Every Jupiter GUI release needs its installer.'
}

function Invoke-Step([string]$Name, [scriptblock]$Command) {
    Write-Host "== $Name" -ForegroundColor Cyan
    & $Command
    if ($LASTEXITCODE -ne 0) { throw "$Name failed (exit $LASTEXITCODE)." }
}

Push-Location $root
try {
    if (-not $SkipChecks) {
        Invoke-Step 'cargo fmt' { cargo fmt --all --check }
        Invoke-Step 'cargo clippy' { cargo clippy --workspace --all-targets --locked --target-dir $target -- -D warnings }
        Invoke-Step 'cargo test' { cargo test --workspace --locked --target-dir $target }
        Invoke-Step 'interop tests' { dotnet test tests/OfflineMaps.Interop.Tests -c Release --nologo -v q }
    }

    if ([IO.Path]::GetFullPath($appDir) -ne [IO.Path]::GetFullPath((Join-Path $root 'dist/OfflineMaps'))) { throw 'Unexpected publish path.' }
    if (Test-Path $appDir) { Remove-Item -LiteralPath $appDir -Recurse -Force }
    Invoke-Step 'publish app' { dotnet publish src/OfflineMaps.App -c Release -o $appDir --nologo -v q }

    # Without the app's own .pri the published app crashes at startup.
    foreach ($file in 'OfflineMaps.exe', 'OfflineMaps.pri', 'offline_maps_ffi.dll', 'Assets/AppIcon.ico', 'THIRD-PARTY-NOTICES.txt',
        'Web/index.html', 'Web/map.js', 'Web/maplibre/maplibre-gl.mjs', 'Web/maplibre/maplibre-gl-worker.mjs', 'Web/contour/maplibre-contour.mjs',
        'Web/styles/offline-maps-dark.json', 'Web/sprites/base.json', 'Web/glyphs/noto_sans_regular/0-255.pbf') {
        if (-not (Test-Path (Join-Path $appDir $file))) { throw "The published folder is missing $file." }
    }
    # Native binaries must be x64 (PE machine 0x8664).
    foreach ($native in 'offline_maps_ffi.dll', 'OfflineMaps.exe') {
        $bytes = [IO.File]::ReadAllBytes((Join-Path $appDir $native))
        $pe = [BitConverter]::ToInt32($bytes, 0x3C)
        if ([BitConverter]::ToUInt16($bytes, $pe + 4) -ne 0x8664) { throw "$native is not an x64 binary." }
    }
    Invoke-Step 'build installer' { & $InnoCompiler /Qp "/DAppVersion=$version" (Join-Path $PSScriptRoot 'installer.iss') }
    $setup = Join-Path $dist "OfflineMaps-$version-win-x64-setup.exe"
    Get-Item $setup | ForEach-Object { '{0}  {1:n1} MB' -f $_.FullName, ($_.Length / 1MB) }
}
finally {
    Pop-Location
}
