using System;
using BepInEx;
using BepInEx.Configuration;
using BepInEx.Logging;
using BepInEx.Unity.IL2CPP;
using HarmonyLib;
using UnityEngine;
using UnityEngine.SceneManagement;

namespace RecNetPlugin;

[BepInPlugin("net.rec.plugin", "RecNet Plugin", "1.0.0")]
public class Plugin : BasePlugin
{
    internal static new ManualLogSource Log;

    public static ConfigEntry<string> AppIdRT { get; private set; }
    public static ConfigEntry<string> AppIdVoice { get; private set; }
    public static ConfigEntry<string> AppIdChat { get; private set; }
    public static ConfigEntry<string> ServerHostname { get; private set; }
    public static ConfigEntry<bool> EnableAdvancedSettings { get; private set; }
    public static ConfigEntry<string> PhotonHostname { get; private set; }
    public static ConfigEntry<int> PhotonPort { get; private set; }
    public static ConfigEntry<bool> Debug { get; private set; }
    public static ConfigEntry<bool> SimulateDUIDMismatch { get; private set; }
    public static ConfigEntry<bool> SuppressDUIDMismatch { get; private set; }
    public static ConfigEntry<bool> CorruptStoredDUID { get; private set; }
    public static ConfigEntry<bool> RestoreStoredDUID { get; private set; }
    public static ConfigEntry<string> DeviceIdResponseOverride { get; private set; }
    public static ConfigEntry<int> DeviceIdResponseStatus { get; private set; }
    public static ConfigEntry<bool> DisableSignatureVerification { get; private set; }
    public static ConfigEntry<bool> DisableTelemetry { get; private set; }
    public static ConfigEntry<bool> ForceNewWatchUI { get; private set; }
    public static ConfigEntry<bool> ForceIsDeveloper { get; private set; }
    public static ConfigEntry<bool> EnableFluxPlus { get; private set; }
    public static ConfigEntry<bool> FixPresenceMapping { get; private set; }
    public static ConfigEntry<bool> EnableUltraGraphics { get; private set; }

    private static bool _corruptDone;

