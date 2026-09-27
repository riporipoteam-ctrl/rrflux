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
// until that patch reports IsSettled, then idles.
//
// Why the driver NEVER self-destructs: home UI rebuilds on login (and after
// certain scene transitions), destroying the inserted logo, tab clones, and
// label changes. A self-destructing driver could settle, die, and never
// re-apply them. Instead the driver persists for the whole session
// (DontDestroyOnLoad) and idles when every patch is settled. On every scene
// change, Plugin calls NotifySceneChanged(), which re-arms every patch so
// retries resume immediately; patches that keep a one-way settled flag
// (HomeLogoPatch, HomeLabelsPatch) also get their flag cleared there
// (Apply() is idempotent, so a redundant pass is harmless).
//
// IL2CPP rules honored: the behaviour is a plain MonoBehaviour added via the
// generic AddComponent<T> (same pattern as FluxPairingPatch's panel); no
// Delegate.CreateDelegate anywhere near here.
using System;
using System.Text;
using Il2CppInterop.Runtime.Injection;
using UnityEngine;

namespace RecNetPlugin.Patches;

internal static class UiDiscoveryRetry
{
    private static GameObject _driver;
    private static RetryBehaviour _behaviour;
    private const float RetryIntervalSeconds = 2f;

    // Idempotent and exception-safe: safe to call from Plugin.Load() (very
    // early) and again from the first OnSceneLoaded.
    internal static void Ensure()
    {
        try
        {
            if (_driver != null && _behaviour != null)
                return;
            _driver = new GameObject("FluxRecUiDiscoveryRetry");
            _driver.hideFlags = HideFlags.HideAndDontSave;
            UnityEngine.Object.DontDestroyOnLoad(_driver);
            // FIX: Register the MonoBehaviour type with Il2Cpp BEFORE AddComponent.
            // Without this, the generic AddComponent<T>() throws
            // MethodInfoStoreGeneric_AddComponent_Public_T_0 type initializer
            // exception on IL2CPP. Every other patch (FluxPairingPatch,
            // PlusBalancePatch, etc.) does this registration; UiDiscoveryRetry
            // was the only one missing it, which broke the retry driver and
            // prevented PlayButtonPatch from ever finding the Create button.
            ClassInjector.RegisterTypeInIl2Cpp<RetryBehaviour>();
            _behaviour = _driver.AddComponent<RetryBehaviour>();
            Plugin.Log.LogDebug("[RETRY] UI discovery retry driver started (5 patches)");
        }
        catch (Exception e)
        {
            Plugin.Log.LogWarning($"[RETRY] could not start the UI discovery retry driver: {e.Message}");
        }
    }

    // Called from Plugin.OnSceneLoaded on every scene change: re-arms all
    // patches so they retry immediately, instead of staying settled from a
    // previous scene whose UI has since been destroyed (e.g. the home UI
    // rebuilding on login). Never destroys the driver — it idles until
    // every patch is settled again.
    internal static void NotifySceneChanged()
    {
        try
        {
            // The home patches keep a one-way settled flag (set once the
            // logo/labels were applied to a previous scene's UI). Clearing it
            // here lets Apply() re-run; both are idempotent, so a redundant
            // pass just re-verifies and settles again.
            HomeLogoPatch.Reset();
            HomeLabelsPatch.Reset();
        }
        catch (Exception e)
        {
            Plugin.Log.LogDebug($"[RETRY] patch reset failed on scene change: {e.Message}");
        }

        var behaviour = _behaviour;
        if (behaviour == null)
        {
            // Driver never started (or was killed somehow) — recreate it.
            Ensure();
            return;
        }
        behaviour.ReArmAll();
    }

    private class RetryBehaviour : MonoBehaviour
    {
        private static readonly TimeSpan StatusLogInterval = TimeSpan.FromSeconds(60);

        private static readonly string[] PatchNames =
            { "Play", "FluxConnect", "Ultra", "HomeLogo", "HomeLabels" };

        private float _nextRun;
        private float _nextStatusLog;
        private bool _firstTick = true;
        private readonly bool[] _settled = new bool[5];

        private void Awake()
        {
            var now = Time.realtimeSinceStartup;
            _nextRun = now + RetryIntervalSeconds;
            _nextStatusLog = now + (float)StatusLogInterval.TotalSeconds;
        }

        // Re-arm every patch after a scene change: the driver keeps ticking
        // and each patch retries from the next tick until it settles again.
        // Also fires one retry pass immediately instead of waiting out the
        // 2-second cadence.
        internal void ReArmAll()
        {
            for (int i = 0; i < _settled.Length; i++)
                _settled[i] = false;
            _nextRun = Time.realtimeSinceStartup;
            Plugin.Log.LogDebug("[RETRY] re-armed all patches after scene change");
        }

        private void Update()
        {
            try
            {
                if (Time.realtimeSinceStartup < _nextRun)
                    return;
                _nextRun = Time.realtimeSinceStartup + RetryIntervalSeconds;

                // Tick every patch. Settled state is re-evaluated live every
                // tick: a patch only stops being ticked while its own
                // IsSettled is true, and re-arming (scene change, or the
                // patch's settled check going false) resumes its retries.
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

                // No self-destruct: when everything is settled the driver
                // just idles here (a few boolean checks per tick) until a
                // scene change re-arms the patches.
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

            // Not settled (anymore): make sure the driver keeps ticking this
            // patch — covers patches whose settled check turned false again
            // after a UI rebuild, even without a scene-change re-arm.
            if (_settled[index])
            {
                _settled[index] = false;
                Plugin.Log.LogInfo($"[RETRY] {PatchNames[index]} unsettled again — resuming retries");
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
    }
}
