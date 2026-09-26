// Adds a "Connect" tab to the real Rec Room Watch UI and wires it to the
// Flux pairing overlay (same as pressing F8). This file implements the
// CLONE + RELABEL + CLICK-HANDLER steps, exactly like PlayButtonPatch.
//
// Why this shape:
//  - The Watch UI is 100% code-built at runtime via RRUI — there are no
//    Watch prefabs to edit. Home tabs live under HomeTop5TabsModel with
//    enum {Rooms=0, Clubs=1, Items=2, Inventions=3, Creators=4}.
//  - Tab button GameObject/type names may be obfuscated and re-rolled per
//    build, so the plugin resolves a tab button by its VISIBLE LABEL
//    ("Rooms", "Clubs", "Items", "Inventions", "Creators", falling back to
//    "Create" on the legacy home screen), never by GameObject/type name.
//  - The plugin clones that button (same prefab, same style, same tab row),
//    inserts the clone as its next sibling and relabels it "Connect". Visual
//    consistency with the game's design language is guaranteed because it is
//    literally the same button GameObject.
//  - The clone's original onClick listeners (which switch to the cloned tab)
//    are removed and replaced with a single listener that opens the Flux
//    pairing overlay. The listener is OUR OWN fresh handler wired through
//    DelegateSupport.ConvertDelegate — we never wrap the game's callbacks,
//    so the v0.1.30 Delegate.CreateDelegate hang cannot recur.
//  - The tab is a pure overlay trigger: it never registers with
//    TabsModel<T>.GoToPage, so it cannot corrupt tab navigation state.
//
// Hard rules honored:
//  - The source tab itself is untouched: the plugin only ADDS a sibling.
//  - Idempotent: the clone is only added once per tab row (name-suffix
//    check), so scene reloads and repeated Apply() calls never duplicate it.
//
// Resolution is by visible label at runtime (never by obfuscated type name).
// IL2CPP rules: click handlers go through
// DelegateSupport.ConvertDelegate<UnityAction>(new Action(...)) — NEVER
// new UnityAction(...) or method-group construction; downcasts go through
// TryCast<T>(), never direct casts; GetComponent / GetComponentsInChildren
// require Il2CppSystem.Type.
//
// One knob, see [Pairing] in the .cfg:
//   Enable Flux Pairing -> THE FEATURE (default true).
using System;
using System.Collections;
using System.Linq;
using System.Reflection;
using UnityEngine;
using UnityEngine.Events;
using UnityEngine.UI;

namespace RecNetPlugin.Patches;

internal static class FluxConnectButton
{
    private const string CloneNameSuffix = "_FluxConnectTab";
    private const string CloneLabel = "Connect";
    private const int MaxAttempts = 20;

    // Visible labels to try as the clone source, in preference order: the
    // Watch home tab row first, then the legacy home screen's Create button.
    private static readonly string[] SourceLabels =
    {
        "Rooms", "Clubs", "Items", "Inventions", "Creators", "Create"
    };

    private static int _attempts;
    private static bool _buttonDone;

    // Called from Plugin.Load and again on each scene load: retries the
    // tab clone until the home screen / Watch tab row exists.
    public static void Apply()
    {
        if (!Plugin.EnableFluxPairing.Value)
            return;

        if (_buttonDone)
            return;

        if (_attempts >= MaxAttempts)
            return;

        _attempts++;
        try
        {
            EnsureConnectTab();
        }
        catch (Exception e)
        {
            Plugin.Log.LogWarning($"[CONNECT] attempt {_attempts} failed: {e.Message}");
        }

        if (_attempts >= MaxAttempts && !_buttonDone)
            Plugin.Log.LogWarning("[CONNECT] gave up adding the Connect tab — " +
                "the Watch home tab row was never found after " + MaxAttempts + " attempts.");
    }

    // Find a source tab button by its visible label and clone it into a
    // "Connect" tab right beside it.
    private static void EnsureConnectTab()
    {
        var source = FindSourceTab();
        if (source == null)
        {
            Plugin.Log.LogDebug("[CONNECT] home tab row not found yet");
            return;
        }

        var parent = source.transform.parent;
        if (parent == null)
        {
            Plugin.Log.LogWarning("[CONNECT] source tab has no parent row — skipping");
            _attempts = MaxAttempts; // structural mismatch — stop retrying
            return;
        }

        // Already added? (a sibling already carrying our suffix)
        for (int i = 0; i < parent.childCount; i++)
        {
            var child = parent.GetChild(i);
            if (child != null && child.name.EndsWith(CloneNameSuffix, StringComparison.Ordinal))
            {
                _buttonDone = true;
                Plugin.Log.LogDebug("[CONNECT] Connect tab already present");
                return;
            }
        }

        CloneAsConnect(source);
        _buttonDone = true;
    }

