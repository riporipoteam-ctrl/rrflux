// Adds an "Ultra" graphics preset next to Low / Medium / High in
// Game Settings -> Visuals -> Graphics Quality, and applies it through
// Unity's QualitySettings API.
//
// Why this shape:
//  - The game's own quality enum (GHCCKFEJDOA) already contains Ultra = 3 and
//    the game's quality applier handles it, but the settings page only ships
//    three radio buttons (Low/Medium/High).
//  - The plugin finds the "High" button BY ITS VISIBLE LABEL ("High") — never
//    by GameObject or type name, which are obfuscated and re-roll per build.
//    The Graphics Quality row is the lowest ancestor of a "High" label whose
//    children also carry "Low" and "Medium" labels; the child carrying "High"
//    (and not Low/Medium) is the clone source.
//  - The plugin clones that button as its next sibling in the same row and
//    relabels the clone "Ultra". The clone keeps the game's own wiring (same
//    prefab, same click path, same radio-group behavior); the plugin only
//    re-points its serialized defaultQualityMapping at Ultra. Clicking it
//    therefore flows through the game's own SettingsModel.set_QualitySetting,
//    and the game's own page-open refresh highlights the button exactly when
//    the current quality is Ultra — so no manual refresh call is needed (the
//    old obfuscated refresh-method name was build-fragile anyway).
//  - On top of the game's Ultra level, the plugin boosts what this Unity
//    build's QualitySettings API exposes at runtime (see UltraQuality):
//    antiAliasing = 8, pixelLightCount = 4, shadowDistance = 150,
//    lodBias = 2.0. (masterTextureLimit has no runtime setter in this Unity
//    version — texture resolution stays driven by the game's Ultra level.)
//
// Hard rules honored:
//  - Medium stays the default: the plugin never touches default-quality logic.
//  - Low/Medium/High are untouched: the plugin only ADDS a button and only
//    overrides QualitySettings when the selected quality IS Ultra.
//
// Resolution is by visible label at runtime (never by obfuscated type or
// GameObject name). The Ultra enum int is re-read from the game's real enum
// at hook time (never trusted blindly from the spec). Harmony parameters are
// bound by the special __instance name or positionally (__0), never by
// obfuscated name.
//
// One knob, see [Graphics] in the .cfg:
//   Enable Ultra Graphics -> THE FEATURE (default true).
using System;
using System.Collections;
using System.Collections.Generic;
using System.Linq;
using System.Reflection;
using HarmonyLib;
using UnityEngine;
using UnityEngine.UI;

namespace RecNetPlugin.Patches;

internal static class UltraGraphicsPatch
{
    private const string MappingFieldName = "defaultQualityMapping";
    private const string NestedImplTypeName = "GraphicsQualityToggleImpl"; // last-resort fallback only
    private const string CloneNameSuffix = "_Ultra";
    private const int MaxAttempts = 10;
    private const int MaxClimbLevels = 12;

    // Visible labels. Compared case-insensitively, so "HIGH" / "high" match
    // too. Kept to the stock labels on purpose — no guessing spree.
    private const string HighLabel = "High";
    private const string MediumLabel = "Medium";
    private const string LowLabel = "Low";
    private const string UltraLabel = "Ultra";

    private static int _attempts;
    private static bool _buttonDone;
    private static bool _hookDone;
    private static bool _ultraActive;
    private static object _settingsModel; // cached from the set_QualitySetting postfix
    private static int _ultraEnumValue = UltraQuality.UltraEnumValue; // corrected from the live enum at hook time
    private static readonly Harmony _harmony = new Harmony("net.rec.plugin.ultra");

