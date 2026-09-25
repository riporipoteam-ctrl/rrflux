// Adds an "Ultra" graphics preset next to Low / Medium / High in
// Game Settings -> Visuals -> Graphics Quality, and applies it through
// Unity's QualitySettings API.
//
// Why this shape:
//  - The game's own quality enum (GHCCKFEJDOA) already contains Ultra = 3 and
//    the game's quality applier handles it, but the settings page only ships
//    three radio buttons (Low/Medium/High). Each button is a
//    SettingsModelController.GraphicsQualityToggleImpl whose serialized
//    defaultQualityMapping picks the enum value it writes.
//  - The plugin clones the "High" button at runtime (same prefab, same style),
//    points the clone at Ultra (3) and relabels it "Ultra". The game's own
//    click/refresh logic then works unchanged: clicking writes
//    QualitySetting = Ultra, and the refresh highlights the button exactly
//    when the current quality is Ultra.
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
// Resolution is by name at runtime (never by obfuscated type name): the toggle
// impl is a private nested type of the unobfuscated SettingsModelController,
// and set_QualitySetting keeps its real name. Harmony parameters are bound
// positionally (__0), never by obfuscated name.
//
// One knob, see [Graphics] in the .cfg:
//   Enable Ultra Graphics -> THE FEATURE (default true).
using System;
using System.Linq;
using System.Reflection;
using HarmonyLib;
using UnityEngine;
using UnityEngine.UI;

namespace RecNetPlugin.Patches;

internal static class UltraGraphicsPatch
{
    private const string ToggleImplName = "GraphicsQualityToggleImpl";
    private const string MappingFieldName = "defaultQualityMapping";
    private const string CloneNameSuffix = "_Ultra";
    private const int MaxAttempts = 10;

    private static int _attempts;
    private static bool _buttonDone;
    private static bool _hookDone;
    private static bool _ultraActive;
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

        _harmony.Patch(target, postfix: new HarmonyMethod(
            typeof(UltraGraphicsPatch).GetMethod(nameof(SetQualityPostfix),
                BindingFlags.Static | BindingFlags.NonPublic)));
        _hookDone = true;
        Plugin.Log.LogInfo($"[ULTRA] hooked {target.DeclaringType.FullName}.{target.Name}");
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

