# What can run 24/7 for free (no bullshit)

| Component | Runs without your PC? | Free forever? |
|-----------|------------------------|---------------|
| Login, match, economy, rooms (Cloudflare Workers) | **Yes** | Yes (within CF limits) |
| UDP voice/room (Tachyon + playit) | **Only if a container/VM runs somewhere** | Free tiers exist; not “infinite Mars years” |

There is **no** magic GitHub/HuggingFace trick that hosts a **UDP game server** while every machine you own is off **and** you never sign up for anything **and** you pay nothing.

The **smallest honest model**:

1. **Workers** already run 24/7 for everyone on Earth (and are not your PC).
2. **One** always-on free container host (Hostless / Botkeep / Omeganodes — no card on listed tiers) runs `ghcr.io/riporipoteam-ctrl/fluxrec-relay`.
3. GitHub **keepalive** workflows only **ping/monitor**; they do not replace step 2.

HF **free** cannot host this Docker/Gradio relay (402 — Pro required).

Your gaming PC sleeping = **only** fine after step 2 is live and playit on the PC is **stopped**.