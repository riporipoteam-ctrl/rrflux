# When Hostless web logs won't load — stream build + runtime logs from your PC.
# Install CLI once: winget install Hostless.HostlessCLI  (or see https://docs.hostless.cloud/cli)
param(
    [string]$AppName = ''
)

$ErrorActionPreference = 'Stop'
if (-not (Get-Command hostless -ErrorAction SilentlyContinue)) {
    Write-Host @"
hostless CLI not found.
Install: winget install Hostless.HostlessCLI
Then:    hostless auth login
"@
    exit 1
}

Write-Host 'Recent runtime logs (errors):'
hostless logs --level error --since 1h 2>$null
if ($AppName) {
    Write-Host "`nDeploy status (wait for build stream):"
    hostless deploy status --wait --timeout 1200 2>$null
} else {
    Write-Host "`nTip: redeploy from dashboard, then run:  hostless deploy status --wait"
    Write-Host 'Or follow logs:  hostless logs -f'
}
Write-Host "`nBuild failures also show here after push:"
Write-Host 'https://github.com/riporipoteam-ctrl/rrflux/actions/workflows/build-relay-docker.yml'