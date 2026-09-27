using System;
using System.Collections.Generic;
using System.Linq;
using System.Reflection;
using HarmonyLib;

namespace RecNetPlugin.Patches;

// Forces the 2023 client's OLD Watch UI (legacy) on, bypassing Statsig entirely.
//
// Why this exists: The new Watch UI (RRUI) cannot be modified reliably —
// the Play button injection fails because the UI discovery crashes.
// The OLD UI has a tab row (Rooms/Clubs/Items/Inventions/Creators) that
// our PlayButtonPatch can clone successfully.
//
// How it works: at load (retried on scene load until it sticks), scan all
// loaded assemblies for the type declaring each gate getter. Force them to
// FALSE to get the legacy watch UI with the tab row.
//
// One knob, see [Watch] in the .cfg:
//   Force New Watch UI -> LEGACY (default false).
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
        // If user wants the new UI, don't force the old one
        if (Plugin.ForceNewWatchUI.Value)
            return;
            
        if (_done || _attempts >= MaxAttempts)
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
            Plugin.Log.LogInfo($"[WATCH] legacy watch UI forced on ({Patched.Count} getters patched to false)");
        else if (_attempts >= MaxAttempts)
            Plugin.Log.LogWarning(
                $"[WATCH] gave up after {_attempts} attempts — patched: {string.Join(",", Patched)}. " +
                "The getter names may have changed in this build.");
    }

    private static void PatchGates()
    {
        var harmony = new Harmony("net.rec.plugin.watchui");
        var prefix = new HarmonyMethod(typeof(WatchUIPatch).GetMethod(nameof(ForceFalsePrefix),
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
            Plugin.Log.LogInfo($"[WATCH] patched {type.FullName}.{getter} -> false (legacy UI)");
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

    // The actual gate: force false (legacy UI), skip the original (which would consult Statsig).
    private static bool ForceFalsePrefix(ref bool __result)
    {
        __result = false;
        return false;
    }
}
