using System;
using System.Collections.Generic;
using System.Linq;
using System.Reflection;
using BestHTTP;
using HarmonyLib;
using Il2CppInterop.Runtime.Injection;
using UnityEngine;

namespace RecNetPlugin.Patches;

// Rebrands Rec Room Plus to Flux Rec + and replaces the real-money
// Steam purchase flow with a token-based purchase (10,000 tokens).
//
// Why this exists: the 2023 client's Plus page tries to load subscription
// prices from Steam. Without Steam, the price load fails with a red error.
// Flux Rec has no Steam integration and must never charge real money.
//
// How it works:
// 1. Intercepts the price-loading HTTP request and returns a fake price
//    (10,000 tokens) so the page shows the price instead of a red error.
// 2. Intercepts BuyRRPlusMembership() to prevent the Steam store from opening.
// 3. Uses the game's own BestHTTP stack (not System.Net.Http) to avoid
//    IL2CPP crashes.
// 4. Backend: POST /api/CampusCard/v1/PurchaseWithTokens (10,000 tokens,
//    sets hasPlus=true, active after next login).
// 5. Shows user-visible success/error feedback through a small IMGUI toast
//    overlay (PlusToastOverlay), since the game's own toast model is not
//    safely resolvable on IL2CPP. HTTP callbacks are marshalled to the main
//    thread through a queue drained in Update().
//
// One knob, see [Plus] in the .cfg:
//   Enable Flux Rec + -> THE FIX (default true).
internal static class FluxPlusPatch
{
    private const int PlusPriceTokens = 10000;
    private const int MaxAttempts = 10;

    private static int _attempts;
    private static bool _done;
    private static bool _branded;
    private static bool _priceInterceptDone;
    private static bool _overlayCreated;
    private static bool _typeRegistered;

    // User-visible purchase feedback (exact strings, keep in sync with docs).
    private const string MsgSuccess =
        "Thanks for becoming a Flux Rec+ member. You now have access to all the member benefits! Please log in again to activate.";
    private const string MsgInsufficientFunds =
        "Not enough tokens. You need 10,000 Flux Rec Tokens.";
    private const string MsgAlreadyOwned =
        "You already have an active Flux Rec+ membership.";
    private const string MsgGenericFailure =
        "Purchase failed. Please try again later.";

    public static void Apply()
    {
        if (!Plugin.EnableFluxPlus.Value)
            return;

        EnsureToastOverlay();

        if (_done || _attempts >= MaxAttempts)
            return;

        _attempts++;

        try
        {
            PatchBuyMethod();
            PatchPriceLoading();
            BrandCommerceStrings();
        }
        catch (Exception e)
        {
            Plugin.Log.LogWarning($"[PLUS] attempt {_attempts} failed: {e.Message}");
            return;
        }

        if (_done)
            Plugin.Log.LogInfo("[PLUS] Flux Rec + patch applied");
        else if (_attempts >= MaxAttempts)
            Plugin.Log.LogWarning("[PLUS] gave up after max attempts");
    }

    // Standalone IMGUI toast overlay (same proven pattern as FluxPairingPatch):
    // created once on the main thread, survives scene loads, draws a transient
    // centered message box. HTTP callbacks enqueue through ShowToast and are
    // drained in Update(), so OnGUI only ever runs on the main thread.
    private static void EnsureToastOverlay()
    {
        if (_overlayCreated) return;

        try
        {
            if (!_typeRegistered)
            {
                ClassInjector.RegisterTypeInIl2Cpp<PlusToastOverlay>();
                _typeRegistered = true;
            }

            var go = new GameObject("FluxPlusToastOverlay");
            go.hideFlags = HideFlags.HideAndDontDestroy;
            UnityEngine.Object.DontDestroyOnLoad(go);
            go.AddComponent<PlusToastOverlay>();
            _overlayCreated = true;
            Plugin.Log.LogInfo("[PLUS] toast overlay ready");
        }
        catch (Exception e)
        {
            Plugin.Log.LogWarning($"[PLUS] toast overlay setup failed: {e.Message}");
        }
    }

    private class PlusToastOverlay : MonoBehaviour
    {
        private const float ToastDurationSec = 6f;

        private static PlusToastOverlay _instance;
        private static readonly Queue<Action> _pending = new Queue<Action>();
        private static readonly object _queueLock = new object();

