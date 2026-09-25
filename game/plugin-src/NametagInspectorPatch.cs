using System;
using System.Linq;
using System.Reflection;
using HarmonyLib;
using UnityEngine;

namespace RecNetPlugin.Patches;

// Runtime inspector for nametag/badge selection.
// When Armin selects "Community Mod" or "Developer" in the Dev tab, this
// captures: which method is called, what value is saved, and where.
// This tells us whether it's a server-persisted setting or local-only,
// and what the nametag renderer expects.
internal static class NametagInspectorPatch
{
    private static bool _done;

    public static void Apply()
    {
        if (_done) return;
        _done = true;

        try
        {
            DumpNametagTypes();
            PatchNametagMethods();
        }
        catch (Exception e)
        {
            Plugin.Log.LogWarning($"[NAMETAG-INSPECT] failed: {e.Message}");
        }
    }

    private static void DumpNametagTypes()
    {
        var keywords = new[] { "Nametag", "NameTag", "Badge", "Title" };
        var found = 0;

        foreach (var asm in AppDomain.CurrentDomain.GetAssemblies())
        {
            Type[] types;
            try { types = asm.GetTypes(); }
            catch { continue; }

            foreach (var t in types)
            {
                if (!keywords.Any(k => t.Name.Contains(k))) continue;
                found++;
                if (found > 20) return;

                Plugin.Log.LogInfo($"[NAMETAG-INSPECT] TYPE {asm.GetName().Name}.{t.FullName}");
                try
                {
                    foreach (var m in t.GetMethods(BindingFlags.Public | BindingFlags.NonPublic | BindingFlags.Instance | BindingFlags.Static | BindingFlags.DeclaredOnly).Take(30))
                    {
                        var ps = string.Join(",", m.GetParameters().Select(p => p.ParameterType.Name));
                        Plugin.Log.LogInfo($"[NAMETAG-INSPECT]   {m.ReturnType.Name} {m.Name}({ps})");
                    }
                }
                catch { }
            }
        }
        Plugin.Log.LogInfo($"[NAMETAG-INSPECT] found {found} nametag-related types");
    }

    private static void PatchNametagMethods()
    {
        // Find methods with "Badge", "Title", or "Nametag" in the name and log when called.
        // This captures the selection flow.
        var harmony = new Harmony("com.fluxrec.nametaginspect");
        var keywords = new[] { "badge", "title", "nametag" };
        var patched = 0;

        foreach (var asm in AppDomain.CurrentDomain.GetAssemblies())
        {
            Type[] types;
            try { types = asm.GetTypes(); }
            catch { continue; }

            foreach (var t in types)
            {
                MethodInfo[] methods;
                try { methods = t.GetMethods(BindingFlags.Public | BindingFlags.NonPublic | BindingFlags.Instance | BindingFlags.Static); }
                catch { continue; }

                foreach (var m in methods)
                {
                    var nameLower = m.Name.ToLower();
                    if (!keywords.Any(k => nameLower.Contains(k))) continue;
                    if (m.GetParameters().Length > 2) continue; // skip complex methods
                    if (patched >= 30) return;

                    try
                    {
                        var prefix = new HarmonyMethod(typeof(NametagInspectorPatch).GetMethod(nameof(LogNametagCall),
                            BindingFlags.Static | BindingFlags.NonPublic));
                        harmony.Patch(m, prefix: prefix);
                        patched++;
                    }
                    catch { }
                }
            }
        }
        Plugin.Log.LogInfo($"[NAMETAG-INSPECT] patched {patched} nametag-related methods for logging");
    }

    private static void LogNametagCall(MethodBase __originalMethod, object[] __args)
    {
        try
        {
            var args = __args != null ? string.Join(",", __args.Select(a => a?.ToString() ?? "null")) : "";
            Plugin.Log.LogInfo($"[NAMETAG-INSPECT] CALL {__originalMethod.DeclaringType?.Name}.{__originalMethod.Name}({args})");
        }
        catch { }
    }
}
