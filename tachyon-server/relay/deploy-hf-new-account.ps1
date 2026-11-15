# Deploy cloud relay on a FRESH Hugging Face account (fresh cpu-basic quota).
# 1) Create account at https://huggingface.co/join (or use an existing spare).
# 2) Token with Write: https://huggingface.co/settings/tokens
# 3) Run:  $env:HF_TOKEN = "hf_..." ; $env:HF_USERNAME = "YourNewUser" ; .\deploy-hf-new-account.ps1

param(
    [string]$Username = $env:HF_USERNAME,
    [string]$SpaceName = "fluxrec-relay"
)

$ErrorActionPreference = 'Stop'
if (-not $Username) {
    Write-Host "Set HF_USERNAME (new Hugging Face username), e.g.:"
    Write-Host '  $env:HF_USERNAME = "ripoteamrelay"'
    exit 1
}
if (-not $env:HF_TOKEN) {
    Write-Host "Set HF_TOKEN from https://huggingface.co/settings/tokens (Write access)"
    Write-Host 'Or: huggingface-cli login'
    exit 1
}

$root = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$space = "$Username/$SpaceName"
$env:HF_SPACE = $space
$env:TACHYON_DATASET = "$Username/fluxrec-tachyon-linux"

Write-Host "Deploying Space $space (Tachyon dataset $env:TACHYON_DATASET)..."
Push-Location $root
python (Join-Path $PSScriptRoot 'deploy_hf_upload.py') --build
Pop-Location

$slug = ($space -replace '/', '-').ToLower()
Write-Host ""
Write-Host "Space: https://huggingface.co/spaces/$space"
Write-Host "Keepalive: https://${slug}.hf.space/"
Write-Host ""
Write-Host "GitHub repo variable (optional): RELAY_KEEPALIVE_URL=https://${slug}.hf.space/"
Write-Host "In Space Settings -> Hardware: try cpu-basic first; if quota blocks, use ZeroGPU."
Write-Host "Stop playit on this PC after the Space shows Running (one agent per secret)."