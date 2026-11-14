// Auto-detect weak PCs/laptops and drop Unity quality before the first room load.
// The 2026 native client is also capped from the launcher; this covers the
// BepInEx 2025 path and any install that still loads the plugin.
using System;
using UnityEngine;

namespace RecNetPlugin.Patches;

internal static class LowEndGraphicsPatch
{
    private static bool _applied;
    private static bool? _isLowEnd;

    internal static bool IsLowEnd
    {
        get
        {
            if (_isLowEnd.HasValue)
                return _isLowEnd.Value;
            try
            {
                var ram = SystemInfo.systemMemorySize;
                var vram = SystemInfo.graphicsMemorySize;
                var cores = SystemInfo.processorCount;
                _isLowEnd = ram > 0 && ram <= 12288
                    || vram > 0 && vram <= 2048
                    || cores > 0 && cores <= 4;
            }
            catch
            {
                _isLowEnd = false;
            }
            return _isLowEnd.Value;
        }
    }

    internal static bool IsSettled => !IsLowEnd || _applied;

    public static void Apply()
    {
        if (!IsLowEnd || _applied)
            return;
        try
        {
            QualitySettings.antiAliasing = UltraQuality.LowAntiAliasing;
            QualitySettings.pixelLightCount = UltraQuality.LowPixelLightCount;
            QualitySettings.shadowDistance = UltraQuality.LowShadowDistance;
            QualitySettings.lodBias = UltraQuality.LowLodBias;
            QualitySettings.vSyncCount = 1;
            Application.targetFrameRate = UltraQuality.LowTargetFrameRate;
            _applied = true;
            Plugin.Log.LogInfo(
                $"[LOW-END] quality capped (RAM={SystemInfo.systemMemorySize}MB VRAM={SystemInfo.graphicsMemorySize}MB cores={SystemInfo.processorCount})");
        }
        catch (Exception e)
        {
            Plugin.Log.LogWarning($"[LOW-END] could not apply graphics caps: {e.Message}");
        }
    }
}