        private string _message = "";
        private bool _isError;
        private float _hideAt;
        private GUIStyle _boxStyle;
        private GUIStyle _labelStyle;

        // Thread-safe: may be called from BestHTTP callbacks.
        public static void ShowToast(string message, bool isError)
        {
            if (string.IsNullOrEmpty(message)) return;
            try
            {
                lock (_queueLock) { _pending.Enqueue(() => SetToast(message, isError)); }
            }
            catch { }
        }

        private static void SetToast(string message, bool isError)
        {
            try
            {
                if (_instance == null) return;
                _instance._message = message;
                _instance._isError = isError;
                _instance._hideAt = Time.realtimeSinceStartup + ToastDurationSec;
            }
            catch { }
        }

        private void Start()
        {
            _instance = this;
        }

        private void Update()
        {
            try
            {
                while (true)
                {
                    Action a = null;
                    lock (_queueLock)
                    {
                        if (_pending.Count == 0) break;
                        a = _pending.Dequeue();
                    }
                    try { a(); } catch { }
                }
            }
            catch { }
        }

        private void EnsureStyles()
        {
            if (_boxStyle != null) return;
            _boxStyle = new GUIStyle(GUI.skin.box);
            _labelStyle = new GUIStyle(GUI.skin.label);
            _labelStyle.wordWrap = true;
            _labelStyle.alignment = TextAnchor.MiddleCenter;
            _labelStyle.normal.textColor = Color.white;
            _labelStyle.fontSize = 16;
        }

        private void OnGUI()
        {
            try
            {
                if (string.IsNullOrEmpty(_message)) return;
                if (Time.realtimeSinceStartup >= _hideAt) { _message = ""; return; }

                EnsureStyles();

                var w = Mathf.Min(Screen.width - 40f, 560f);
                var h = 92f;
                var rect = new Rect((Screen.width - w) / 2f, Screen.height - h - 90f, w, h);

                var oldBg = GUI.backgroundColor;
                GUI.backgroundColor = _isError
                    ? new Color(0.45f, 0.12f, 0.12f, 0.95f)
                    : new Color(0.10f, 0.35f, 0.14f, 0.95f);
                GUI.Box(rect, GUIContent.none, _boxStyle);
                GUI.backgroundColor = oldBg;

                var labelRect = new Rect(rect.x + 16f, rect.y + 12f, rect.width - 32f, rect.height - 24f);
                GUI.Label(labelRect, _message, _labelStyle);
            }
            catch { }
        }
    }

    private static void PatchBuyMethod()
    {
        var harmony = new Harmony("com.fluxrec.pluspatch");
        var prefix = new HarmonyMethod(typeof(FluxPlusPatch).GetMethod(nameof(InterceptBuy),
            BindingFlags.Static | BindingFlags.NonPublic));

        var method = AppDomain.CurrentDomain.GetAssemblies()
            .SelectMany(a => {
                try { return a.GetTypes(); }
                catch { return Array.Empty<Type>(); }
            })
            .SelectMany(t => {
                try { return t.GetMethods(BindingFlags.Public | BindingFlags.NonPublic | BindingFlags.Instance | BindingFlags.Static); }
                catch { return Array.Empty<MethodInfo>(); }
            })
            .FirstOrDefault(m => m.Name == "BuyRRPlusMembership" && m.GetParameters().Length == 0);

        if (method == null)
        {
            Plugin.Log.LogWarning("[PLUS] BuyRRPlusMembership not found — will retry");
            return;
        }

        harmony.Patch(method, prefix: prefix);
        _done = true;
        Plugin.Log.LogInfo($"[PLUS] intercepted {method.DeclaringType?.Name}.{method.Name}");
    }

    // Intercept the Plus price-loading request. The client tries to fetch
    // subscription prices (from Steam or our backend). We return a fake
    // 10,000-token price so the page shows the price instead of a red error.
    private static void PatchPriceLoading()
    {
        if (_priceInterceptDone) return;
        _priceInterceptDone = true;

        try
        {
            var harmony = new Harmony("com.fluxrec.plusprice");
            // Patch HTTPManager.SendRequest to intercept price requests
            var sendRequest = typeof(HTTPManager).GetMethod("SendRequest", new[] { typeof(HTTPRequest) });
            var postfix = new HarmonyMethod(typeof(FluxPlusPatch).GetMethod(nameof(OnRequestSent),
                BindingFlags.Static | BindingFlags.NonPublic));
            harmony.Patch(sendRequest, postfix: postfix);
            Plugin.Log.LogInfo("[PLUS] price loading interceptor installed");
        }
        catch (Exception e)
        {
            Plugin.Log.LogWarning($"[PLUS] price intercept failed: {e.Message}");
        }
    }