    public override void Load()
    {
        Log = base.Log;

        AppIdRT = Config.Bind("Photon", "App Id Realtime", "", "Photon Realtime App ID");
        AppIdVoice = Config.Bind("Photon", "App Id Voice", "", "Photon Voice App ID");
        AppIdChat = Config.Bind("Photon", "App Id Chat", "", "Photon Chat App ID");
        EnableAdvancedSettings = Config.Bind("Advanced", "Enabled Advanced Settings", false, "Allows other fields below in the advanced section to be modified.");
        PhotonHostname = Config.Bind("Advanced", "Photon NameServer", "", "Custom Photon NameServer");
        PhotonPort = Config.Bind("Advanced", "Photon NameServer Port", 0, "Custom Photon NameServer Port (if 0, it will be default)");
        ServerHostname = Config.Bind("Server", "RecNet NameServer Host", "https://ns.rec.net", "Host for the RecNet NameServer.");
        Debug = Config.Bind("Advanced", "Debug", false, "Show debug logs (HTTP tracing, etc. WARNING: will include sensitive information such as passwords and auth tokens in the logs, be careful when sharing them!)");
        SimulateDUIDMismatch = Config.Bind("Advanced", "Simulate DUID Mismatch", false, "Force CheckForDUIDMismatch to return TRUE (fakes the comparison only). Reproduces the hang path but does not corrupt any stored value. Leave false for normal play.");
        SuppressDUIDMismatch = Config.Bind("Advanced", "Suppress DUID Mismatch", true, "Force CheckForDUIDMismatch to return FALSE (the workaround fix, ON by default): the client never migrates and never takes the Create Account hang path. No-op on healthy machines (the real check returns false anyway); on mismatched machines it skips the hang. Set false only to observe the real mismatch behavior for debugging.");
        CorruptStoredDUID = Config.Bind("Advanced", "Corrupt Stored DUID", false, "ONE-SHOT TEST: on next launch, write a truncated device id into the DUID pref via the game's own WriteDUIDs, producing a genuinely corrupt STORED value (real current id) — exactly the friend's condition. After it logs '[CORRUPT] wrote', set this back to false and relaunch to drive the real mismatch path. Use 'Restore Stored DUID' to undo.");
        RestoreStoredDUID = Config.Bind("Advanced", "Restore Stored DUID", false, "ONE-SHOT UNDO: on next launch, call WriteDUIDs with the real device id, overwriting any corrupt stored value with a good one. Set back to false after it logs '[CORRUPT] restored'.");
        DeviceIdResponseOverride = Config.Bind("Advanced", "DeviceId Response Override", "", "Replace the body of the PlayerReporting/v1/deviceId response with this text, to test what shape the client will accept. Empty = leave the server's response alone.");
        DeviceIdResponseStatus = Config.Bind("Advanced", "DeviceId Response Status", 200, "HTTP status to force on the PlayerReporting/v1/deviceId response. Only applies when the override body is set.");

        DisableSignatureVerification = Config.Bind("Signing", "Disable Signature Verification", true, "Force RSA signature verification to succeed (ON by default), so the client stops checking that images are signed with Rec Room's private key. This is what lets a self-hosted server serve its own images without the baked-in modulus matching. NOTE: this forces ALL mscorlib RSA verification to pass, not just image signatures — that breadth is deliberate, see CLAUDE.md.");

        DisableTelemetry = Config.Bind("Analytics", "Disable Telemetry", true, "Stop the client reporting to third-party telemetry services (ON by default). Covers: Amplitude analytics (every AmplitudeAnalyticsClient.Log* call plus any upload to amplitude.com, so batches queued in earlier sessions can't be flushed later); the data-collection endpoint (any host whose name starts with 'datacollection', e.g. datacollection.recflare.net); and Backtrace crash reports, minidumps and metrics to submit.backtrace.io. Blocked uploads get a synthetic 200 so the client carries on as if they had been accepted. It also asks Unity's own Analytics and Performance Reporting to switch off, but that part is KNOWN NOT TO WORK on this game build — those send from native engine code and the opt-out is refused, so perf-events.cloud.unity3d.com uploads continue; see the [UNITY-TELEMETRY] line in LogOutput.log. Not covered: RudderStack, gamesight, and minidumps sent by the native crash handler on the launch after a hard crash. Set false to let all of it through.");

        ForceNewWatchUI = Config.Bind("Watch", "Force New Watch UI", true, "Force the 2023 client's new Watch UI (RRUI) on by forcing its Statsig gate getters to true (ON by default). The client fetches Statsig gates directly from statsigapi.net — not from our backend — so without this the client always renders the legacy watch UI. Set false to fall back to the legacy watch if the new UI renders empty tabs.");

        ForceIsDeveloper = Config.Bind("Developer", "Force IsDeveloper", true, "Force SessionManager.get_IsDeveloper() to true, unlocking the client's developer-gated UI (ON by default). This reveals the developer slider in Settings, the dev badge on profiles, and other HideIfNotDeveloper UI elements. The backend also flags the account, but this works even if the backend flag is lost.");

        EnableFluxPlus = Config.Bind("Plus", "Enable Flux Rec Plus", true, "Rebrand Rec Room Plus to Flux Rec Plus and replace the Steam real-money purchase with a 10,000-token purchase (ON by default). Intercepts BuyRRPlusMembership() to prevent the Steam store from opening. Set false to restore the original (broken) Steam flow.");

        FixPresenceMapping = Config.Bind("Presence", "Fix Appear Online To Mapping", true, "Fix the Game Settings -> Experience -> PRESENCE slider so All/Friends/Favorites/No One store the correct values (ON by default). The slider's notch labels run opposite to the internal enum order, so without this selecting \"All\" stores Offline (\"No One\"). Set false if a future client build ships the labels in enum order.");

        EnableUltraGraphics = Config.Bind("Graphics", "Enable Ultra Graphics", true, "Add an \"Ultra\" preset button next to Low/Medium/High in Game Settings -> Visuals -> Graphics Quality and apply it through Unity's QualitySettings API (8x MSAA, 4 pixel lights, 150m shadow distance, 2x LOD bias). Medium stays the default; Low/Medium/High are untouched. Set false to hide the button and skip the QualitySettings boost.");

        // Not a patch — Unity's telemetry has a real opt-out, so we just set it. Retried from
        // OnSceneLoaded until it takes, since the native setters can refuse this early.
        Patches.UnityTelemetryPatch.Apply();

        // Statsig-gate force-on: retried from OnSceneLoaded until every getter is patched, since
        // the declaring type may live in an assembly that isn't loaded yet at plugin Load().
        Patches.WatchUIPatch.Apply();

        // Developer flag force-on: retried from OnSceneLoaded until patched, since
        // SessionManager may not be loaded yet at plugin Load().
        Patches.DeveloperPatch.Apply();

        // Flux Rec Plus rebrand + token purchase: retried from OnSceneLoaded
        // until patched, since the commerce types may not be loaded yet.
        Patches.FluxPlusPatch.Apply();

        // Plus page inspector: dumps Plus-related types and UI hierarchy
        // for writing precise patches (diagnostic only).
        Patches.PlusInspectorPatch.Apply();

        // Presence slider mapping fix (write+read inversion) and the Ultra
        // graphics hook: same retry-on-scene-load pattern as the watch gates.
        Patches.PresenceMappingPatch.Apply();
        Patches.UltraGraphicsPatch.Apply();

        Harmony.CreateAndPatchAll(typeof(Plugin).Assembly);

        // be.788: UnityAction<T0,T1> ctor no longer accepts a managed method group;
        // convert via DelegateSupport instead.
        SceneManager.sceneLoaded += Il2CppInterop.Runtime.DelegateSupport.ConvertDelegate<UnityEngine.Events.UnityAction<Scene, LoadSceneMode>>(
            new Action<Scene, LoadSceneMode>(OnSceneLoaded));
    }

