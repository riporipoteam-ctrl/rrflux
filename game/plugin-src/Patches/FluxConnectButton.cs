// Adds a "Connect" tab to the real Rec Room Watch UI and wires it to the
// Flux pairing overlay (same as pressing F8). This file implements the
// CLONE + RELABEL + CLICK-HANDLER steps, exactly like PlayButtonPatch.
//
// Why this shape:
//  - The Watch UI is 100% code-built at runtime via RRUI — there are no
//    Watch prefabs to edit. Home tabs live under HomeTop5TabsModel with
//    enum {Rooms=0, Clubs=1, Items=2, Inventions=3, Creators=4}.
//  - Tab button GameObject/type names may be obfuscated and re-rolled per
//    build, and the tab LABELS are unreliable too: tabs can be icon-only
//    (no label at all), labels may be empty/disabled at runtime, and
//    HomeLabelsPatch rewrites the visible labels to the configured values,
//    so any label-based matching breaks the moment labels are empty or
//    renamed. Discovery here uses NO label matching at all — labels are
//    only ever read for log output.
//  - Discovery runs three strategies, in order, and logs a full row scan
//    every time one runs (row GO name, child count, per-child name, Button
//    presence, label text or "empty"/"no label"):
//      A. MODEL-ANCHORED: resolve the unobfuscated HomeTop5TabsModel type
//         via FindObjectsOfType, take its GameObject. Its transform is the
//         tab row when it carries >=2 button children; otherwise a bounded
//         BFS of its subtree finds the row (preferring exactly 5 buttons,
//         the Top5 row).
//      B. INDEX-BASED (positional fallback, only when the model type is
//         absent): find the home/Watch root, BFS to the first Transform
//         with >=2 button-carrying active children (preferring exactly 5),
//         and use the FIRST child with a Button component as the clone
//         source. No name hints, no labels.
//      C. FAILURE: a detailed per-attempt log plus a full scene snapshot
//         (row hierarchy dump) at give-up.
//  - The plugin clones the source tab (same button, same style, same tab
//    row), inserts the clone as its next sibling and relabels its first
//    text to "Connect". Visual consistency with the game's design language
//    is guaranteed because it is literally the same button GameObject.
//  - The clone's original onClick listeners (which switch to the cloned tab)
//    are removed and replaced with a single listener that opens the Flux
//    pairing overlay (FluxPairingPatch.ShowOverlay). The listener is OUR OWN
//    fresh handler wired through DelegateSupport.ConvertDelegate — we never
//    wrap the game's callbacks, so the v0.1.30 Delegate.CreateDelegate hang
//    cannot recur.
//  - The tab is a pure overlay trigger: it never registers with
//    TabsModel<T>.GoToPage, so it cannot corrupt tab navigation state.
//  - The clone is named "<source>_FluxConnectTab"; HomeLabelsPatch skips
//    any GameObject whose name CONTAINS that marker (IsCloneTab), so our
//    clone is never relabeled by the labels patch regardless of Apply()
//    ordering. The marker must stay exactly "_FluxConnectTab".
//
// Hard rules honored:
//  - The source tab itself is untouched: the plugin only ADDS a sibling.
//  - Idempotent: the clone is only added once per tab row (name-marker
//    check), so scene reloads and repeated Apply() calls never duplicate it.
//
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

    // The HomeTop5TabsModel type, once resolved, never changes for the
    // session — cache it so every retry tick doesn't rescan all assemblies.
    private static Type _modelType;
    private static bool _modelTypeResolved;

    private static int _attempts;
    private static bool _buttonDone;
    private static bool _gaveUp;
    private static DateTime _firstAttemptUtc = DateTime.MinValue;
    private static DateTime _lastProgressLogUtc = DateTime.MinValue;

    // Row-scan logging throttle: a full per-child row scan is logged at Info
    // the first time a given row state is seen, and again only when the row
    // state changes (new children, buttons appearing). Between changes the
    // tick stays quiet so the 2-second retry loop can't flood the log.
    private static string _lastRowScanSignature;

    // True once there is nothing left to do: feature disabled in config, the
    // Connect tab was added, or the retry budget ran out. The
    // UiDiscoveryRetry driver stops ticking this patch once settled.
    internal static bool IsSettled =>
        !Plugin.EnableFluxPairing.Value || _buttonDone || _gaveUp;

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
            $"(attempt {_attempts}, {elapsed.TotalSeconds:F0}s elapsed). {DescribeSceneState()}");
    }

    // Final give-up: Warning level, states exactly what was searched for,
    // how many attempts ran, and dumps the FULL tab row hierarchy (GO names,
    // components, label texts) so the next fix has real data to work with.
    private static void LogGiveUp()
    {
        var elapsed = DateTime.UtcNow - _firstAttemptUtc;
        Plugin.Log.LogWarning("[CONNECT] GAVE UP adding the Connect tab after " +
            $"{_attempts} attempts over {elapsed.TotalMinutes:F1} minutes. " +
            "Searched: (A) MODEL-ANCHORED — the HomeTop5TabsModel instance's " +
            "GameObject (transform is the row when it carries >=2 button " +
            "children, else a bounded BFS of its subtree preferring 5), " +
            "source = first child with a Button component; (B) INDEX-BASED — " +
            "only when the model type is absent — home/Watch root -> first " +
            "row by position (>=2 button children, prefer 5), source = first " +
            "child with a Button; (C) detailed failure logged per attempt. " +
            "No label-based matching was used (labels may be empty or " +
            "rewritten by HomeLabelsPatch). Tab row contents at give-up:\n" + DescribeSceneState());
    }

    // Compact scene-state snapshot for the progress log, plus the full tab
    // row hierarchy dump (GO names, components, label texts) used at give-up.
    private static string DescribeSceneState()
    {
        try
        {
            var parts = new List<string>();
            var modelType = ResolveModelType();
            if (modelType == null)
            {
                parts.Add("HomeTop5TabsModel type not found in loaded assemblies.");
            }
            else
            {
                var modelGo = FirstGameObjectOfType(modelType);
                if (modelGo == null)
                    parts.Add("HomeTop5TabsModel type present but no live instance in the scene.");
                else
                {
                    parts.Add("HomeTop5TabsModel instance: '" + modelGo.name + "' " +
                        $"(path: {TransformPath(modelGo.transform)}).");
                    var row = FindTabRowByModel();
                    parts.Add(row == null
                        ? "No tab row found under the model instance."
                        : "Tab row under model instance:\n" + DumpRowHierarchy(row));
                }
            }

            var root = FindHomeRoot();
            parts.Add(root == null
                ? "No Home/Watch-named root found in the scene."
                : $"Home root candidate: '{root.name}' (path: {TransformPath(root)}).");
            return string.Join("\n", parts);
        }
        catch (Exception e)
        {
            return $"scene scan failed: {e.Message}";
        }
    }

    // Full hierarchy dump of a tab row: per child GO name, active state,
    // component type names, and the first readable label text. Capped so a
    // pathological row can't flood the log.
    private static string DumpRowHierarchy(Transform row)
    {
        var lines = new List<string>();
        try
        {
            lines.Add($"'{row.name}' childCount={row.childCount} active={row.gameObject.activeInHierarchy} " +
                $"path={TransformPath(row)}");
            int shown = Math.Min(row.childCount, 12);
            for (int i = 0; i < shown; i++)
            {
                lines.Add("  " + DescribeChild(row.GetChild(i), i));
            }
            if (row.childCount > shown)
                lines.Add($"  ... and {row.childCount - shown} more children (capped).");
        }
        catch (Exception e)
        {
            lines.Add($"  hierarchy dump failed: {e.Message}");
        }
        var text = string.Join("\n", lines);
        return text.Length > 6000 ? text.Substring(0, 6000) + "\n  ... (dump truncated)" : text;
    }

    // One child, one log line: index, name, active state, Button presence,
    // and label text ("empty" / "no label" when there is none). Labels are
    // DIAGNOSTIC ONLY — never used for matching.
    private static string DescribeChild(Transform child, int index)
    {
        try
        {
            if (child == null)
                return $"[{index}] <null>";
            var go = child.gameObject;
            if (go == null)
                return $"[{index}] <no GameObject>";
            var label = ReadFirstLabel(go);
            var labelText = label == null ? "no label" : (label.Length == 0 ? "empty" : label);
            var button = FindButton(go) != null;
            var comps = ComponentNames(go, 10);
            return $"[{index}] '{go.name}' active={go.activeInHierarchy} " +
                $"children={child.childCount} button={(button ? "yes" : "no")} " +
                $"label='{labelText}' components=[{string.Join(", ", comps)}]";
        }
        catch (Exception e)
        {
            return $"[{index}] describe failed: {e.Message}";
        }
    }

    private static List<string> ComponentNames(GameObject go, int max)
    {
        var names = new List<string>();
        try
        {
            // be.788: GetComponents requires Il2CppSystem.Type, not System.Type.
            var compType = Il2CppSystem.Type.GetType(typeof(Component).AssemblyQualifiedName);
            var comps = go.GetComponents(compType);
            if (comps == null)
                return names;
            foreach (var c in comps)
            {
                if (names.Count >= max)
                    break;
                try { names.Add(c.GetType().Name); }
                catch { names.Add("<?>"); }
            }
        }
        catch
        {
            // best effort only
        }
        return names;
    }

    private static string TransformPath(Transform t)
    {
        var parts = new List<string>();
        try
        {
            var cur = t;
            while (cur != null && parts.Count < 12)
            {
                parts.Add(cur.name ?? "?");
                cur = cur.parent;
            }
            parts.Reverse();
        }
        catch
        {
            // best effort only
        }
        return "/" + string.Join("/", parts);
    }

    // Find a source tab button and clone it into a "Connect" tab right
    // beside it. Idempotent: never clones twice into the same row.
    private static void EnsureConnectTab()
    {
        var (row, source, strategy) = FindSourceTab();
        if (row == null || source == null)
            return;

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

        // Already added? (a sibling already carrying our marker)
        for (int i = 0; i < parent.childCount; i++)
        {
            var child = parent.GetChild(i);
            if (child != null && (child.name ?? string.Empty).Contains(CloneNameSuffix, StringComparison.Ordinal))
            {
                _buttonDone = true;
                Plugin.Log.LogDebug("[CONNECT] Connect tab already present");
                return;
            }
        }

        Plugin.Log.LogInfo($"[CONNECT] strategy {strategy}: cloning '{source.name}' " +
            $"from row '{row.name}' as the Connect tab");
        CloneAsConnect(source);
        _buttonDone = true;
    }

    // Three-tier discovery, NO label matching anywhere:
    //   A. MODEL-ANCHORED: HomeTop5TabsModel instance -> its GameObject ->
    //      the tab row (the model's own transform, or a descendant row).
    //   B. INDEX-BASED (only when the model type is absent): home root ->
    //      tab row by POSITION (first descendant row with >=2 button-
    //      carrying children, preferring exactly 5).
    //   C. FAILURE: detailed per-attempt log with a row scan / scene
    //      snapshot (DescribeChild output), so a failed attempt still leaves
    //      real data in the log.
    // If a row exists but has no usable tab buttons yet, the UI is still
    // building: skip the expensive fallback scan this tick and retry on the
    // next UiDiscoveryRetry tick.
    // Returns (row, source tab, strategy name); row or source is null when
    // discovery failed this attempt.
    private static (Transform row, GameObject source, string strategy) FindSourceTab()
    {
        // A: model-anchored.
        var modelRow = FindTabRowByModel();
        if (modelRow != null)
        {
            var modelType = ResolveModelType();
            LogDiscoveryRun("A (model-anchored)", modelRow, modelType);
            var source = PickSourceTab(modelRow);
            if (source != null)
                return (modelRow, source, "A");
            Plugin.Log.LogDebug("[CONNECT] strategy A: HomeTop5TabsModel row exists " +
                "but has no usable tab buttons yet — retrying on the next tick");
            return (null, null, null);
        }

        var typeResolved = ResolveModelType() != null;
        if (typeResolved)
        {
            Plugin.Log.LogDebug("[CONNECT] strategy A: HomeTop5TabsModel type present " +
                "but no live instance in the scene — retrying on the next tick");
            return (null, null, null);
        }

        // B: index-based positional fallback (only when the model type is
        // absent entirely — never as a shortcut over a real model).
        Plugin.Log.LogDebug("[CONNECT] strategy A failed (model type absent) — trying strategy B (index-based)");
        var fallbackRow = FindTabRowByPosition();
        if (fallbackRow == null)
        {
            LogDiscoveryFailureB();
            return (null, null, null);
        }
        LogDiscoveryRun("B (index-based)", fallbackRow, null);
        var fallbackSource = PickSourceTab(fallbackRow);
        if (fallbackSource != null)
            Plugin.Log.LogWarning("[CONNECT] strategy B: used the positional fallback row " +
                $"'{fallbackRow.name}' (model type absent) — source tab '{fallbackSource.name}'");
        else
            Plugin.Log.LogDebug("[CONNECT] strategy B: fallback row found but no usable tab buttons yet");
        return (fallbackRow, fallbackSource, "B");
    }

    // Detailed discovery log, run once per row state: the row's GO name and
    // path, child count, and per child — name, active state, Button
    // presence, and label text ("empty"/"no label"). Labels are logged only;
    // matching never uses them. Repeated identical row states are suppressed
    // to Debug so the 2-second retry tick can't flood the log.
    private static void LogDiscoveryRun(string strategy, Transform row, Type modelType)
    {
        try
        {
            int buttons = CountButtonChildren(row);
            var signature = $"{row.GetInstanceID()}|{row.childCount}|{buttons}";
            if (signature == _lastRowScanSignature)
            {
                Plugin.Log.LogDebug($"[CONNECT] strategy {strategy}: row '{row.name}' unchanged since last scan");
                return;
            }
            _lastRowScanSignature = signature;

            var lines = new List<string>
            {
                $"[CONNECT] strategy {strategy}: row scan of '{row.name}' " +
                $"(path: {TransformPath(row)}, childCount={row.childCount}, " +
                $"active={row.gameObject.activeInHierarchy}, buttonChildren={buttons})"
            };
            int shown = Math.Min(row.childCount, 16);
            for (int i = 0; i < shown; i++)
                lines.Add("[CONNECT]   " + DescribeChild(row.GetChild(i), i));
            if (row.childCount > shown)
                lines.Add($"[CONNECT]   ... and {row.childCount - shown} more children (capped).");
            if (modelType != null)
                lines.Add($"[CONNECT]   anchored on {modelType.FullName} instance '{row.name}'");
            Plugin.Log.LogInfo(string.Join("\n", lines));
        }
        catch (Exception e)
        {
            Plugin.Log.LogDebug($"[CONNECT] row scan logging failed: {e.Message}");
        }
    }

    // Detailed failure log when strategy B also finds no row: what was
    // scanned and what the scene contains, at Debug so the retry tick stays
    // quiet (the full snapshot lands in the 60-second progress log and at
    // give-up).
    private static void LogDiscoveryFailureB()
    {
        try
        {
            var root = FindHomeRoot();
            var lines = new List<string>
            {
                "[CONNECT] strategy B failed: no positional tab row found. " +
                $"home root={(root == null ? "<none>" : $"'{root.name}' (path: {TransformPath(root)})")}"
            };
            if (root != null)
            {
                int shown = Math.Min(root.childCount, 8);
                for (int i = 0; i < shown; i++)
                    lines.Add("[CONNECT]   root child: " + DescribeChild(root.GetChild(i), i));
            }
            Plugin.Log.LogDebug(string.Join("\n", lines));
        }
        catch (Exception e)
        {
            Plugin.Log.LogDebug($"[CONNECT] failure logging failed: {e.Message}");
        }
    }

    // A: MODEL-ANCHORED. Resolve the unobfuscated HomeTop5TabsModel type,
    // find its live GameObject, and get the tab row from it: the model's
    // own transform when it carries >=2 button children, else a bounded BFS
    // of its subtree (the model may be a container above the actual row).
    private static Transform FindTabRowByModel()
    {
        var modelType = ResolveModelType();
        if (modelType == null)
            return null;
        var modelGo = FirstGameObjectOfType(modelType);
        if (modelGo == null)
            return null;
        var modelT = modelGo.transform;
        if (modelT != null && IsTabRow(modelT, out _))
            return modelT;
        // The model instance exists but isn't itself the row — look below it.
        var subRow = FindTabRowInSubtree(modelT);
        if (subRow != null)
            Plugin.Log.LogInfo($"[CONNECT] strategy A: model instance '{modelGo.name}' is a container — " +
                $"tab row is descendant '{subRow.name}'");
        return subRow;
    }

    private static Type ResolveModelType()
    {
        if (_modelTypeResolved)
            return _modelType;
        _modelType = FindTypeByName("HomeTop5TabsModel");
        if (_modelType != null)
        {
            _modelTypeResolved = true;
            Plugin.Log.LogInfo("[CONNECT] resolved the HomeTop5TabsModel type");
        }
        return _modelType;
    }

    // Pick the clone source inside a tab row by INDEX, never by label or
    // name: the FIRST active child carrying a uGUI Button (excluding our own
    // clone). First-in-sibling-order is the most stable choice — the leading
    // tab always exists and is enabled, while later tabs may be dynamic.
    private static GameObject PickSourceTab(Transform row)
    {
        GameObject first = null;
        int usable = 0;
        for (int i = 0; i < row.childCount; i++)
        {
            var child = row.GetChild(i);
            if (child == null)
                continue;
            var go = child.gameObject;
            if (go == null || !go.activeInHierarchy)
                continue;
            // Skip our own clone if it somehow exists without the marker check.
            if ((go.name ?? string.Empty).Contains(CloneNameSuffix, StringComparison.Ordinal))
                continue;
            if (FindButton(go) == null)
                continue;
            usable++;
            if (first == null)
                first = go;
        }

        if (first != null)
            Plugin.Log.LogInfo($"[CONNECT] picked source tab '{first.name}' in row '{row.name}' " +
                $"(first button child by index, {usable} usable tabs, " +
                $"label='{ReadFirstLabel(first) ?? "no label"}')");
        return first;
    }

    // B: INDEX-BASED positional fallback — find the home root, then BFS for
    // the first Transform with >=2 button-carrying active children,
    // preferring a row with exactly 5 (the Top5 row). Bounded so a huge
    // scene can't stall the tick. Never used when the model type resolved.
    private static Transform FindTabRowByPosition()
    {
        var root = FindHomeRoot();
        if (root == null)
            return null;
        return FindTabRowInSubtree(root);
    }

    // Bounded BFS of a subtree for the first tab-row-like Transform,
    // preferring exactly 5 button children (the Top5 row). The root itself
    // is skipped — it must be a descendant row.
    private static Transform FindTabRowInSubtree(Transform root)
    {
        const int MaxNodes = 5000;
        int visited = 0;
        Transform firstRow = null;
        var queue = new Queue<Transform>();
        queue.Enqueue(root);
        while (queue.Count > 0 && visited < MaxNodes)
        {
            var t = queue.Dequeue();
            if (t == null)
                continue;
            visited++;
            if (t != root && IsTabRow(t, out int buttonChildren))
            {
                if (buttonChildren == 5)
                    return t; // the Top5 row — best positional match
                if (firstRow == null)
                    firstRow = t;
            }
            for (int i = 0; i < t.childCount; i++)
            {
                try { queue.Enqueue(t.GetChild(i)); }
                catch
                {
                    // keep scanning
                }
            }
        }
        return firstRow;
    }

    // A tab row, positionally: >=2 active children carrying uGUI Buttons,
    // and the row itself isn't one of our clones.
    private static bool IsTabRow(Transform t, out int buttonChildren)
    {
        buttonChildren = 0;
        try
        {
            var go = t.gameObject;
            if (go == null || !go.activeInHierarchy)
                return false;
            if ((go.name ?? string.Empty).Contains(CloneNameSuffix, StringComparison.Ordinal))
                return false;
            buttonChildren = CountButtonChildren(t);
            return buttonChildren >= 2;
        }
        catch
        {
            return false;
        }
    }

    private static int CountButtonChildren(Transform t)
    {
        int count = 0;
        try
        {
            for (int i = 0; i < t.childCount && count < 6; i++)
            {
                var child = t.GetChild(i);
                if (child == null)
                    continue;
                var cgo = child.gameObject;
                if (cgo == null || !cgo.activeInHierarchy)
                    continue;
                if (FindButton(cgo) != null)
                    count++;
            }
        }
        catch
        {
            // best effort only
        }
        return count;
    }

    // Anchor the home UI without relying on labels: the unobfuscated
    // HomeTop5TabsModel instance's highest Home/Watch-named ancestor, else
    // any active Transform with Home/Watch in its name.
    private static Transform FindHomeRoot()
    {
        var modelType = ResolveModelType();
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
                        pn.IndexOf("Watch", StringComparison.OrdinalIgnoreCase) >= 0)
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

    // The uGUI Button for a tab: on the GameObject itself first, else nested
    // (RRUI composes buttons from parts). Null when there is none.
    private static Button FindButton(GameObject go)
    {
        // be.788: GetComponent requires Il2CppSystem.Type, not System.Type.
        var buttonType = Il2CppSystem.Type.GetType(typeof(Button).AssemblyQualifiedName);
        try
        {
            var direct = go.GetComponent(buttonType)?.TryCast<Button>();
            if (direct != null)
                return direct;
            var nested = go.GetComponentsInChildren(buttonType, true);
            if (nested != null)
            {
                foreach (var b in nested)
                {
                    var button = ((UnityEngine.Object)b).TryCast<Button>();
                    if (button != null)
                        return button;
                }
            }
        }
        catch
        {
            // keep scanning
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
            if (go != null && go.activeInHierarchy)
                return go;
        }
        return null;
    }

    // First non-empty visible label under go (uGUI Text, then TMPro via
    // reflection), or null. DIAGNOSTIC ONLY — never used for matching, only
    // for log output and for choosing which text the clone's relabel edits.
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

        // Relabel the clone's first text to "Connect" — no label matching
        // involved, the target is simply the first readable text under the
        // clone (icon-only tabs have no text and keep working as buttons).
        RelabelClone(cloneGo);

        // Replace the tab-switch click handler with the pairing overlay.
        ReplaceClickHandler(cloneGo);

        Plugin.Log.LogInfo($"[CONNECT] added Connect tab '{cloneGo.name}' next to '{sourceGo.name}' " +
            $"(parent '{sourceGo.transform.parent?.name}', sibling index {cloneGo.transform.GetSiblingIndex()}, " +
            $"active={cloneGo.activeInHierarchy})");
    }

    private static void RelabelClone(GameObject cloneGo)
    {
        // uGUI path: first text under the clone becomes "Connect".
        var textType = Il2CppSystem.Type.GetType(typeof(Text).AssemblyQualifiedName);
        var dstTexts = cloneGo.GetComponentsInChildren(textType, true);
        if (dstTexts != null)
        {
            foreach (var c in dstTexts)
            {
                var label = ((UnityEngine.Object)c).TryCast<Text>();
                if (label == null)
                    continue;
                var old = label.text;
                label.text = CloneLabel;
                Plugin.Log.LogInfo($"[CONNECT] relabeled clone text '{old?.Trim()}' -> '{CloneLabel}'");
                return;
            }
        }

        // TMPro fallback (no compile-time dependency).
        try
        {
            var tmproAsm = AppDomain.CurrentDomain.GetAssemblies()
                .FirstOrDefault(a => a.GetName().Name == "Unity.TextMeshPro");
            var tmproType = tmproAsm?.GetType("TMPro.TextMeshProUGUI");
            if (tmproType == null)
            {
                Plugin.Log.LogInfo("[CONNECT] clone has no text label — Connect tab keeps the source's visuals");
                return;
            }
            var getTexts = typeof(GameObject).GetMethod("GetComponentsInChildren",
                new[] { typeof(Type), typeof(bool) });
            var dst = (IEnumerable)getTexts.Invoke(cloneGo, new object[] { tmproType, true });
            var textProp = tmproType.GetProperty("text");
            foreach (var c in dst)
            {
                var old = (string)textProp.GetValue(c, null);
                textProp.SetValue(c, CloneLabel, null);
                Plugin.Log.LogInfo($"[CONNECT] relabeled clone TMPro text '{old?.Trim()}' -> '{CloneLabel}'");
                return;
            }
            Plugin.Log.LogInfo("[CONNECT] clone has no text label — Connect tab keeps the source's visuals");
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
        var button = FindButton(cloneGo);
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
