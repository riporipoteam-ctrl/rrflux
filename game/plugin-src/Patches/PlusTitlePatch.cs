using System;
using System.Collections;
using System.Collections.Generic;
using System.Linq;
using System.Reflection;
using Il2CppInterop.Runtime.Injection;
using UnityEngine;
using UnityEngine.UI;

namespace RecNetPlugin.Patches;

// Repairs the Flux Rec+ page title strings.
//
// Root cause (NOT the plugin): the installer's RRPLUS_TITLE_PATCH does a
// same-length byte replacement in the RR+ UI AssetBundle
// (" Room+ Membership" -> " Flux Rec+ Member"), assuming "Rec" was a separate
// UI element. It isn't (the match lands inside the contiguous
// "Rec Room+ Membership" string), so the visible result is
// "Rec" + " Flux Rec+ Member" = "Rec Flux Rec+ Member" on the header and
// back button, while the first-occurrence-only patch leaves other copies
// (the page title) as unrebranded "Rec Room+ Membership".
//
// Why a runtime sweep: Armin's installed v0.1.33 already has the bad bytes
// baked into his game files. A corrected installer patch would not match
// ("pattern not found — may already be patched") and his install would stay
// broken. This repairs it in place and is a harmless no-op once the
// installer is fixed.
//
// Design (v0.1.35): the old SetActive gate is GONE. It only armed when the
// page activated, but the title text is populated ASYNC after activation, so
// the gate never saw the broken string. Instead a lightweight pump sweeps
// every 5 seconds. Efficiency guard: the sweep only scans active GameObjects
// whose name contains "Plus" or "Membership" (e.g.
// RecNetRRPlusMembershipPage, MembershipBenefitsScreen) — never the whole
// scene. Both uGUI Text and TMPro are covered (TMPro via the TMP_Text base
// type, so TextMeshProUGUI and TextMeshPro are both caught). Replacements are
// longest-first and idempotent: text is rewritten only when it actually
// changes, so the sweep is silent once everything is fixed.
//
// IL2CPP safety (same proven patterns as PlusPricePatch / PlusBalancePatch):
// - TryCast<T>() for downcasts; GetComponentsInChildren takes
//   Il2CppSystem.Type.
// - TMPro resolved by reflection (no compile-time dependency).
// - Fail-soft: every step wrapped; never throws into game code.
//
// Wiring: PlusTitlePatch.Apply() is called from PlusPricePatch.Apply()
// (which Plugin.cs already calls from Load() and OnSceneLoaded()), so no
// Plugin.cs change is needed.
internal static class PlusTitlePatch
{
    // Longest-first. Note "Rec Room+ Membership" contains "Rec Room+ Member"
    // as a prefix, so the longer pattern MUST be applied first. Outputs never
    // contain any input pattern, so the sweep is idempotent: running it twice
    // changes nothing the second time.
    private static readonly KeyValuePair<string, string>[] Replacements =
    {
        new KeyValuePair<string, string>("Rec Flux Rec+ Member", "Flux Rec+ Membership"),
        new KeyValuePair<string, string>("Rec Room+ Membership", "Flux Rec+ Membership"),
        new KeyValuePair<string, string>("Rec Room+ Member", "Flux Rec+ Member"),
    };

    private const float SweepIntervalSeconds = 5f;
    private const string PumpObjectName = "FluxTitleRepairPump";

    private static bool _pumpCreated;
    private static bool _typeRegistered;

    // Cached reflection (resolved lazily once — avoids per-sweep cost).
    private static Il2CppSystem.Type _uguiTextType;
    private static Type _tmproTextType;
    private static PropertyInfo _tmproTextProp;
    private static bool _tmproResolved;

    public static void Apply()
    {
        if (!Plugin.EnableFluxPlus.Value)
            return;

        try
        {
            EnsurePump();
            Plugin.Log.LogInfo("[PLUS] title repair sweep installed (every 5s, Plus pages only)");
        }
        catch (Exception e)
        {
            Plugin.Log.LogWarning($"[PLUS] title repair setup failed: {e.Message}");
        }
    }

