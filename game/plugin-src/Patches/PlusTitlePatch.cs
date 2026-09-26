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
// Design (v0.1.35, FIXED TIMING): the Plus page title populates
// ASYNCHRONOUSLY after activation, so any one-shot scan at open time misses
// it. Instead a Harmony prefix on GameObject.SetActive (the proven
// PlusInspectorPatch pattern) detects the Plus page opening and STARTS a
// timed sweep: every 2 seconds for 30 seconds the repair runs against the
// opened page root(s), catching late-populated text. After 30s the timer
// stops (zero cost afterwards) and re-arms the next time the page opens.
//
// The sweep covers both uGUI Text AND TMPro (the Plus page uses TMPro) —
// TMPro via the TMP_Text base type, so TextMeshProUGUI and TextMeshPro are
// both caught. Replacements are longest-first and idempotent: text is
// rewritten only when it actually changes, so the sweep is silent once
// everything is fixed.
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
    // as a prefix, so the longer pattern MUST be applied first. The "Rec Room+"
    // catch-all runs LAST so it can't fire inside a longer match. Outputs never
    // contain any input pattern, so the sweep is idempotent: running it twice
    // changes nothing the second time.
    private static readonly KeyValuePair<string, string>[] Replacements =
    {
        new KeyValuePair<string, string>("Rec Flux Rec+ Member", "Flux Rec+ Membership"),
        new KeyValuePair<string, string>("Rec Room+ Membership", "Flux Rec+ Membership"),
        new KeyValuePair<string, string>("Rec Room+ Member", "Flux Rec+ Member"),
        new KeyValuePair<string, string>("Rec Room+", "Flux Rec+"),
    };

    private const float SweepIntervalSeconds = 2f;
    private const float SweepWindowSeconds = 30f;
    private const string PumpObjectName = "FluxTitleRepairPump";
    private const string HarmonyId = "com.fluxrec.plustitle";

    private static bool _patched;
    private static bool _pumpCreated;
    private static bool _typeRegistered;

    // Timed-sweep state. Armed by OnPlusPageOpened, driven by the pump.
    private static bool _sweepActive;
    private static float _sweepEndsAt;   // unscaled time the 30s window closes
    private static float _nextSweepAt;   // unscaled time of the next 2s tick
    private static readonly List<GameObject> _pageRoots = new();
    private static readonly object _rootsLock = new();

    // Cached reflection (resolved lazily once — avoids per-sweep cost).
    private static Il2CppSystem.Type _uguiTextType;
    private static Type _tmproTextType;
    private static PropertyInfo _tmproTextProp;
    private static bool _tmproResolved;

    public static void Apply()
    {
        if (!Plugin.EnableFluxPlus.Value)
            return;
        if (_patched)
            return;
        _patched = true;

        try
        {
            // Detect Plus page opens: SetActive hook (PlusInspectorPatch pattern).
            var harmony = new Harmony(HarmonyId);
            var setActive = typeof(GameObject).GetMethod(nameof(GameObject.SetActive));
            var prefix = new HarmonyMethod(
                typeof(PlusTitlePatch).GetMethod(nameof(OnSetActive),
                    BindingFlags.Static | BindingFlags.NonPublic));
            harmony.Patch(setActive, prefix: prefix);
            EnsurePump();
            Plugin.Log.LogInfo("[PLUS] title repair armed (2s sweep x30s when Plus page opens)");
        }
        catch (Exception e)
        {
            Plugin.Log.LogWarning($"[PLUS] title repair setup failed: {e.Message}");
        }
    }

    // Harmony prefix on GameObject.SetActive: fires on the game thread when
    // the Plus page (e.g. RecNetRRPlusMembershipPage) becomes visible.
    private static void OnSetActive(GameObject __instance, bool value)
    {
        try
        {
            if (!value || __instance == null)
                return;
            if (!Plugin.EnableFluxPlus.Value)
                return;
            var name = __instance.name;
            if (string.IsNullOrEmpty(name))
                return;
            if (name.IndexOf("Plus", StringComparison.OrdinalIgnoreCase) < 0 &&
                name.IndexOf("Membership", StringComparison.OrdinalIgnoreCase) < 0)
                return;
            OnPlusPageOpened(__instance);
        }
        catch { }
    }

    // Page opened: (re-)start the 30s repair window, run the first pass NOW.
    private static void OnPlusPageOpened(GameObject root)
    {
        try
        {
            lock (_rootsLock)
            {
                if (!_pageRoots.Contains(root))
                    _pageRoots.Add(root);
            }
            var now = Time.unscaledTime;
            if (!_sweepActive)
                Plugin.Log.LogInfo("[PLUS] Plus page opened — starting 30s title repair sweep");
            _sweepActive = true;
            _sweepEndsAt = now + SweepWindowSeconds;
            _nextSweepAt = now; // immediate first pass; the pump ticks every 2s after
        }
        catch { }
    }

    // Called from the pump's Update. Runs the 2s tick while the 30s window
    // is open, then stops itself (zero cost afterwards). Re-arms on the
    // next page open via OnPlusPageOpened.
    private static void PumpTick()
    {
        try
        {
            if (!_sweepActive)
                return;
            if (!Plugin.EnableFluxPlus.Value)
            {
                _sweepActive = false;
                return;
            }

            var now = Time.unscaledTime;

            // Window closed: stop the timer, drop roots, stay quiet until
            // the next page open.
            if (now >= _sweepEndsAt)
            {
                _sweepActive = false;
                lock (_rootsLock)
                {
                    _pageRoots.Clear();
                }
                Plugin.Log.LogInfo("[PLUS] title repair sweep done (30s window closed)");
                return;
            }

            if (now < _nextSweepAt)
                return;
            _nextSweepAt = now + SweepIntervalSeconds;

            int repaired = 0;
            GameObject[] roots;
            lock (_rootsLock)
            {
                roots = _pageRoots.ToArray();
            }
            foreach (var root in roots)
            {
                try
                {
                    repaired += RepairTitles(root);
                }
                catch { } // destroyed page root — next tick or window-close cleans up
            }

            // Self-throttled: log only when a repair actually happened.
            if (repaired > 0)
                Plugin.Log.LogInfo($"[PLUS] Repaired {repaired} titles");
        }
        catch { }
    }

    // Repairs every Text / TMP_Text under the page root. Longest-first
    // replacements make this idempotent.
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
        // TextMeshPro; no compile-time dependency). The Plus page uses TMPro.
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
                var comps = (System.Collections.IEnumerable)getTexts.Invoke(root,
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

    // Persistent pump (created once via Apply). Update() is a near-zero-cost
    // bool+time check when no sweep window is active; PumpTick() does the
    // real work only during the 30s window after a Plus page opens.
    private class TitleRepairPump : MonoBehaviour
    {
        void Update()
        {
            PumpTick();
        }
    }
}
