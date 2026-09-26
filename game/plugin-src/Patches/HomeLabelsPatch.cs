// Ensures the home-screen tab icons show "Rooms" labels.
//
// Why this shape:
//  - The home screen is 100% code-built at runtime via RRUI: the tab buttons
//    (TabButtonImpl under HomeTop5TabsModel) follow an icon + label pattern,
//    but labels can be missing, empty, or invisible depending on which UI
//    variant renders (RRUI vs legacy, per the WatchUIPatch gates).
//  - The patch does two things, in order:
//      1. Re-activates label components that exist but are disabled.
//      2. Sets each tab's label text from the [Home] "Home Tab Labels" config
//         (semicolon-separated, applied left-to-right in visual sibling
//         order), so the tabs get proper "Rooms" labels.
//  - Tab contents are data-driven (RRUI TabsModel with obfuscated members),
//    so exact tab identities can't be confirmed statically. The patch logs
//    every tab's ORIGINAL label once at Info, so the BepInEx log shows what
//    the tabs actually are and the config list can be corrected without a
//    rebuild. If the configured label count doesn't match the tab count, the
//    patch warns and changes no text (never mislabels a tab).
//  - PlayButtonPatch's "_Play" clone is explicitly skipped, so the "Play"
//    label is never clobbered regardless of Apply() ordering between the
//    two patches.
//
// Hard rules honored (see PlayButtonPatch / HomeLogoPatch):
//  - Resolve by unobfuscated type name ("HomeTop5TabsModel") or by
//    name-substring on Transforms, never by obfuscated member names.
//  - TryCast for downcasts; FindObjectsOfType via the proven reflection
//    pattern; GetComponentsInChildren needs Il2CppSystem.Type.
//  - Idempotent: one full verification pass sets _labelsDone; scene reloads
//    never duplicate work. Text is only rewritten when it differs.
//  - Fail-soft: every step is wrapped, retries stop after MaxAttempts.
//  - TMPro is resolved by reflection (no compile-time dependency).
//
// Coexistence (checked against the other home-screen patches):
//  - HomeLogoPatch: inserts a "FluxHomeLogo" Image ABOVE the tab row. It is
//    not a tab button, so this patch never touches it (and the logo patch
//    never touches tab buttons).
//  - PlayButtonPatch: inserts a "_Play"-suffixed sibling of Create INSIDE
//    the tab row. Skipped by name here — different feature, no overlap.
//  - FluxConnectButton: standalone IMGUI overlay in the top-right corner;
//    touches zero game UI types — no conflict by construction.
//  - WatchUIPatch: only forces which home variant renders; this patch works
//    on whichever tab row exists.
//
// Two knobs, see [Home] in the .cfg:
//   Enable Flux Home Branding -> master switch (default true).
//   Home Tab Labels           -> semicolon-separated labels in tab order.
using System;
using System.Collections;
using System.Linq;
using System.Reflection;
using UnityEngine;
using UnityEngine.UI;

namespace RecNetPlugin.Patches;

internal static class HomeLabelsPatch
{
    private const string PlayCloneSuffix = "_Play";
    private const int MaxAttempts = 20;

    private static int _attempts;
    private static bool _labelsDone;
    private static bool _loggedLabels;

    // Called from Plugin.Load and again on each scene load: retries until the
    // home tab row exists.
    public static void Apply()
    {
        if (!Plugin.EnableFluxHomeBranding.Value)
            return;

        if (_labelsDone || _attempts >= MaxAttempts)
            return;

        _attempts++;
        try
        {
            EnsureLabels();
        }
        catch (Exception e)
        {
            Plugin.Log.LogWarning($"[HOMELABELS] attempt {_attempts} failed: {e.Message}");
        }

        if (_attempts >= MaxAttempts && !_labelsDone)
            Plugin.Log.LogWarning("[HOMELABELS] gave up — the home tab row was never found.");
    }

    private static void EnsureLabels()
    {
        var tabRow = FindTabRow();
        if (tabRow == null)
        {
            Plugin.Log.LogDebug("[HOMELABELS] home tab row not found yet");
            return;
        }

        int checkedButtons = 0;
        int reactivated = 0;
        var tabButtons = new System.Collections.Generic.List<GameObject>();
        foreach (Transform child in tabRow)
        {
            if (child == null)
                continue;
            var go = child.gameObject;
            if (go == null)
                continue;

            // Never touch the Play button clone (or the logo, which isn't a
            // button anyway).
            if (go.name.EndsWith(PlayCloneSuffix, StringComparison.Ordinal))
                continue;
            if (!HasButton(go))
                continue;

            checkedButtons++;
            tabButtons.Add(go);
            reactivated += EnsureLabelVisible(go);
        }

        if (!_loggedLabels)
        {
            _loggedLabels = true;
            LogTabLabels(tabButtons);
        }

        if (checkedButtons > 0)
        {
            ApplyConfiguredLabels(tabButtons);
            _labelsDone = true;
            Plugin.Log.LogInfo($"[HOMELABELS] verified {checkedButtons} tab buttons " +
                $"({reactivated} labels re-activated)");
        }
        else
        {
            Plugin.Log.LogDebug("[HOMELABELS] tab row found but no tab buttons yet");
        }
    }

