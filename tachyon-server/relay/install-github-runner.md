# Self-hosted runner (free, no credit card)

Hugging Face **free** accounts cannot host long-running Gradio/Docker Spaces (402 — needs [HF Pro](https://huggingface.co/pro) or use your PC).

This gives **zero-cost automation while your operator PC is powered on**:

1. Download the Windows x64 runner for `riporipoteam-ctrl/rrflux` from  
   https://github.com/riporipoteam-ctrl/rrflux/settings/actions/runners/new
2. Unzip to e.g. `C:\ActionsRunner`, run `config.cmd` (use the token GitHub shows).
3. Run `run.cmd` **once** to test, then install as a service: `.\svc.install` / `.\svc.start`
4. Merge/push workflow `relay-keepalive-selfhosted.yml` (needs `workflow` scope once).
5. GitHub → Actions → **Relay keepalive (self-hosted)** will run every 10 minutes and execute `start-relay.ps1`.

**While the PC sleeps:** relay is still off — use **Windows: never sleep on AC** for launch nights, or pay for HF Pro / a VPS later.

Also run once as Admin:

```powershell
powershell -ExecutionPolicy Bypass -File C:\Games\rrflux\tachyon-server\relay\windows\install-keepalive-task.ps1
```