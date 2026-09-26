// Visible "Flux Connect" button: a small floating IMGUI button pinned to the
// top-right corner of the screen that opens the existing pairing overlay
// (same as pressing F8).
//
// Why this shape:
//  - The overlay (FluxPairingPatch) is deliberately standalone — it touches
//    zero game types so it can't break on obfuscated per-build renames.
//    The button follows the same philosophy: pure IMGUI on its own injected
//    MonoBehaviour, no hooks into RRUI/home-screen types.
//  - Positioning: top-right corner. Visible on every screen but never over
//    the pairing window itself (top-left) or center-screen dialogs.
//  - Clicking opens the overlay (FluxPairingPatch.ShowOverlay()); it never
//    closes it — F8 still toggles.
//  - When already paired, the button tints green and shows the linked Flux
//    account name so the state is visible at a glance.
//
// IL2CPP rules: ClassInjector for the MonoBehaviour, HideAndDontDestroy +
// DontDestroyOnLoad so it survives scene changes, everything fail-soft.
//
// One knob, see [Pairing] in the .cfg:
//   Enable Flux Pairing -> THE FEATURE (default true). The button only
//   exists while the pairing feature is enabled.
using System;
using Il2CppInterop.Runtime.Injection;
using UnityEngine;

namespace RecNetPlugin.Patches;

internal static class FluxConnectButton
{
    private const float ButtonWidth = 170f;
    private const float ButtonHeight = 40f;
    private const float Margin = 12f;

    private static bool _buttonCreated;
    private static bool _typeRegistered;

    // Called from Plugin.Load and again on each scene load (no-op once the
    // button GameObject exists).
    public static void Apply()
    {
        if (_buttonCreated || !Plugin.EnableFluxPairing.Value)
            return;

        try
        {
            if (!_typeRegistered)
            {
                ClassInjector.RegisterTypeInIl2Cpp<FluxConnectButtonOverlay>();
                _typeRegistered = true;
            }

            var go = new GameObject("FluxConnectButton");
            go.hideFlags = HideFlags.HideAndDontSave;
            UnityEngine.Object.DontDestroyOnLoad(go);
            go.AddComponent<FluxConnectButtonOverlay>();
            _buttonCreated = true;
            Plugin.Log.LogInfo("[PAIRING] Flux Connect button ready (top-right corner)");
        }
        catch (Exception e)
        {
            Plugin.Log.LogWarning($"[PAIRING] Flux Connect button setup failed: {e.Message}");
        }
    }

    private class FluxConnectButtonOverlay : MonoBehaviour
    {
        private GUIStyle _style;

        private void OnGUI()
        {
            try
            {
                if (_style == null)
                {
                    _style = new GUIStyle(GUI.skin.button)
                    {
                        fontSize = 15,
                        fontStyle = FontStyle.Bold,
                    };
                }

                var rect = new Rect(
                    Screen.width - ButtonWidth - Margin,
                    Margin,
                    ButtonWidth,
                    ButtonHeight);

                var pairedName = (Plugin.PairedFluxAccount.Value ?? "").Trim();
                var paired = pairedName.Length > 0;

                var oldBg = GUI.backgroundColor;
                if (paired)
                    GUI.backgroundColor = new Color(0.35f, 0.8f, 0.45f);

                var label = paired ? "Flux: " + Truncate(pairedName, 12) : "Flux Connect";
                if (GUI.Button(rect, label, _style))
                {
                    Cursor.visible = true;
                    FluxPairingPatch.ShowOverlay();
                }
                GUI.backgroundColor = oldBg;
            }
            catch (Exception e)
            {
                Plugin.Log.LogWarning($"[PAIRING] Flux Connect button draw failed: {e.Message}");
            }
        }

        private static string Truncate(string s, int max)
        {
            if (string.IsNullOrEmpty(s) || s.Length <= max)
                return s;
            return s.Substring(0, max) + "...";
        }
    }
}
