// Pure spec for the "Ultra" graphics preset.
//
// This file is intentionally free of Unity / BepInEx / Harmony dependencies so
// the values can be unit-tested on any machine (see
// hidden_files/plugin-work/tests/).
//
// The game's own quality enum (GHCCKFEJDOA) already contains Ultra = 3, and the
// game's quality applier handles it — but this Unity build's QualitySettings
// API only exposes a subset of knobs at runtime (verified against the
// 2023-04-14 dump: masterTextureLimit is get-only, and there is no
// shadowCascades / anisotropicFiltering / softParticles property). The plugin
// therefore boosts what IS settable when Ultra is selected:
//
//   antiAliasing   8     (better AA)
//   pixelLightCount 4    (better lighting; still sane — 4 per-object lights)
//   shadowDistance 150   (better shadows; further shadow draw distance)
//   lodBias        2.0   (higher detail: LODs/textures stay sharp further out)
//
// Not touched: vSyncCount, maxQueuedFrames (leave the user's choices alone).
// Texture resolution itself is driven by the game's own Ultra quality level
// (masterTextureLimit has no runtime setter in this Unity version).
namespace RecNetPlugin.Patches;

public static class UltraQuality
{
    /// <summary>GHCCKFEJDOA enum value for Ultra (from the 2023-04-14 client).</summary>
    public const int UltraEnumValue = 3;

    /// <summary>MSAA sample count for Ultra.</summary>
    public const int AntiAliasing = 8;

    /// <summary>Per-object pixel light count for Ultra (sane, not maxed).</summary>
    public const int PixelLightCount = 4;

    /// <summary>Shadow draw distance in meters for Ultra.</summary>
    public const float ShadowDistance = 150f;

    /// <summary>LOD bias for Ultra (higher = crisper detail further away).</summary>
    public const float LodBias = 2.0f;
}
