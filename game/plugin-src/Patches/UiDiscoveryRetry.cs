// Persistent time-based retry driver for the UI-discovery patches
// (PlayButtonPatch, FluxConnectButton, UltraGraphicsPatch).
//
// Why this exists: SceneManager.sceneLoaded was the only retry trigger the
// patches had, but every one of their targets builds asynchronously INSIDE a
// scene, long after sceneLoaded has fired:
//   - the Watch home tab row / icon row finishes building post-login inside
//     the menu scene;
//   - the Settings page is built lazily the first time the user opens it.
// The first 2-3 scene loads of a session burned the old fixed attempt budgets
// while the UI didn't exist yet, and afterwards retries only happened on room
// travel — so the patches could sit idle forever while the user stared at the
// home screen. This driver re-invokes each patch's Apply() every 2 seconds
// until that patch reports IsSettled (feature done, time budget exhausted, or
// feature disabled in config), then destroys itself. The patches' own retry
// budgets are time-based (10 minutes), so nothing spins forever.
//
// IL2CPP rules honored: the behaviour is a plain MonoBehaviour added via the
// generic AddComponent<T> (same pattern as FluxPairingPatch's panel); no
// Delegate.CreateDelegate anywhere near here.
using System;
using UnityEngine;

namespace RecNetPlugin.Patches;

internal static class UiDiscoveryRetry
{
    private static GameObject _driver;
    private const float RetryIntervalSeconds = 2f;

    // Idempotent and exception-safe: safe to call from Plugin.Load() (very
    // early) and again from the first OnSceneLoaded.
    internal static void Ensure()
    {
        try
        {
            if (_driver != null)
                return;
            _driver = new GameObject("FluxRecUiDiscoveryRetry");
            _driver.hideFlags = HideFlags.HideAndDontSave;
            UnityEngine.Object.DontDestroyOnLoad(_driver);
            _driver.AddComponent<RetryBehaviour>();
            Plugin.Log.LogDebug("[RETRY] UI discovery retry driver started");
        }
        catch (Exception e)
        {
            Plugin.Log.LogWarning($"[RETRY] could not start the UI discovery retry driver: {e.Message}");
        }
    }

    private class RetryBehaviour : MonoBehaviour
    {
        private float _nextRun;

        private void Update()
        {
            try
            {
                if (Time.realtimeSinceStartup < _nextRun)
                    return;
                _nextRun = Time.realtimeSinceStartup + RetryIntervalSeconds;

                if (!PlayButtonPatch.IsSettled)
                    PlayButtonPatch.Apply();
                if (!FluxConnectButton.IsSettled)
                    FluxConnectButton.Apply();
                if (!UltraGraphicsPatch.IsSettled)
                    UltraGraphicsPatch.Apply();

                if (PlayButtonPatch.IsSettled &&
                    FluxConnectButton.IsSettled &&
                    UltraGraphicsPatch.IsSettled)
                {
                    Plugin.Log.LogInfo("[RETRY] all UI discovery patches settled — stopping the retry driver");
                    UnityEngine.Object.Destroy(_driver);
                    _driver = null;
                }
            }
            catch (Exception e)
            {
                Plugin.Log.LogDebug($"[RETRY] driver tick failed: {e.Message}");
                _nextRun = Time.realtimeSinceStartup + RetryIntervalSeconds;
            }
        }
    }
}
