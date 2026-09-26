// Adds a "Play" button beside "Create" on the home screen and wires it to
// quick play (join a random public room). This file implements the
// CLONE + RELABEL + CLICK-HANDLER steps.
//
// Why this shape:
//  - The home screen is 100% code-built at runtime via RRUI (Rec Room's UI
//    framework) — there are no home-screen prefabs to edit. The tab buttons
//    follow an icon + label pattern (TabButtonImpl under HomeTop5TabsModel,
//    or GoToCreateRoomScreenButtonController on the legacy home screen).
//  - Type/GameObject names on the home screen may be obfuscated and re-rolled
//    per build, so the plugin resolves the Create button by its VISIBLE
//    LABEL ("Create") on a uGUI Button, not by GameObject/type name.
//  - The plugin clones the Create button (same prefab, same style, same row),
//    inserts the clone as its next sibling and relabels it "Play". Visual
//    consistency with the game's design language is guaranteed because it is
//    literally the same button GameObject.
//  - The clone's original onClick listeners (which open the room-creation
//    flow) are removed and replaced with a single listener that triggers
//    Quick Play. The listener is OUR OWN fresh handler wired through
//    DelegateSupport.ConvertDelegate — we never wrap the game's callbacks,
//    so the v0.1.30 Delegate.CreateDelegate hang cannot recur.
//
// Hard rules honored:
//  - The Create button itself is untouched: the plugin only ADDS a sibling.
//  - Idempotent: the clone is only added once per Create button (name-suffix
//    check), so scene reloads and repeated Apply() calls never duplicate it.
//
// Resolution is by visible label at runtime (never by obfuscated type name).
// IL2CPP rules: downcasts go through TryCast<T>(), GetComponent /
// GetComponentsInChildren require Il2CppSystem.Type, never direct casts.
//
// One knob, see [Home] in the .cfg:
//   Enable Play Button -> THE FEATURE (default true).
using System;
using System.Collections;
using System.Collections.Generic;
using System.Linq;
using System.Reflection;
using HarmonyLib;
using UnityEngine;
using UnityEngine.Events;
using UnityEngine.UI;

namespace RecNetPlugin.Patches;

internal static class PlayButtonPatch
{
    private const string CloneNameSuffix = "_Play";
    private const string SourceLabel = "Create";
    private const string CloneLabel = "Play";

    // Retry budget is TIME-based, not attempt-based. The Watch home screen
    // finishes building asynchronously inside the menu scene (post-login),
    // long after SceneManager.sceneLoaded has fired, so a fixed attempt
    // count burns out before the UI exists. Apply() is re-invoked on a
    // 2-second timer by UiDiscoveryRetry (plus every scene load) until
    // IsSettled.
    private static readonly TimeSpan MaxRetryTime = TimeSpan.FromMinutes(10);
    private static readonly TimeSpan ProgressLogInterval = TimeSpan.FromSeconds(60);
    // The assembly type scan (FindByCreateButtonType) is the expensive
    // discovery step — throttle it so the 2-second retry driver can't hitch
    // the game with repeated GetTypes() walks.
    private static readonly TimeSpan DeepScanInterval = TimeSpan.FromSeconds(30);

    private static int _attempts;
    private static bool _buttonDone;
    private static bool _gaveUp;
    private static DateTime _firstAttemptUtc = DateTime.MinValue;
    private static DateTime _lastProgressLogUtc = DateTime.MinValue;
    private static DateTime _lastDeepScanUtc = DateTime.MinValue;

    // True once there is nothing left to do: feature disabled in config, the
    // Play button was added, or the retry budget ran out. The
    // UiDiscoveryRetry driver stops ticking this patch once settled.
    internal static bool IsSettled =>
        !Plugin.EnablePlayButton.Value || _buttonDone || _gaveUp;

