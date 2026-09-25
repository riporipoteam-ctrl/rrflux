using System;
using System.Linq;
using System.Reflection;
using HarmonyLib;

namespace RecNetPlugin.Patches;

// Defensive guard against Store page crashes.
//
// The Store page crashes with an unknown root cause. Without client logs,
// we can't identify the exact failing method. This patch takes a defensive
// approach:
// 1. Finds Store-related UI types (Store, Shop, Storefront, Catalog)
// 2. Wraps their initialization/update methods in try-catch via Harmony
//    finalizers (which run even if the method throws)
// 3. Logs the exception instead of letting it crash the game
//
// This doesn't fix the underlying data issue, but it prevents a hard crash
// and gives us the exception details in the log for a real fix.
internal static class StoreCrashGuardPatch
{
    private static bool _done;
    private static int _attempts;
    private const int MaxAttempts = 5;

    public static void Apply()
    {
        if (_done || _attempts >= MaxAttempts) return;
        _attempts++;

        try
        {
            var keywords = new[] { "Store", "Shop", "Storefront", "Catalog" };
            var harmony = new Harmony("com.fluxrec.storeguard");
            var finalizer = new HarmonyMethod(typeof(StoreCrashGuardPatch).GetMethod(nameof(LogException),
                BindingFlags.Static | BindingFlags.NonPublic));

            int patched = 0;
            foreach (var asm in AppDomain.CurrentDomain.GetAssemblies())
            {
                Type[] types;
                try { types = asm.GetTypes(); }
                catch { continue; }

                foreach (var t in types)
                {
                    // Only UI-related store types, not data models
                    if (!keywords.Any(k => t.Name.Contains(k))) continue;
                    if (!t.Name.Contains("UI") && !t.Name.Contains("Page") &&
                        !t.Name.Contains("View") && !t.Name.Contains("Controller"))
                        continue;
                    if (patched >= 10) break; // Max 10 types

                    // Patch Awake/Start/OnEnable/Initialize methods
                    var methods = t.GetMethods(BindingFlags.Public | BindingFlags.NonPublic | BindingFlags.Instance | BindingFlags.DeclaredOnly)
                        .Where(m => m.Name == "Awake" || m.Name == "Start" ||
                                   m.Name == "OnEnable" || m.Name.Contains("Initialize"))
                        .Take(3);

                    foreach (var m in methods)
                    {
                        try
                        {
                            harmony.Patch(m, finalizer: finalizer);
                            patched++;
                            Plugin.Log.LogInfo($"[STORE-GUARD] guarding {t.Name}.{m.Name}");
                        }
                        catch { }
                    }
                }
                if (patched >= 10) break;
            }

            Plugin.Log.LogInfo($"[STORE-GUARD] guarding {patched} methods");
            _done = true;
        }
        catch (Exception e)
        {
            Plugin.Log.LogWarning($"[STORE-GUARD] attempt {_attempts} failed: {e.Message}");
        }
    }

    // Finalizer: runs after the method (even if it threw).
    // If __exception is not null, the method threw — we log it and
    // return null to SUPPRESS the exception (prevent crash).
    private static Exception LogException(MethodBase __originalMethod, Exception __exception)
    {
        if (__exception != null)
        {
            Plugin.Log.LogError($"[STORE-GUARD] suppressed crash in {__originalMethod.DeclaringType?.Name}.{__originalMethod.Name}: {__exception.Message}");
            Plugin.Log.LogError($"[STORE-GUARD] stack: {__exception.StackTrace}");
            return null; // Suppress the exception — prevents hard crash
        }
        return null;
    }
}
