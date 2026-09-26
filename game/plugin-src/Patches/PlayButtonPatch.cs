// Adds a "Play" tab to the Watch home tab row by cloning the Rooms tab and
// rewiring its click to Play navigation. v0.1.35 rewrite (retargeted).
//
// Why this shape (and why the old "Create button" hunt is gone):
//  - Research for v0.1.35 confirmed there is NO Create tab on the RRUI home
//    screen (the home tabs are Rooms/Clubs/Items/Inventions/Creators under
//    HomeTop5TabsModel, enum EKNOKKDNHFP) and the legacy createRoomButton is
//    a Button3D, not a uGUI Button — so hunting a "Create" uGUI Button could
//    never succeed. The old file targeted a button that does not exist.
//    There is deliberately NO "Create" label search anywhere in this file.
//  - The home screen is 100% code-built at runtime via RRUI (Rec Room's UI
//    framework) — there are no home-screen prefabs to edit. The plugin
//    clones the Rooms tab (same prefab, same style, same row), inserts the
//    clone as its next sibling and relabels it "Play". Visual consistency
//    with the game's design language is guaranteed because it is literally
//    the same tab GameObject. This mirrors FluxConnectButton's proven
//    clone + relabel + click-handler pattern.
//  - The clone's original onClick listeners (which open the Rooms tab) are
//    removed and replaced with a single listener wired through
//    DelegateSupport.ConvertDelegate — our OWN fresh handler, never a
//    wrapped game callback, so the v0.1.30 Delegate.CreateDelegate hang
//    cannot recur.
//  - The click handler calls the game's own Play navigation:
//    WatchUI.ShowScreenAndGoToPlay(bool) — public and unobfuscated in build
//    20230414, resolved at runtime by name (a compile-time reference would
//    break the build if the interop ever regenerates without it). The method
//    is resolved ONCE (static field) and only invoked from the click
//    handler, which runs on the Unity main thread. If the method is ever
//    missing, the click logs a one-time Warning with the nav-probe
//    diagnostics (nav enum ELCPGOKLPLO, Play = 8) instead of silently dying.
//
// Hard rules honored:
//  - The source tab itself is untouched: the plugin only ADDS a sibling.
//  - Idempotent: the clone is only added once per tab row (name check), so
//    scene reloads and repeated Apply() calls never duplicate it.
//  - The clone is named "<source>_FluxPlayTab_Play": it carries the
//    _FluxPlayTab identity AND ends with the "_Play" suffix that
//    HomeLabelsPatch already skips, so the "Play" label is never clobbered
//    regardless of Apply() ordering between the two patches (HomeLabelsPatch
//    is owned by another patch and is not edited from here).
//  - The tab is a pure navigation trigger: it never registers with
//    TabsModel<T>.GoToPage, so it cannot corrupt tab navigation state.
//
// Discovery is label-assisted but never label-dependent. PRIMARY: the Watch
// home tab row resolved via the unobfuscated HomeTop5TabsModel type (the
// same anchor HomeLabelsPatch uses) — prefer the child whose visible label
// reads "Rooms", else take the first active child carrying a uGUI Button
// (tabs are ordered Rooms=0, Clubs=1, Items=2, Inventions=3, Creators=4).
// FALLBACK: a visible-label "Rooms" search over all active uGUI Buttons,
// preferring buttons under a Home/Watch ancestor.
//
// IL2CPP rules: click handlers go through
// DelegateSupport.ConvertDelegate<UnityAction>(new Action(...)) — NEVER
// new UnityAction(...) or method-group construction; downcasts go through
// TryCast<T>(), never direct casts; GetComponent / GetComponentsInChildren
// require Il2CppSystem.Type; no IMGUI. Game methods are invoked through
// reflection (MethodInfo.Invoke), the same precedent PlusBuyDialog uses for
// WatchUI methods.
//
// One knob, see [Home] in the .cfg:
//   Enable Play Button -> THE FEATURE (default true).
using System;
using System.Collections;
using System.Collections.Generic;
using System.Linq;
using System.Reflection;
using UnityEngine;
using UnityEngine.Events;
using UnityEngine.UI;

namespace RecNetPlugin.Patches;

