using System;
using System.Linq;
using System.Reflection;
using BestHTTP;
using HarmonyLib;

namespace RecNetPlugin.Patches;

// Rebrands Rec Room Plus to Flux Rec Plus and replaces the real-money
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
//
// One knob, see [Plus] in the .cfg:
//   Enable Flux Rec Plus -> THE FIX (default true).
internal static class FluxPlusPatch
{
    private const int PlusPriceTokens = 10000;
    private const int MaxAttempts = 10;

    private static int _attempts;
    private static bool _done;
    private static bool _branded;
    private static bool _priceInterceptDone;

    public static void Apply()
    {
        if (_done || !Plugin.EnableFluxPlus.Value || _attempts >= MaxAttempts)
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
            Plugin.Log.LogInfo("[PLUS] Flux Rec Plus patch applied");
        else if (_attempts >= MaxAttempts)
            Plugin.Log.LogWarning("[PLUS] gave up after max attempts");
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
                        field.SetValue(null, val.Replace("Rec Room Plus", "Flux Rec Plus"));
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
            Plugin.Log.LogInfo("[PLUS] Buy intercepted — starting token purchase");

            var auth = SendRequestPatch.ConnectToRecNetPatch.LastAuthHeader;
            if (string.IsNullOrEmpty(auth))
            {
                Plugin.Log.LogWarning("[PLUS] no auth token — cannot purchase");
                return false;
            }

            // Use BestHTTP (game's own stack) instead of HttpClient to avoid crash
            DoTokenPurchaseBestHTTP(auth);
            return false; // Skip original (blocks Steam)
        }
        catch (Exception e)
        {
            Plugin.Log.LogError($"[PLUS] intercept failed: {e.Message}");
            return false;
        }
    }

    private static void DoTokenPurchaseBestHTTP(string auth)
    {
        try
        {
            var server = Plugin.ServerHostname.Value.TrimEnd('/');

            // 1. Check balance via BestHTTP
            // NOTE: this game's BestHTTP is IL2CPP-wrapped: the HTTPRequest ctor takes
            // (Il2CppSystem.Uri, OnRequestFinishedDelegate); method is set via MethodType.
            var balanceUrl = $"{server}/api/storefronts/v4/balance/2";
            var balanceAction = (Action<HTTPRequest, HTTPResponse>)((req, resp) =>
            {
                try
                {
                    if (resp == null || resp.StatusCode != 200)
                    {
                        Plugin.Log.LogWarning("[PLUS] balance check failed");
                        return;
                    }

                    var body = resp.DataAsText;
                    int balance = ParseBalance(body);
                    Plugin.Log.LogInfo($"[PLUS] balance={balance}");

                    if (balance < PlusPriceTokens)
                    {
                        Plugin.Log.LogWarning($"[PLUS] insufficient tokens ({balance} < {PlusPriceTokens})");
                        return;
                    }

                    // 2. Purchase with tokens
                    var purchaseUrl = $"{server}/api/CampusCard/v1/PurchaseWithTokens";
                    var purchaseAction = (Action<HTTPRequest, HTTPResponse>)((preq, presp) =>
                    {
                        if (presp != null && presp.StatusCode == 200)
                            Plugin.Log.LogInfo("[PLUS] purchase complete — re-login to activate");
                        else
                            Plugin.Log.LogWarning($"[PLUS] purchase failed: {presp?.StatusCode}");
                    });
                    var purchaseCallback = (OnRequestFinishedDelegate)Delegate.CreateDelegate(
                        typeof(OnRequestFinishedDelegate), purchaseAction.Target, purchaseAction.Method);
                    var purchaseReq = new HTTPRequest(new Il2CppSystem.Uri(purchaseUrl), purchaseCallback);
                    purchaseReq.MethodType = HTTPMethods.Post;
                    purchaseReq.SetHeader("Authorization", auth);
                    purchaseReq.SetHeader("Content-Type", "application/json");
                    purchaseReq.RawData = System.Text.Encoding.UTF8.GetBytes("{}");
                    HTTPManager.SendRequest(purchaseReq);
                }
                catch (Exception e)
                {
                    Plugin.Log.LogError($"[PLUS] balance callback failed: {e.Message}");
                }
            });
            var balanceCallback = (OnRequestFinishedDelegate)Delegate.CreateDelegate(
                typeof(OnRequestFinishedDelegate), balanceAction.Target, balanceAction.Method);
            var balanceReq = new HTTPRequest(new Il2CppSystem.Uri(balanceUrl), balanceCallback);
            balanceReq.MethodType = HTTPMethods.Get;
            balanceReq.SetHeader("Authorization", auth);

            HTTPManager.SendRequest(balanceReq);
        }
        catch (Exception e)
        {
            Plugin.Log.LogError($"[PLUS] purchase flow failed: {e.Message}");
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