    private static void OnRequestSent(HTTPRequest request)
    {
        // SAFETY FIX 2026-09-25: Do NOT touch request.Callback here.
        // Delegate.CreateDelegate hangs on IL2CPP and breaks ALL HTTP requests,
        // including the critical "Connecting to server" startup flow.
        // The actual Plus fix is PatchBuyMethod (blocks Steam store); this
        // interceptor is diagnostic-only.
        try
        {
            var url = request?.Uri?.AbsoluteUri ?? "";
            if (!url.Contains("CampusCard") && !url.Contains("subscription") && !url.Contains("Subscription"))
                return;
            Plugin.Log.LogInfo($"[PLUS] price request seen: {url}");
        }
        catch { }
    }

    private static void BrandCommerceStrings()
    {
        if (_branded) return;

        try
        {
            var configType = AppDomain.CurrentDomain.GetAssemblies()
                .SelectMany(a => {
                    try { return a.GetTypes(); }
                    catch { return Array.Empty<Type>(); }
                })
                .FirstOrDefault(t => t.Name == "PlayerCommerceConfig");

            if (configType == null) return;

            foreach (var field in configType.GetFields(BindingFlags.Public | BindingFlags.NonPublic | BindingFlags.Static))
            {
                if (field.FieldType == typeof(string))
                {
                    var val = field.GetValue(null) as string;
                    if (!string.IsNullOrEmpty(val) && val.Contains("Rec Room Plus"))
                    {
                        field.SetValue(null, val.Replace("Rec Room Plus", "Flux Rec +"));
                        Plugin.Log.LogInfo($"[PLUS] rebranded field {field.Name}");
                    }
                }
            }

            _branded = true;
        }
        catch (Exception e)
        {
            Plugin.Log.LogWarning($"[PLUS] branding failed: {e.Message}");
        }
    }

    private static bool InterceptBuy(object __instance)
    {
        try
        {
            Plugin.Log.LogInfo("[PLUS] Buy intercepted — showing confirmation dialog");

            var auth = SendRequestPatch.ConnectToRecNetPatch.LastAuthHeader;
            if (string.IsNullOrEmpty(auth))
            {
                Plugin.Log.LogWarning("[PLUS] no auth token — cannot purchase");
                PlusToastOverlay.ShowToast(MsgGenericFailure, true);
                return false;
            }

            // Show the confirmation dialog first; the dialog's Buy button
            // triggers DoTokenPurchaseBestHTTP (game's own BestHTTP stack).
            PlusBuyDialog.Show();
            return false; // Skip original (blocks Steam)
        }
        catch (Exception e)
        {
            Plugin.Log.LogError($"[PLUS] intercept failed: {e.Message}");
            PlusToastOverlay.ShowToast(MsgGenericFailure, true);
            return false;
        }
    }

    // SAFETY FIX 2026-09-26: the previous implementation built its callbacks
    // with Delegate.CreateDelegate(typeof(OnRequestFinishedDelegate),
    // action.Target, action.Method). That reflection-based binding hangs on
    // IL2CPP and breaks ALL HTTP requests (see the 2026-09-25 SAFETY FIX note
    // on OnRequestSent). The rewrite below constructs the delegate directly
    // from a method group (new OnRequestFinishedDelegate(flow.OnX)) — the
    // compiler emits a plain delegate construction, zero runtime reflection —
    // and only ever attaches it to OUR OWN fresh requests, never the game's.
    // Called by PlusBuyDialog after the player confirms the purchase.
    internal static void DoTokenPurchaseBestHTTP(string auth)
    {
        try
        {
            var server = Plugin.ServerHostname.Value.TrimEnd('/');

            // 1. Check balance via BestHTTP (game's own stack).
            // NOTE: this game's BestHTTP is IL2CPP-wrapped: the HTTPRequest ctor takes
            // (Il2CppSystem.Uri, OnRequestFinishedDelegate); method is set via MethodType.
            var flow = new TokenPurchaseFlow { Auth = auth, Server = server };
            var balanceUrl = $"{server}/api/storefronts/v4/balance/2";
            var balanceReq = new HTTPRequest(
                new Il2CppSystem.Uri(balanceUrl),
                new OnRequestFinishedDelegate(flow.OnBalanceFinished));
            balanceReq.MethodType = HTTPMethods.Get;
            balanceReq.SetHeader("Authorization", auth);

            HTTPManager.SendRequest(balanceReq);
        }
        catch (Exception e)
        {
            Plugin.Log.LogError($"[PLUS] purchase flow failed: {e.Message}");
            PlusToastOverlay.ShowToast(MsgGenericFailure, true);
        }
    }