    // Called from Plugin.Load and again on each scene load: installs the
    // set_QualitySetting hook once, and retries the button clone until the
    // settings page exists.
    public static void Apply()
    {
        if (!Plugin.EnableUltraGraphics.Value)
            return;

        if (!_hookDone)
        {
            try { PatchQualitySetter(); }
            catch (Exception e)
            {
                Plugin.Log.LogWarning($"[ULTRA] hook failed: {e.Message}");
            }
        }

        if (_buttonDone || _attempts >= MaxAttempts)
        {
            ReapplyIfNeeded();
            return;
        }

        _attempts++;
        try { EnsureUltraButton(); }
        catch (Exception e)
        {
            Plugin.Log.LogWarning($"[ULTRA] attempt {_attempts} failed: {e.Message}");
        }

        ReapplyIfNeeded();

        if (_attempts >= MaxAttempts && !_buttonDone)
            Plugin.Log.LogWarning("[ULTRA] gave up adding the Ultra button — " +
                "the Graphics Quality row was never found. QualitySettings boost still applies when Ultra is selected.");
    }

    // Postfix on SettingsModel.set_QualitySetting (concrete class only — never
    // the abstract settings interface): when Ultra is selected, boost the
    // runtime QualitySettings on top of the game's own Ultra level.
    private static void PatchQualitySetter()
    {
        var target = FindQualitySetter();
        if (target == null)
        {
            Plugin.Log.LogWarning("[ULTRA] set_QualitySetting not found — will retry");
            return;
        }

        ResolveUltraEnumValue(target.GetParameters()[0].ParameterType);

        _harmony.Patch(target, postfix: new HarmonyMethod(
            typeof(UltraGraphicsPatch).GetMethod(nameof(SetQualityPostfix),
                BindingFlags.Static | BindingFlags.NonPublic)));
        _hookDone = true;
        Plugin.Log.LogInfo($"[ULTRA] hooked {target.DeclaringType.FullName}.{target.Name} (Ultra={_ultraEnumValue})");
    }