internal static class PlayButtonPatch
{
    // The clone's distinguishing name fragment. The full clone name is
    // "<sourceTabName>_FluxPlayTab_Play": the "_FluxPlayTab" part is this
    // feature's identity (mirrors the _FluxConnectTab pattern), and the
    // trailing "_Play" part is what HomeLabelsPatch already skips — so the
    // "Play" label survives HomeLabelsPatch's renames whichever patch runs
    // first.
    private const string CloneNameFragment = "_FluxPlayTab";
    private const string CloneNameSuffix = "_FluxPlayTab_Play";
    private const string ConnectCloneSuffix = "_FluxConnectTab";
    private const string CloneLabel = "Play";
    private const string RoomsLabel = "Rooms";

    // Retry budget is TIME-based, not attempt-based. The Watch home tab row
    // finishes building asynchronously inside the menu scene (post-login),
    // long after SceneManager.sceneLoaded has fired, so a fixed attempt
    // count burns out before the UI exists. Apply() is re-invoked on a
    // 2-second timer by UiDiscoveryRetry (plus every scene load) until
    // IsSettled.
    private static readonly TimeSpan MaxRetryTime = TimeSpan.FromMinutes(10);
    private static readonly TimeSpan ProgressLogInterval = TimeSpan.FromSeconds(60);

    private static int _attempts;
    private static bool _tabDone;
    private static bool _gaveUp;
    private static DateTime _firstAttemptUtc = DateTime.MinValue;
    private static DateTime _lastProgressLogUtc = DateTime.MinValue;

    // Cached Play-nav resolution. The nav call site is resolved lazily on
    // the FIRST click (never during Apply — the Watch may not exist yet and
    // reflection is not free), then reused for every later click.
    private static bool _navResolved;
    private static MethodInfo _showScreenAndGoToPlay; // WatchUI.ShowScreenAndGoToPlay(bool), or null
    private static bool _navIsStatic;                 // true if the resolved method is static
    private static string _navParamName;              // bool param name, for the log
    private static bool _navMissingLogged;            // one-time Warning when the method is absent
    private static string _navProbeSummary;           // diagnostic fallback when the method is absent

    // True once there is nothing left to do: feature disabled in config, the
    // Play tab was added, or the retry budget ran out. The UiDiscoveryRetry
    // driver stops ticking this patch once settled.
    internal static bool IsSettled =>
        !Plugin.EnablePlayButton.Value || _tabDone || _gaveUp;