    // Called from Plugin.Load, on each scene load, and on a 2-second timer
    // by UiDiscoveryRetry: retries the button clone until the home screen
    // exists (or the 10-minute time budget runs out).
    public static void Apply()
    {
        if (!Plugin.EnablePlayButton.Value)
            return;

        if (_buttonDone || _gaveUp)
            return;

        if (_firstAttemptUtc == DateTime.MinValue)
        {
            _firstAttemptUtc = DateTime.UtcNow;
            Plugin.Log.LogInfo("[PLAY] looking for the Create button on the home screen " +
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
            EnsurePlayButton();
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
        if (_buttonDone || _gaveUp)
            return;
        var now = DateTime.UtcNow;
        if (now - _lastProgressLogUtc < ProgressLogInterval)
            return;
        _lastProgressLogUtc = now;
        var elapsed = now - _firstAttemptUtc;
        Plugin.Log.LogWarning($"[PLAY] still looking for the Create button " +
            $"(attempt {_attempts}, {elapsed.TotalSeconds:F0}s elapsed). {DescribeSceneButtons()}");
    }

    // Final give-up: Warning level, states exactly what was searched for,
    // how many attempts ran, and what the scene actually contained.
    private static void LogGiveUp()
    {
        var elapsed = DateTime.UtcNow - _firstAttemptUtc;
        Plugin.Log.LogWarning("[PLAY] GAVE UP adding the Play button after " +
            $"{_attempts} attempts over {elapsed.TotalMinutes:F1} minutes. " +
            "Searched: (1) every active uGUI Button for a child label exactly 'Create' " +
            "(case-insensitive), preferring buttons under a 'Home'-named ancestor; " +
            "(2) the Watch home hierarchy (anchored on the HomeTop5TabsModel type) for a " +
            "button with 'create' in its GameObject name (the home icon row is icon-only); " +
            "(3) GameObject name hints (create/button_create/createbutton/...); " +
            "(4) component types named *Create*Button*/*Create*Tab*. " +
            "Scene contents at give-up: " + DescribeSceneButtons());
    }

    // Diagnostic snapshot of the scene's buttons: how many exist, how many
    // carry text, which distinct labels were seen, whether the home model
    // type is present, and which buttons live under a Home ancestor.
    private static string DescribeSceneButtons()
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

            int total = 0, withLabels = 0;
            var labels = new HashSet<string>();
            var homeButtons = new List<string>();
            foreach (var o in all)
            {
                var button = ((UnityEngine.Object)o).TryCast<Button>();
                if (button == null)
                    continue;
                var go = button.gameObject;
                if (go == null)
                    continue;
                total++;
                var label = ReadFirstLabel(go);
                if (!string.IsNullOrEmpty(label))
                {
                    withLabels++;
                    if (labels.Count < 25)
                        labels.Add(label);
                }
                if (IsUnderHome(go) && homeButtons.Count < 25)
                    homeButtons.Add($"'{go.name}' label='{label ?? "<none>"}'");
            }

            var modelFound = FindTypeByName("HomeTop5TabsModel") != null;
            return $"scanned {total} uGUI Buttons ({withLabels} with a text label); " +
                $"distinct labels: [{string.Join(", ", labels)}]; " +
                $"HomeTop5TabsModel type present: {modelFound}; " +
                $"buttons under a Home-named ancestor: [{string.Join(", ", homeButtons)}].";
        }
        catch (Exception e)
        {
            return $"button scan failed: {e.Message}";
        }
    }

    // Find the Create button by its visible label and clone it into a Play
    // button right beside it.
    private static void EnsurePlayButton()
    {
        var source = FindCreateButton();
        if (source == null)
        {
            Plugin.Log.LogDebug("[PLAY] Create button not found yet");
            return;
        }

        var parent = source.transform.parent;
        if (parent == null)
        {
            // Structural mismatch: the button was found but has no row to
            // clone into. Retrying won't fix this — give up loudly with the
            // full diagnostic snapshot so the log shows what went wrong.
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
                Plugin.Log.LogDebug("[PLAY] Play button already present");
                return;
            }
        }

