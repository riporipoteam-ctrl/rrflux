---
title: Ripo Team Server (Flux Rec relay)
emoji: 🎮
colorFrom: red
colorTo: green
sdk: gradio
sdk_version: "6.32.0"
app_file: app.py
python_version: "3.12"
app_port: 7860
pinned: true
short_description: Linux Gradio host — Tachyon + playit for Flux Rec
---

# Ripo Team Server

Same pattern as **RipoBot Desktop**: Hugging Face gives you a **Linux container**; `packages.txt`
installs system tools; Gradio keeps port **7860** open.

Background processes:

- **TachyonServer** (UDP 7777 → playit tunnel)
- **playit** agent (public `RECFLARE_TACHYON_HOST_PORT`)

**Space secrets:** `PLAYIT_SECRET_KEY`, `TACHYON_SERVER_KEY_B64`, `TACHYON_PRESENCE_BASE`

Game binaries / manifest live on dataset **[Echoxr/rrflux-game](https://huggingface.co/datasets/Echoxr/rrflux-game)** —
this Space is only the **24/7 multiplayer relay**.

Deploy: `python tachyon-server/relay/deploy_hf_upload.py --build`