    // Called from Plugin.Load, on each scene load, and on a 2-second timer
    // by UiDiscoveryRetry: retries the tab clone until the home tab row
    // exists (or the 10-minute time budget runs out).
    public static void Apply()
    {
        if (!Plugin.EnablePlayButton.Value)
            return;

        if (_tabDone || _gaveUp)
            return;

        if (_firstAttemptUtc == DateTime.MinValue)
        {
            _firstAttemptUtc = DateTime.UtcNow;
            Plugin.Log.LogInfo("[PLAY] looking for the Rooms tab on the home screen " +
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
            EnsurePlayTab();
        }
        catch (Exception e)
        {
            Plugin.Log.LogWarning($"[PLAY] attempt {_attempts} failed: {e.Message}");
        }

        MaybeLogProgress();
    }

    // Throttled progress report: the per-attempt "not found yet" notes stay
    // at Debug, but every 60 seconds a Warning summarizes what the search is
    // (not) finding so a user reading the log can see the patch is alive and
    // what the scene actually contains.
    private static void MaybeLogProgress()
    {
        if (_tabDone || _gaveUp)
            return;
        var now = DateTime.UtcNow;
        if (now - _lastProgressLogUtc < ProgressLogInterval)
            return;
        _lastProgressLogUtc = now;
        var elapsed = now - _firstAttemptUtc;
        Plugin.Log.LogWarning($"[PLAY] still looking for the Rooms tab " +
            $"(attempt {_attempts}, {elapsed.TotalSeconds:F0}s elapsed). {DescribeTabs()}");
    }

    // Final give-up: Warning level, states exactly what was searched for,
    // how many attempts ran, and what the scene actually contained.
    private static void LogGiveUp()
    {
        var elapsed = DateTime.UtcNow - _firstAttemptUtc;
        Plugin.Log.LogWarning("[PLAY] GAVE UP adding the Play tab after " +
            $"{_attempts} attempts over {elapsed.TotalMinutes:F1} minutes. " +
            "Searched: (1) the Watch home tab row via the unobfuscated HomeTop5TabsModel " +
            "type — first active child carrying a uGUI Button, preferring the child " +
            "whose visible label reads 'Rooms' (skipping our own _FluxPlayTab clone " +
            "and the _FluxConnectTab clone); " +
            "(2) every active uGUI Button in the scene for a child label 'Rooms' " +
            "(case-insensitive), preferring buttons under a Home/Watch ancestor. " +
            "Note: there is deliberately NO 'Create' button search — research " +
            "confirmed no Create tab exists on the RRUI home and the legacy " +
            "createRoomButton is a Button3D, not a uGUI Button. " +
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

    // Find the Rooms tab and clone it into a "Play" tab right beside it.
    private static void EnsurePlayTab()
    {
        var source = FindRoomsTab();
        if (source == null)
        {
            Plugin.Log.LogDebug("[PLAY] Rooms tab not found yet");
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

        // Already added? (a sibling already carrying our clone identity)
        for (int i = 0; i < parent.childCount; i++)
        {
            var child = parent.GetChild(i);
            if (child != null && IsOurClone(child.gameObject))
            {
                _tabDone = true;
                Plugin.Log.LogDebug("[PLAY] Play tab already present");
                return;
            }
        }

        if (CloneAsPlay(source))
            _tabDone = true;
    }

    // True when go is this feature's clone: its name contains the
    // _FluxPlayTab fragment (covers the full "<source>_FluxPlayTab_Play"
    // clone name). A stock game object will never contain this fragment.
    private static bool IsOurClone(GameObject go)
    {
        return go != null && (go.name ?? string.Empty).Contains(CloneNameFragment);
    }

    // Locate the Rooms tab. PRIMARY: the Watch home tab row resolved via the
    // unobfuscated HomeTop5TabsModel type (same anchor HomeLabelsPatch uses).
    // FALLBACK: a visible-label "Rooms" search over all active uGUI Buttons.
    private static GameObject FindRoomsTab()
    {
        var byModel = FindRoomsTabByModel();
        if (byModel != null)
            return byModel;
        return FindRoomsTabByLabel();
    }

    // Model-based tab discovery: find the Watch home tab row through the
    // unobfuscated HomeTop5TabsModel type. Label-assisted, not
    // label-dependent: prefer the child whose visible label reads "Rooms",
    // else take the first active child carrying a uGUI Button (the tabs are
    // ordered Rooms=0, Clubs=1, Items=2, Inventions=3, Creators=4 per
    // EKNOKKDNHFP). Never clones our own Play tab or the Connect tab.
    private static GameObject FindRoomsTabByModel()
    {
        var modelType = FindTypeByName("HomeTop5TabsModel");
        if (modelType == null)
        {
            Plugin.Log.LogDebug("[PLAY] HomeTop5TabsModel type not found yet");
            return null;
        }
        var row = FirstGameObjectOfType(modelType)?.transform;
        if (row == null)
        {
            Plugin.Log.LogDebug("[PLAY] HomeTop5TabsModel instance not in the scene yet");
            return null;
        }

        GameObject firstButtonChild = null;
        for (int i = 0; i < row.childCount; i++)
        {
            var child = row.GetChild(i);
            if (child == null)
                continue;
            var go = child.gameObject;
            if (go == null || !go.activeInHierarchy)
                continue;
            // Never clone our own Play tab or the Connect tab (both live in
            // the same row).
            if (IsOurClone(go) ||
                (go.name ?? string.Empty).EndsWith(ConnectCloneSuffix, StringComparison.Ordinal))
                continue;
            if (!HasButton(go))
                continue;
            if (firstButtonChild == null)
                firstButtonChild = go;
            var label = ReadFirstLabel(go);
            if (!string.IsNullOrEmpty(label) &&
                label.Equals(RoomsLabel, StringComparison.OrdinalIgnoreCase))
            {
                Plugin.Log.LogInfo($"[PLAY] found Rooms tab via HomeTop5TabsModel (label match): '{go.name}'");
                return go;
            }
        }

        if (firstButtonChild != null)
        {
            Plugin.Log.LogInfo("[PLAY] found Rooms tab via HomeTop5TabsModel (first tab button, " +
                $"label='{ReadFirstLabel(firstButtonChild) ?? "<none>"}'): '{firstButtonChild.name}'");
            return firstButtonChild;
        }

        Plugin.Log.LogDebug($"[PLAY] HomeTop5TabsModel row '{row.name}' found but no tab button child yet");
        return null;
    }

    // Fallback: any active uGUI Button whose child label reads "Rooms",
    // preferring buttons under a Home/Watch ancestor (the Watch tab row)
    // over same-labeled buttons in other menus.
    private static GameObject FindRoomsTabByLabel()
    {
        var find = typeof(UnityEngine.Object).GetMethod("FindObjectsOfType",
            new[] { typeof(Type) });
        if (find == null)
            return null;

        var all = (IEnumerable)find.Invoke(null, new object[] { typeof(Button) });
        if (all == null)
            return null;

        GameObject best = null;
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
            if (IsOurClone(go))
                continue;
            if (!HasLabel(go, RoomsLabel))
                continue;

            bool isHome = IsUnderHome(go);
            if (best == null || (isHome && !bestIsHome))
            {
                best = go;
                bestIsHome = isHome;
                if (isHome)
                    break; // Watch/home Rooms is the one we want
            }
        }

        if (best != null)
            Plugin.Log.LogInfo($"[PLAY] found Rooms tab by label: '{best.name}'");
        return best;
    }

    private static bool HasButton(GameObject go)
    {
        // be.788: GetComponent requires Il2CppSystem.Type, not System.Type.
        var btnType = Il2CppSystem.Type.GetType(typeof(Button).AssemblyQualifiedName);
        try { return go.GetComponent(btnType) != null; }
        catch { return false; }
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
            Plugin.Log.LogDebug($"[PLAY] TMPro label scan failed: {e.Message}");
        }

        return false;
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

    // Clone the Rooms tab into the Play tab. Returns true when the clone was
    // placed (idempotency is handled by the caller).
    private static bool CloneAsPlay(GameObject sourceGo)
    {
        var cloneObj = UnityEngine.Object.Instantiate(sourceGo);
        var cloneGo = cloneObj.TryCast<GameObject>();
        if (cloneGo == null)
        {
            Plugin.Log.LogWarning("[PLAY] clone failed (not a GameObject)");
            return false;
        }

        cloneGo.transform.SetParent(sourceGo.transform.parent, false);
        cloneGo.transform.SetSiblingIndex(sourceGo.transform.GetSiblingIndex() + 1);
        // _FluxPlayTab identity + trailing _Play so HomeLabelsPatch's
        // existing suffix skip leaves the "Play" label alone.
        cloneGo.name = sourceGo.name + CloneNameSuffix;

        // Relabel the source label -> "Play" (uGUI Text; TMPro fallback via
        // reflection).
        var sourceLabel = ReadFirstLabel(sourceGo);
        if (!string.IsNullOrEmpty(sourceLabel))
            RelabelClone(cloneGo, sourceLabel, CloneLabel);
        else
            Plugin.Log.LogWarning("[PLAY] no label Text found on the source tab — Play tab keeps its label");

        // Replace the tab-switch click handler with Play navigation.
        ReplaceClickHandler(cloneGo);

        Plugin.Log.LogInfo($"[PLAY] added Play tab '{cloneGo.name}' next to '{sourceGo.name}' " +
            $"(parent '{sourceGo.transform.parent?.name}', sibling index {cloneGo.transform.GetSiblingIndex()}, " +
            $"active={cloneGo.activeInHierarchy}, label='{ReadFirstLabel(cloneGo) ?? "<none>"}')");
        return true;
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
                Plugin.Log.LogWarning("[PLAY] no label Text found on the Play tab — Play tab keeps its label");
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
            Plugin.Log.LogWarning("[PLAY] no matching label found on the clone — Play tab keeps its label");
        }
        catch (Exception e)
        {
            Plugin.Log.LogWarning($"[PLAY] TMPro relabel failed: {e.Message}");
        }
    }

    // THE CLICK HANDLER WIRING: drop the clone's tab-switch onClick
    // listeners, then wire our own fresh Play-navigation handler. This is OUR
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
            Plugin.Log.LogWarning("[PLAY] no uGUI Button on the Play clone — click handler not wired");
            return;
        }

        button.onClick.RemoveAllListeners();
        var playAction = Il2CppInterop.Runtime.DelegateSupport.ConvertDelegate<UnityAction>(
            new Action(OnPlayClicked));
        button.onClick.AddListener(playAction);
        Plugin.Log.LogInfo("[PLAY] Play tab wired to WatchUI.ShowScreenAndGoToPlay");
    }

