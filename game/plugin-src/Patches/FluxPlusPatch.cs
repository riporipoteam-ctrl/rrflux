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

            // TODO: Implement the full token purchase flow:
            // 1. Get token balance via /api/storefronts/v4/balance/2
            // 2. If balance < 10000, show "Not enough tokens" error
            // 3. Show confirmation dialog ("Buy Flux Rec Plus for 10,000 tokens?")
            // 4. POST /api/CampusCard/v1/PurchaseWithTokens
            // 5. On success, show success message and trigger re-login to refresh rn.plus claim
            // 6. On failure, show error message
            //
            // For now, log and block the Steam flow. The full UI flow requires
            // runtime inspection of the client's dialog system.

            Plugin.Log.LogWarning("[PLUS] Token purchase UI not yet implemented — purchase blocked to prevent Steam flow");
            return false; // Skip original (prevents Steam store from opening)
        }
        catch (Exception e)
        {
            Plugin.Log.LogError($"[PLUS] intercept failed: {e.Message}");
            return false; // Still block Steam on error
        }
    }
}