    // Locate a source tab button: any active uGUI Button whose child label
    // text matches one of SourceLabels (checked in preference order).
    // Prefers candidates living under a Home/Watch-named ancestor (the RRUI
    // home screen / Watch UI) over identically-labeled buttons elsewhere.
    private static GameObject FindSourceTab()
    {
        var find = typeof(UnityEngine.Object).GetMethod("FindObjectsOfType",
            new[] { typeof(Type) });
        if (find == null)
            return null;

        var all = (IEnumerable)find.Invoke(null, new object[] { typeof(Button) });
        if (all == null)
            return null;

        GameObject best = null;
        string bestLabel = null;
        bool bestIsHome = false;
        foreach (var o in all)
        {
            // Il2Cpp downcasts must go through TryCast, never a direct cast.
            var button = ((UnityEngine.Object)o).TryCast<Button>();
            if (button == null)
                continue;
            var go = button.gameObject;
            if (go == null || !go.activeInHierarchy)
                continue;
            // Skip our own clone if it somehow exists without the suffix check.
            if (go.name.EndsWith(CloneNameSuffix, StringComparison.Ordinal))
                continue;

            var label = MatchSourceLabel(go);
            if (label == null)
                continue;

            bool isHome = IsUnderHome(go);
            if (best == null || (isHome && !bestIsHome))
            {
                best = go;
                bestLabel = label;
                bestIsHome = isHome;
                if (isHome)
                    break; // home-screen / Watch tab is the one we want
            }
        }

        if (best != null)
            Plugin.Log.LogInfo($"[CONNECT] found source tab '{bestLabel}': '{best.name}'");
        return best;
    }

    // Returns the matching source label for go's child label, or null.
    private static string MatchSourceLabel(GameObject go)
    {
        foreach (var label in SourceLabels)
        {
            if (HasLabel(go, label))
                return label;
        }
        return null;
    }

    private static bool IsUnderHome(GameObject go)
    {
        var t = go.transform.parent;
        while (t != null)
        {
            var n = t.name ?? string.Empty;
            if (n.IndexOf("Home", StringComparison.OrdinalIgnoreCase) >= 0 ||
                n.IndexOf("Watch", StringComparison.OrdinalIgnoreCase) >= 0)
                return true;
            t = t.parent;
        }
        return false;
    }

    // True when any uGUI Text or TMPro label under go reads exactly `label`.
    private static bool HasLabel(GameObject go, string label)
    {
        // uGUI path
        // be.788: GetComponentsInChildren requires Il2CppSystem.Type, not System.Type.
        var textType = Il2CppSystem.Type.GetType(typeof(Text).AssemblyQualifiedName);
        var texts = go.GetComponentsInChildren(textType, true);
        if (texts != null)
        {
            foreach (var c in texts)
            {
                var txt = c.TryCast<Text>()?.text;
                if (!string.IsNullOrWhiteSpace(txt) &&
                    txt.Trim().Equals(label, StringComparison.OrdinalIgnoreCase))
                    return true;
            }
        }

        // TMPro fallback (no compile-time dependency)
        try
        {
            var tmproAsm = AppDomain.CurrentDomain.GetAssemblies()
                .FirstOrDefault(a => a.GetName().Name == "Unity.TextMeshPro");
            var tmproType = tmproAsm?.GetType("TMPro.TextMeshProUGUI");
            if (tmproType == null)
                return false;
            var getTexts = typeof(GameObject).GetMethod("GetComponentsInChildren",
                new[] { typeof(Type), typeof(bool) });
            var list = (IEnumerable)getTexts.Invoke(go, new object[] { tmproType, true });
            var textProp = tmproType.GetProperty("text");
            foreach (var c in list)
            {
                var txt = (string)textProp.GetValue(c, null);
                if (!string.IsNullOrWhiteSpace(txt) &&
                    txt.Trim().Equals(label, StringComparison.OrdinalIgnoreCase))
                    return true;
            }
        }
        catch (Exception e)
        {
            Plugin.Log.LogDebug($"[CONNECT] TMPro label scan failed: {e.Message}");
        }

        return false;
    }