    private static void OnPlayClicked()
    {
        try
        {
            Plugin.Log.LogInfo("[PLAY] Play tab pressed");
            if (TryPlayNavigation())
                return;
            Plugin.Log.LogWarning("[PLAY] Play pressed but the nav call did not fire — see diagnostics above");
        }
        catch (Exception e)
        {
            Plugin.Log.LogWarning($"[PLAY] Play click handler failed: {e.Message}");
        }
    }

    // Play navigation. Returns true when the game's own nav call was issued,
    // false when the nav API could not be resolved (caller logs the safe
    // fallback message).
    //
    // The call site: WatchUI.ShowScreenAndGoToPlay(bool) — public and
    // unobfuscated in build 20230414 per the v0.1.35 research. The MethodInfo
    // is resolved lazily on the FIRST click (never during Apply: the Watch
    // may not exist yet and reflection is not free) and cached for every
    // later click. The invoke itself is side-effectful only in the sense
    // the game does its own Play navigation — nothing else is touched.
    private static bool TryPlayNavigation()
    {
        if (!_navResolved)
        {
            _navResolved = true;
            try
            {
                _showScreenAndGoToPlay = ResolveShowScreenAndGoToPlay();
                if (_showScreenAndGoToPlay != null)
                {
                    Plugin.Log.LogInfo("[PLAY] nav resolved: " +
                        $"WatchUI.ShowScreenAndGoToPlay(bool {_navParamName}) " +
                        $"({_navIsStatic ? "static" : "instance"})");
                }
                else
                {
                    _navProbeSummary = ProbePlayNavApi();
                }
            }
            catch (Exception e)
            {
                Plugin.Log.LogWarning($"[PLAY] nav resolution failed: {e.Message}");
            }
        }

        if (_showScreenAndGoToPlay == null)
        {
            // Research says this method exists and is unobfuscated, so its
            // absence is a genuine surprise — say it ONCE, with the probe
            // evidence researchers need to find the real call site.
            if (!_navMissingLogged)
            {
                _navMissingLogged = true;
                Plugin.Log.LogWarning("[PLAY] WatchUI.ShowScreenAndGoToPlay(bool) NOT found — " +
                    "Play nav unwired. Probe evidence: " + _navProbeSummary);
            }
            return false;
        }

        try
        {
            object target = null;
            if (!_navIsStatic)
            {
                target = GetWatchUILocal();
                if (target == null)
                {
                    Plugin.Log.LogWarning("[PLAY] WatchUI.get_Local() returned null — nav call skipped");
                    return false;
                }
            }
            _showScreenAndGoToPlay.Invoke(target, new object[] { true });
            Plugin.Log.LogInfo("[PLAY] invoked WatchUI.ShowScreenAndGoToPlay(true)");
            return true;
        }
        catch (Exception e)
        {
            Plugin.Log.LogWarning($"[PLAY] nav invoke failed: {e.Message}");
            return false;
        }
    }

