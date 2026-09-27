using System;
using System.Collections.Generic;
using System.Linq;
using System.Reflection;
using HarmonyLib;

namespace RecNetPlugin.Patches;

// Forces the 2023 client's new Watch UI (RRUI) on, bypassing Statsig entirely.
//
// Why this exists: the 2023 client fetches Statsig gate values directly from
// https://statsigapi.net/v1 — not from our backend — and our backend serves
// UseStatSig=false with an empty key, so every gate falls back to its code
// default (false) and the client renders the legacy watch UI. There is no
// backend lever for this: we don't have Rec Room's Statsig client key, and we
// can't serve a valid cert for statsigapi.net. So we force the client-side
// gate getters to true instead.
//
// How it works: at load (retried on scene load until it sticks), scan all
// loaded assemblies for the type declaring each gate getter. The declaring
// type's name is obfuscated and re-rolled per build, but the getter method
// names are NOT obfuscated, so we resolve by method name, not type name. All
// five element-level getters are forced together — forcing only the home
// getter would leave the notifications/events/people tabs on the legacy UI.
//
// Do NOT extend this to get_IsOnRRUIStandalonePage or
// get_ShouldUseRRUINotificationsScreen: those are read-only state queries,
// not gates.
//
// One knob, see [Watch] in the .cfg:
//   Force New Watch UI -> THE FIX (default true).
//
// Caveat: forcing the gates makes the client render the RRUI watch, but if any
// RRUI data source (routes prefill, tab content) depends on a backend endpoint
// we haven't implemented, tabs can render empty. If the new watch shows but a
// tab is empty after this patch, that's a backend content gap to investigate
// with real client traffic — not a gate problem. The knob exists to fall back
// to the legacy watch in that case.
internal static class WatchUIPatch
{
    // Gate getter method names to force true. Resolved by name because the
    // declaring type is obfuscated; the names themselves are not.
    private static readonly string[] GateGetters =
    {
        "get_UseRRUIHomeScreen",          // main home-screen gate
        "get_UseNewWatchArchitecture",    // new watch architecture gate
        "get_UseRRUINotificationsScreen", // notifications tab
        "get_UseRRUIEventsScreen",        // events tab
        "get_UseRRUIPeopleScreen",        // people tab
    };

    private const int MaxAttempts = 10;

    private static int _attempts;
    private static bool _done;
    private static readonly HashSet<string> Patched = new();

    // Called from Plugin.Load and again on each scene load until every getter
    // is patched — the declaring type may live in an assembly that isn't
    // loaded yet when BepInEx runs Load().
    public static void Apply()
    {
        if (_done || !Plugin.ForceNewWatchUI.Value || _attempts >= MaxAttempts)
            return;

        _attempts++;

        try
        {
            PatchGates();
        }
        catch (Exception e)
        {
            Plugin.Log.LogWarning($"[WATCH] attempt {_attempts} failed: {e.Message}");
            return;
        }

        _done = GateGetters.All(g => Patched.Contains(g));
        if (_done)
            Plugin.Log.LogInfo($"[WATCH] new watch UI forced on ({Patched.Count} getters patched)");
        else if (_attempts >= MaxAttempts)
            Plugin.Log.LogWarning(
                $"[WATCH] gave up after {_attempts} attempts — patched: {string.Join(",", Patched)}. " +
                "Legacy watch UI remains. The getter names may have changed in this build.");
    }

    private static void PatchGates()
    {
        var harmony = new Harmony("net.rec.plugin.watchui");
        var prefix = new HarmonyMethod(typeof(WatchUIPatch).GetMethod(nameof(ForceTruePrefix),
            BindingFlags.Static | BindingFlags.NonPublic));

        foreach (var getter in GateGetters)
        {
            if (Patched.Contains(getter))
                continue;

            var (type, method) = FindGetter(getter);
            if (method == null)
            {
                Plugin.Log.LogWarning($"[WATCH] {getter} not found in loaded assemblies — will retry");
                continue;
            }

            harmony.Patch(method, prefix: prefix);
            Patched.Add(getter);
            Plugin.Log.LogInfo($"[WATCH] patched {type.FullName}.{getter} -> true");
        }
    }

    // Resolve one getter by method name across all loaded assemblies. Takes
    // the first non-interface type whose method is a real (non-abstract)
    // 0-param bool getter — never the abstract IL2CPP "interface" stub
    // (patching an abstract method compiles but the prefix never runs,
    // because the game dispatches to the concrete implementation).
    private static (Type, MethodInfo) FindGetter(string getterName)
    {
        foreach (var asm in AppDomain.CurrentDomain.GetAssemblies())
        {
            Type[] types;
            try { types = asm.GetTypes(); }
            catch (ReflectionTypeLoadException e) { types = e.Types.Where(t => t != null).ToArray(); }
            catch { continue; }

            foreach (var t in types)
            {
                if (t == null || t.IsInterface)
                    continue;
                var m = t.GetMethod(getterName,
                    BindingFlags.Public | BindingFlags.NonPublic | BindingFlags.Static | BindingFlags.Instance);
                if (m == null || m.IsAbstract || m.GetParameters().Length != 0 || m.ReturnType != typeof(bool))
                    continue;
                return (t, m);
            }
        }
        return (null, null);
    }

    // The actual gate: force true, skip the original (which would consult Statsig).
    private static bool ForceTruePrefix(ref bool __result)
    {
        __result = true;
        return false;
    }
}