    private static MethodInfo FindQualitySetter()
    {
        MethodInfo fallback = null;
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
                var m = t.GetMethod("set_QualitySetting",
                    BindingFlags.Public | BindingFlags.NonPublic | BindingFlags.Instance);
                if (m == null)
                    continue;
                var ps = m.GetParameters();
                if (ps.Length != 1 || !ps[0].ParameterType.IsEnum)
                    continue;
                if (!Enum.GetNames(ps[0].ParameterType).Contains("Ultra"))
                    continue;
                if (t.Name == "SettingsModel")
                    return m; // concrete model the toggle writes to
                fallback ??= m;
            }
        }
        return fallback;
    }

    // Read the REAL Ultra int out of the game's quality enum instead of
    // trusting the spec constant blindly. If the enum ever moves Ultra, the
    // clone mapping and the postfix both follow automatically.
    private static void ResolveUltraEnumValue(Type enumType)
    {
        try
        {
            var names = Enum.GetNames(enumType);
            var values = Enum.GetValues(enumType);
            for (int i = 0; i < names.Length && i < values.Length; i++)
            {
                if (!names[i].Equals("Ultra", StringComparison.Ordinal))
                    continue;
                _ultraEnumValue = Convert.ToInt32(values.GetValue(i));
                if (_ultraEnumValue != UltraQuality.UltraEnumValue)
                    Plugin.Log.LogWarning($"[ULTRA] game enum Ultra = {_ultraEnumValue}, spec says {UltraQuality.UltraEnumValue} — using the game's value");
                else
                    Plugin.Log.LogInfo($"[ULTRA] game enum Ultra = {_ultraEnumValue} (matches spec)");
                return;
            }
            Plugin.Log.LogWarning("[ULTRA] quality enum has no Ultra member — using spec value " + UltraQuality.UltraEnumValue);
        }
        catch (Exception e)
        {
            Plugin.Log.LogWarning($"[ULTRA] could not read Ultra enum value: {e.Message}");
        }
    }

    private static void SetQualityPostfix(object __instance, object __0)
    {
        try
        {
            if (__instance != null)
                _settingsModel = __instance;
            int value = Convert.ToInt32(__0);
            if (value == _ultraEnumValue)
            {
                _ultraActive = true;
                ApplyUltraQualitySettings();
            }
            else
            {
                _ultraActive = false;
            }
        }
        catch (Exception e)
        {
            Plugin.Log.LogWarning($"[ULTRA] postfix failed: {e.Message}");
        }
    }

    public static void ApplyUltraQualitySettings()
    {
        QualitySettings.antiAliasing = UltraQuality.AntiAliasing;
        QualitySettings.pixelLightCount = UltraQuality.PixelLightCount;
        QualitySettings.shadowDistance = UltraQuality.ShadowDistance;
        QualitySettings.lodBias = UltraQuality.LodBias;
        Plugin.Log.LogInfo("[ULTRA] Ultra preset applied " +
            $"(AA={UltraQuality.AntiAliasing}, lights={UltraQuality.PixelLightCount}, " +
            $"shadowDist={UltraQuality.ShadowDistance}, lodBias={UltraQuality.LodBias})");
    }

    // Keeps the Ultra setting stuck across scene loads: re-checks the LIVE
    // model (not just the postfix flag), so a scene change that restores the
    // game's own Ultra level without calling the setter still gets our boost
    // re-applied on top of it.
    private static void ReapplyIfNeeded()
    {
        try
        {
            if (IsUltraCurrentlySelected())
                _ultraActive = true;
        }
        catch (Exception e)
        {
            Plugin.Log.LogDebug($"[ULTRA] selection check failed: {e.Message}");
        }

        if (_ultraActive)
        {
            try { ApplyUltraQualitySettings(); }
            catch (Exception e) { Plugin.Log.LogWarning($"[ULTRA] reapply failed: {e.Message}"); }
        }
    }

    private static bool IsUltraCurrentlySelected()
    {
        if (_settingsModel == null)
            return false;
        var getter = _settingsModel.GetType().GetMethod("get_QualitySetting",
            BindingFlags.Public | BindingFlags.NonPublic | BindingFlags.Instance);
        if (getter == null)
            return false;
        var current = getter.Invoke(_settingsModel, null);
        return Convert.ToInt32(current) == _ultraEnumValue;
    }

    // Clone the "High" radio button into an "Ultra" button in the same row.
    private static void EnsureUltraButton()
    {
        if (!FindHighButton(out var row, out var sourceGo, out bool confident))
        {
            Plugin.Log.LogDebug("[ULTRA] High graphics-quality toggle not found yet");
            return;
        }

        // Already added? (a sibling already carrying our suffix)
        for (int i = 0; i < row.childCount; i++)
        {
            var child = row.GetChild(i);
            if (child != null && child.name.EndsWith(CloneNameSuffix, StringComparison.Ordinal))
            {
                _buttonDone = true;
                Plugin.Log.LogDebug("[ULTRA] Ultra button already present");
                return;
            }
        }

        var mappingField = FindMappingField(sourceGo);
        if (mappingField == null)
        {
            Plugin.Log.LogWarning("[ULTRA] defaultQualityMapping field not found — cannot aim the clone at Ultra");
            if (confident)
                _attempts = MaxAttempts; // structural mismatch on the real row — stop retrying
            // (low-confidence fallback: keep retrying, the real row may appear later)
            return;
        }

        if (CloneAsUltra(sourceGo, row, mappingField))
            _buttonDone = true;
    }

    // Locate the "High" quality button by its VISIBLE LABEL (never by
    // GameObject/type name). Primary: the lowest ancestor of a "High" label
    // whose direct children also carry "Low" and "Medium" labels — that
    // ancestor IS the Graphics Quality row, and the child carrying "High"
    // (but not Low/Medium) is the button to clone. Fallback: the nearest
    // clickable (Button/Toggle) ancestor of a "High" label, for label sources
    // the sibling check can't see. `confident` is true only for the primary
    // path; the fallback must never burn the retry budget on a wrong button.
    private static bool FindHighButton(out Transform row, out GameObject sourceGo, out bool confident)
    {
        row = null;
        sourceGo = null;
        confident = false;

        var labelObjects = FindLabelObjects(HighLabel);
        foreach (var labelGo in labelObjects)
        {
            var t = labelGo.transform;
            int levels = 0;
            while (t != null && levels++ < MaxClimbLevels)
            {
                if (RowHasQualityLabels(t, out var highChild) && highChild != null)
                {
                    row = t;
                    sourceGo = highChild;
                    confident = true;
                    Plugin.Log.LogInfo($"[ULTRA] found Graphics Quality row '{t.name}', High button '{highChild.name}'");
                    return true;
                }
                t = t.parent;
            }
        }

        foreach (var labelGo in labelObjects)
        {
            if (FindClickableAncestor(labelGo, out var buttonGo) && buttonGo.transform.parent != null)
            {
                row = buttonGo.transform.parent;
                sourceGo = buttonGo;
                Plugin.Log.LogInfo($"[ULTRA] found High button by clickable-ancestor fallback: '{buttonGo.name}'");
                return true;
            }
        }

        return false;
    }

    // True when the row's direct children carry "Low" and "Medium" labels
    // (each checked through its own subtree). Outputs the child carrying
    // "High" — preferring the one whose subtree does NOT also carry
    // Low/Medium (so a stray "High" caption inside another cell can't win).
    private static bool RowHasQualityLabels(Transform row, out GameObject highChild)
    {
        highChild = null;
        GameObject highFallback = null;
        bool low = false, medium = false;
        for (int i = 0; i < row.childCount; i++)
        {
            var child = row.GetChild(i);
            if (child == null)
                continue;
            bool hasLow = SubtreeHasLabel(child, LowLabel);
            bool hasMedium = SubtreeHasLabel(child, MediumLabel);
            bool hasHigh = SubtreeHasLabel(child, HighLabel);
            if (hasLow) low = true;
            if (hasMedium) medium = true;
            if (hasHigh)
            {
                if (!hasLow && !hasMedium)
                    highChild = child.gameObject;
                else if (highFallback == null)
                    highFallback = child.gameObject;
            }
            if (low && medium && highChild != null)
                return true;
        }
        highChild ??= highFallback;
        return low && medium && highChild != null;
    }

    // Nearest ancestor (up to MaxClimbLevels) carrying a uGUI Button or Toggle.
    private static bool FindClickableAncestor(GameObject labelGo, out GameObject buttonGo)
    {
        buttonGo = null;
        Il2CppSystem.Type buttonType;
        Il2CppSystem.Type toggleType;
        try
        {
            // be.788: GetComponent requires Il2CppSystem.Type, not System.Type.
            buttonType = Il2CppSystem.Type.GetType(typeof(Button).AssemblyQualifiedName);
            toggleType = Il2CppSystem.Type.GetType(typeof(Toggle).AssemblyQualifiedName);
        }
        catch
        {
            return false;
        }

        var t = labelGo.transform;
        int levels = 0;
        while (t != null && levels++ < MaxClimbLevels)
        {
            var go = t.gameObject;
            try
            {
                Component hit = go.GetComponent(buttonType);
                hit ??= go.GetComponent(toggleType);
                if (hit != null)
                {
                    buttonGo = go;
                    return true;
                }
            }
            catch
            {
                // keep climbing
            }
            t = t.parent;
        }
        return false;
    }

    // Find the field that aims a quality button at its preset. Primary: scan
    // the source button's own components for an instance field named
    // defaultQualityMapping (no type names involved). Fallback: the private
    // nested impl type on SettingsModelController.
    private static FieldInfo FindMappingField(GameObject sourceGo)
    {
        try
        {
            foreach (var o in GetAllComponents(sourceGo))
            {
                // Il2Cpp downcasts must go through TryCast, never a direct cast.
                var comp = ((UnityEngine.Object)o).TryCast<Component>();
                if (comp == null)
                    continue;
                var field = FindFieldInHierarchy(comp.GetType(), MappingFieldName);
                if (field != null && field.FieldType.IsEnum)
                    return field;
            }
        }
        catch (Exception e)
        {
            Plugin.Log.LogDebug($"[ULTRA] component scan failed: {e.Message}");
        }

        try
        {
            var toggleType = typeof(SettingsModelController)
                .GetNestedType(NestedImplTypeName, BindingFlags.NonPublic);
            var field = toggleType?.GetField(MappingFieldName,
                BindingFlags.Public | BindingFlags.NonPublic | BindingFlags.Instance);
            if (field != null)
                Plugin.Log.LogDebug("[ULTRA] mapping field resolved via nested impl type fallback");
            return field;
        }
        catch (Exception e)
        {
            Plugin.Log.LogDebug($"[ULTRA] nested-type fallback failed: {e.Message}");
            return null;
        }
    }

    private static FieldInfo FindFieldInHierarchy(Type type, string name)
    {
        for (var t = type; t != null; t = t.BaseType)
        {
            var f = t.GetField(name,
                BindingFlags.Public | BindingFlags.NonPublic | BindingFlags.Instance | BindingFlags.DeclaredOnly);
            if (f != null)
                return f;
        }
        return null;
    }

    private static IEnumerable GetAllComponents(GameObject go)
    {
        var compType = Il2CppSystem.Type.GetType(typeof(Component).AssemblyQualifiedName);
        return (IEnumerable)go.GetComponents(compType);
    }

    // Clone the High button as its next sibling, aim it at Ultra, relabel it.
    // Returns false when the clone had to be discarded (caller retries).
    private static bool CloneAsUltra(GameObject sourceGo, Transform row, FieldInfo mappingField)
    {
        var cloneObj = UnityEngine.Object.Instantiate(sourceGo);
        var cloneGo = cloneObj.TryCast<GameObject>();
        if (cloneGo == null)
        {
            Plugin.Log.LogWarning("[ULTRA] clone failed (not a GameObject)");
            return false;
        }

        cloneGo.transform.SetParent(row, false);
        cloneGo.transform.SetSiblingIndex(sourceGo.transform.GetSiblingIndex() + 1);
        cloneGo.name = sourceGo.name + CloneNameSuffix;

        // Aim the clone at Ultra instead of High. The Ultra int was read from
        // the game's own quality enum at hook time — never a blind constant.
        // be.788: GetComponent requires Il2CppSystem.Type, not System.Type.
        var implIl2CppType = Il2CppSystem.Type.GetType(mappingField.DeclaringType.AssemblyQualifiedName);
        Component cloneImpl = cloneGo.GetComponent(implIl2CppType);
        Component sourceImpl = sourceGo.GetComponent(implIl2CppType);
        if (cloneImpl == null || sourceImpl == null)
        {
            Plugin.Log.LogWarning("[ULTRA] clone lost its toggle component — discarding clone");
            UnityEngine.Object.Destroy(cloneGo);
            return false;
        }
        mappingField.SetValue(cloneImpl,
            Enum.ToObject(mappingField.FieldType, _ultraEnumValue));

        // Relabel "High" -> "Ultra" (uGUI Text; TMPro fallback via reflection).
        RelabelClone(cloneGo);

        // Wire the runtime model (set by the binding system, not serialized)
        // so the game's own click path writes Ultra into SettingsModel.
        // The game's own page-open refresh highlights the clone exactly when
        // the current quality is Ultra, so no manual refresh call is needed.
        CopyProperty(sourceImpl, cloneImpl, "Model");
        CopyProperty(sourceImpl, cloneImpl, "Controller");
        WarnIfModelMissing(cloneImpl);

        Plugin.Log.LogInfo($"[ULTRA] added Ultra button next to '{sourceGo.name}' (Ultra={_ultraEnumValue})");
        return true;
    }

    private static void CopyProperty(Component source, Component cloneImpl, string name)
    {
        try
        {
            var prop = source.GetType().GetProperty(name,
                BindingFlags.Public | BindingFlags.NonPublic | BindingFlags.Instance);
            var setter = prop?.GetSetMethod(true);
            if (setter == null)
                return;
            setter.Invoke(cloneImpl, new[] { prop.GetValue(source, null) });
        }
        catch (Exception e)
        {
            Plugin.Log.LogDebug($"[ULTRA] could not copy {name}: {e.Message}");
        }
    }

    // A null Model means the clone's clicks can't reach SettingsModel until
    // the settings page is rebuilt — worth a loud warning, not a silent bug.
    private static void WarnIfModelMissing(Component cloneImpl)
    {
        try
        {
            var prop = cloneImpl.GetType().GetProperty("Model",
                BindingFlags.Public | BindingFlags.NonPublic | BindingFlags.Instance);
            if (prop != null && prop.GetValue(cloneImpl, null) == null)
                Plugin.Log.LogWarning("[ULTRA] clone has no Model yet — its clicks may not register until the settings page is reopened");
        }
        catch (Exception e)
        {
            Plugin.Log.LogDebug($"[ULTRA] Model check failed: {e.Message}");
        }
    }

    // Relabel the clone: the first label reading "High" becomes "Ultra".
    private static void RelabelClone(GameObject cloneGo)
    {
        // uGUI path
        // be.788: GetComponentsInChildren requires Il2CppSystem.Type, not System.Type.
        var textType = Il2CppSystem.Type.GetType(typeof(Text).AssemblyQualifiedName);
        var getTexts = typeof(GameObject).GetMethod("GetComponentsInChildren",
            new[] { typeof(Type), typeof(bool) });
        if (getTexts != null)
        {
            var dstTexts = (IEnumerable)getTexts.Invoke(cloneGo, new object[] { textType, true });
            if (dstTexts != null)
            {
                foreach (var c in dstTexts)
                {
                    var label = ((UnityEngine.Object)c).TryCast<Text>();
                    if (label == null)
                        continue;
                    if (IsLabelMatch(label.text, HighLabel))
                    {
                        label.text = UltraLabel;
                        return;
                    }
                }
            }
        }

        // TMPro fallback (no compile-time dependency)
        try
        {
            var tmproAsm = AppDomain.CurrentDomain.GetAssemblies()
                .FirstOrDefault(a => a.GetName().Name == "Unity.TextMeshPro");
            var tmproType = tmproAsm?.GetType("TMPro.TextMeshProUGUI");
            if (tmproType == null || getTexts == null)
            {
                Plugin.Log.LogWarning("[ULTRA] no label Text found on the High button — Ultra button keeps its label");
                return;
            }
            var dst = (IEnumerable)getTexts.Invoke(cloneGo, new object[] { tmproType, true });
            var textProp = tmproType.GetProperty("text");
            foreach (var c in dst)
            {
                var txt = (string)textProp.GetValue(c, null);
                if (IsLabelMatch(txt, HighLabel))
                {
                    textProp.SetValue(c, UltraLabel, null);
                    return;
                }
            }
            Plugin.Log.LogWarning("[ULTRA] no matching label found on the clone — Ultra button keeps its label");
        }
        catch (Exception e)
        {
            Plugin.Log.LogWarning($"[ULTRA] TMPro relabel failed: {e.Message}");
        }
    }

    private static bool IsLabelMatch(string text, string label) =>
        !string.IsNullOrWhiteSpace(text) &&
        text.Trim().Equals(label, StringComparison.OrdinalIgnoreCase);

    // Every GameObject in the scene carrying a visible label `label`
    // (uGUI Text, then TMPro). Case-insensitive: matches "High"/"HIGH"/"high".
    private static List<GameObject> FindLabelObjects(string label)
    {
        var result = new List<GameObject>();
        var seen = new HashSet<int>();
        var find = typeof(UnityEngine.Object).GetMethod("FindObjectsOfType",
            new[] { typeof(Type) });
        if (find == null)
            return result;

        void AddMatches(IEnumerable all, Func<object, string> readText)
        {
            if (all == null)
                return;
            foreach (var o in all)
            {
                try
                {
                    var comp = ((UnityEngine.Object)o).TryCast<Component>();
                    if (comp == null)
                        continue;
                    var go = comp.gameObject;
                    if (go == null || !seen.Add(go.GetInstanceID()))
                        continue;
                    if (IsLabelMatch(readText(o), label))
                        result.Add(go);
                }
                catch
                {
                    // keep scanning
                }
            }
        }

        try
        {
            var textType = Il2CppSystem.Type.GetType(typeof(Text).AssemblyQualifiedName);
            AddMatches((IEnumerable)find.Invoke(null, new object[] { textType }),
                o => ((UnityEngine.Object)o).TryCast<Text>()?.text);
        }
        catch (Exception e)
        {
            Plugin.Log.LogDebug($"[ULTRA] uGUI label scan failed: {e.Message}");
        }

        try
        {
            var tmproAsm = AppDomain.CurrentDomain.GetAssemblies()
                .FirstOrDefault(a => a.GetName().Name == "Unity.TextMeshPro");
            var tmproType = tmproAsm?.GetType("TMPro.TextMeshProUGUI");
            if (tmproType != null)
            {
                var textProp = tmproType.GetProperty("text");
                AddMatches((IEnumerable)find.Invoke(null, new object[] { tmproType }),
                    o => (string)textProp.GetValue(o, null));
            }
        }
        catch (Exception e)
        {
            Plugin.Log.LogDebug($"[ULTRA] TMPro label scan failed: {e.Message}");
        }

        return result;
    }

    // True when any uGUI Text or TMPro label under `root` reads `label`.
    private static bool SubtreeHasLabel(Transform root, string label)
    {
        if (root == null)
            return false;
        var go = root.gameObject;
        if (go == null)
            return false;

        // be.788: GetComponentsInChildren requires Il2CppSystem.Type, not System.Type.
        var getTexts = typeof(GameObject).GetMethod("GetComponentsInChildren",
            new[] { typeof(Type), typeof(bool) });
        if (getTexts == null)
            return false;

        try
        {
            var textType = Il2CppSystem.Type.GetType(typeof(Text).AssemblyQualifiedName);
            var texts = (IEnumerable)getTexts.Invoke(go, new object[] { textType, true });
            if (texts != null)
            {
                foreach (var c in texts)
                {
                    var txt = ((UnityEngine.Object)c).TryCast<Text>()?.text;
                    if (IsLabelMatch(txt, label))
                        return true;
                }
            }
        }
        catch (Exception e)
        {
            Plugin.Log.LogDebug($"[ULTRA] uGUI subtree scan failed: {e.Message}");
        }

        try
        {
            var tmproAsm = AppDomain.CurrentDomain.GetAssemblies()
                .FirstOrDefault(a => a.GetName().Name == "Unity.TextMeshPro");
            var tmproType = tmproAsm?.GetType("TMPro.TextMeshProUGUI");
            if (tmproType == null)
                return false;
            var list = (IEnumerable)getTexts.Invoke(go, new object[] { tmproType, true });
            if (list == null)
                return false;
            var textProp = tmproType.GetProperty("text");
            foreach (var c in list)
            {
                var txt = (string)textProp.GetValue(c, null);
                if (IsLabelMatch(txt, label))
                    return true;
            }
        }
        catch (Exception e)
        {
            Plugin.Log.LogDebug($"[ULTRA] TMPro subtree scan failed: {e.Message}");
        }

        return false;
    }
}
