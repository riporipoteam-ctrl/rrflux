// Ultra graphics knobs: when High is selected in Game Settings -> Visuals ->
// Graphics Quality, the game applies its own Ultra level — High is already
// Ultra=3 on PC. This just ensures the 4 runtime-settable knobs are maxed.
//
// This Unity build only exposes FOUR settable QualitySettings at runtime
// (verified against the 2023-04-14 dump: pixelLightCount, shadowDistance,
// lodBias, antiAliasing; everything else — shadowCascades,
// shadowResolution, anisotropicFiltering, masterTextureLimit — is get-only).
// Texture resolution itself is driven by the game's own Ultra quality level,
// so there is no redirect logic to do — it would be a no-op.
//
// One knob, see [Graphics] in the .cfg:
//   Enable Ultra Graphics -> THE FEATURE (default true).
using System;
using UnityEngine;

namespace RecNetPlugin.Patches;

internal static class UltraGraphicsPatch
{
    private static bool _applied;

    // Work done (or the feature disabled): the UiDiscoveryRetry driver stops
    // ticking this patch once settled, and later Apply() calls are no-ops.
    internal static bool IsSettled =>
        !Plugin.EnableUltraGraphics.Value || _applied;

    // Called from Plugin.Load, on each scene load, and on a 2-second timer
    // by UiDiscoveryRetry until IsSettled. Idempotent: the knobs are written
    // once and never re-applied.
    public static void Apply()
    {
        if (!Plugin.EnableUltraGraphics.Value)
            return;
        if (_applied)
            return;

        try
        {
            QualitySettings.antiAliasing = UltraQuality.AntiAliasing;
            QualitySettings.pixelLightCount = UltraQuality.PixelLightCount;
            QualitySettings.shadowDistance = UltraQuality.ShadowDistance;
            QualitySettings.lodBias = UltraQuality.LodBias;
            _applied = true;
            Plugin.Log.LogInfo("[ULTRA] Graphics knobs maxed (High is already Ultra on PC)");
        }
        catch (Exception e)
        {
            Plugin.Log.LogWarning($"[ULTRA] could not apply graphics knobs: {e.Message}");
        }
    }
}
