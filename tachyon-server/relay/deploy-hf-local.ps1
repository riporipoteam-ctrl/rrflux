# Deploy Echoxr/fluxrec-relay without waiting on GitHub workflow push.
# Prereq:  HF_TOKEN env var OR `hf auth login` once (write access).
param(
    [string]$RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path
)

$env:Path = [System.Environment]::GetEnvironmentVariable("Path", "Machine") + ";" + [System.Environment]::GetEnvironmentVariable("Path", "User")
python -m pip install --quiet huggingface_hub
if (-not $env:HF_TOKEN) {
    Write-Host "Set HF_TOKEN from https://huggingface.co/settings/tokens (Write), or run: hf auth login"
    exit 1
}

Push-Location $RepoRoot
dotnet publish -c Release -r linux-x64 --self-contained true `
  -p:PublishSingleFile=true -p:IncludeNativeLibrariesForSelfExtract=true `
  tachyon-server/src/TachyonServer/TachyonServer.csproj `
  -o huggingface-space/fluxrec-relay/publish
Copy-Item -Force huggingface-space/fluxrec-relay/publish/TachyonServer huggingface-space/fluxrec-relay/TachyonServer

$py = @'
import os
from pathlib import Path
from huggingface_hub import HfApi

repo = "Echoxr/fluxrec-relay"
api = HfApi(token=os.environ["HF_TOKEN"])
try:
    api.repo_info(repo_id=repo, repo_type="space")
except Exception:
    api.create_repo(repo_id=repo, repo_type="space", space_sdk="docker", space_hardware="cpu-basic", exist_ok=True)

for key in ("PLAYIT_SECRET_KEY", "TACHYON_SERVER_KEY_B64"):
    val = os.environ.get(key, "")
    if not val and key == "PLAYIT_SECRET_KEY":
        val = Path(r"C:\Games\FluxRec\tachyon-server\playit-secret.txt").read_text().strip()
    if not val and key == "TACHYON_SERVER_KEY_B64":
        import base64
        val = base64.b64encode(Path(r"C:\Games\FluxRec\tachyon-server\server-key.xml").read_bytes()).decode()
    api.add_space_secret(repo_id=repo, key=key, value=val)

api.add_space_secret(repo_id=repo, key="TACHYON_PRESENCE_BASE", value="https://match.ripo-ripoteam.workers.dev")

root = Path("huggingface-space/fluxrec-relay")
for name in ("Dockerfile", "README.md", "entrypoint.sh", "health_server.py", "TachyonServer"):
    api.upload_file(path_or_fileobj=str(root / name), path_in_repo=name, repo_id=repo, repo_type="space", commit_message="deploy fluxrec relay")
    print("uploaded", name)
print("Done — open https://huggingface.co/spaces/Echoxr/fluxrec-relay")
'@
$env:PLAYIT_SECRET_KEY = (Get-Content "C:\Games\FluxRec\tachyon-server\playit-secret.txt" -Raw).Trim()
$env:TACHYON_SERVER_KEY_B64 = [Convert]::ToBase64String([IO.File]::ReadAllBytes("C:\Games\FluxRec\tachyon-server\server-key.xml"))
python -c $py
Pop-Location
Write-Host "Stop playit on this PC after the Space shows Running (one agent per secret)."