    // Resolve WatchUI.ShowScreenAndGoToPlay(bool) by name at runtime.
    // Returns null when the type or method is absent. Prefers a public
    // instance method, accepts public/non-public, static or instance, as
    // long as it takes exactly one bool parameter.
    private static MethodInfo ResolveShowScreenAndGoToPlay()
    {
        var watchType = FindTypeByName("WatchUI");
        if (watchType == null)
        {
            Plugin.Log.LogDebug("[PLAY] WatchUI type not found during nav resolution");
            return null;
        }

        MethodInfo fallback = null;
        bool fallbackIsStatic = false;
        foreach (var m in watchType.GetMethods(BindingFlags.Public | BindingFlags.NonPublic |
            BindingFlags.Static | BindingFlags.Instance))
        {
            if (m == null || !m.Name.Equals("ShowScreenAndGoToPlay", StringComparison.Ordinal))
                continue;
            var ps = m.GetParameters();
            if (ps.Length != 1 || ps[0].ParameterType != typeof(bool))
            {
                Plugin.Log.LogDebug("[PLAY] ShowScreenAndGoToPlay overload skipped " +
                    $"(params: [{string.Join(", ", ps.Select(p => p.ParameterType.Name))}])");
                continue;
            }

            bool isPublicInstance = m.IsPublic && !m.IsStatic;
            if (fallback == null)
            {
                fallback = m;
                fallbackIsStatic = m.IsStatic;
            }
            if (isPublicInstance)
            {
                _navIsStatic = false;
                _navParamName = ps[0].Name;
                return m;
            }
        }

        if (fallback != null)
        {
            _navIsStatic = fallbackIsStatic;
            _navParamName = fallback.GetParameters()[0].Name;
        }
        return fallback;
    }

