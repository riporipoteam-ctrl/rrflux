# One-shot: upload operator secrets to GitHub for HF relay deploy (no card, no Oracle).
param(
    [string]$KeyPath = "C:\Games\FluxRec\tachyon-server\server-key.xml",
    [string]$Repo = "riporipoteam-ctrl/rrflux"
)

if (-not (Test-Path $KeyPath)) {
    Write-Error "Missing $KeyPath"
    exit 1
}
$b64 = [Convert]::ToBase64String([IO.File]::ReadAllBytes($KeyPath))
gh secret set TACHYON_SERVER_KEY_B64 -b $b64 -R $Repo
Write-Host "TACHYON_SERVER_KEY_B64 set on $Repo. HF_TOKEN and PLAYIT_SECRET_KEY should already exist."
Write-Host "Then: GitHub Actions -> Deploy HF relay (24/7) -> Run workflow."