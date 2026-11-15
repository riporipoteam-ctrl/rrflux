# Flux Rec public relay — operator PC at logon (Task Scheduler).
# Starts playit UDP tunnel and Tachyon (127.0.0.1:7777) with match shard lookups.
$ErrorActionPreference = "SilentlyContinue"
$dir = $PSScriptRoot
$presence = [Environment]::GetEnvironmentVariable("TACHYON_PRESENCE_BASE", "Machine")
if (-not $presence) {
    $presence = "https://match.ripo-ripoteam.workers.dev"
}

if (-not (Get-Process playit -ErrorAction SilentlyContinue)) {
    $secret = (Get-Content "$dir\playit-secret.txt" -Raw).Trim()
    $cfgDir = Join-Path $env:APPDATA "playit_gg"
    New-Item -ItemType Directory -Force -Path $cfgDir | Out-Null
    "secret_key = `"$secret`"" | Set-Content (Join-Path $cfgDir "playit.toml") -NoNewline
    "secret_key = `"$secret`"" | Set-Content (Join-Path $dir "playit.toml") -NoNewline
    Start-Process -FilePath "$dir\playit.exe" -WorkingDirectory $dir -WindowStyle Hidden
    Start-Sleep -Seconds 2
}

if (-not (Get-NetUDPEndpoint -LocalPort 7777 -ErrorAction SilentlyContinue)) {
    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName = Join-Path $dir "TachyonServer.exe"
    $psi.WorkingDirectory = $dir
    $psi.UseShellExecute = $false
    $psi.CreateNoWindow = $true
    $psi.EnvironmentVariables["TACHYON_PRESENCE_BASE"] = $presence
    [System.Diagnostics.Process]::Start($psi) | Out-Null
}