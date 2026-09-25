// Fixes the Game Settings -> Experience -> PRESENCE ("Appear Online To") slider
// so All / Friends / Favorites / No One map to the correct stored values.
//
// Root cause (verified by disassembly of GameAssembly.dll, client 2023-04-14):
// the slider's notch labels run opposite to the JFAAGFOLICM enum order
// (Public=0, FriendsOnly=1, FavoriteFriendsOnly=2, Offline=3), i.e.
// notch 0 = "No One" ... notch 3 = "All". The game's own slider impl converts
// position <-> enum with an identity cast in both directions, so selecting
// "All" (notch 3) stored Offline(3) and displayed "No One".
//
// Fix: prefix-patch both directions of SettingsModelController.
// AppearOnlineToSliderImpl and invert the mapping (see PresenceMapping):
//   write: store (JFAAGFOLICM)(3 - round(sliderValue))
//   read:  slider.value = 3 - (int)currentEnumValue
//
// Both directions are patched: patching only the write would store the right
// value but leave the knob sitting at the wrong ("No One") end after refresh.
//
// Resolution is by name at runtime (never by obfuscated type name): the impl is
// a private nested type of the unobfuscated SettingsModelController, and its
// method names (FHPNDLFMODO / NGFFCLPHKJN) are the obfuscated-but-stable
// overrides shared by every slider impl in this build. Per the repo gotchas,
// Harmony parameters are bound positionally (__0), never by obfuscated name.
//
// One knob, see [Presence] in the .cfg:
//   Fix Appear Online To Mapping -> THE FIX (default true).
using System;
using System.Linq;
using System.Reflection;
using HarmonyLib;
using UnityEngine;
using UnityEngine.UI;

namespace RecNetPlugin.Patches;

internal static class PresenceMappingPatch
{
    private const string ImplTypeName = "AppearOnlineToSliderImpl";
    private const string WriteMethodName = "FHPNDLFMODO"; // slider -> model (float)
    private const string ReadMethodName = "NGFFCLPHKJN";  // model -> slider (void)

    private const int MaxAttempts = 10;

    private static int _attempts;
    private static bool _done;
    private static readonly Harmony _harmony = new Harmony("net.rec.plugin.presence");

    // Called from Plugin.Load and again on each scene load until the impl type
    // is resolvable — Assembly-CSharp may not be fully loaded at plugin Load().
    public static void Apply()
    {
        if (_done || !Plugin.FixPresenceMapping.Value || _attempts >= MaxAttempts)
            return;

        _attempts++;

        try
        {
            PatchImpl();
        }
        catch (Exception e)
        {
            Plugin.Log.LogWarning($"[PRESENCE] attempt {_attempts} failed: {e.Message}");
            return;
        }

        if (_done)
            Plugin.Log.LogInfo("[PRESENCE] Appear-Online-To slider mapping fixed (write+read inverted)");
        else if (_attempts >= MaxAttempts)
            Plugin.Log.LogWarning("[PRESENCE] gave up after " + _attempts +
                " attempts — AppearOnlineToSliderImpl not found. Mapping left as-is.");
    }

    private static void PatchImpl()
    {
        var implType = typeof(SettingsModelController)
            .GetNestedType(ImplTypeName, BindingFlags.NonPublic);
        if (implType == null)
        {
            Plugin.Log.LogWarning($"[PRESENCE] {ImplTypeName} not found — will retry");
            return;
        }

        var write = implType.GetMethod(WriteMethodName,
            BindingFlags.Public | BindingFlags.NonPublic | BindingFlags.Instance);
        var read = implType.GetMethod(ReadMethodName,
            BindingFlags.Public | BindingFlags.NonPublic | BindingFlags.Instance);
        if (write == null || read == null)
        {
            Plugin.Log.LogWarning(
                $"[PRESENCE] methods not found on {ImplTypeName} (write={write != null}, read={read != null}) — will retry");
            return;
        }

        _harmony.Patch(write, prefix: new HarmonyMethod(
            typeof(PresenceMappingPatch).GetMethod(nameof(WritePrefix),
                BindingFlags.Static | BindingFlags.NonPublic)));
        _harmony.Patch(read, prefix: new HarmonyMethod(
            typeof(PresenceMappingPatch).GetMethod(nameof(ReadPrefix),
                BindingFlags.Static | BindingFlags.NonPublic)));
        _done = true;
    }

    // Slider -> model: store the inverted enum value, skip the original
    // identity cast.
    private static bool WritePrefix(float __0, object __instance)
    {
        try
        {
            var model = GetModel(__instance);
            var setter = model.GetType().GetMethod("set_AppearOnlineTo",
                BindingFlags.Public | BindingFlags.NonPublic | BindingFlags.Instance);
            if (setter == null)
            {
                Plugin.Log.LogWarning("[PRESENCE] set_AppearOnlineTo not found on model");
                return false;
            }

            var enumType = setter.GetParameters()[0].ParameterType;
            int fixedValue = PresenceMapping.SliderFloatToPresenceValue(__0);
            setter.Invoke(model, new object[] { Enum.ToObject(enumType, fixedValue) });
            Plugin.Log.LogDebug($"[PRESENCE] slider {__0} -> stored {(int)Enum.ToObject(enumType, fixedValue)}");
        }
        catch (Exception e)
        {
            Plugin.Log.LogWarning($"[PRESENCE] write prefix failed: {e.Message}");
        }
        return false; // skip original
    }

    // Model -> slider: move the knob to the inverted position, skip the
    // original identity assignment.
    private static bool ReadPrefix(object __instance)
    {
        try
        {
            var implType = __instance.GetType();
            var sliderField = implType
                .GetFields(BindingFlags.Public | BindingFlags.NonPublic | BindingFlags.Instance)
                .FirstOrDefault(f => f.FieldType == typeof(Slider));
            if (sliderField == null)
            {
                Plugin.Log.LogWarning("[PRESENCE] Slider field not found on impl");
                return false;
            }

            var slider = (Slider)sliderField.GetValue(__instance);
            if (slider == null)
                return false;

            var model = GetModel(__instance);
            var getter = model.GetType().GetMethod("get_AppearOnlineTo",
                BindingFlags.Public | BindingFlags.NonPublic | BindingFlags.Instance);
            if (getter == null)
            {
                Plugin.Log.LogWarning("[PRESENCE] get_AppearOnlineTo not found on model");
                return false;
            }

            int current = Convert.ToInt32(getter.Invoke(model, null));
            slider.value = PresenceMapping.PresenceValueToSliderPosition(current);
        }
        catch (Exception e)
        {
            Plugin.Log.LogWarning($"[PRESENCE] read prefix failed: {e.Message}");
        }
        return false; // skip original
    }

    private static object GetModel(object implInstance)
    {
        // ControllerImplementation<T>.Model — protected auto-property on the base.
        var prop = implInstance.GetType().GetProperty("Model",
            BindingFlags.Public | BindingFlags.NonPublic | BindingFlags.Instance);
        return prop.GetValue(implInstance, null);
    }
}
