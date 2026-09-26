// Ultra-via-High: selecting "High" in Game Settings -> Visuals -> Graphics
// Quality makes the game render with Ultra quality — but the UI still shows
// the stock "Low / Medium / High" labels. No extra "Ultra" button is added.
//
// How it works:
//  - The plugin hooks the concrete SettingsModel's set_QualitySetting (the
//    real method name in this build's C# layer; the parameter is the game's
//    quality enum, which contains Ultra = 3 — re-read from the live enum at
//    hook time, never trusted blindly from the spec).
//  - At startup it locates the Graphics Quality row BY VISIBLE LABEL
//    ("High"/"Medium"/"Low" — never by GameObject or type name, which are
//    obfuscated) and reads which enum value the "High" toggle is wired to
//    (its QualitySettingForPlatform / defaultQualityMapping).
//  - Postfix: when the incoming value is High's mapped value, the plugin
//    re-invokes the setter with the game's Ultra value (recursion-guarded),
//    so the game's OWN quality system (RecRoom.Core.Quality listeners, the
//    game's Ultra applier) drives every subsystem at Ultra — this is what
//    makes "the High option BE Ultra". On top of that it boosts the Unity
//    QualitySettings knobs that this Unity build exposes at runtime
//    (see UltraQuality): antiAliasing = 8, pixelLightCount = 4,
//    shadowDistance = 150, lodBias = 2.0.
//  - If High already maps to Ultra (mapping == 3), no redirect is needed —
//    the postfix just applies the boost.
//  - Scene changes can restore the game's own level without calling the
//    setter, so each Apply() re-checks the LIVE model and re-applies the
//    boost/redirect when High (or Ultra) is currently selected.
//
// Why not just the QualitySettings boost (the old approach)?
//  - This Unity build only exposes FOUR settable QualitySettings at runtime
//    (verified against the 2023-04-14 dump: pixelLightCount, shadowDistance,
//    lodBias, antiAliasing; everything else — shadowCascades,
//    shadowResolution, anisotropicFiltering, masterTextureLimit — is
//    get-only). Redirecting to the game's own Ultra level additionally
//    drives all of the game's per-subsystem quality controllers at Ultra,
//    which is where the real "better lighting / realistic" difference comes
//    from.
//
// Hard rules honored:
//  - Medium stays the default: the plugin never touches default-quality logic.
//  - Low/Medium are untouched: only High's mapped value is ever redirected.
//  - Resolution is by visible label at runtime (never by obfuscated type or
//    GameObject name). Harmony parameters are bound by the special
//    __instance name or positionally (__0), never by obfuscated name.
//  - IL2CPP rules: no direct delegate construction (not needed here),
//    downcasts via .TryCast<T>(), GetComponent via Il2CppSystem.Type.
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
    private const string PlatformMappingPropertyName = "QualitySettingForPlatform";
    private const int MaxClimbLevels = 12;

    // Retry budget is TIME-based, not attempt-based. The Settings page is
    // built lazily the first time the user opens it — long after
    // SceneManager.sceneLoaded has fired — so a fixed attempt count burns
    // out before the page exists. Apply() is re-invoked on a 2-second timer
    // by UiDiscoveryRetry (plus every scene load) until IsSettled.
    private static readonly TimeSpan MaxRetryTime = TimeSpan.FromMinutes(10);
    private static readonly TimeSpan ProgressLogInterval = TimeSpan.FromSeconds(60);

    // Visible labels. Compared case-insensitively, so "HIGH" / "high" match
    // too. Kept to the stock labels on purpose — no guessing spree.
    private const string HighLabel = "High";
    private const string MediumLabel = "Medium";
    private const string LowLabel = "Low";

    private static int _attempts;
    private static bool _hookDone;
    private static bool _highResolved;
    private static bool _gaveUp;
    private static DateTime _firstAttemptUtc = DateTime.MinValue;
    private static DateTime _lastProgressLogUtc = DateTime.MinValue;
    private static DateTime _lastHookLogUtc = DateTime.MinValue;
    private static int _highEnumValue = -1;
    private static int _ultraEnumValue = UltraQuality.UltraEnumValue; // corrected from the live enum at hook time
    private static Type _qualityEnumType;
    private static MethodInfo _qualitySetter;
    private static object _settingsModel; // cached from the set_QualitySetting postfix
    private static bool _redirecting; // recursion guard for the High -> Ultra re-invoke
    private static readonly Harmony _harmony = new Harmony("net.rec.plugin.ultra");

    // Discovery complete: hook installed AND the High toggle's mapped
    // value resolved. True once there is nothing left to do: feature
    // disabled in config, discovery complete, or the retry budget ran out.
    // The UiDiscoveryRetry driver stops ticking this patch once settled.
    internal static bool IsSettled =>
        !Plugin.EnableUltraGraphics.Value || _gaveUp || (_hookDone && _highResolved);

    // Called from Plugin.Load, on each scene load, and on a 2-second timer
    // by UiDiscoveryRetry: installs the set_QualitySetting hook once,
    // resolves which enum value the "High" toggle is wired to (retried until
    // the settings page exists), and re-applies the boost when High/Ultra is
    // the live selection.
    public static void Apply()
    {
        if (!Plugin.EnableUltraGraphics.Value)
            return;

        if (_gaveUp)
        {
            ReapplyIfNeeded();
            return;
        }

        if (_firstAttemptUtc == DateTime.MinValue)
        {
            _firstAttemptUtc = DateTime.UtcNow;
            Plugin.Log.LogInfo("[ULTRA] Ultra-via-High discovery started " +
                $"(retry budget {MaxRetryTime.TotalMinutes:F0} minutes)");
        }

        if (DateTime.UtcNow - _firstAttemptUtc >= MaxRetryTime && !(_hookDone && _highResolved))
        {
            _gaveUp = true;
            LogGiveUp();
            return;
        }

        if (!_hookDone)
        {
            try { PatchQualitySetter(); }
            catch (Exception e)
            {
                Plugin.Log.LogWarning($"[ULTRA] hook failed: {e.Message}");
            }
        }

        if (!_highResolved)
        {
            _attempts++;
            try { ResolveHighMapping(); }
            catch (Exception e)
            {
                Plugin.Log.LogWarning($"[ULTRA] High-mapping resolution attempt {_attempts} failed: {e.Message}");
            }
        }

        ReapplyIfNeeded();
        MaybeLogProgress();
    }

    // Throttled progress report: the per-attempt "not found yet" notes stay
    // at Debug, but every 60 seconds a Warning summarizes what the search is
    // (not) finding so a user reading the log can see the patch is alive and
    // what the scene actually contains.
    private static void MaybeLogProgress()
    {
        if (IsSettled)
            return;
        var now = DateTime.UtcNow;
        if (now - _lastProgressLogUtc < ProgressLogInterval)
            return;
        _lastProgressLogUtc = now;
        var elapsed = now - _firstAttemptUtc;
        Plugin.Log.LogWarning("[ULTRA] Ultra-via-High not ready yet " +
            $"(attempt {_attempts}, {elapsed.TotalSeconds:F0}s elapsed; " +
            $"hook installed: {_hookDone}, High mapping resolved: {_highResolved}). " +
            DescribeSettings());
    }

    // Final give-up: Warning level, states exactly what was searched for,
    // how many attempts ran, and what the scene actually contained.
    private static void LogGiveUp()
    {
        var elapsed = DateTime.UtcNow - _firstAttemptUtc;
        Plugin.Log.LogWarning("[ULTRA] GAVE UP Ultra-via-High discovery after " +
            $"{_attempts} attempts over {elapsed.TotalMinutes:F1} minutes " +
            $"(hook installed: {_hookDone}, High mapping resolved: {_highResolved}). " +
            "Searched: (1) SettingsModel.set_QualitySetting — the concrete method with a " +
            "single enum parameter containing 'Ultra' — for the Harmony hook; " +
            "(2) the Settings -> Visuals -> Graphics Quality row BY VISIBLE LABEL: the lowest " +
            "ancestor of a 'High' label whose direct children also carry 'Low' and 'Medium' " +
            "labels, then read the High toggle's defaultQualityMapping/QualitySettingForPlatform. " +
            "Scene contents at give-up: " + DescribeSettings());
    }

    // Diagnostic snapshot: how many Low/Medium/High labels exist at all, and
    // whether anything looking like the Settings page is present. Zeros
    // across the board means the Settings page hasn't been built yet (it is
    // built lazily on first open) — not a code bug.
    private static string DescribeSettings()
    {
        try
        {
            int high = FindLabelObjects(HighLabel).Count;
            int low = FindLabelObjects(LowLabel).Count;
            int medium = FindLabelObjects(MediumLabel).Count;
            int gfx = FindLabelObjects("Graphics Quality").Count + FindLabelObjects("Graphics").Count;
            int visuals = FindLabelObjects("Visuals").Count;
            return $"'High' labels: {high}, 'Low': {low}, 'Medium': {medium}, " +
                $"'Graphics (Quality)' labels: {gfx}, 'Visuals' labels: {visuals}.";
        }
        catch (Exception e)
        {
            return $"settings scan failed: {e.Message}";
        }
    }

    // Postfix on SettingsModel.set_QualitySetting (concrete class only — never
    // the abstract settings interface): when High is selected, redirect to
    // the game's own Ultra level and boost the runtime QualitySettings on top.
    private static void PatchQualitySetter()
    {
        var target = FindQualitySetter();
        if (target == null)
        {
            // Throttled: Apply() now ticks every 2s via UiDiscoveryRetry, so
            // an unthrottled Warning here would spam the log until the
            // declaring assembly loads.
            var now = DateTime.UtcNow;
            if (now - _lastHookLogUtc >= ProgressLogInterval)
            {
                _lastHookLogUtc = now;
                Plugin.Log.LogWarning("[ULTRA] set_QualitySetting not found yet — will keep retrying " +
                    "(the declaring type may live in an assembly that isn't loaded yet)");
            }
            else
            {
                Plugin.Log.LogDebug("[ULTRA] set_QualitySetting not found yet");
            }
            return;
        }

        _qualitySetter = target;
        _qualityEnumType = target.GetParameters()[0].ParameterType;
        ResolveUltraEnumValue(_qualityEnumType);

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
    // redirect and the postfix both follow automatically.
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
            if (_redirecting)
                return; // our own High -> Ultra re-invoke: don't loop
            int value = Convert.ToInt32(__0);
            if (_highResolved && value == _highEnumValue && value != _ultraEnumValue)
            {
                RedirectHighToUltra(__instance);
                ApplyUltraQualitySettings();
            }
            else if (value == _ultraEnumValue)
            {
                ApplyUltraQualitySettings();
            }
        }
        catch (Exception e)
        {
            Plugin.Log.LogWarning($"[ULTRA] postfix failed: {e.Message}");
        }
    }

    // Makes "the High option BE Ultra": re-invokes the game's own quality
    // setter with the Ultra enum value so every quality listener (lighting,
    // shadows, LOD, particles, terrain, …) applies the game's Ultra level.
    // The nested postfix call is skipped via _redirecting, so this cannot
    // recurse.
    private static void RedirectHighToUltra(object settingsModel)
    {
        if (_qualitySetter == null || _qualityEnumType == null || settingsModel == null)
        {
            Plugin.Log.LogWarning("[ULTRA] cannot redirect High -> Ultra: setter not available");
            return;
        }

        _redirecting = true;
        try
        {
            _qualitySetter.Invoke(settingsModel,
                new object[] { Enum.ToObject(_qualityEnumType, _ultraEnumValue) });
            Plugin.Log.LogInfo($"[ULTRA] High selected -> applied the game's Ultra level ({_ultraEnumValue}) under the High label");
        }
        catch (Exception e)
        {
            Plugin.Log.LogWarning($"[ULTRA] High -> Ultra redirect failed: {e.Message}");
        }
        finally
        {
            _redirecting = false;
        }
    }

    public static void ApplyUltraQualitySettings()
    {
        QualitySettings.antiAliasing = UltraQuality.AntiAliasing;
        QualitySettings.pixelLightCount = UltraQuality.PixelLightCount;
        QualitySettings.shadowDistance = UltraQuality.ShadowDistance;
        QualitySettings.lodBias = UltraQuality.LodBias;
        Plugin.Log.LogInfo("[ULTRA] Ultra boost applied on top " +
            $"(AA={UltraQuality.AntiAliasing}, lights={UltraQuality.PixelLightCount}, " +
            $"shadowDist={UltraQuality.ShadowDistance}, lodBias={UltraQuality.LodBias})");
    }

    // Keeps Ultra-via-High stuck across scene loads: re-checks the LIVE model
    // (not just the postfix), because a scene change can restore the game's
    // own level without calling the setter.
    private static void ReapplyIfNeeded()
    {
        try
        {
            int? current = GetCurrentQualitySetting();
            if (!current.HasValue)
                return;
            if (current.Value == _ultraEnumValue)
            {
                ApplyUltraQualitySettings();
            }
            else if (_highResolved && current.Value == _highEnumValue && current.Value != _ultraEnumValue)
            {
                // High is the live selection but the redirect never fired
                // (e.g. High was set before the mapping resolved): fix it now.
                RedirectHighToUltra(_settingsModel);
                ApplyUltraQualitySettings();
            }
        }
        catch (Exception e)
        {
            Plugin.Log.LogDebug($"[ULTRA] reapply check failed: {e.Message}");
        }
    }

    private static int? GetCurrentQualitySetting()
    {
        if (_settingsModel == null)
            return null;
        var getter = _settingsModel.GetType().GetMethod("get_QualitySetting",
            BindingFlags.Public | BindingFlags.NonPublic | BindingFlags.Instance);
        if (getter == null)
            return null;
        return Convert.ToInt32(getter.Invoke(_settingsModel, null));
    }

    // Find the Graphics Quality row BY VISIBLE LABEL and read which quality
    // enum value the "High" toggle is wired to (its platform mapping). No
    // cloning, no UI changes — pure read.
    private static void ResolveHighMapping()
    {
        if (!FindHighButton(out var sourceGo, out bool confident))
        {
            Plugin.Log.LogDebug("[ULTRA] High graphics-quality toggle not found yet");
            return;
        }

        if (!TryReadHighMapping(sourceGo, out int value))
        {
            // Structural mismatch on the real row: loud, but keep retrying
            // within the time budget — a settings-page rebuild can fix it.
            Plugin.Log.LogWarning("[ULTRA] defaultQualityMapping not found on the High toggle " +
                $"'{sourceGo.name}' — cannot resolve its quality value yet");
            return;
        }

        _highEnumValue = value;
        _highResolved = true;
        if (_highEnumValue == _ultraEnumValue)
            Plugin.Log.LogInfo($"[ULTRA] High already maps to the game's Ultra level ({_highEnumValue}) — no redirect needed, boost applies on top");
        else
            Plugin.Log.LogInfo($"[ULTRA] High maps to quality value {_highEnumValue} — selecting High will redirect to Ultra ({_ultraEnumValue})");
    }

    // Read the High toggle's mapped quality value: prefer the platform-aware
    // QualitySettingForPlatform property, fall back to defaultQualityMapping.
    private static bool TryReadHighMapping(GameObject sourceGo, out int value)
    {
        value = -1;
        try
        {
            foreach (var o in GetAllComponents(sourceGo))
            {
                // Il2Cpp downcasts must go through TryCast, never a direct cast.
                var comp = ((UnityEngine.Object)o).TryCast<Component>();
                if (comp == null)
                    continue;
                var type = comp.GetType();
                var field = FindFieldInHierarchy(type, MappingFieldName);
                if (field == null || !field.FieldType.IsEnum)
                    continue;

                object raw = null;
                try
                {
                    var prop = type.GetProperty(PlatformMappingPropertyName,
                        BindingFlags.Public | BindingFlags.NonPublic | BindingFlags.Instance);
                    raw = prop?.GetValue(comp, null);
                }
                catch
                {
                    // fall back to the serialized field below
                }
                raw ??= field.GetValue(comp);

                value = Convert.ToInt32(raw);
                return true;
            }
        }
        catch (Exception e)
        {
            Plugin.Log.LogDebug($"[ULTRA] High mapping read failed: {e.Message}");
        }
        return false;
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

    // Locate the "High" quality button by its VISIBLE LABEL (never by
    // GameObject/type name). Primary: the lowest ancestor of a "High" label
    // whose direct children also carry "Low" and "Medium" labels — that
    // ancestor IS the Graphics Quality row, and the child carrying "High"
    // (but not Low/Medium) is the button. Fallback: the nearest clickable
    // (Button/Toggle) ancestor of a "High" label, for label sources the
    // sibling check can't see. `confident` is true only for the primary
    // path; the fallback must never burn the retry budget on a wrong button.
    private static bool FindHighButton(out GameObject sourceGo, out bool confident)
    {
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
            if (FindClickableAncestor(labelGo, out var buttonGo))
            {
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

    private static bool IsLabelMatch(string text, string label) =>
        !string.IsNullOrWhiteSpace(text) &&
        text.Trim().Equals(label, StringComparison.OrdinalIgnoreCase);
}
