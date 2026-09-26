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
using System.Collections.Generic;
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

    // Retry budget is TIME-based, not attempt-based. The Watch home tab row
    // finishes building asynchronously inside the menu scene (post-login),
    // long after SceneManager.sceneLoaded has fired, so a fixed attempt
    // count burns out before the UI exists. Apply() is re-invoked on a
    // 2-second timer by UiDiscoveryRetry (plus every scene load) until
    // IsSettled.
    private static readonly TimeSpan MaxRetryTime = TimeSpan.FromMinutes(10);
    private static readonly TimeSpan ProgressLogInterval = TimeSpan.FromSeconds(60);

    private static int _attempts;
    private static bool _buttonDone;
    private static bool _gaveUp;
    private static DateTime _firstAttemptUtc = DateTime.MinValue;
    private static DateTime _lastProgressLogUtc = DateTime.MinValue;

    // True once there is nothing left to do: feature disabled in config, the
    // Connect tab was added, or the retry budget ran out. The
    // UiDiscoveryRetry driver stops ticking this patch once settled.
    internal static bool IsSettled =>
        !Plugin.EnableFluxPairing.Value || _buttonDone || _gaveUp;

    // Visible labels to try as the clone source, in preference order: the
    // Watch home tab row first, then the legacy home screen's Create button.
    private static readonly string[] SourceLabels =
    {
        "Rooms", "Clubs", "Items", "Inventions", "Creators", "Create"
    };

    // Called from Plugin.Load, on each scene load, and on a 2-second timer
    // by UiDiscoveryRetry: retries the tab clone until the home tab row
    // exists (or the 10-minute time budget runs out).
    public static void Apply()
    {
        if (!Plugin.EnableFluxPairing.Value)
            return;

        if (_buttonDone || _gaveUp)
            return;

        if (_firstAttemptUtc == DateTime.MinValue)
        {
            _firstAttemptUtc = DateTime.UtcNow;
            Plugin.Log.LogInfo("[CONNECT] looking for the Watch home tab row " +
                $"(retry budget {MaxRetryTime.TotalMinutes:F0} minutes)");
        }

        if (DateTime.UtcNow - _firstAttemptUtc >= MaxRetryTime)
        {
            _gaveUp = true;
            LogGiveUp();
            return;
        }

        _attempts++;
        try
        {
            EnsureConnectTab();
        }
        catch (Exception e)
        {
            Plugin.Log.LogWarning($"[CONNECT] attempt {_attempts} failed: {e.Message}");
        }

        MaybeLogProgress();
    }

    // Throttled progress report: the per-attempt "not found yet" notes stay
    // at Debug, but every 60 seconds a Warning summarizes what the search is
    // (not) finding so a user reading the log can see the patch is alive and
    // what the scene actually contains.
    private static void MaybeLogProgress()
    {
        if (_buttonDone || _gaveUp)
            return;
        var now = DateTime.UtcNow;
        if (now - _lastProgressLogUtc < ProgressLogInterval)
            return;
        _lastProgressLogUtc = now;
        var elapsed = now - _firstAttemptUtc;
        Plugin.Log.LogWarning($"[CONNECT] still looking for the Watch home tab row " +
            $"(attempt {_attempts}, {elapsed.TotalSeconds:F0}s elapsed). {DescribeTabs()}");
    }

    // Final give-up: Warning level, states exactly what was searched for,
    // how many attempts ran, and what the scene actually contained.
    private static void LogGiveUp()
    {
        var elapsed = DateTime.UtcNow - _firstAttemptUtc;
        Plugin.Log.LogWarning("[CONNECT] GAVE UP adding the Connect tab after " +
            $"{_attempts} attempts over {elapsed.TotalMinutes:F1} minutes. " +
            "Searched: (1) the Watch home tab row via the unobfuscated HomeTop5TabsModel " +
            "type (first child carrying a uGUI Button becomes the clone source); " +
            "(2) every active uGUI Button for a child label matching one of " +
            "'Rooms'/'Clubs'/'Items'/'Inventions'/'Creators'/'Create' (case-insensitive). " +
            "Scene contents at give-up: " + DescribeTabs());
    }

    // Diagnostic snapshot: is the HomeTop5TabsModel type/instance present,
    // what does the tab row contain, and which button labels exist at all.
    private static string DescribeTabs()
    {
        try
        {
            var modelType = FindTypeByName("HomeTop5TabsModel");
            if (modelType == null)
                return "HomeTop5TabsModel type not found in loaded assemblies; " + DescribeSceneLabels();

            var row = FirstGameObjectOfType(modelType)?.transform;
            if (row == null)
                return "HomeTop5TabsModel type present but no live instance in the scene; " + DescribeSceneLabels();

            var children = new List<string>();
            int buttons = 0;
            for (int i = 0; i < row.childCount && children.Count < 25; i++)
            {
                var child = row.GetChild(i);
                if (child == null)
                    continue;
                var go = child.gameObject;
                if (go == null)
                    continue;
                var label = ReadFirstLabel(go);
                children.Add($"'{go.name}' label='{label ?? "<none>"}' active={go.activeInHierarchy}");
                if (HasButton(go))
                    buttons++;
            }
            return $"HomeTop5TabsModel row '{row.name}' found with {row.childCount} children " +
                $"({buttons} carrying uGUI Buttons): [{string.Join(", ", children)}]; " + DescribeSceneLabels();
        }
        catch (Exception e)
        {
            return $"tab scan failed: {e.Message}";
        }
    }

    // How many uGUI Buttons exist at all, and which distinct text labels were
    // seen — tells us whether label-based discovery ever had a chance.
    private static string DescribeSceneLabels()
    {
        try
        {
            var find = typeof(UnityEngine.Object).GetMethod("FindObjectsOfType",
                new[] { typeof(Type) });
            if (find == null)
                return "FindObjectsOfType(Type) not available via reflection.";
            var all = (IEnumerable)find.Invoke(null, new object[] { typeof(Button) });
            if (all == null)
                return "button scan returned null.";

            int total = 0;
            var labels = new HashSet<string>();
            foreach (var o in all)
            {
                var button = ((UnityEngine.Object)o).TryCast<Button>();
                if (button == null || button.gameObject == null)
                    continue;
                total++;
                var label = ReadFirstLabel(button.gameObject);
                if (!string.IsNullOrEmpty(label) && labels.Count < 25)
                    labels.Add(label);
            }
            return $"scene has {total} uGUI Buttons; distinct labels seen: [{string.Join(", ", labels)}].";
        }
        catch (Exception e)
        {
            return $"label scan failed: {e.Message}";
        }
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
            // Structural mismatch: the tab was found but has no row to clone
            // into. Retrying won't fix this — give up loudly with the full
            // diagnostic snapshot so the log shows what went wrong.
            _gaveUp = true;
            LogGiveUp();
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

    // Locate a source tab button. PRIMARY: the Watch home tab row resolved
    // via the unobfuscated HomeTop5TabsModel type (same anchor
    // HomeLabelsPatch uses) — label-independent, so it works when the tabs
    // are icon-only or when HomeLabelsPatch has already renamed the tab
    // labels to the configured values. FALLBACK: the visible-label search.
    private static GameObject FindSourceTab()
    {
        var byModel = FindSourceTabByModel();
        if (byModel != null)
            return byModel;

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

    // Model-based tab discovery: find the Watch home tab row through the
    // unobfuscated HomeTop5TabsModel type and take the first child carrying
    // a uGUI Button as the clone source. Works regardless of what the tab
    // labels currently read (icon-only tabs, or labels already rewritten by
    // HomeLabelsPatch).
    private static GameObject FindSourceTabByModel()
    {
        var modelType = FindTypeByName("HomeTop5TabsModel");
        if (modelType == null)
            return null;
        var row = FirstGameObjectOfType(modelType)?.transform;
        if (row == null)
            return null;

        for (int i = 0; i < row.childCount; i++)
        {
            var child = row.GetChild(i);
            if (child == null)
                continue;
            var go = child.gameObject;
            if (go == null || !go.activeInHierarchy)
                continue;
            // Skip our own clone if it somehow exists without the suffix check.
            if (go.name.EndsWith(CloneNameSuffix, StringComparison.Ordinal))
                continue;
            if (!HasButton(go))
                continue;
            Plugin.Log.LogInfo($"[CONNECT] found source tab via HomeTop5TabsModel: '{go.name}' " +
                $"(label='{ReadFirstLabel(go) ?? "<none>"}')");
            return go;
        }
        return null;
    }

    private static bool HasButton(GameObject go)
    {
        // be.788: GetComponent requires Il2CppSystem.Type, not System.Type.
        var btnType = Il2CppSystem.Type.GetType(typeof(Button).AssemblyQualifiedName);
        try { return go.GetComponent(btnType) != null; }
        catch { return false; }
    }

    private static Type FindTypeByName(string name)
    {
        foreach (var asm in AppDomain.CurrentDomain.GetAssemblies())
        {
            Type[] types;
            try { types = asm.GetTypes(); }
            catch (ReflectionTypeLoadException e) { types = e.Types.Where(t => t != null).ToArray(); }
            catch { continue; }

            foreach (var t in types)
            {
                if (t != null && t.Name == name)
                    return t;
            }
        }
        return null;
    }

    private static GameObject FirstGameObjectOfType(Type type)
    {
        var find = typeof(UnityEngine.Object).GetMethod("FindObjectsOfType",
            new[] { typeof(Type) });
        if (find == null)
            return null;

        var all = find.Invoke(null, new object[] { type }) as IEnumerable;
        if (all == null)
            return null;

        foreach (var o in all)
        {
            var comp = ((UnityEngine.Object)o).TryCast<Component>();
            var go = comp != null ? comp.gameObject : null;
            if (go != null)
                return go;
        }
        return null;
    }

    // First non-empty visible label under go (uGUI Text, then TMPro via
    // reflection), or null. Shared by the diagnostics and the clone log.
    private static string ReadFirstLabel(GameObject go)
    {
        try
        {
            var textType = Il2CppSystem.Type.GetType(typeof(Text).AssemblyQualifiedName);
            var texts = go.GetComponentsInChildren(textType, true);
            if (texts != null)
            {
                foreach (var c in texts)
                {
                    var txt = ((UnityEngine.Object)c).TryCast<Text>()?.text;
                    if (!string.IsNullOrWhiteSpace(txt))
                        return txt.Trim();
                }
            }
        }
        catch
        {
            // fall through to TMPro
        }

        try
        {
            var tmproAsm = AppDomain.CurrentDomain.GetAssemblies()
                .FirstOrDefault(a => a.GetName().Name == "Unity.TextMeshPro");
            var tmproType = tmproAsm?.GetType("TMPro.TextMeshProUGUI");
            if (tmproType == null)
                return null;
            var getTexts = typeof(GameObject).GetMethod("GetComponentsInChildren",
                new[] { typeof(Type), typeof(bool) });
            var list = (IEnumerable)getTexts.Invoke(go, new object[] { tmproType, true });
            var textProp = tmproType.GetProperty("text");
            foreach (var c in list)
            {
                var txt = (string)textProp.GetValue(c, null);
                if (!string.IsNullOrWhiteSpace(txt))
                    return txt.Trim();
            }
        }
        catch
        {
            // no label readable
        }
        return null;
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

        Plugin.Log.LogInfo($"[CONNECT] added Connect tab '{cloneGo.name}' next to '{sourceGo.name}' " +
            $"(parent '{sourceGo.transform.parent?.name}', sibling index {cloneGo.transform.GetSiblingIndex()}, " +
            $"active={cloneGo.activeInHierarchy}, source label='{sourceLabel ?? "<none>"}')");
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
