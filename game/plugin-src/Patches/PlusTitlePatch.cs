using System;
using System.Collections.Generic;
using System.Linq;
using System.Reflection;
using HarmonyLib;
using Il2CppInterop.Runtime.Injection;
using UnityEngine;
using UnityEngine.UI;

namespace RecNetPlugin.Patches;

// Repairs the Flux Rec+ page title strings.
//
// Root cause (NOT the plugin — see below): the installer's RRPLUS_TITLE_PATCH
// (game/installer/src/patches.rs) does a same-length byte replacement in the
// RR+ UI AssetBundle:
//
//     from: " Room+ Membership"  ->  to: " Flux Rec+ Member"
//
// The patch author assumed "Rec" was a separate UI element. It is not (or the
// match lands inside the contiguous "Rec Room+ Membership" string), so the
// visible result is:
//
//     "Rec" + " Flux Rec+ Member"  =  "Rec Flux Rec+ Member"
//
// exactly what v0.1.33 shows on the header and back button. The patch also
// only rewrites the FIRST occurrence in the bundle, so other copies (the page
// title) still show the unrebranded "Rec Room+ Membership".
//
// What this does (runtime repair, works on already-installed broken clients):
// scans every Text under the Plus page when it opens (plus a short rescan
// window for late-built UI) and applies these replacements, LONGEST FIRST,
// each only when the text actually contains the pattern (idempotent — already
// correct text is never touched):
//     "Rec Flux Rec+ Member"  -> "Flux Rec+ Member"
//     "Rec Room+ Membership"  -> "Flux Rec+ Membership"
//     "Rec Room+ Member"      -> "Flux Rec+ Member"
//     "Rec Room Plus"         -> "Flux Rec+"
//
// Why a runtime sweep instead of only fixing the installer: Armin's installed
// v0.1.33 already has the bad bytes baked into his game files. A corrected
// installer patch would not match ("pattern not found — may already be
// patched") and his install would stay broken. This sweep repairs it in place,
// and is a harmless no-op once the installer is fixed.
//
// IL2CPP safety (same proven patterns as PlusPricePatch / PlusBalancePatch):
// - TryCast<T>() for downcasts; GetComponentsInChildren needs Il2CppSystem.Type.
// - TMPro resolved by reflection (no compile-time dependency).
// - Fail-soft: every step wrapped; never throws into game code.
// - Text is only rewritten when it actually changes (no layout thrash).
//
// Wiring: PlusTitlePatch.Apply() is called from PlusPricePatch.Apply()
// (which Plugin.cs already calls from Load() and OnSceneLoaded()), so no
// Plugin.cs change is needed.
internal static class PlusTitlePatch
{
    // Ordered longest-first. Outputs never contain any input pattern, so the
    // sweep is idempotent: running it twice changes nothing the second time.
    private static readonly KeyValuePair<string, string>[] Replacements =
    {
        new KeyValuePair<string, string>("Rec Flux Rec+ Member", "Flux Rec+ Member"),
        new KeyValuePair<string, string>("Rec Room+ Membership", "Flux Rec+ Membership"),
        new KeyValuePair<string, string>("Rec Room+ Member", "Flux Rec+ Member"),
        new KeyValuePair<string, string>("Rec Room Plus", "Flux Rec+"),
    };

    private const float RescanWindowSeconds = 10f;

    private static bool _watcherInstalled;
    private static bool _pumpCreated;
    private static bool _typeRegistered;

    private static GameObject _activePlusPage;
    private static float _rescanUntil;

    public static void Apply()
    {
        if (!Plugin.EnableFluxPlus.Value)
            return;

        try
        {
            EnsurePump();
            if (!_watcherInstalled)
            {
                var harmony = new Harmony("com.fluxrec.plustitle");
                var setActive = typeof(GameObject).GetMethod(nameof(GameObject.SetActive));
                var prefix = new HarmonyMethod(typeof(PlusTitlePatch).GetMethod(nameof(OnSetActive),
                    BindingFlags.Static | BindingFlags.NonPublic));
                harmony.Patch(setActive, prefix: prefix);
                _watcherInstalled = true;
                Plugin.Log.LogInfo("[PLUS-TITLE] page watcher installed");
            }
        }
        catch (Exception e)
        {
            Plugin.Log.LogWarning($"[PLUS-TITLE] setup failed: {e.Message}");
        }
    }

    private static void OnSetActive(GameObject __instance, bool value)
    {
        try
        {
            if (!value || __instance == null)
                return;
            var name = __instance.name ?? "";
            if (name.IndexOf("Plus", StringComparison.OrdinalIgnoreCase) < 0 &&
                name.IndexOf("Membership", StringComparison.OrdinalIgnoreCase) < 0)
                return;

            // Structural confirmation: only treat it as the Plus page when it
            // actually contains one of the broken/unrebranded strings. This
            // keeps random "Plus" popups untouched.
            if (!PageNeedsRepair(__instance))
                return;

            Plugin.Log.LogInfo($"[PLUS-TITLE] repairing titles on Plus page: {name}");
            _activePlusPage = __instance;
            _rescanUntil = Time.time + RescanWindowSeconds;
            RepairTitles(__instance);
        }
        catch { }
    }

