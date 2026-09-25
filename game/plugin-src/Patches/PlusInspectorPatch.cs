using System;
using System.Collections.Generic;
using System.Linq;
using System.Reflection;
using HarmonyLib;
using UnityEngine;

namespace RecNetPlugin.Patches;

// Runtime inspector for the Rec Room+ Membership page.
// Dumps type/method info and UI hierarchy to the BepInEx log so we can
// write precise patches without interactive debugging.
//
// What it captures:
// 1. All types with Plus/Membership/CampusCard/Price in the name + their methods
// 2. GameObject hierarchy when a Plus-related page opens
// 3. Which method loads membership prices (to intercept it)
//
// This is diagnostic only — it does not change behavior.
internal static class PlusInspectorPatch
{
    private static bool _dumped;
    private static readonly HashSet<string> _loggedPages = new();

    public static void Apply()
    {
        if (_dumped) return;
        _dumped = true;

        try
        {
            DumpPlusTypes();
            PatchPageDetection();
        }
        catch (Exception e)
        {
            Plugin.Log.LogWarning($"[PLUS-INSPECT] failed: {e.Message}");
        }
    }

    private static void DumpPlusTypes()
    {
        var keywords = new[] { "Plus", "Membership", "CampusCard", "Subscription" };
        var found = new List<(string TypeName, string Assembly, List<string> Methods)>();

        foreach (var asm in AppDomain.CurrentDomain.GetAssemblies())
        {
            Type[] types;
            try { types = asm.GetTypes(); }
            catch { continue; }

            foreach (var t in types)
            {
                if (!keywords.Any(k => t.Name.Contains(k))) continue;

                var methods = new List<string>();
                try
                {
                    foreach (var m in t.GetMethods(BindingFlags.Public | BindingFlags.NonPublic | BindingFlags.Instance | BindingFlags.Static | BindingFlags.DeclaredOnly))
                    {
                        var ps = string.Join(",", m.GetParameters().Select(p => p.ParameterType.Name));
                        methods.Add($"{m.ReturnType.Name} {m.Name}({ps})");
                    }
                }
                catch { }

                found.Add((t.FullName ?? t.Name, asm.GetName().Name ?? "?", methods));
            }
        }

        Plugin.Log.LogInfo($"[PLUS-INSPECT] found {found.Count} Plus-related types:");
        foreach (var (typeName, asmName, methods) in found.Take(30))
        {
            Plugin.Log.LogInfo($"[PLUS-INSPECT] TYPE {asmName}.{typeName} ({methods.Count} methods)");
            foreach (var m in methods.Take(40))
                Plugin.Log.LogInfo($"[PLUS-INSPECT]   METHOD {m}");
        }
    }

    private static void PatchPageDetection()
    {
        // Patch UnityEngine.GameObject.SetActive to detect when Plus page opens
        // by watching for GameObjects with "Plus" or "Membership" in the name.
        var harmony = new Harmony("com.fluxrec.plusinspect");
        var setActive = typeof(GameObject).GetMethod(nameof(GameObject.SetActive));
        var prefix = new HarmonyMethod(typeof(PlusInspectorPatch).GetMethod(nameof(OnSetActive),
            BindingFlags.Static | BindingFlags.NonPublic));
        harmony.Patch(setActive, prefix: prefix);
    }

    private static void OnSetActive(GameObject __instance, bool value)
    {
        try
        {
            if (!value || __instance == null) return;
            var name = __instance.name;
            if (name == null) return;
            if (!name.Contains("Plus") && !name.Contains("Membership")) return;
            if (!_loggedPages.Add(name)) return; // log each page once

            Plugin.Log.LogInfo($"[PLUS-INSPECT] Plus page activated: {name}");
            DumpHierarchy(__instance.transform, 0, 4);
        }
        catch { }
    }

    private static void DumpHierarchy(Transform t, int depth, int maxDepth)
    {
        if (t == null || depth > maxDepth) return;
        var indent = new string(' ', depth * 2);
        var comps = t.GetComponents<Component>().Select(c => c?.GetType().Name ?? "?").ToArray();
        Plugin.Log.LogInfo($"[PLUS-INSPECT] {indent}{t.gameObject.name} [{string.Join(",", comps.Take(5))}]");

        // If it's a Text component, log the text content
        foreach (var c in t.GetComponents<Component>())
        {
            if (c == null) continue;
            var cn = c.GetType().Name;
            if (cn.Contains("Text"))
            {
                try
                {
                    var prop = c.GetType().GetProperty("text");
                    var txt = prop?.GetValue(c) as string;
                    if (!string.IsNullOrEmpty(txt) && txt.Length < 200)
                        Plugin.Log.LogInfo($"[PLUS-INSPECT] {indent}  TEXT: \"{txt}\"");
                }
                catch { }
            }
        }

        if (depth < maxDepth)
        {
            for (int i = 0; i < Math.Min(t.childCount, 20); i++)
                DumpHierarchy(t.GetChild(i), depth + 1, maxDepth);
        }
    }
}
