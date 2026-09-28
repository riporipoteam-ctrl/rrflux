// Standalone Play Button Fix plugin for Flux Rec v0.2.7
// Unhides the Play button on the 2023.04.14 home screen icon row.
//
// This is a minimal BepInEx plugin that only depends on UnityEngine,
// avoiding the broken Assembly-CSharp interop.

using System;
using System.Linq;
using BepInEx;
using BepInEx.Logging;
using BepInEx.Unity.IL2CPP;
using UnityEngine;
using UnityEngine.UI;
using UnityEngine.SceneManagement;

namespace PlayButtonFix;

[BepInPlugin("com.fluxrec.playbuttonfix", "Play Button Fix", "1.0.0")]
public class PlayButtonFixPlugin : BasePlugin
{
    private static ManualLogSource _log;
    private static bool _done;
    private const string CloneName = "FluxPlayButton";

    public override void Load()
    {
        _log = Log;
        _log.LogInfo("PlayButtonFix loaded");
        // Retry on scene load until home screen exists
        SceneManager.sceneLoaded += OnSceneLoaded;
        // Also try immediately in case we're already in the home scene
        TryApply();
    }

    private static void OnSceneLoaded(Scene scene, LoadSceneMode mode)
    {
        if (_done) return;
        TryApply();
    }

    private static void TryApply()
    {
        if (_done) return;
        try
        {
            if (TryUnhidePlayButton())
            {
                _done = true;
                _log.LogInfo("[HOME-PLAY] Play button unhidden/created successfully");
            }
        }
        catch (Exception e)
        {
            _log.LogWarning($"[HOME-PLAY] failed: {e.Message}");
        }
    }

    private static bool TryUnhidePlayButton()
    {
        var canvases = UnityEngine.Object.FindObjectsOfType<Canvas>();
        foreach (var canvas in canvases)
        {
            if (canvas == null) continue;
            var go = canvas.gameObject;
            if (go == null) continue;

            var found = SearchForIconRow(go.transform);
            if (found)
                return true;
        }
        return false;
    }

    private static bool SearchForIconRow(Transform root)
    {
        foreach (var t in root.GetComponentsInChildren<Transform>(true))
        {
            if (t == null) continue;

            var buttons = t.GetComponentsInChildren<Button>(true);
            if (buttons.Length >= 5 && buttons.Length <= 10)
            {
                if (ProcessIconRow(t, buttons))
                    return true;
            }
        }
        return false;
    }

    private static bool ProcessIconRow(Transform row, Button[] buttons)
    {
        _log.LogInfo($"[HOME-PLAY] found icon row with {buttons.Length} buttons: {row.name}");

        // First: look for an existing but hidden Play button
        foreach (var btn in buttons)
        {
            if (btn == null) continue;
            var go = btn.gameObject;
            if (go == null) continue;

            string name = go.name.ToLowerInvariant();
            if (name.Contains("play"))
            {
                if (!go.activeSelf)
                {
                    go.SetActive(true);
                    _log.LogInfo($"[HOME-PLAY] unhidden existing Play button: {go.name}");
                }
                var p = go.transform.parent;
                while (p != null)
                {
                    if (!p.gameObject.activeSelf)
                    {
                        p.gameObject.SetActive(true);
                        _log.LogInfo($"[HOME-PLAY] activated parent: {p.name}");
                    }
                    p = p.parent;
                }
                return true;
            }
        }

        // Check if we already cloned one
        foreach (var btn in buttons)
        {
            if (btn != null && btn.gameObject != null && btn.gameObject.name == CloneName)
            {
                _log.LogInfo("[HOME-PLAY] clone already exists");
                return true;
            }
        }

        // Clone the first button as a Play button fallback
        var source = buttons[0];
        if (source == null || source.gameObject == null)
            return false;

        try
        {
            var clone = UnityEngine.Object.Instantiate(source.gameObject);
            clone.name = CloneName;
            clone.transform.SetParent(row, false);
            clone.SetActive(true);
            SetButtonLabel(clone, "Play");
            _log.LogInfo("[HOME-PLAY] cloned Create button as Play button");
            return true;
        }
        catch (Exception e)
        {
            _log.LogWarning($"[HOME-PLAY] clone failed: {e.Message}");
            return false;
        }
    }

    private static void SetButtonLabel(GameObject buttonGo, string text)
    {
        try
        {
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
            var uiText = buttonGo.GetComponentInChildren<Text>(true);
            if (uiText != null)
                uiText.text = text;
        }
        catch { }
    }
}
