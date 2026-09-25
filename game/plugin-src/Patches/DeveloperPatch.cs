using System;
using System.Linq;
using System.Reflection;
using HarmonyLib;

namespace RecNetPlugin.Patches;

// Forces SessionManager.get_IsDeveloper() to true, unlocking the client's
// developer-gated UI elements.
//
// Why this exists: Armin (account 28, FluxRec) is now flagged isDeveloper=1 in
// D1 and the accounts worker serves IsDeveloper=true, but the plugin force is
// belt-and-suspenders — it works even if the backend flag is ever lost, and it
// guarantees the dev UI appears without depending on the account DTO.
//
// What it unlocks (per research):
// - SettingsModelController.DeveloperDisplaySliderImpl (dev slider in Settings)
// - AccountModelController.HideDeveloperBadgeImpl (dev badge on profile)
// - GlobalModelController.HideIfNotDeveloperImpl GameObjects (scattered dev UI)
// - Any get_IsDeveloperOnly tabs/pages
//
// There is NO pre-built dev tab in Settings — this only reveals the existing
// scattered dev elements. A custom dev tab is future work.
//
// One knob, see [Developer] in the .cfg:
//   Force IsDeveloper -> THE FIX (default true).
internal static class DeveloperPatch
{
    private static bool _done;

    // Called from Plugin.Load and again on each scene load until patched —
    // the declaring type (SessionManager) may live in an assembly that isn't
    // loaded yet when BepInEx runs Load().
    public static void Apply()
    {
        if (_done || !Plugin.ForceIsDeveloper.Value)
            return;

        try
        {
            // Find the type declaring get_IsDeveloper by scanning for the method name.
            // The declaring type (SessionManager) is obfuscated; the method name is not.
            var target = AppDomain.CurrentDomain.GetAssemblies()
                .SelectMany(a => {
                    try { return a.GetTypes(); }
                    catch { return Array.Empty<Type>(); }
                })
                .SelectMany(t => {
                    try { return t.GetMethods(BindingFlags.Public | BindingFlags.NonPublic | BindingFlags.Static | BindingFlags.Instance); }
                    catch { return Array.Empty<MethodInfo>(); }
                })
                .FirstOrDefault(m => m.Name == "get_IsDeveloper" && m.GetParameters().Length == 0 && m.ReturnType == typeof(bool));

            if (target == null)
            {
                // Type not loaded yet — retry on next scene load.
                return;
            }

            var harmony = new Harmony("com.fluxrec.developerpatch");
            harmony.Patch(
                target,
                prefix: new HarmonyMethod(typeof(DeveloperPatch).GetMethod(nameof(ForceTrue), BindingFlags.NonPublic | BindingFlags.Static))
            );
            _done = true;
            Plugin.Log.LogInfo($"[DeveloperPatch] Forced {target.DeclaringType?.Name}.{target.Name} -> true.");
        }
        catch (Exception ex)
        {
            Plugin.Log.LogError($"[DeveloperPatch] Failed: {ex.Message}");
        }
    }

    private static bool ForceTrue(ref bool __result)
    {
        __result = true;
        return false; // skip original
    }
}
