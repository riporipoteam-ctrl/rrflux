// Persistent time-based retry driver for the UI-discovery patches
// (PlayButtonPatch, FluxConnectButton, UltraGraphicsPatch, HomeLogoPatch,
// HomeLabelsPatch).
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
// until that patch reports IsSettled (feature done, retry budget exhausted,
// or feature disabled in config), then destroys itself.
//
// Self-destruct vs keep-running: the driver destroys itself once all 5
// patches are settled. Scene-rebuild handling stays where it already is —
// the patches' own Apply() calls from Plugin.OnSceneLoaded plus their
// internal idempotence — so there is nothing for a longer-lived driver to
// do, and a dead MonoBehaviour can't leak ticks. A 15-minute driver-lifetime
// backstop stops the driver even if a settled-check ever misbehaves.
//
// IL2CPP rules honored: the behaviour is a plain MonoBehaviour added via the
// generic AddComponent<T> (same pattern as FluxPairingPatch's panel); no
// Delegate.CreateDelegate anywhere near here.
using System;
using System.Text;
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
            Plugin.Log.LogDebug("[RETRY] UI discovery retry driver started (5 patches)");
        }
        catch (Exception e)
        {
            Plugin.Log.LogWarning($"[RETRY] could not start the UI discovery retry driver: {e.Message}");
        }
    }

    private class RetryBehaviour : MonoBehaviour
    {
        // Backstop: the patches' own budgets top out at 10 minutes, so if
        // the driver is still alive at 15 something is wrong — stop instead
        // of ticking forever.
        private static readonly TimeSpan MaxDriverLifetime = TimeSpan.FromMinutes(15);
        private static readonly TimeSpan StatusLogInterval = TimeSpan.FromSeconds(60);

        private static readonly string[] PatchNames =
            { "Play", "FluxConnect", "Ultra", "HomeLogo", "HomeLabels" };

        private float _startTime;
        private float _nextRun;
        private float _nextStatusLog;
        private bool _firstTick = true;
        private readonly bool[] _settled = new bool[5];

        private void Awake()
        {
            _startTime = Time.realtimeSinceStartup;
            _nextRun = _startTime + RetryIntervalSeconds;
            _nextStatusLog = _startTime + (float)StatusLogInterval.TotalSeconds;
        }

        private void Update()
        {
            try
            {
                if (Time.realtimeSinceStartup < _nextRun)
                    return;
                _nextRun = Time.realtimeSinceStartup + RetryIntervalSeconds;

                // Tick every patch that hasn't settled yet.
                TickPatch(0, () => PlayButtonPatch.IsSettled, () => PlayButtonPatch.Apply());
                TickPatch(1, () => FluxConnectButton.IsSettled, () => FluxConnectButton.Apply());
                TickPatch(2, () => UltraGraphicsPatch.IsSettled, () => UltraGraphicsPatch.Apply());
                TickPatch(3, () => IsHomeLogoSettled(), () => HomeLogoPatch.Apply());
                TickPatch(4, () => HomeLabelsPatch.IsSettled, () => HomeLabelsPatch.Apply());
                _firstTick = false;

                if (Time.realtimeSinceStartup >= _nextStatusLog)
                {
                    _nextStatusLog = Time.realtimeSinceStartup + (float)StatusLogInterval.TotalSeconds;
                    LogStatus();
                }

                if (AllSettled())
                {
                    Plugin.Log.LogInfo("[RETRY] all 5 UI discovery patches settled — stopping the retry driver");
                    StopDriver();
                    return;
                }

                if (Time.realtimeSinceStartup - _startTime > (float)MaxDriverLifetime.TotalSeconds)
                {
                    Plugin.Log.LogWarning("[RETRY] driver lifetime exceeded 15 minutes with patches still pending — stopping anyway");
                    LogStatus();
                    StopDriver();
                }
            }
            catch (Exception e)
            {
                Plugin.Log.LogDebug($"[RETRY] driver tick failed: {e.Message}");
                _nextRun = Time.realtimeSinceStartup + RetryIntervalSeconds;
            }
        }

        // HomeLogoPatch.Apply() is a no-op when [Home] branding is disabled
        // but doesn't mark itself settled in that case, so the driver folds
        // the config check into settled-state here (same definition every
        // other IsSettled uses). HomeLabelsPatch already settles itself.
        private static bool IsHomeLogoSettled()
            => HomeLogoPatch.IsSettled || !Plugin.EnableFluxHomeBranding.Value;

        private void TickPatch(int index, Func<bool> isSettled, Action apply)
        {
            bool settled;
            try
            {
                settled = isSettled();
            }
            catch (Exception e)
            {
                Plugin.Log.LogDebug($"[RETRY] {PatchNames[index]} settled-check failed: {e.Message}");
                settled = false; // keep retrying rather than dropping the patch
            }

            if (settled)
            {
                if (!_settled[index])
                {
                    _settled[index] = true;
                    // The very first tick records initial state silently; only
                    // later transitions get their own line (the 60s status log
                    // already shows the full picture).
                    if (!_firstTick)
                        Plugin.Log.LogInfo($"[RETRY] {PatchNames[index]} settled");
                }
                return;
            }

            try
            {
                apply();
            }
            catch (Exception e)
            {
                Plugin.Log.LogDebug($"[RETRY] {PatchNames[index]} apply failed: {e.Message}");
            }
        }

        private bool AllSettled()
        {
            for (int i = 0; i < _settled.Length; i++)
                if (!_settled[i])
                    return false;
            return true;
        }

        private void LogStatus()
        {
            var pending = new StringBuilder();
            var settled = new StringBuilder();
            for (int i = 0; i < PatchNames.Length; i++)
            {
                var sb = _settled[i] ? settled : pending;
                if (sb.Length > 0)
                    sb.Append(", ");
                sb.Append(PatchNames[i]);
            }
            Plugin.Log.LogInfo($"[RETRY] status — pending: [{pending}] settled: [{settled}]");
        }

        private void StopDriver()
        {
            UnityEngine.Object.Destroy(_driver);
            _driver = null;
        }
    }
}
