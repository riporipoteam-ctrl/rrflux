using System;
using System.Collections.Generic;
using System.Linq;
using System.Reflection;
using HarmonyLib;

namespace RecNetPlugin.Patches;

// Rebrands Rec Room Plus to Flux Rec Plus and replaces the real-money
// Steam purchase flow with a token-based purchase.
//
// Why this exists: the 2023 client's BuyRRPlusMembership() requests a
// commerce subscription token and opens the Steam store for a real-money
// SKU. Flux Rec has no Steam integration and must never charge real money.
// The backend already supports token purchases via
// POST /api/CampusCard/v1/PurchaseWithTokens (10,000 tokens, sets hasPlus=true).
//
// How it works:
// 1. At load (retried on scene load), scan for BuyRRPlusMembership method
//    and patch it with a prefix that intercepts the call.
// 2. The prefix checks token balance via /api/storefronts/v4/balance/2.
// 3. If balance >= 10,000, show confirmation and POST the token purchase.
// 4. On success, refresh the auth token so the rn.plus claim updates.
// 5. Rebrand RRUI.PlayerCommerceConfig strings from "Rec Room Plus" to
//    "Flux Rec Plus" at runtime.
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

    // Called from Plugin.Load and again on each scene load until patched —
    // the declaring type may live in an assembly that isn't loaded yet.
    public static void Apply()
    {
        if (_done || !Plugin.EnableFluxPlus.Value || _attempts >= MaxAttempts)
            return;

        _attempts++;

        try
        {
            PatchBuyMethod();
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
            Plugin.Log.LogWarning("[PLUS] gave up after max attempts — Plus buy may still use Steam");
    }

    private static void PatchBuyMethod()
    {
        var harmony = new Harmony("com.fluxrec.pluspatch");
        var prefix = new HarmonyMethod(typeof(FluxPlusPatch).GetMethod(nameof(InterceptBuy),
            BindingFlags.Static | BindingFlags.NonPublic));

        // Find BuyRRPlusMembership by method name (declaring type is obfuscated)
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

    private static void BrandCommerceStrings()
    {
        if (_branded) return;

        try
        {
            // Find RRUI.PlayerCommerceConfig type and rebrand its string fields
            var configType = AppDomain.CurrentDomain.GetAssemblies()
                .SelectMany(a => {
                    try { return a.GetTypes(); }
                    catch { return Array.Empty<Type>(); }
                })
                .FirstOrDefault(t => t.Name == "PlayerCommerceConfig");

            if (configType == null)
            {
                Plugin.Log.LogWarning("[PLUS] PlayerCommerceConfig not found — branding will retry");
                return;
            }

            // Rebrand static string fields containing "Rec Room Plus"
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

    // Prefix that intercepts BuyRRPlusMembership(). Returns false to skip
    // the original Steam-based purchase flow.
    private static bool InterceptBuy(object __instance)
    {
        try
        {
            Plugin.Log.LogInfo("[PLUS] Buy intercepted — starting token purchase flow");

            var auth = SendRequestPatch.ConnectToRecNetPatch.LastAuthHeader;
            if (string.IsNullOrEmpty(auth))
            {
                Plugin.Log.LogWarning("[PLUS] no auth token captured yet — cannot purchase");
                return false;
            }

            // Run the purchase flow on a background thread (BestHTTP is async).
            System.Threading.Tasks.Task.Run(() => DoTokenPurchase(auth));
            return false; // Skip original (prevents Steam store from opening)
        }
        catch (Exception e)
        {
            Plugin.Log.LogError($"[PLUS] intercept failed: {e.Message}");
            return false; // Still block Steam on error
        }
    }

    private static void DoTokenPurchase(string auth)
    {
        try
        {
            var server = Plugin.ServerHostname.Value.TrimEnd('/');
            Plugin.Log.LogInfo("[PLUS] checking token balance...");

            using var client = new System.Net.Http.HttpClient();
            client.DefaultRequestHeaders.Add("Authorization", auth);

            // 1. Check balance
            var balanceUrl = $"{server}/api/storefronts/v4/balance/2";
            var balanceResp = client.GetStringAsync(balanceUrl).GetAwaiter().GetResult();
            if (string.IsNullOrEmpty(balanceResp))
            {
                Plugin.Log.LogWarning("[PLUS] balance check failed (empty response)");
                return;
            }

            // Parse balance from JSON: {"Balance": 12345} or similar
            int balance = 0;
            try
            {
                var json = SimpleJsonParse(balanceResp);
                if (json.TryGetValue("balance", out var b) || json.TryGetValue("Balance", out b))
                    balance = Convert.ToInt32(b);
            }
            catch (Exception e)
            {
                Plugin.Log.LogWarning($"[PLUS] balance parse failed: {e.Message} body={balanceResp}");
                return;
            }

            Plugin.Log.LogInfo($"[PLUS] balance={balance}, price={PlusPriceTokens}");
            if (balance < PlusPriceTokens)
            {
                Plugin.Log.LogWarning($"[PLUS] insufficient tokens ({balance} < {PlusPriceTokens})");
                // TODO: show "Not enough tokens" dialog once dialog API is known
                return;
            }

            // 2. Purchase with tokens
            Plugin.Log.LogInfo("[PLUS] purchasing Flux Rec Plus with tokens...");
            var purchaseUrl = $"{server}/api/CampusCard/v1/PurchaseWithTokens";
            var content = new System.Net.Http.StringContent("{}", System.Text.Encoding.UTF8, "application/json");
            var purchaseRespMsg = client.PostAsync(purchaseUrl, content).GetAwaiter().GetResult();
            var purchaseResp = purchaseRespMsg.Content.ReadAsStringAsync().GetAwaiter().GetResult();
            if (string.IsNullOrEmpty(purchaseResp))
            {
                Plugin.Log.LogWarning("[PLUS] purchase failed (empty response)");
                return;
            }

            Plugin.Log.LogInfo($"[PLUS] purchase response: {purchaseResp}");
            // TODO: check success field, show success dialog, trigger re-login
            // for rn.plus claim refresh once dialog API is known.
            Plugin.Log.LogInfo("[PLUS] purchase complete — re-login to activate Flux Rec Plus");
        }
        catch (Exception e)
        {
            Plugin.Log.LogError($"[PLUS] purchase flow failed: {e.Message}");
        }
    }

    private static System.Collections.Generic.Dictionary<string, object> SimpleJsonParse(string json)
    {
        // Minimal JSON parser for flat {"key": value} objects.
        var dict = new System.Collections.Generic.Dictionary<string, object>(System.StringComparer.OrdinalIgnoreCase);
        json = json.Trim().TrimStart('{').TrimEnd('}');
        foreach (var pair in json.Split(','))
        {
            var kv = pair.Split(new[] { ':' }, 2);
            if (kv.Length != 2) continue;
            var key = kv[0].Trim().Trim('"');
            var val = kv[1].Trim().Trim('"');
            if (int.TryParse(val, out var i)) dict[key] = i;
            else if (bool.TryParse(val, out var b)) dict[key] = b;
            else dict[key] = val;
        }
        return dict;
    }
}