    // Step 2: set each tab's label from the [Home] Home Tab Labels config,
    // left-to-right in visual sibling order. Skipped entirely when the
    // configured count doesn't match the tab count (fail-soft).
    private static void ApplyConfiguredLabels(System.Collections.Generic.List<GameObject> tabButtons)
    {
        var desired = Plugin.HomeTabLabels.Value
            .Split(new[] { ';' }, StringSplitOptions.RemoveEmptyEntries)
            .Select(s => s.Trim())
            .Where(s => s.Length > 0)
            .ToArray();

        if (desired.Length == 0)
        {
            Plugin.Log.LogDebug("[HOMELABELS] no labels configured — leaving tab label text unchanged");
            return;
        }

        if (desired.Length != tabButtons.Count)
        {
            Plugin.Log.LogWarning("[HOMELABELS] configured label count (" + desired.Length +
                ") doesn't match tab count (" + tabButtons.Count +
                ") — skipping text changes to avoid mislabeling. Fix [Home] Home Tab Labels in the .cfg " +
                "(the original labels were logged above at Info).");
            return;
        }

        for (int i = 0; i < tabButtons.Count; i++)
            SetLabelText(tabButtons[i], desired[i]);
    }

    // Write the label text into the tab's label slot: prefer the slot that
    // already carries text, else fill the first (possibly empty) slot.
    // uGUI Text first, TMPro fallback via reflection.
    private static void SetLabelText(GameObject buttonGo, string label)
    {
        // uGUI path.
        // be.788: GetComponentsInChildren requires Il2CppSystem.Type, not System.Type.
        var textType = Il2CppSystem.Type.GetType(typeof(Text).AssemblyQualifiedName);
        var texts = buttonGo.GetComponentsInChildren(textType, true);
        if (texts != null)
        {
            Text target = null;
            foreach (var c in texts)
            {
                var t = ((UnityEngine.Object)c).TryCast<Text>();
                if (t == null)
                    continue;
                if (target == null)
                    target = t;
                if (!string.IsNullOrWhiteSpace(t.text))
                {
                    target = t;
                    break;
                }
            }
            if (target != null)
            {
                if (target.text != label)
                {
                    Plugin.Log.LogInfo($"[HOMELABELS] tab '{buttonGo.name}': '{target.text}' -> '{label}'");
                    target.text = label;
                }
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
                Plugin.Log.LogDebug($"[HOMELABELS] tab '{buttonGo.name}' has no label Text component — skipped");
                return;
            }
            var getTexts = typeof(GameObject).GetMethod("GetComponentsInChildren",
                new[] { typeof(Type), typeof(bool) });
            var list = (IEnumerable)getTexts.Invoke(buttonGo, new object[] { tmproType, true });
            var textProp = tmproType.GetProperty("text");
            object target = null;
            foreach (var c in list)
            {
                if (target == null)
                    target = c;
                var txt = (string)textProp.GetValue(c, null);
                if (!string.IsNullOrWhiteSpace(txt))
                {
                    target = c;
                    break;
                }
            }
            if (target != null)
            {
                var current = (string)textProp.GetValue(target, null);
                if (current != label)
                {
                    Plugin.Log.LogInfo($"[HOMELABELS] tab '{buttonGo.name}': '{current}' -> '{label}' (TMPro)");
                    textProp.SetValue(target, label, null);
                }
            }
            else
            {
                Plugin.Log.LogDebug($"[HOMELABELS] tab '{buttonGo.name}' has no TMPro label — skipped");
            }
        }
        catch (Exception e)
        {
            Plugin.Log.LogWarning($"[HOMELABELS] TMPro label set failed on '{buttonGo.name}': {e.Message}");
        }
    }