    // Holds per-purchase state so the callbacks don't need closures or
    // reflection. One instance per Buy click; the delegate keeps it alive
    // until the request completes, so overlapping purchases are safe.
    private sealed class TokenPurchaseFlow
    {
        public string Auth = "";
        public string Server = "";

        public void OnBalanceFinished(HTTPRequest req, HTTPResponse resp)
        {
            try
            {
                if (resp == null || resp.StatusCode != 200)
                {
                    Plugin.Log.LogWarning("[PLUS] balance check failed");
                    PlusToastOverlay.ShowToast(MsgGenericFailure, true);
                    return;
                }

                int balance = ParseBalance(resp.DataAsText);
                Plugin.Log.LogInfo($"[PLUS] balance={balance}");

                if (balance < PlusPriceTokens)
                {
                    Plugin.Log.LogWarning($"[PLUS] insufficient tokens ({balance} < {PlusPriceTokens})");
                    PlusToastOverlay.ShowToast(MsgInsufficientFunds, true);
                    return;
                }

                // 2. Purchase with tokens.
                var purchaseUrl = $"{Server}/api/CampusCard/v1/PurchaseWithTokens";
                var purchaseReq = new HTTPRequest(
                    new Il2CppSystem.Uri(purchaseUrl),
                    new OnRequestFinishedDelegate(OnPurchaseFinished));
                purchaseReq.MethodType = HTTPMethods.Post;
                purchaseReq.SetHeader("Authorization", Auth);
                purchaseReq.SetHeader("Content-Type", "application/json");
                purchaseReq.RawData = System.Text.Encoding.UTF8.GetBytes("{}");
                HTTPManager.SendRequest(purchaseReq);
            }
            catch (Exception e)
            {
                Plugin.Log.LogError($"[PLUS] balance callback failed: {e.Message}");
                PlusToastOverlay.ShowToast(MsgGenericFailure, true);
            }
        }

        private static void OnPurchaseFinished(HTTPRequest req, HTTPResponse resp)
        {
            try
            {
                if (resp != null && resp.StatusCode == 200)
                {
                    Plugin.Log.LogInfo("[PLUS] purchase complete — re-login to activate");
                    PlusToastOverlay.ShowToast(MsgSuccess, false);
                }
                else if (resp != null && resp.StatusCode == 400)
                {
                    var code = FluxPairingPatch.GetJsonString(resp.DataAsText ?? "", "error") ?? "";
                    Plugin.Log.LogWarning($"[PLUS] purchase rejected: {code}");
                    if (code == "already_owned")
                        PlusToastOverlay.ShowToast(MsgAlreadyOwned, true);
                    else if (code == "insufficient_funds")
                        PlusToastOverlay.ShowToast(MsgInsufficientFunds, true);
                    else
                        PlusToastOverlay.ShowToast(MsgGenericFailure, true);
                }
                else
                {
                    Plugin.Log.LogWarning($"[PLUS] purchase failed: {resp?.StatusCode}");
                    PlusToastOverlay.ShowToast(MsgGenericFailure, true);
                }
            }
            catch (Exception e)
            {
                Plugin.Log.LogError($"[PLUS] purchase callback failed: {e.Message}");
                PlusToastOverlay.ShowToast(MsgGenericFailure, true);
            }
        }
    }

    private static int ParseBalance(string json)
    {
        try
        {
            // Minimal parser for {"Balance": 12345} or {"balance": 12345}
            json = json.Trim().TrimStart('{').TrimEnd('}');
            foreach (var pair in json.Split(','))
            {
                var kv = pair.Split(new[] { ':' }, 2);
                if (kv.Length != 2) continue;
                var key = kv[0].Trim().Trim('"').ToLower();
                if (key == "balance")
                {
                    var val = kv[1].Trim().Trim('"');
                    if (int.TryParse(val, out var i)) return i;
                }
            }
        }
        catch { }
        return 0;
    }
}
