# Copy these into Hostless → App → Environment (do not commit values).
$dir = "C:\Games\FluxRec\tachyon-server"
$playit = (Get-Content "$dir\playit-secret.txt" -Raw).Trim()
$key = [Convert]::ToBase64String([IO.File]::ReadAllBytes("$dir\server-key.xml"))
Write-Host "PLAYIT_SECRET_KEY=<copied to clipboard as one line>"
$envBlock = @"
PLAYIT_SECRET_KEY=$playit
TACHYON_SERVER_KEY_B64=$key
TACHYON_PRESENCE_BASE=https://match.ripo-ripoteam.workers.dev
"@
Set-Clipboard -Value $envBlock
Write-Host "Three env vars copied to clipboard — paste into Hostless."
Write-Host "TACHYON_PRESENCE_BASE=https://match.ripo-ripoteam.workers.dev"