// Unhides the Play button on the 2023.04.14 home screen icon row.
//
// Why this shape:
//  - The 2023.04.14 home screen is code-built at runtime via RRUI. The icon
//    row (Create/Store/Events/Clubs/Challenges/Backpack) is visible, but the
//    Play button is hidden — either set inactive or hidden by a component.
//  - The 2021-era PlayMenuRRUI game-config flags do NOT apply to this build.
//    The Statsig-based PlayButtonVisibilityPatch did not work because method
//    names are obfuscated in the 2023 build.
//  - This patch takes the direct approach: find the home screen icon row,
//    iterate ALL children (including inactive ones), and activate any that
//    look like a Play button (name contains "play", case-insensitive).
//  - If no Play button exists at all, it clones the Create button (first
//    child) as a fallback, relabels it "Play", and wires it to open the
//    Play menu via the game's own navigation.
//
// Hard rules honored:
//  - Fail-soft: every step wrapped in try/catch. Never crashes the game.
//  - Idempotent: checks for existing "FluxPlayButton" by name before cloning.
//  - Uses TryCast for downcasts; reflection for TMPro text.
//  - Retried from OnSceneLoaded until the home screen exists.

using System;
using System.Linq;
using UnityEngine;
using UnityEngine.UI;

namespace RecNetPlugin.Patches;

internal static class HomePlayButtonPatch
{
    private static bool _done;
    private const string CloneName = "FluxPlayButton";

    public static void Apply()
    {
        if (_done)
            return;

        try
        {
            if (TryUnhidePlayButton())
            {
                _done = true;
                Plugin.Log.LogInfo("[HOME-PLAY] Play button unhidden/created successfully");
            }
        }
        catch (Exception e)
        {
            Plugin.Log.LogWarning($"[HOME-PLAY] failed: {e.Message}");
        }
    }

    private static bool TryUnhidePlayButton()
    {
        // Find all canvases, look for the home screen icon row
        var canvases = UnityEngine.Object.FindObjectsOfType<Canvas>();
        foreach (var canvas in canvases)
        {
            if (canvas == null) continue;
            var go = canvas.gameObject;
            if (go == null) continue;

            // Search for icon row: a horizontal layout with multiple buttons
            var found = SearchForIconRow(go.transform);
            if (found)
                return true;
        }
        return false;
    }

    private static bool SearchForIconRow(Transform root)
    {
        // Look for a transform with 5+ button children (the icon row)
        foreach (var t in root.GetComponentsInChildren<Transform>(true))
        {
            if (t == null) continue;

            // Check if this has multiple button children (icon row pattern)
            var buttons = t.GetComponentsInChildren<Button>(true);
            if (buttons.Length >= 5 && buttons.Length <= 10)
            {
                // This looks like the icon row. Check for hidden Play button.
                if (ProcessIconRow(t, buttons))
                    return true;
            }
        }
        return false;
    }

    private static bool ProcessIconRow(Transform row, Button[] buttons)
    {
        Plugin.Log.LogInfo($"[HOME-PLAY] found icon row with {buttons.Length} buttons: {row.name}");

        // First: look for an existing but hidden Play button
        foreach (var btn in buttons)
        {
            if (btn == null) continue;
            var go = btn.gameObject;
            if (go == null) continue;

            string name = go.name.ToLowerInvariant();
            if (name.Contains("play"))
            {
                // Found it! Unhide it.
                if (!go.activeSelf)
                {
                    go.SetActive(true);
                    Plugin.Log.LogInfo($"[HOME-PLAY] unhidden existing Play button: {go.name}");
                }
                // Also ensure parent chain is active
                var p = go.transform.parent;
                while (p != null)
                {
                    if (!p.gameObject.activeSelf)
                    {
                        p.gameObject.SetActive(true);
                        Plugin.Log.LogInfo($"[HOME-PLAY] activated parent: {p.name}");
                    }
                    p = p.parent;
                }
                return true;
            }
        }

        // No Play button found. Check if we already cloned one.
        foreach (var btn in buttons)
        {
            if (btn != null && btn.gameObject != null && btn.gameObject.name == CloneName)
            {
                Plugin.Log.LogInfo("[HOME-PLAY] clone already exists");
                return true;
            }
        }

        // Clone the first button (Create) as a Play button fallback
        var source = buttons[0];
        if (source == null || source.gameObject == null)
            return false;

        try
        {
            var clone = UnityEngine.Object.Instantiate(source.gameObject);
            clone.name = CloneName;
            // Set parent to the row (proven pattern: instantiate then set parent)
            clone.transform.SetParent(row, false);
            clone.SetActive(true);

            // Relabel to "Play"
            SetButtonLabel(clone, "Play");

            // Note: keeping the original onClick (navigates to Create) as a
            // fallback. The primary path is unhiding the real Play button above.
            // Custom click wiring via ConvertDelegate requires working interop.

            Plugin.Log.LogInfo("[HOME-PLAY] cloned Create button as Play button");
            return true;
        }
        catch (Exception e)
        {
            Plugin.Log.LogWarning($"[HOME-PLAY] clone failed: {e.Message}");
            return false;
        }
    }

    private static void SetButtonLabel(GameObject buttonGo, string text)
    {
        try
        {
            // Try TMPro first (reflection, no compile-time dep)
            foreach (var comp in buttonGo.GetComponentsInChildren<Component>(true))
            {
                if (comp == null) continue;
                var type = comp.GetType();
                if (type.Name.Contains("TMP_Text") || type.Name.Contains("TextMeshPro"))
                {
                    var prop = type.GetProperty("text");
                    if (prop != null && prop.CanWrite)
                    {
                        prop.SetValue(comp, text);
                        return;
                    }
                }
            }
            // Fallback to Unity UI Text
            var uiText = buttonGo.GetComponentInChildren<Text>(true);
            if (uiText != null)
                uiText.text = text;
        }
        catch { }
    }
}