    private void OnSceneLoaded(Scene scene, LoadSceneMode mode)
    {
        // No-op once the switches have stuck; must run before the early return below.
        Patches.UnityTelemetryPatch.Apply();

        // Retry until every Statsig watch gate is patched (no-op once done).
        Patches.WatchUIPatch.Apply();

        // Retry the developer flag force until patched (no-op once done).
        Patches.DeveloperPatch.Apply();

        // Retry the Flux Rec Plus patch until patched (no-op once done).
        Patches.FluxPlusPatch.Apply();

        // Retry the presence-mapping fix and the Ultra graphics button until
        // their target types / the settings page are available (no-op once done).
        Patches.PresenceMappingPatch.Apply();
        Patches.UltraGraphicsPatch.Apply();

        // CheatManager boots us out of rooms when it runs, but it's ALSO the DUID service the DI
        // container resolves for account creation / login (destroying it removes that service).
        // So instead of destroying it, *deactivate* the GameObject: it stops running (no Update /
        // coroutines, so no boot) while the component still exists, so the DI container can still
        // resolve PGECJHKNIEN and call its DUID methods. It's recreated per scene, so deactivate
        // each freshly-spawned (active) instance on every load. (GameObject.Find only returns active
        // objects, so once deactivated it isn't found again.)
        var cheatMgr = GameObject.Find("GameRoot/(Startup)(Clone)/Core Systems/[CheatManager]");
        if (cheatMgr == null)
            return;

        // One-shot corruption for testing: must run while the component is still active (before we
        // deactivate it below), because it calls the live CheatManager.WriteDUIDs().
        if (CorruptStoredDUID.Value && !_corruptDone)
            _corruptDone = Patches.CorruptDUIDPatch.CorruptStored(cheatMgr);
        else if (RestoreStoredDUID.Value && !_corruptDone)
            _corruptDone = Patches.CorruptDUIDPatch.RestoreStored(cheatMgr);

        cheatMgr.SetActive(false);
        Log.LogInfo("cheatmanager deactivated");
    }
}
