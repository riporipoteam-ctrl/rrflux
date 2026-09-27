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
        "HideIfNotInDisplayOrderConfig", // Additional Statsig hide variant
        "HideIfLayerParamsFalse", // Plural variant
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

                // Search METHODS directly by name pattern (types are obfuscated,
                // but method names like HideIfSortableTransformNotInludedInTestGroup
                // are preserved in metadata)
                MethodInfo[] methods;
                try
                {
                    methods = t.GetMethods(BindingFlags.Public | BindingFlags.NonPublic | BindingFlags.Instance | BindingFlags.Static | BindingFlags.DeclaredOnly);
                }
                catch { continue; }

                foreach (var m in methods)
                {
                    if (m == null)
                        continue;

                    // Check if method name matches any hide pattern
                    bool isHideMethod = false;
                    foreach (var pattern in HideMethodPatterns)
                    {
                        if (m.Name.Contains(pattern))
                        {
                            isHideMethod = true;
                            break;
                        }
                    }

                    if (!isHideMethod)
                        continue;

                    // Skip if already patched
                    string key = $"{t.FullName}.{m.Name}";
                    if (Patched.Contains(key))
                        continue;

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

    // Prefix that disables the hide behavior - skip the original method entirely
    private static bool DisableHidePrefix()
    {
        // Return false to skip the original method (which would hide the UI)
        return false;
    }
}
