using System;
using System.Linq;
using System.Reflection;
using HarmonyLib;

namespace RecNetPlugin.Patches;

// Forces the Play button (and other Statsig-gated UI elements) to be visible
// by disabling the Statsig-based hide components.
//
// Root cause: The RRUI home screen uses Statsig to control which icons appear
// in the icon row. The `HideIfSortableTransformNotInludedInTestGroupImpl`
// component hides any icon NOT in the Statsig-defined list. Since our backend
// serves UseStatSig=false, the client falls back to code defaults, and the
// Play button is excluded from the default list.
//
// This patch disables the hide behavior entirely, making all icons visible
// regardless of Statsig configuration.
internal static class PlayButtonVisibilityPatch
{
    private static bool _done;
    private static readonly System.Collections.Generic.HashSet<string> Patched = new();

    // Target method names (from IL2CPP metadata, may be obfuscated in practice)
    // We search by partial name match to handle obfuscation.
    private static readonly string[] HideMethodPatterns =
    {
        "HideIfSortableTransformNotInludedInTestGroup", // Note: "Inluded" typo is in the binary
        "HideIfLayerParamTrue",
        "HideIfLayerParamFalse",
    };

    public static void Apply()
    {
        if (_done)
            return;

        try
        {
            PatchHideMethods();
        }
        catch (Exception e)
        {
            Plugin.Log.LogWarning($"[PLAY-VIS] failed: {e.Message}");
            return;
        }

        if (Patched.Count > 0)
        {
            _done = true;
            Plugin.Log.LogInfo($"[PLAY-VIS] disabled {Patched.Count} Statsig hide components: {string.Join(",", Patched)}");
        }
    }

    private static void PatchHideMethods()
    {
        var harmony = new Harmony("net.rec.plugin.playvis");

        foreach (var asm in AppDomain.CurrentDomain.GetAssemblies())
        {
            Type[] types;
            try { types = asm.GetTypes(); }
            catch (ReflectionTypeLoadException e) { types = e.Types.Where(t => t != null).ToArray(); }
            catch { continue; }

            foreach (var t in types)
            {
                if (t == null)
                    continue;

                // Look for the hide component types
                foreach (var pattern in HideMethodPatterns)
                {
                    if (!t.Name.Contains(pattern))
                        continue;

                    // Find the method that does the hiding (likely OnEnable, Update, or similar)
                    // We'll patch all methods that might trigger the hide
                    var methods = t.GetMethods(BindingFlags.Public | BindingFlags.NonPublic | BindingFlags.Instance | BindingFlags.Static);
                    foreach (var m in methods)
                    {
                        // Skip if already patched
                        string key = $"{t.FullName}.{m.Name}";
                        if (Patched.Contains(key))
                            continue;

                        // Patch methods that could hide the UI
                        // The safest is to patch the type's main behavior method
                        // For now, we'll log what we find and patch conservatively
                        if (m.Name == "OnEnable" || m.Name == "Start" || m.Name == "Update" || m.Name.Contains("Hide"))
                        {
                            try
                            {
                                var prefix = new HarmonyMethod(typeof(PlayButtonVisibilityPatch).GetMethod(nameof(DisableHidePrefix),
                                    BindingFlags.Static | BindingFlags.NonPublic));
                                harmony.Patch(m, prefix: prefix);
                                Patched.Add(key);
                                Plugin.Log.LogInfo($"[PLAY-VIS] patched {key} -> never hides");
                            }
                            catch (Exception e)
                            {
                                Plugin.Log.LogDebug($"[PLAY-VIS] skip {key}: {e.Message}");
                            }
                        }
                    }
                }
            }
        }
    }

    // Prefix that disables the hide behavior - skip the original method entirely
    private static bool DisableHidePrefix()
    {
        // Return false to skip the original method (which would hide the UI)
        return false;
    }
}