        CloneAsPlay(source);
        _buttonDone = true;
    }

    // Locate the Create button. Tries the visible-label search first (most
    // robust against obfuscated GameObject/type names), then falls back to
    // GameObject-name hints and component-type resolution.
    private static GameObject FindCreateButton()
    {
        var byLabel = FindByVisibleLabel();
        if (byLabel != null)
            return byLabel;
        // The Watch home's Create entry is an ICON in the home icon row
        // (Create, Store, Events, …) and may carry no text label at all —
        // find it by position in the home hierarchy instead of by label.
        var inHome = FindCreateInHomeHierarchy();
        if (inHome != null)
            return inHome;
        var byName = FindByNameHints();
        if (byName != null)
            return byName;
        // Deepest fallback (assembly type scan) is throttled: the cheap
        // label/hierarchy/name searches run every tick, this one at most
        // every DeepScanInterval.
        if (DeepScanDue())
            return FindByCreateButtonType();
        return null;
    }

    // True at most once per DeepScanInterval — gates the expensive
    // assembly-wide type scan so the 2-second retry driver can't hitch.
    private static bool DeepScanDue()
    {
        var now = DateTime.UtcNow;
        if (now - _lastDeepScanUtc < DeepScanInterval)
            return false;
        _lastDeepScanUtc = now;
        return true;
    }

    // Locate the Create button: any uGUI Button in the active scene whose
    // child label text is exactly "Create". Prefers candidates living under a
    // "Home"-named ancestor (the RRUI home screen) over Create buttons in
    // other menus (maker pen, room creation, etc.).
    private static GameObject FindByVisibleLabel()
    {
        var find = typeof(UnityEngine.Object).GetMethod("FindObjectsOfType",
            new[] { typeof(Type) });
        if (find == null)
            return null;

        var all = (IEnumerable)find.Invoke(null, new object[] { typeof(Button) });

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
            // Skip our own clone if it somehow exists without the suffix check.
            if (go.name.EndsWith(CloneNameSuffix, StringComparison.Ordinal))
                continue;
            if (!HasLabel(go, SourceLabel))
                continue;

            bool isHome = IsUnderHome(go);
            if (best == null || (isHome && !bestIsHome))
            {
                best = go;
                bestIsHome = isHome;
                if (isHome)
                    break; // home-screen Create is the one we want
            }
        }

        if (best != null)
            Plugin.Log.LogInfo($"[PLAY] found Create button: '{best.name}'");
        return best;
    }

    // Home-hierarchy discovery: the Watch home's Create entry is an ICON in
    // the home icon row (Create, Store, Events, …) and may carry no text
    // label at all, so neither the label search nor the name-hint fallback
    // can see it. Anchor on the unobfuscated HomeTop5TabsModel type (the same
    // anchor HomeLabelsPatch uses), walk up to the home-screen root, then
    // scan its descendants for an active uGUI Button whose GameObject name
    // contains "create".
    private static GameObject FindCreateInHomeHierarchy()
    {
        var homeRoot = FindHomeRoot();
        if (homeRoot == null)
            return null;

        // be.788: GetComponent requires Il2CppSystem.Type, not System.Type.
        var buttonType = Il2CppSystem.Type.GetType(typeof(Button).AssemblyQualifiedName);
        var stack = new Stack<Transform>();
        stack.Push(homeRoot);
        while (stack.Count > 0)
        {
            var t = stack.Pop();
            if (t == null)
                continue;
            var go = t.gameObject;
            if (go != null && go.activeInHierarchy &&
                !go.name.EndsWith(CloneNameSuffix, StringComparison.Ordinal))
            {
                var lower = (go.name ?? string.Empty).ToLowerInvariant();
                if (lower.Contains("create") &&
                    !lower.Contains("invention") &&
                    !lower.Contains("subroom") &&
                    !lower.Contains("sub_room") &&
                    !lower.Contains("choose"))
                {
                    try
                    {
                        if (go.GetComponent(buttonType) != null)
                        {
                            Plugin.Log.LogInfo($"[PLAY] found Create button in the home hierarchy: '{go.name}'");
                            return go;
                        }
                    }
                    catch
                    {
                        // keep scanning
                    }
                }
            }
            for (int i = 0; i < t.childCount; i++)
            {
                try { stack.Push(t.GetChild(i)); }
                catch
                {
                    // keep scanning
                }
            }
        }
        return null;
    }

    // Anchor the home UI without relying on labels: the unobfuscated
    // HomeTop5TabsModel instance's highest Home/Watch/Title-named ancestor,
    // else any active Transform with Home/Watch/Title in its name.
    private static Transform FindHomeRoot()
    {
        var modelType = FindTypeByName("HomeTop5TabsModel");
        if (modelType != null)
        {
            var modelGo = FirstGameObjectOfType(modelType);
            if (modelGo != null)
            {
                var top = modelGo.transform;
                var t = top.parent;
                while (t != null)
                {
                    var pn = t.name ?? string.Empty;
                    if (pn.IndexOf("Home", StringComparison.OrdinalIgnoreCase) >= 0 ||
                        pn.IndexOf("Watch", StringComparison.OrdinalIgnoreCase) >= 0 ||
                        pn.IndexOf("Title", StringComparison.OrdinalIgnoreCase) >= 0)
                        top = t;
                    t = t.parent;
                }
                return top;
            }
        }

        var find = typeof(UnityEngine.Object).GetMethod("FindObjectsOfType",
            new[] { typeof(Type) });
        if (find == null)
            return null;
        var all = (IEnumerable)find.Invoke(null, new object[] { typeof(Transform) });
        if (all == null)
            return null;
        foreach (var o in all)
        {
            var t = ((UnityEngine.Object)o).TryCast<Transform>();
            if (t == null)
                continue;
            var go = t.gameObject;
            if (go == null || !go.activeInHierarchy)
                continue;
            var n = t.name ?? string.Empty;
            if (n.IndexOf("Home", StringComparison.OrdinalIgnoreCase) >= 0 ||
                n.IndexOf("Watch", StringComparison.OrdinalIgnoreCase) >= 0)
                return t;
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

    // Fallback when the visible-label search finds nothing (e.g. labels fed
    // from obfuscated or localized sources): match by GameObject name hints
    // on uGUI Buttons. Cheap (scene-object walk only) — runs every tick.
    private static GameObject FindByNameHints()
    {
        var find = typeof(UnityEngine.Object).GetMethod("FindObjectsOfType",
            new[] { typeof(Type) });
        if (find == null)
            return null;

        // 1) GameObject name hints on uGUI Buttons.
        var all = (IEnumerable)find.Invoke(null, new object[] { typeof(Button) });

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
            if (go.name.EndsWith(CloneNameSuffix, StringComparison.Ordinal))
                continue;

            var lower = (go.name ?? string.Empty).ToLowerInvariant();
            bool nameHit = lower == "create" ||
                           lower == "button_create" ||
                           lower == "createbutton" ||
                           lower == "create_button" ||
                           (lower.Contains("create") &&
                            !lower.Contains("invention") &&
                            !lower.Contains("subroom") &&
                            !lower.Contains("sub_room") &&
                            !lower.Contains("choose"));
            if (!nameHit)
                continue;

            bool isHome = IsUnderHome(go);
            if (best == null || (isHome && !bestIsHome))
            {
                best = go;
                bestIsHome = isHome;
                if (isHome)
                    break; // home-screen Create is the one we want
            }
        }

        if (best != null)
        {
            Plugin.Log.LogInfo($"[PLAY] found Create button by name: '{best.name}'");
            return best;
        }

        return null;
    }

    // Last resort: resolve by component type name. Mirrors the FindQualitySetter
    // pattern in UltraGraphicsPatch — scan loaded assemblies for non-abstract
    // types whose names look like a Create button/tab, then FindObjectsOfType
    // each one. Il2Cpp downcasts must go through TryCast, never a direct cast.
    private static GameObject FindByCreateButtonType()
    {
        var find = typeof(UnityEngine.Object).GetMethod("FindObjectsOfType",
            new[] { typeof(Type) });
        if (find == null)
            return null;

        foreach (var asm in AppDomain.CurrentDomain.GetAssemblies())
        {
            Type[] types;
            try { types = asm.GetTypes(); }
            catch (ReflectionTypeLoadException e) { types = e.Types.Where(t => t != null).ToArray(); }
            catch { continue; }

            foreach (var t in types)
            {
                if (t == null || t.IsAbstract || t.IsInterface)
                    continue;
                if (!typeof(UnityEngine.Object).IsAssignableFrom(t))
                    continue;

                var fullName = t.FullName ?? string.Empty;
                // Skip engine assemblies — the button impl lives in game code.
                if (fullName.StartsWith("UnityEngine.", StringComparison.Ordinal) ||
                    fullName.StartsWith("Unity.", StringComparison.Ordinal))
                    continue;

                var name = t.Name;
                if (name.IndexOf("Create", StringComparison.OrdinalIgnoreCase) < 0)
                    continue;
                if (name.IndexOf("Button", StringComparison.OrdinalIgnoreCase) < 0 &&
                    name.IndexOf("Tab", StringComparison.OrdinalIgnoreCase) < 0)
                    continue;
                // Skip room-creation sub-dialog impls.
                if (name.IndexOf("Invention", StringComparison.OrdinalIgnoreCase) >= 0 ||
                    name.IndexOf("SubRoom", StringComparison.OrdinalIgnoreCase) >= 0 ||
                    name.IndexOf("Subroom", StringComparison.OrdinalIgnoreCase) >= 0)
                    continue;

                IEnumerable all;
                try { all = (IEnumerable)find.Invoke(null, new object[] { t }); }
                catch { continue; }
                if (all == null)
                    continue;

                foreach (var o in all)
                {
                    var comp = ((UnityEngine.Object)o).TryCast<Component>();
                    if (comp == null)
                        continue;
                    var go = comp.gameObject;
                    if (go == null || !go.activeInHierarchy)
                        continue;
                    Plugin.Log.LogInfo($"[PLAY] found Create button by type {t.FullName}: '{go.name}'");
                    return go;
                }
            }
        }

        return null;
    }

    private static bool IsUnderHome(GameObject go)
    {
        var t = go.transform.parent;
        while (t != null)
        {
            if (t.name.IndexOf("Home", StringComparison.OrdinalIgnoreCase) >= 0)
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

    private static void CloneAsPlay(GameObject sourceGo)
    {
        var cloneObj = UnityEngine.Object.Instantiate(sourceGo);
        var cloneGo = cloneObj.TryCast<GameObject>();
        if (cloneGo == null)
        {
            Plugin.Log.LogWarning("[PLAY] clone failed (not a GameObject)");
            return;
        }

        cloneGo.transform.SetParent(sourceGo.transform.parent, false);
        cloneGo.transform.SetSiblingIndex(sourceGo.transform.GetSiblingIndex() + 1);
        cloneGo.name = sourceGo.name + CloneNameSuffix;

        // Relabel "Create" -> "Play" (uGUI Text; TMPro fallback via reflection).
        RelabelClone(cloneGo, SourceLabel, CloneLabel);

        // Replace the room-creation click handler with Quick Play.
        ReplaceClickHandler(cloneGo);

        Plugin.Log.LogInfo($"[PLAY] added Play button '{cloneGo.name}' next to '{sourceGo.name}' " +
            $"(parent '{sourceGo.transform.parent?.name}', sibling index {cloneGo.transform.GetSiblingIndex()}, " +
            $"active={cloneGo.activeInHierarchy}, label='{ReadFirstLabel(cloneGo) ?? "<none>"}')");
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
                Plugin.Log.LogWarning("[PLAY] no label Text found on the Create button — Play button keeps its label");
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
            Plugin.Log.LogWarning("[PLAY] no matching label found on the clone — Play button keeps its label");
        }
        catch (Exception e)
        {
            Plugin.Log.LogWarning($"[PLAY] TMPro relabel failed: {e.Message}");
        }
    }

    // THE CLICK HANDLER WIRING: drop the clone's room-creation onClick
    // listeners, then wire our own fresh Quick Play handler. This is OUR OWN
    // delegate (never a wrapped game callback), converted via
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
        Plugin.Log.LogInfo("[PLAY] Play button wired to Quick Play");
    }

    private static void OnPlayClicked()
    {
        try
        {
            Plugin.Log.LogInfo("[PLAY] Play pressed — starting Quick Play");
            if (TryGameQuickPlay())
                return;
            TryPhotonJoinRandom();
        }
        catch (Exception e)
        {
            Plugin.Log.LogWarning($"[PLAY] Quick Play failed: {e.Message}");
        }
    }

    // Layer 1 — the game's own matchmaking entry point: static, parameterless,
    // void, with an unobfuscated QuickPlay/matchmaking name (confirmed present
    // in the 20230414 metadata). Proper integration beats a raw Photon call.
    private static bool TryGameQuickPlay()
    {
        string[] entryNames =
        {
            "QuickPlay", "PlayQuickPlay", "StartQuickPlay",
            "JoinPublicRoom", "JoinRandomPublicRoom"
        };

        foreach (var asm in AppDomain.CurrentDomain.GetAssemblies())
        {
            Type[] types;
            try { types = asm.GetTypes(); }
            catch (ReflectionTypeLoadException e) { types = e.Types.Where(t => t != null).ToArray(); }
            catch { continue; }

            foreach (var t in types)
            {
                if (t == null)
                    continue;
                foreach (var name in entryNames)
                {
                    var m = t.GetMethod(name,
                        BindingFlags.Public | BindingFlags.NonPublic | BindingFlags.Static,
                        null, Type.EmptyTypes, null);
                    if (m == null || m.ReturnType != typeof(void))
                        continue;
                    try
                    {
                        m.Invoke(null, null);
                        Plugin.Log.LogInfo($"[PLAY] Quick Play via {t.FullName}.{m.Name}()");
                        return true;
                    }
                    catch (Exception e)
                    {
                        Plugin.Log.LogDebug($"[PLAY] {t.FullName}.{m.Name}() threw: {e.Message}");
                    }
                }
            }
        }

        Plugin.Log.LogDebug("[PLAY] no game-level Quick Play entry point found");
        return false;
    }

    // Layer 2 — fallback: PhotonNetwork.JoinRandomRoom() directly.
    // On JoinRandomFailed the game's existing error handling shows the
    // message; we don't touch it.
    private static void TryPhotonJoinRandom()
    {
        try
        {
            var photonType = FindTypeByName("PhotonNetwork");
            if (photonType == null)
            {
                Plugin.Log.LogWarning("[PLAY] PhotonNetwork type not found — Quick Play unavailable");
                return;
            }

            if (!IsPhotonConnected(photonType))
            {
                Plugin.Log.LogWarning("[PLAY] Photon not connected — Quick Play skipped");
                return;
            }

            var join = photonType.GetMethod("JoinRandomRoom",
                BindingFlags.Public | BindingFlags.Static, null, Type.EmptyTypes, null);
            if (join == null)
            {
                Plugin.Log.LogWarning("[PLAY] PhotonNetwork.JoinRandomRoom() not found");
                return;
            }

            var result = join.Invoke(null, null);
            Plugin.Log.LogInfo($"[PLAY] PhotonNetwork.JoinRandomRoom() invoked (returned {result})");
        }
        catch (Exception e)
        {
            Plugin.Log.LogWarning($"[PLAY] Photon JoinRandomRoom failed: {e.Message}");
        }
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
                if (t != null && (t.Name == name || t.FullName == "Photon.Pun." + name))
                    return t;
            }
        }
        return null;
    }

    private static bool IsPhotonConnected(Type photonType)
    {
        // PUN2: static bool IsConnected; PUN1: static bool connected.
        foreach (var memberName in new[] { "IsConnected", "connected" })
        {
            try
            {
                var prop = photonType.GetProperty(memberName,
                    BindingFlags.Public | BindingFlags.Static);
                if (prop != null && prop.PropertyType == typeof(bool))
                    return (bool)prop.GetValue(null, null);
                var field = photonType.GetField(memberName,
                    BindingFlags.Public | BindingFlags.Static);
                if (field != null && field.FieldType == typeof(bool))
                    return (bool)field.GetValue(null);
            }
            catch
            {
                // fall through to the next member name
            }
        }
        return true; // fail-open: attempt the join and let Photon report
    }
}