    // Periodic sweep: find likely-open Plus pages, repair their titles.
    // Only logs when something actually changed (self-throttled).
    private static void Sweep()
    {
        int repaired = 0;

        try
        {
            var all = UnityEngine.Object.FindObjectsOfType(typeof(GameObject));
            if (all == null)
                return;

            foreach (var o in all)
            {
                var go = o?.TryCast<GameObject>();
                if (go == null)
                    continue;

                var name = go.name;
                if (string.IsNullOrEmpty(name) || name == PumpObjectName)
                    continue;
                if (name.IndexOf("Plus", StringComparison.OrdinalIgnoreCase) < 0 &&
                    name.IndexOf("Membership", StringComparison.OrdinalIgnoreCase) < 0)
                    continue;
                if (!go.activeInHierarchy)
                    continue; // page not actually open — skip, no wasted work

                repaired += RepairTitles(go);
            }
        }
        catch (Exception e)
        {
            Plugin.Log.LogWarning($"[PLUS] title sweep failed: {e.Message}");
        }

        if (repaired > 0)
            Plugin.Log.LogInfo($"[PLUS] Repaired {repaired} titles");
    }

    private static int RepairTitles(GameObject root)
    {
        if (root == null)
            return 0;

        int repaired = 0;

        // uGUI Text path.
        try
        {
            if (_uguiTextType == null)
                _uguiTextType = Il2CppSystem.Type.GetType(typeof(Text).AssemblyQualifiedName);

            var texts = root.GetComponentsInChildren(_uguiTextType, true);
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
            Plugin.Log.LogWarning($"[PLUS] uGUI scan failed: {e.Message}");
        }

        // TMPro path via the TMP_Text base type (covers TextMeshProUGUI and
        // TextMeshPro; no compile-time dependency).
        try
        {
            if (!_tmproResolved)
            {
                var tmproAsm = AppDomain.CurrentDomain.GetAssemblies()
                    .FirstOrDefault(a => a.GetName().Name == "Unity.TextMeshPro");
                _tmproTextType = tmproAsm?.GetType("TMPro.TMP_Text");
                _tmproTextProp = _tmproTextType?.GetProperty("text");
                _tmproResolved = true;
            }

            if (_tmproTextType != null && _tmproTextProp != null)
            {
                var getTexts = typeof(GameObject).GetMethod("GetComponentsInChildren",
                    new[] { typeof(Type), typeof(bool) });
                var comps = (IEnumerable)getTexts.Invoke(root,
                    new object[] { _tmproTextType, true });
                foreach (var c in comps)
                {
                    var cur = _tmproTextProp.GetValue(c, null) as string;
                    var fixed_ = ApplyReplacements(cur);
                    if (fixed_ != null)
                    {
                        _tmproTextProp.SetValue(c, fixed_, null);
                        repaired++;
                    }
                }
            }
        }
        catch (Exception e)
        {
            Plugin.Log.LogWarning($"[PLUS] TMPro scan failed: {e.Message}");
        }

        return repaired;
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
            var go = new GameObject(PumpObjectName);
            go.hideFlags = HideFlags.HideAndDontSave;
            UnityEngine.Object.DontDestroyOnLoad(go);
            go.AddComponent<TitleRepairPump>();
            _pumpCreated = true;
        }
        catch (Exception e)
        {
            Plugin.Log.LogWarning($"[PLUS] title pump failed: {e.Message}");
        }
    }

    private class TitleRepairPump : MonoBehaviour
    {
        private float _accum;

        void Update()
        {
            // Throttled sweep: the page title is populated async AFTER the
            // page opens, so a one-shot scan on activation misses it. A 5s
            // sweep catches late-built UI without per-frame cost.
            try
            {
                if (!Plugin.EnableFluxPlus.Value)
                    return;
                _accum += Time.unscaledDeltaTime;
                if (_accum < SweepIntervalSeconds)
                    return;
                _accum = 0f;
                Sweep();
            }
            catch { }
        }
    }
}