    private static bool PageNeedsRepair(GameObject root)
    {
        try
        {
            foreach (var s in EnumerateTexts(root))
            {
                if (string.IsNullOrEmpty(s))
                    continue;
                foreach (var pair in Replacements)
                {
                    if (s.Contains(pair.Key))
                        return true;
                }
            }
        }
        catch { }
        return false;
    }

    private static void RepairTitles(GameObject root)
    {
        if (root == null)
            return;

        int repaired = 0;

        // uGUI Text path.
        try
        {
            var textType = Il2CppSystem.Type.GetType(typeof(Text).AssemblyQualifiedName);
            var texts = root.GetComponentsInChildren(textType, true);
            if (texts != null)
            {
                foreach (var t in texts)
                {
                    var txt = t.TryCast<Text>();
                    if (txt == null)
                        continue;
                    var fixed_ = ApplyReplacements(txt.text);
                    if (fixed_ != null)
                    {
                        txt.text = fixed_;
                        repaired++;
                    }
                }
            }
        }
        catch (Exception e)
        {
            Plugin.Log.LogWarning($"[PLUS-TITLE] uGUI scan failed: {e.Message}");
        }

        // TMPro fallback (no compile-time dependency).
        try
        {
            var tmproAsm = AppDomain.CurrentDomain.GetAssemblies()
                .FirstOrDefault(a => a.GetName().Name == "Unity.TextMeshPro");
            var tmproType = tmproAsm?.GetType("TMPro.TextMeshProUGUI");
            if (tmproType != null)
            {
                var getTexts = typeof(GameObject).GetMethod("GetComponentsInChildren",
                    new[] { typeof(Type), typeof(bool) });
                var comps = (System.Collections.IEnumerable)getTexts.Invoke(root,
                    new object[] { tmproType, true });
                var textProp = tmproType.GetProperty("text");
                foreach (var c in comps)
                {
                    var cur = textProp.GetValue(c, null) as string;
                    var fixed_ = ApplyReplacements(cur);
                    if (fixed_ != null)
                    {
                        textProp.SetValue(c, fixed_, null);
                        repaired++;
                    }
                }
            }
        }
        catch (Exception e)
        {
            Plugin.Log.LogWarning($"[PLUS-TITLE] TMPro scan failed: {e.Message}");
        }

        if (repaired > 0)
            Plugin.Log.LogInfo($"[PLUS-TITLE] repaired {repaired} title text(s)");
    }

    // Returns the repaired string, or null when nothing matched (caller keeps
    // the original text untouched). Longest-first ordering prevents a shorter
    // pattern from firing inside a longer one's match.
    private static string ApplyReplacements(string s)
    {
        if (string.IsNullOrEmpty(s))
            return null;
        string result = s;
        foreach (var pair in Replacements)
        {
            if (result.Contains(pair.Key))
                result = result.Replace(pair.Key, pair.Value);
        }
        return result != s ? result : null;
    }

    // Enumerates current text values without modifying anything (used for the
    // structural confirmation check).
    private static IEnumerable<string> EnumerateTexts(GameObject root)
    {
        var out_ = new List<string>();
        try
        {
            var textType = Il2CppSystem.Type.GetType(typeof(Text).AssemblyQualifiedName);
            var texts = root.GetComponentsInChildren(textType, true);
            if (texts != null)
            {
                foreach (var t in texts)
                {
                    var txt = t.TryCast<Text>();
                    if (txt != null)
                        out_.Add(txt.text ?? "");
                }
            }
        }
        catch { }
        try
        {
            var tmproAsm = AppDomain.CurrentDomain.GetAssemblies()
                .FirstOrDefault(a => a.GetName().Name == "Unity.TextMeshPro");
            var tmproType = tmproAsm?.GetType("TMPro.TextMeshProUGUI");
            if (tmproType != null)
            {
                var getTexts = typeof(GameObject).GetMethod("GetComponentsInChildren",
                    new[] { typeof(Type), typeof(bool) });
                var comps = (System.Collections.IEnumerable)getTexts.Invoke(root,
                    new object[] { tmproType, true });
                var textProp = tmproType.GetProperty("text");
                foreach (var c in comps)
                    out_.Add((textProp.GetValue(c, null) as string) ?? "");
            }
        }
        catch { }
        return out_;
    }

    private static void EnsurePump()
    {
        if (_pumpCreated)
            return;
        try
        {
            if (!_typeRegistered)
            {
                ClassInjector.RegisterTypeInIl2Cpp<TitleRepairPump>();
                _typeRegistered = true;
            }
            var go = new GameObject("FluxPlusTitlePump");
            go.hideFlags = HideFlags.HideAndDontSave;
            UnityEngine.Object.DontDestroyOnLoad(go);
            go.AddComponent<TitleRepairPump>();
            _pumpCreated = true;
        }
        catch (Exception e)
        {
            Plugin.Log.LogWarning($"[PLUS-TITLE] pump failed: {e.Message}");
        }
    }

    private class TitleRepairPump : MonoBehaviour
    {
        void Update()
        {
            // Re-scan the active Plus page while the window lasts. Catches
            // title text built a few frames after the page activates.
            try
            {
                if (_activePlusPage != null)
                {
                    if (Time.time < _rescanUntil)
                        RepairTitles(_activePlusPage);
                    else
                        _activePlusPage = null;
                }
            }
            catch { }
        }
    }
}