    private static void CloneAsConnect(GameObject sourceGo)
    {
        var cloneObj = UnityEngine.Object.Instantiate(sourceGo);
        var cloneGo = cloneObj.TryCast<GameObject>();
        if (cloneGo == null)
        {
            Plugin.Log.LogWarning("[CONNECT] clone failed (not a GameObject)");
            return;
        }

        cloneGo.transform.SetParent(sourceGo.transform.parent, false);
        cloneGo.transform.SetSiblingIndex(sourceGo.transform.GetSiblingIndex() + 1);
        cloneGo.name = sourceGo.name + CloneNameSuffix;

        // Relabel the source label -> "Connect" (uGUI Text; TMPro fallback
        // via reflection).
        var sourceLabel = MatchSourceLabel(sourceGo) ?? MatchSourceLabel(cloneGo);
        if (sourceLabel != null)
            RelabelClone(cloneGo, sourceLabel, CloneLabel);
        else
            Plugin.Log.LogWarning("[CONNECT] no label Text found on the source tab — Connect tab keeps its label");

        // Replace the tab-switch click handler with the pairing overlay.
        ReplaceClickHandler(cloneGo);

        Plugin.Log.LogInfo($"[CONNECT] added Connect tab next to '{sourceGo.name}'");
    }

    private static void RelabelClone(GameObject cloneGo, string from, string to)
    {
        // uGUI path
        var textType = Il2CppSystem.Type.GetType(typeof(Text).AssemblyQualifiedName);
        var dstTexts = cloneGo.GetComponentsInChildren(textType, true);
        if (dstTexts != null)
        {
            foreach (var c in dstTexts)
            {
                var label = c.TryCast<Text>();
                if (label == null)
                    continue;
                if (!string.IsNullOrWhiteSpace(label.text) &&
                    label.text.Trim().Equals(from, StringComparison.OrdinalIgnoreCase))
                {
                    label.text = to;
                    return;
                }
            }
        }

        // TMPro fallback (no compile-time dependency)
        try
        {
            var tmproAsm = AppDomain.CurrentDomain.GetAssemblies()
                .FirstOrDefault(a => a.GetName().Name == "Unity.TextMeshPro");
            var tmproType = tmproAsm?.GetType("TMPro.TextMeshProUGUI");
            if (tmproType == null)
            {
                Plugin.Log.LogWarning("[CONNECT] no label Text found on the source tab — Connect tab keeps its label");
                return;
            }
            var getTexts = typeof(GameObject).GetMethod("GetComponentsInChildren",
                new[] { typeof(Type), typeof(bool) });
            var dst = (IEnumerable)getTexts.Invoke(cloneGo, new object[] { tmproType, true });
            var textProp = tmproType.GetProperty("text");
            foreach (var c in dst)
            {
                var txt = (string)textProp.GetValue(c, null);
                if (!string.IsNullOrWhiteSpace(txt) &&
                    txt.Trim().Equals(from, StringComparison.OrdinalIgnoreCase))
                {
                    textProp.SetValue(c, to, null);
                    return;
                }
            }
            Plugin.Log.LogWarning("[CONNECT] no matching label found on the clone — Connect tab keeps its label");
        }
        catch (Exception e)
        {
            Plugin.Log.LogWarning($"[CONNECT] TMPro relabel failed: {e.Message}");
        }
    }

    // THE CLICK HANDLER WIRING: drop the clone's tab-switch onClick
    // listeners, then wire our own fresh pairing-overlay handler. This is OUR
    // OWN delegate (never a wrapped game callback), converted via
    // DelegateSupport.ConvertDelegate exactly like Plugin.cs does for
    // SceneManager.sceneLoaded — so the v0.1.30 Delegate.CreateDelegate
    // hang cannot recur.
    private static void ReplaceClickHandler(GameObject cloneGo)
    {
        // be.788: GetComponent requires Il2CppSystem.Type, not System.Type.
        var buttonType = Il2CppSystem.Type.GetType(typeof(Button).AssemblyQualifiedName);
        var button = cloneGo.GetComponent(buttonType)?.TryCast<Button>();
        if (button == null)
        {
            // The clickable Button may be nested (RRUI composes buttons from parts).
            var buttons = cloneGo.GetComponentsInChildren(buttonType, true);
            if (buttons != null)
            {
                foreach (var b in buttons)
                {
                    button = b.TryCast<Button>();
                    if (button != null)
                        break;
                }
            }
        }
        if (button == null)
        {
            Plugin.Log.LogWarning("[CONNECT] no uGUI Button on the Connect clone — click handler not wired");
            return;
        }

        button.onClick.RemoveAllListeners();
        var connectAction = Il2CppInterop.Runtime.DelegateSupport.ConvertDelegate<UnityAction>(
            new Action(OnConnectClicked));
        button.onClick.AddListener(connectAction);
        Plugin.Log.LogInfo("[CONNECT] Connect tab wired to the Flux pairing overlay");
    }

    private static void OnConnectClicked()
    {
        try
        {
            Plugin.Log.LogInfo("[CONNECT] Connect tab pressed — opening the Flux pairing overlay");
            Cursor.visible = true;
            FluxPairingPatch.ShowOverlay();
        }
        catch (Exception e)
        {
            Plugin.Log.LogWarning($"[CONNECT] opening the pairing overlay failed: {e.Message}");
        }
    }
}