    // Returns 1 when a disabled label was re-activated, else 0. Never changes
    // label text.
    private static int EnsureLabelVisible(GameObject buttonGo)
    {
        int fixedCount = 0;

        // uGUI path.
        var textType = Il2CppSystem.Type.GetType(typeof(Text).AssemblyQualifiedName);
        var texts = buttonGo.GetComponentsInChildren(textType, true);
        if (texts != null)
        {
            foreach (var c in texts)
            {
                var label = ((UnityEngine.Object)c).TryCast<Text>();
                if (label == null)
                    continue;
                if (!label.gameObject.activeSelf)
                {
                    label.gameObject.SetActive(true);
                    fixedCount = 1;
                }
            }
        }

        // TMPro fallback (no compile-time dependency).
        try
        {
            var tmproAsm = AppDomain.CurrentDomain.GetAssemblies()
                .FirstOrDefault(a => a.GetName().Name == "Unity.TextMeshPro");
            var tmproType = tmproAsm?.GetType("TMPro.TextMeshProUGUI");
            if (tmproType == null)
                return fixedCount;
            var getTexts = typeof(GameObject).GetMethod("GetComponentsInChildren",
                new[] { typeof(Type), typeof(bool) });
            var list = (IEnumerable)getTexts.Invoke(buttonGo, new object[] { tmproType, true });
            foreach (var c in list)
            {
                var comp = ((UnityEngine.Object)c).TryCast<Component>();
                if (comp == null || comp.gameObject.activeSelf)
                    continue;
                comp.gameObject.SetActive(true);
                fixedCount = 1;
            }
        }
        catch (Exception e)
        {
            Plugin.Log.LogDebug($"[HOMELABELS] TMPro label scan failed: {e.Message}");
        }

        return fixedCount;
    }

    // Logs each stock tab button's current label once, so the configured
    // labels can be verified against real in-game data instead of guesses.
    private static void LogTabLabels(System.Collections.Generic.List<GameObject> tabButtons)
    {
        foreach (var go in tabButtons)
        {
            var label = ReadLabel(go);
            Plugin.Log.LogInfo($"[HOMELABELS] tab '{go.name}' original label: '{label ?? "<none>"}'");
        }
    }

    private static string ReadLabel(GameObject go)
    {
        var textType = Il2CppSystem.Type.GetType(typeof(Text).AssemblyQualifiedName);
        var texts = go.GetComponentsInChildren(textType, true);
        if (texts != null)
        {
            foreach (var c in texts)
            {
                var label = ((UnityEngine.Object)c).TryCast<Text>();
                var txt = label?.text;
                if (!string.IsNullOrWhiteSpace(txt))
                    return txt.Trim();
            }
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
        catch { }
        return null;
    }

    private static bool HasButton(GameObject go)
    {
        var btnType = Il2CppSystem.Type.GetType(typeof(Button).AssemblyQualifiedName);
        return go.GetComponent(btnType) != null;
    }

    // Find the home tab row: the HomeTop5TabsModel type's GameObject, else any
    // active Transform with "top5tabs"/"tabrow" in the name, else a
    // home-ancestor row with >= 3 buttons.
    private static Transform FindTabRow()
    {
        var implType = FindTypeByName("HomeTop5TabsModel");
        if (implType != null)
        {
            var go = FirstGameObjectOfType(implType);
            if (go != null)
                return go.transform;
        }

        var find = typeof(UnityEngine.Object).GetMethod("FindObjectsOfType",
            new[] { typeof(Type) });
        if (find == null)
            return null;

        var all = (IEnumerable)find.Invoke(null, new object[] { typeof(Transform) });
        Transform fallback = null;
        foreach (var o in all)
        {
            var t = ((UnityEngine.Object)o).TryCast<Transform>();
            if (t == null)
                continue;
            var go = t.gameObject;
            if (go == null || !go.activeInHierarchy)
                continue;
            var n = t.name ?? "";
            if (n.IndexOf("top5tabs", StringComparison.OrdinalIgnoreCase) >= 0
                || n.IndexOf("tabrow", StringComparison.OrdinalIgnoreCase) >= 0
                || n.IndexOf("tabsmodel", StringComparison.OrdinalIgnoreCase) >= 0)
                return t;
            if (fallback == null && IsPlausibleTabRow(t))
                fallback = t;
        }
        return fallback;
    }

    private static bool IsPlausibleTabRow(Transform t)
    {
        bool underHome = false;
        for (var p = t.parent; p != null; p = p.parent)
        {
            if (p.name.IndexOf("Home", StringComparison.OrdinalIgnoreCase) >= 0)
            {
                underHome = true;
                break;
            }
        }
        if (!underHome)
            return false;

        int buttons = 0;
        foreach (Transform child in t)
        {
            if (child == null || child.gameObject == null)
                continue;
            if (child.gameObject.name.EndsWith(PlayCloneSuffix, StringComparison.Ordinal))
                continue;
            if (HasButton(child.gameObject))
                buttons++;
        }
        return buttons >= 3;
    }

    private static Type FindTypeByName(string name)
    {
        foreach (var asm in AppDomain.CurrentDomain.GetAssemblies())
        {
            Type[] types;
            try { types = asm.GetTypes(); }
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
}