    private static void SetQualityPostfix(object __0)
    {
        try
        {
            int value = Convert.ToInt32(__0);
            if (value == UltraQuality.UltraEnumValue)
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

    private static void ReapplyIfNeeded()
    {
        if (_ultraActive)
        {
            try { ApplyUltraQualitySettings(); }
            catch (Exception e) { Plugin.Log.LogWarning($"[ULTRA] reapply failed: {e.Message}"); }
        }
    }

    // Clone the "High" radio button into an "Ultra" button in the same row.
    private static void EnsureUltraButton()
    {
        var toggleType = typeof(SettingsModelController)
            .GetNestedType(ToggleImplName, BindingFlags.NonPublic);
        if (toggleType == null)
        {
            Plugin.Log.LogDebug("[ULTRA] GraphicsQualityToggleImpl not found yet");
            return;
        }

        var find = typeof(UnityEngine.Object).GetMethod("FindObjectsOfType",
            new[] { typeof(Type) });
        var all = (System.Collections.IEnumerable)find.Invoke(null, new object[] { toggleType });

        // Group toggles by their parent row.
        var groups = new System.Collections.Generic.Dictionary<Transform,
            System.Collections.Generic.List<Component>>();
        foreach (var o in all)
        {
            // Il2Cpp downcasts must go through TryCast, never a direct cast.
            var comp = ((UnityEngine.Object)o).TryCast<Component>();
            if (comp == null)
                continue;
            var parent = comp.transform.parent;
            if (parent == null)
                continue;
            if (!groups.TryGetValue(parent, out var list))
                groups[parent] = list = new System.Collections.Generic.List<Component>();
            list.Add(comp);
        }

        var mappingField = toggleType.GetField(MappingFieldName,
            BindingFlags.Public | BindingFlags.NonPublic | BindingFlags.Instance);
        if (mappingField == null)
        {
            Plugin.Log.LogWarning("[ULTRA] defaultQualityMapping field not found");
            _attempts = MaxAttempts; // structural mismatch — stop retrying
            return;
        }

        foreach (var kv in groups)
        {
            if (kv.Value.Count < 3)
                continue; // not the Low/Medium/High row

            // Already added? (a toggle in this row already mapped to Ultra)
            bool hasUltra = kv.Value.Any(c =>
                Convert.ToInt32(mappingField.GetValue(c)) == UltraQuality.UltraEnumValue &&
                c.gameObject.name.Contains("Ultra"));
            if (hasUltra)
            {
                _buttonDone = true;
                return;
            }

            // "High" = the toggle with the highest mapped quality value.
            var source = kv.Value
                .OrderByDescending(c => Convert.ToInt32(mappingField.GetValue(c)))
                .First();
            CloneAsUltra(source, kv.Key, toggleType, mappingField);
            _buttonDone = true;
            return;
        }

        Plugin.Log.LogDebug("[ULTRA] Graphics Quality row not found yet");
    }

    private static void CloneAsUltra(Component source, Transform row, Type toggleType, FieldInfo mappingField)
    {
        var sourceGo = source.gameObject;

        var cloneObj = UnityEngine.Object.Instantiate(sourceGo);
        var cloneGo = cloneObj.TryCast<GameObject>();
        if (cloneGo == null)
        {
            Plugin.Log.LogWarning("[ULTRA] clone failed (not a GameObject)");
            return;
        }

        cloneGo.transform.SetParent(row, false);
        cloneGo.transform.SetSiblingIndex(sourceGo.transform.GetSiblingIndex() + 1);
        cloneGo.name = sourceGo.name + CloneNameSuffix;

        // Point the clone at Ultra instead of High.
        // be.788: GetComponent requires Il2CppSystem.Type, not System.Type.
        var cloneImpl = cloneGo.GetComponent(Il2CppSystem.Type.GetType(toggleType.AssemblyQualifiedName));
        mappingField.SetValue(cloneImpl,
            Enum.ToObject(mappingField.FieldType, UltraQuality.UltraEnumValue));

        // Relabel "High" -> "Ultra" (uGUI Text; TMPro fallback via reflection).
        RelabelClone(sourceGo, cloneGo);

        // Wire the runtime model (set by the binding system, not serialized)
        // and refresh so the highlight state is correct immediately.
        CopyProperty(source, cloneImpl, "Model");
        CopyProperty(source, cloneImpl, "Controller");
        var refresh = toggleType.GetMethod("NGFFCLPHKJN",
            BindingFlags.Public | BindingFlags.NonPublic | BindingFlags.Instance);
        refresh?.Invoke(cloneImpl, null);

        Plugin.Log.LogInfo($"[ULTRA] added Ultra button next to '{sourceGo.name}'");
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

    private static void RelabelClone(GameObject sourceGo, GameObject cloneGo)
    {
        // uGUI path
        // be.788: GetComponentsInChildren requires Il2CppSystem.Type, not System.Type.
        var textType = Il2CppSystem.Type.GetType(typeof(Text).AssemblyQualifiedName);
        var srcTexts = sourceGo.GetComponentsInChildren(textType, true);
        if (srcTexts != null && srcTexts.Length > 0)
        {
            var dstTexts = cloneGo.GetComponentsInChildren(textType, true);
            int labelIdx = FindLabelIndex(srcTexts);
            if (labelIdx >= 0 && labelIdx < dstTexts.Length)
            {
                var label = dstTexts[labelIdx].TryCast<Text>();
                if (label != null)
                {
                    label.text = "Ultra";
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
                Plugin.Log.LogWarning("[ULTRA] no label Text found on the High button — Ultra button keeps its label");
                return;
            }
            var getTexts = typeof(GameObject).GetMethod("GetComponentsInChildren",
                new[] { typeof(Type), typeof(bool) });
            var src = (System.Collections.IEnumerable)getTexts.Invoke(sourceGo,
                new object[] { tmproType, true });
            var dst = (System.Collections.IEnumerable)getTexts.Invoke(cloneGo,
                new object[] { tmproType, true });
            var srcList = src.Cast<object>().ToList();
            var dstList = dst.Cast<object>().ToList();
            var textProp = tmproType.GetProperty("text");
            int labelIdx = -1;
            for (int i = 0; i < srcList.Count; i++)
            {
                var txt = (string)textProp.GetValue(srcList[i], null);
                if (!string.IsNullOrWhiteSpace(txt))
                {
                    if (txt.Trim().Equals("High", StringComparison.OrdinalIgnoreCase))
                    {
                        labelIdx = i;
                        break;
                    }
                    if (labelIdx < 0)
                        labelIdx = i;
                }
            }
            if (labelIdx >= 0 && labelIdx < dstList.Count)
                textProp.SetValue(dstList[labelIdx], "Ultra", null);
        }
        catch (Exception e)
        {
            Plugin.Log.LogWarning($"[ULTRA] TMPro relabel failed: {e.Message}");
        }
    }

    private static int FindLabelIndex(Component[] texts)
    {
        int fallback = -1;
        for (int i = 0; i < texts.Length; i++)
        {
            var txt = texts[i].TryCast<Text>()?.text;
            if (string.IsNullOrWhiteSpace(txt))
                continue;
            if (txt.Trim().Equals("High", StringComparison.OrdinalIgnoreCase))
                return i;
            if (fallback < 0)
                fallback = i;
        }
        return fallback;
    }
}
