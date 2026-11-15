# Build linux Tachyon locally, then fly deploy (prebuilt Dockerfile).
$ErrorActionPreference = 'Stop'
$root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$secretsDir = 'C:\Games\FluxRec\tachyon-server'
$publish = Join-Path $PSScriptRoot 'publish'

if (-not (Get-Command flyctl -ErrorAction SilentlyContinue) -and -not (Get-Command fly -ErrorAction SilentlyContinue)) {
    Write-Host 'Install: winget install fly.io.flyctl'
    Write-Host 'Then:    fly auth login'
    exit 1
}
$fly = if (Get-Command flyctl -ErrorAction SilentlyContinue) { 'flyctl' } else { 'fly' }

foreach ($f in @('playit-secret.txt', 'server-key.xml')) {
    if (-not (Test-Path (Join-Path $secretsDir $f))) {
        Write-Host "Missing $secretsDir\$f"
        exit 1
    }
}

$playit = (Get-Content (Join-Path $secretsDir 'playit-secret.txt') -Raw).Trim()
$keyB64 = [Convert]::ToBase64String([IO.File]::ReadAllBytes((Join-Path $secretsDir 'server-key.xml')))

Write-Host 'Publishing TachyonServer (linux-x64)...'
New-Item -ItemType Directory -Force -Path $publish | Out-Null
Push-Location $root
dotnet publish -c Release -r linux-x64 --self-contained true `
    -p:PublishSingleFile=true -p:IncludeNativeLibrariesForSelfExtract=true `
    tachyon-server/src/TachyonServer/TachyonServer.csproj `
    -o $publish
Pop-Location
if (-not (Test-Path (Join-Path $publish 'TachyonServer'))) {
    Write-Host 'dotnet publish failed'
    exit 1
}

Write-Host 'Setting Fly secrets...'
& $fly secrets set `
    "PLAYIT_SECRET_KEY=$playit" `
    "TACHYON_SERVER_KEY_B64=$keyB64" `
    "TACHYON_PRESENCE_BASE=https://match.ripo-ripoteam.workers.dev"

Write-Host 'Deploying...'
Push-Location $root
& $fly deploy
Pop-Location
Write-Host "Done. Logs: $fly logs"