    // WatchUI.get_Local() via reflection (same pattern PlusBuyDialog uses).
    // The result is passed straight into MethodInfo.Invoke — no game type is
    // ever referenced at compile time.
    private static object GetWatchUILocal()
    {
        var watchType = FindTypeByName("WatchUI");
        if (watchType == null)
            return null;
        var getLocal = watchType.GetMethod("get_Local",
            BindingFlags.Public | BindingFlags.NonPublic | BindingFlags.Static);
        if (getLocal == null || getLocal.GetParameters().Length != 0)
            return null;
        try { return getLocal.Invoke(null, null); }
        catch { return null; }
    }

    // Side-effect-free discovery fallback: find the nav enum the v0.1.35
    // research named (ELCPGOKLPLO, Play = 8) and any type exposing a
    // page-navigation method that could take it. NOTHING is invoked — log
    // evidence for the researchers only. Only runs when
    // ShowScreenAndGoToPlay was not found.
    private static string ProbePlayNavApi()
    {
        Type navEnumType = null;
        string playMember = null;
        var candidateMethods = new List<string>();

        foreach (var asm in AppDomain.CurrentDomain.GetAssemblies())
        {
            Type[] types;
            try { types = asm.GetTypes(); }
            catch (ReflectionTypeLoadException e) { types = e.Types.Where(t => t != null).ToArray(); }
            catch { continue; }

            foreach (var t in types)
            {
                if (t == null || navEnumType != null)
                    continue;
                try
                {
                    if (!t.IsEnum)
                        continue;
                    if (t.Name.IndexOf("ELCPGOKLPLO", StringComparison.OrdinalIgnoreCase) >= 0 ||
                        Enum.GetNames(t).Any(n => n.Equals("Play", StringComparison.OrdinalIgnoreCase)))
                    {
                        navEnumType = t;
                        foreach (var name in Enum.GetNames(t))
                        {
                            if (name.Equals("Play", StringComparison.OrdinalIgnoreCase))
                            {
                                playMember = name + "=" + Convert.ToInt32(Enum.Parse(t, name));
                                break;
                            }
                        }
                    }
                }
                catch
                {
                    // Not a usable enum under IL2CPP reflection — keep scanning.
                }
            }
            if (navEnumType != null)
                break;
        }

        if (navEnumType != null)
        {
            // Look for a type with a plausible page-nav method taking the enum.
            foreach (var asm in AppDomain.CurrentDomain.GetAssemblies())
            {
                Type[] types;
                try { types = asm.GetTypes(); }
                catch (ReflectionTypeLoadException e) { types = e.Types.Where(t => t != null).ToArray(); }
                catch { continue; }

                foreach (var t in types)
                {
                    if (t == null || candidateMethods.Count >= 10)
                        break;
                    MethodInfo[] methods;
                    try
                    {
                        methods = t.GetMethods(BindingFlags.Public | BindingFlags.NonPublic |
                            BindingFlags.Static | BindingFlags.Instance);
                    }
                    catch { continue; }

                    foreach (var m in methods)
                    {
                        var ps = m.GetParameters();
                        if (ps.Length != 1 || ps[0].ParameterType != navEnumType)
                            continue;
                        var mn = m.Name ?? string.Empty;
                        if (mn.IndexOf("GoTo", StringComparison.OrdinalIgnoreCase) >= 0 ||
                            mn.IndexOf("Navigate", StringComparison.OrdinalIgnoreCase) >= 0 ||
                            mn.IndexOf("ShowPage", StringComparison.OrdinalIgnoreCase) >= 0 ||
                            mn.IndexOf("OpenPage", StringComparison.OrdinalIgnoreCase) >= 0)
                        {
                            candidateMethods.Add($"{t.FullName}.{m.Name}({navEnumType.Name})");
                            if (candidateMethods.Count >= 10)
                                break;
                        }
                    }
                }
                if (candidateMethods.Count >= 10)
                    break;
            }
        }

        return "nav enum: " + (navEnumType == null
            ? "not found"
            : navEnumType.FullName + " (" + (playMember ?? "no Play member") + ")") +
            "; candidate nav methods: [" + string.Join(", ", candidateMethods) + "].";
    }
}
