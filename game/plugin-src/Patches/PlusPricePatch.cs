using System;
using System.Collections.Generic;
using System.Linq;
using System.Reflection;
using System.Text;
using BestHTTP;
using HarmonyLib;
using Il2CppInterop.Runtime.Injection;
using UnityEngine;
using UnityEngine.UI;

namespace RecNetPlugin.Patches;

// Fixes the red "Error loading membership prices" on the Flux Rec+ membership page.
//
// v0.1.35 (R11) — CORRECT TARGET: SkuModel.GetDisplayPrice() (verified in the
// 20230414 dump, RVA 0x14ED9B0). This is the method the Plus page actually calls
// to render the membership price. A Harmony prefix skips the original (which
// fails against the platform store with the Goldberg emulator / no Steam
// client, producing the red error bar) and returns the Flux Rec+ token price
// directly: "10,000 Flux Rec Tokens" ("3,500 Flux Rec Tokens" on Saturdays
// UTC). The error string itself is backend-sourced, not present in the client,
// so overriding the price at this point prevents the error from appearing.
//
// v0.1.35 (R11b) — SKU FILTER: the prefix now ONLY intercepts when the instance
// is positively identified as the RR+ membership SKU (checks the underlying
// Sku from SkuModel.get_Sku(): Source==1 / membership-typed source, or a
// membership/RR+ product name). All other SKUs pass through to the original
// method untouched, so unrelated store prices can't break. Identification is
// conservative: if we can't positively identify the SKU, we do NOT intercept
// (the text-sweep fallback below still covers any red error text that shows).
//
// REMOVED in R11: the old PlayerCommerceModel.get_RRPMembershipSKUPrice() hook.
// Re-analysis showed that getter is a trivial cache getter — hooking it did
// nothing useful (the real price load happens in SkuModel.GetDisplayPrice).
// (Per the R11 task: "probably remove, it does nothing useful".)
//
// What else stays:
// 1. FALLBACK: the UI-text sweep is kept — it discovers price-loading methods
//    at runtime (types with Plus/Membership/CampusCard/Subscription in the
//    name, methods with "Price" in the name) and Harmony-patches them: a
//    prefix warms the price cache, a postfix replaces the red error text with
//    the Flux Rec+ token price.
//    (Postfix, not skip-prefix: we don't know the methods' return types, so
//    skipping them could break callers. Replacing the text after the fact is
//    behavior-preserving and safe.)
// 2. The token price comes from GET /api/subscriptionseasons/v1/seasons/current
//    (the TokenPrice field, which already accounts for the Saturday discount).
//    Falls back to local Saturday logic (3,500 on Saturday UTC, 10,000 otherwise)
//    if the backend is unreachable.
// 3. A page watcher (GameObject.SetActive hook, the PlusInspectorPatch pattern)
//    detects when the Plus page opens and re-scans for the error text for ~15s,
//    catching async price failures that land after the load method returns.
// 4. PlusTitlePatch (wired from Apply() below) repairs the page title strings
//    broken by the installer's RRPLUS_TITLE_PATCH ("Rec Flux Rec+ Member").
//
// NOTE: SkuModel is NOT referenced at compile time (typeof(SkuModel) won't
// build against the interop set checked in here) — it is resolved at runtime
// via AccessTools-style name lookup, with a warning + retry if not found.
// IL2CPP safety (see UltraGraphicsPatch / FluxPairingPatch):
// - TryCast<T>() for downcasts, never direct casts.
// - GetComponentsInChildren requires Il2CppSystem.Type, not System.Type.
// - BestHTTP only for our own requests; Delegate.CreateDelegate is used ONLY
//   for our own fresh HTTPRequest (the working FluxPairingPatch pattern).
//   We never touch the game's request callbacks (the v0.1.30 hang).
// - HTTP callbacks marshal to the main thread via a queue pumped by a
//   ClassInjector-registered MonoBehaviour (Unity UI must be touched on the
//   main thread).
// - Everything is fail-soft: any failure logs a warning and leaves the
//   original behavior intact.
// - Retried from OnSceneLoaded (MaxAttempts = 10) since commerce types load lazily.
//
// One knob, see [Plus] in the .cfg:
//   Enable Flux Rec Plus -> THE FIX (default true, shared with FluxPlusPatch).
//
// NOTE: Plugin.cs must call PlusPricePatch.Apply() from Load() and OnSceneLoaded(),
// alongside the existing Patches.FluxPlusPatch.Apply() calls.
internal static class PlusPricePatch
{
    private const int MaxAttempts = 10;
    private const int FallbackPrice = 10000;
    private const int FallbackSaturdayPrice = 3500;
    private const string PriceEndpoint = "/api/subscriptionseasons/v1/seasons/current";
    private const float RescanWindowSeconds = 15f;

    private static int _attempts;
    private static bool _methodsPatched;
    private static bool _watcherPatched;
    private static bool _skuPatched;
    private static bool _pumpCreated;
    private static bool _typeRegistered;

    // The patched MethodInfo for SkuModel.GetDisplayPrice (kept so the prefix
    // knows whether the target is static — a static target has no instance to
    // identify a SKU from).
    private static MethodInfo _displayPriceMethod;

    // Throttled override log: we only announce the first interception and any
    // change of the returned string afterwards.
    private static string _lastLoggedPrice;

    // First-seen diagnostics for the SKU filter (so we can verify in logs that
    // the membership SKU was identified and other SKUs passed through).
    private static bool _loggedMembershipSeen;
    private static bool _loggedPassthroughSeen;

    private static readonly List<string> _patchedMethods = new List<string>();

    // Price cache. Written on the main thread (queue pump) or under _priceLock.
    private static int _cachedPrice = FallbackPrice;
    private static bool _priceFetched;
    private static bool _fetchInFlight;
    private static readonly object _priceLock = new object();

    // Main-thread queue: BestHTTP callbacks arrive on background threads, and
    // Unity UI must only be touched on the main thread.
    private static readonly Queue<Action> _mainThreadQueue = new Queue<Action>();
    private static readonly object _queueLock = new object();

    // Active Plus page + re-scan window (catches async Steam failures).
    private static GameObject _activePlusPage;
    private static float _rescanUntil;

    public static void Apply()
    {
        if (!Plugin.EnableFluxPlus.Value)
            return;

        // Title repair (double-brand fix) rides along: Plugin.cs already calls
        // this Apply() from Load() and OnSceneLoaded(), so no Plugin.cs change
        // is needed to wire PlusTitlePatch.
        try { PlusTitlePatch.Apply(); }
        catch (Exception e)
        {
            Plugin.Log.LogWarning($"[PLUS-PRICE] title patch apply failed: {e.Message}");
        }

        if ((_methodsPatched && _watcherPatched && _skuPatched) || _attempts >= MaxAttempts)
            return;

        _attempts++;

        try
        {
            EnsureQueuePump();
            if (!_methodsPatched)
                PatchPriceMethods();
            if (!_watcherPatched)
                PatchPageWatcher();
            if (!_skuPatched)
                PatchSkuDisplayPrice();
            if (!_priceFetched)
                EnsurePriceFetched();
        }
        catch (Exception e)
        {
            Plugin.Log.LogWarning($"[PLUS-PRICE] attempt {_attempts} failed: {e.Message}");
            return;
        }

        if (_methodsPatched && _watcherPatched && _skuPatched)
            Plugin.Log.LogInfo("[PLUS-PRICE] price display patch applied");
        else if (_attempts >= MaxAttempts)
            Plugin.Log.LogWarning("[PLUS-PRICE] gave up after max attempts");
    }

    // ------------------------------------------------------------------
    // Price-method discovery + patching
    // ------------------------------------------------------------------

    private static void PatchPriceMethods()
    {
        var harmony = new Harmony("com.fluxrec.plusprice");
        var typeKeywords = new[] { "Plus", "Membership", "CampusCard", "Subscription" };

        var prefix = new HarmonyMethod(typeof(PlusPricePatch).GetMethod(nameof(PrefixPriceLoad),
            BindingFlags.Static | BindingFlags.NonPublic));
        var postfix = new HarmonyMethod(typeof(PlusPricePatch).GetMethod(nameof(PostfixPriceLoad),
            BindingFlags.Static | BindingFlags.NonPublic));

        int count = 0;
        foreach (var asm in AppDomain.CurrentDomain.GetAssemblies())
        {
            var asmName = asm.GetName().Name ?? "";
            // Skip our own plugin, BepInEx/Harmony, and engine assemblies for speed.
            if (asmName.StartsWith("RecNetPlugin", StringComparison.Ordinal) ||
                asmName.StartsWith("BepInEx", StringComparison.Ordinal) ||
                asmName.StartsWith("Harmony", StringComparison.Ordinal) ||
                asmName.StartsWith("System", StringComparison.Ordinal) ||
                asmName.StartsWith("mscorlib", StringComparison.Ordinal) ||
                asmName.StartsWith("UnityEngine", StringComparison.Ordinal))
                continue;

            Type[] types;
            try { types = asm.GetTypes(); }
            catch { continue; }

            foreach (var t in types)
            {
                if (!typeKeywords.Any(k => t.Name.IndexOf(k, StringComparison.OrdinalIgnoreCase) >= 0))
                    continue;

                MethodInfo[] methods;
                try
                {
                    methods = t.GetMethods(BindingFlags.Public | BindingFlags.NonPublic |
                        BindingFlags.Instance | BindingFlags.Static | BindingFlags.DeclaredOnly);
                }
                catch { continue; }

                foreach (var m in methods)
                {
                    if (m.Name.IndexOf("Price", StringComparison.OrdinalIgnoreCase) < 0)
                        continue;
                    if (m.IsSpecialName)
                        continue; // skip get_/set_/add_/remove_
                    if (m.GetParameters().Length > 2)
                        continue; // keep Harmony binding simple

                    try
                    {
                        harmony.Patch(m, prefix: prefix, postfix: postfix);
                        _patchedMethods.Add(t.Name + "." + m.Name);
                        count++;
                    }
                    catch (Exception e)
                    {
                        Plugin.Log.LogWarning($"[PLUS-PRICE] could not patch {t.Name}.{m.Name}: {e.Message}");
                    }
                }
            }
        }

        if (count > 0)
        {
            _methodsPatched = true;
            var shown = string.Join(", ", _patchedMethods.Take(10).ToArray());
            Plugin.Log.LogInfo($"[PLUS-PRICE] patched {count} price method(s): {shown}");
        }
        else
        {
            Plugin.Log.LogWarning("[PLUS-PRICE] no price methods found yet — will retry");
        }
    }

    // Prefix: warm the price cache so the postfix (or page watcher) has a
    // value ready. Returns true (never skips the original — we don't know the
    // method's return type, and skipping could break callers).
    private static bool PrefixPriceLoad(object __instance)
    {
        try
        {
            if (!_priceFetched)
                EnsurePriceFetched();
        }
        catch { }
        return true;
    }

    // Postfix: after the original price load runs (and possibly sets the red
    // error), replace the error text with the Flux Rec+ token price.
    private static void PostfixPriceLoad(object __instance)
    {
        try
        {
            var root = InstanceToGameObject(__instance);
            if (root == null)
                return;
            if (!IsPlusPage(root))
                return;

            _activePlusPage = root;
            _rescanUntil = Time.time + RescanWindowSeconds;
            ReplacePriceText(root);
        }
        catch { }
    }

    private static GameObject InstanceToGameObject(object instance)
    {
        try
        {
            if (instance is GameObject go)
                return go;
            if (instance is Component comp)
                return comp.gameObject;
            if (instance is Transform tr)
                return tr.gameObject;
        }
        catch { }
        return null;
    }

    private static bool IsPlusPage(GameObject root)
    {
        try
        {
            var name = root.name ?? "";
            if (name.IndexOf("Plus", StringComparison.OrdinalIgnoreCase) >= 0 ||
                name.IndexOf("Membership", StringComparison.OrdinalIgnoreCase) >= 0)
                return true;
            // Walk up a few parents in case the method lives on a child panel.
            var t = root.transform.parent;
            for (int i = 0; i < 4 && t != null; i++, t = t.parent)
            {
                var pn = t.gameObject.name ?? "";
                if (pn.IndexOf("Plus", StringComparison.OrdinalIgnoreCase) >= 0 ||
                    pn.IndexOf("Membership", StringComparison.OrdinalIgnoreCase) >= 0)
                    return true;
            }
        }
        catch { }
        return false;
    }

    // ------------------------------------------------------------------
    // Page watcher (catches async Steam failures)
    // ------------------------------------------------------------------

    private static void PatchPageWatcher()
    {
        var harmony = new Harmony("com.fluxrec.pluspricewatch");
        var setActive = typeof(GameObject).GetMethod(nameof(GameObject.SetActive));
        var prefix = new HarmonyMethod(typeof(PlusPricePatch).GetMethod(nameof(OnSetActive),
            BindingFlags.Static | BindingFlags.NonPublic));
        harmony.Patch(setActive, prefix: prefix);
        _watcherPatched = true;
        Plugin.Log.LogInfo("[PLUS-PRICE] page watcher installed");
    }

    // ------------------------------------------------------------------
    // Precise price intercept: SkuModel.GetDisplayPrice (R11)
    // ------------------------------------------------------------------
    //
    // The method the Plus page ACTUALLY calls to render the membership price is
    // SkuModel.GetDisplayPrice() (verified in the 20230414 dump, RVA
    // 0x14ED9B0). The error string shown on failure is backend-sourced and does
    // not exist in the client, so overriding the price at this exact point
    // prevents the error from ever appearing.
    //
    // This Harmony prefix skips the original platform-store query entirely and
    // returns the Flux Rec+ token price directly (10,000 tokens, 3,500 on
    // Saturdays UTC — matching the backend's currentPlusPrice() and the token
    // purchase flow) — but ONLY when the instance is positively identified as
    // the RR+ membership SKU (see IsRRPlusMembershipSku). Other SKUs pass
    // through to the original method, so unrelated store prices are untouched.
    //
    // R11 removed the old PlayerCommerceModel.get_RRPMembershipSKUPrice() hook:
    // re-analysis showed that getter is a trivial cache getter and hooking it
    // did nothing useful.
    //
    // SkuModel is resolved at runtime (name lookup across all assemblies) and
    // NEVER referenced as typeof(SkuModel) — it isn't in the interop set this
    // plugin builds against. If the type or method isn't found yet (commerce
    // types load lazily), we log a warning and retry via the Apply() loop —
    // never crash.
    private static void PatchSkuDisplayPrice()
    {
        try
        {
            Type skuType = null;
            foreach (var asm in AppDomain.CurrentDomain.GetAssemblies())
            {
                var asmName = asm.GetName().Name ?? "";
                if (asmName.StartsWith("RecNetPlugin", StringComparison.Ordinal) ||
                    asmName.StartsWith("BepInEx", StringComparison.Ordinal) ||
                    asmName.StartsWith("Harmony", StringComparison.Ordinal))
                    continue;

                Type[] types;
                try { types = asm.GetTypes(); }
                catch { continue; }

                skuType = types.FirstOrDefault(t => t.Name == "SkuModel" ||
                    (t.FullName != null && t.FullName.EndsWith(".SkuModel", StringComparison.Ordinal)));
                if (skuType != null)
                    break;
            }

            if (skuType == null)
            {
                Plugin.Log.LogWarning("[PLUS] SkuModel type not found yet — will retry");
                return;
            }

            var method = skuType.GetMethod("GetDisplayPrice",
                BindingFlags.Public | BindingFlags.NonPublic | BindingFlags.Instance | BindingFlags.Static,
                null, Type.EmptyTypes, null);

            if (method == null)
            {
                Plugin.Log.LogWarning("[PLUS] SkuModel.GetDisplayPrice not found — will retry");
                return;
            }

            if (method.ReturnType != typeof(string))
            {
                Plugin.Log.LogWarning($"[PLUS] SkuModel.GetDisplayPrice has unexpected signature (returns {method.ReturnType?.Name}) — skipping, will retry");
                return;
            }

            var harmony = new Harmony("com.fluxrec.plusskudisplayprice");
            var prefix = new HarmonyMethod(typeof(PlusPricePatch).GetMethod(nameof(PrefixDisplayPrice),
                BindingFlags.Static | BindingFlags.NonPublic));
            harmony.Patch(method, prefix: prefix);
            _displayPriceMethod = method;
            _skuPatched = true;
            Plugin.Log.LogInfo($"[PLUS] SkuModel.GetDisplayPrice intercepted on {skuType.FullName} (static={method.IsStatic}) — membership SKU price overridden");
        }
        catch (Exception e)
        {
            Plugin.Log.LogWarning($"[PLUS] SkuModel intercept failed: {e.Message}");
        }
    }

    // Returns false: skips the original platform-store price query entirely —
    // but ONLY for the RR+ membership SKU. Any other SKU (or an instance we
    // cannot positively identify) returns true so the original method runs
    // untouched. Fail-open: any unexpected error lets the original run, so we
    // can never break the store UI.
    //
    // v0.1.36: throttled per-call diagnostics — every 30s the prefix logs
    // whether it fired and what the SKU looked like, so the log proves
    // whether the hook is installed and why the filter passes/fails.
    private static DateTime _lastPrefixDiagUtc = DateTime.MinValue;
    private static readonly TimeSpan PrefixDiagInterval = TimeSpan.FromSeconds(30);

    private static bool PrefixDisplayPrice(object __instance, ref string __result)
    {
        bool intercept;
        string diagReason;
        try
        {
            // A static target has no instance to identify a SKU from; the
            // membership price UI is the only caller of that shape, so allow it.
            if (_displayPriceMethod != null && _displayPriceMethod.IsStatic)
            {
                intercept = true;
                diagReason = "static target";
            }
            else
            {
                intercept = IsRRPlusMembershipSku(__instance, out diagReason);
            }
        }
        catch (Exception e)
        {
            MaybeLogPrefixDiag(false, "exception: " + e.Message);
            return true; // fail-open: never break store UI on a check failure
        }

        MaybeLogPrefixDiag(intercept, diagReason);

        if (!intercept)
        {
            if (!_loggedPassthroughSeen)
            {
                _loggedPassthroughSeen = true;
                Plugin.Log.LogInfo("[PLUS] GetDisplayPrice called for a non-membership SKU — passing through to original");
            }
            return true;
        }

        if (!_loggedMembershipSeen)
        {
            _loggedMembershipSeen = true;
            Plugin.Log.LogInfo("[PLUS] RR+ membership SKU identified — overriding price");
        }

        try
        {
            __result = GetPriceString();

            // Throttled log: announce the first override, then only when the
            // returned string changes (e.g. Saturday discount kicking in).
            if (_lastLoggedPrice != __result)
            {
                _lastLoggedPrice = __result;
                Plugin.Log.LogInfo($"[PLUS] Price overridden: {__result}");
            }
        }
        catch
        {
            __result = "10,000 Flux Rec Tokens";
        }
        return false;
    }

    // Throttled per-call diagnostic for the SKU filter.
    private static void MaybeLogPrefixDiag(bool intercept, string reason)
    {
        var now = DateTime.UtcNow;
        if (now - _lastPrefixDiagUtc < PrefixDiagInterval)
            return;
        _lastPrefixDiagUtc = now;
        Plugin.Log.LogInfo($"[PLUS] GetDisplayPrice hook fired: intercept={intercept} ({reason})");
    }

    // TRUE only when the instance is positively identified as the RR+
    // membership SKU. Conservative by design: unknown or unidentifiable
    // instances return false (the original runs; the text-sweep fallback still
    // covers any red error text that appears). diagReason explains the
    // decision for the log.
    private static bool IsRRPlusMembershipSku(object skuModel, out string diagReason)
    {
        diagReason = "null instance";
        if (skuModel == null)
            return false;

        try
        {
            // Verified chain: SkuModel.get_Sku() returns the underlying Sku.
            object sku = null;
            var t = skuModel.GetType();
            MethodInfo getSku = null;
            try
            {
                getSku = t.GetMethod("get_Sku",
                    BindingFlags.Public | BindingFlags.NonPublic | BindingFlags.Instance);
            }
            catch { }

            if (getSku != null && getSku.GetParameters().Length == 0)
            {
                try { sku = getSku.Invoke(skuModel, null); }
                catch { }
            }

            // Inspect both the Sku and the SkuModel itself — identifiers may
            // live on either object depending on the build.
            string skuDiag = null, modelDiag = null;
            if (sku != null && IsMembershipSkuObject(sku, out skuDiag))
            {
                diagReason = "Sku matched: " + skuDiag;
                return true;
            }
            if (IsMembershipSkuObject(skuModel, out modelDiag))
            {
                diagReason = "SkuModel matched: " + modelDiag;
                return true;
            }
            diagReason = "no membership identifiers (sku: " +
                (skuDiag ?? "n/a") + "; model: " + (modelDiag ?? "n/a") + ")";
        }
        catch (Exception e)
        {
            diagReason = "check exception: " + e.Message;
        }
        return false;
    }

    // Checks one object (Sku or SkuModel) for membership identifiers:
    //  - a "Source" member whose value is 1, or whose enum name mentions
    //    Membership/Subscription/Plus;
    //  - a name-ish string member (Name/DisplayName/Title/SkuId/...) that
    //    looks like the RR+ membership product;
    //  - a type name that mentions Membership/RRPlus.
    // diag describes what was seen (for the throttled hook log).
    private static bool IsMembershipSkuObject(object o, out string diag)
    {
        diag = "null";
        if (o == null)
            return false;

        try
        {
            var t = o.GetType();
            var typeName = t.Name ?? "";
            if (typeName.IndexOf("Membership", StringComparison.OrdinalIgnoreCase) >= 0 ||
                typeName.IndexOf("RRPlus", StringComparison.OrdinalIgnoreCase) >= 0)
            {
                diag = "type name '" + typeName + "'";
                return true;
            }

            MemberInfo[] members;
            try
            {
                var props = t.GetProperties(BindingFlags.Public | BindingFlags.NonPublic | BindingFlags.Instance);
                var fields = t.GetFields(BindingFlags.Public | BindingFlags.NonPublic | BindingFlags.Instance);
                members = new MemberInfo[props.Length + fields.Length];
                Array.Copy(props, members, props.Length);
                Array.Copy(fields, 0, members, props.Length, fields.Length);
            }
            catch { diag = "member enum failed"; return false; }

            var seen = new List<string>();
            foreach (var m in members)
            {
                string name = m.Name ?? "";
                object value = null;
                try
                {
                    if (m is PropertyInfo pi)
                    {
                        if (!pi.CanRead || pi.GetIndexParameters().Length > 0)
                            continue;
                        value = pi.GetValue(o, null);
                    }
                    else if (m is FieldInfo fi)
                    {
                        value = fi.GetValue(o);
                    }
                    else
                    {
                        continue;
                    }
                }
                catch { continue; }

                if (value == null)
                    continue;

                // "Source==1" check (per research note): the membership SKU's source.
                if (name.Equals("Source", StringComparison.OrdinalIgnoreCase))
                {
                    if (IsMembershipSource(value))
                    {
                        diag = "Source matched (" + SafeToString(value) + ")";
                        return true;
                    }
                    seen.Add("Source=" + SafeToString(value));
                    continue;
                }

                // Name-ish string identifiers.
                if (name.Equals("Name", StringComparison.OrdinalIgnoreCase) ||
                    name.Equals("DisplayName", StringComparison.OrdinalIgnoreCase) ||
                    name.Equals("Title", StringComparison.OrdinalIgnoreCase) ||
                    name.Equals("SkuName", StringComparison.OrdinalIgnoreCase) ||
                    name.Equals("SkuId", StringComparison.OrdinalIgnoreCase) ||
                    name.Equals("ProductId", StringComparison.OrdinalIgnoreCase) ||
                    name.Equals("ProductName", StringComparison.OrdinalIgnoreCase) ||
                    name.Equals("Id", StringComparison.OrdinalIgnoreCase))
                {
                    string s = value as string;
                    if (s == null)
                    {
                        try { s = value.ToString(); }
                        catch { s = null; }
                    }
                    if (LooksLikeMembershipName(s))
                    {
                        diag = name + "='" + s + "'";
                        return true;
                    }
                    if (seen.Count < 6)
                        seen.Add(name + "='" + (s ?? "?") + "'");
                }
            }
            diag = "type '" + typeName + "' [" + string.Join(", ", seen.ToArray()) + "]";
        }
        catch (Exception e)
        {
            diag = "exception: " + e.Message;
        }
        return false;
    }

    private static string SafeToString(object v)
    {
        try { return v?.ToString() ?? "?"; }
        catch { return "?"; }
    }

    // The membership SKU source: numeric value 1, or an enum whose name
    // mentions Membership/Subscription/Plus.
    private static bool IsMembershipSource(object sourceValue)
    {
        if (sourceValue == null)
            return false;

        try
        {
            var svt = sourceValue.GetType();
            if (svt.IsEnum)
            {
                string name = null;
                try { name = sourceValue.ToString(); }
                catch { }
                if (!string.IsNullOrEmpty(name) &&
                    (name.IndexOf("Membership", StringComparison.OrdinalIgnoreCase) >= 0 ||
                     name.IndexOf("Subscription", StringComparison.OrdinalIgnoreCase) >= 0 ||
                     name.IndexOf("Plus", StringComparison.OrdinalIgnoreCase) >= 0))
                    return true;
            }
            if (sourceValue is IConvertible)
            {
                try { return Convert.ToInt64(sourceValue) == 1; }
                catch { }
            }
        }
        catch { }
        return false;
    }

    // Strong membership signals first ("membership", "rr+"), then the
    // "Rec Room+ / Rec Room Plus" product name. "plus" alone is deliberately
    // NOT matched — too weak a signal on its own.
    private static bool LooksLikeMembershipName(string s)
    {
        if (string.IsNullOrEmpty(s))
            return false;
        var lower = s.ToLowerInvariant();
        if (lower.Contains("membership"))
            return true;
        if (lower.Contains("rr+"))
            return true;
        if (lower.Contains("rec room+") || lower.Contains("rec room plus"))
            return true;
        return false;
    }

    // The Flux Rec+ token price string for the membership SKU intercept:
    // "10,000 Flux Rec Tokens" normally, "3,500 Flux Rec Tokens" on Saturdays
    // UTC — matching backend pricing. Prefers the backend-fetched price (GET
    // /api/subscriptionseasons/v1/seasons/current, whose TokenPrice already
    // accounts for the Saturday discount); falls back to local Saturday logic
    // if the backend price hasn't been fetched yet.
    private static string GetPriceString()
    {
        try
        {
            lock (_priceLock)
            {
                if (_priceFetched && _cachedPrice > 0)
                    return string.Format("{0:N0} Flux Rec Tokens", _cachedPrice);
            }
            return DateTime.UtcNow.DayOfWeek == DayOfWeek.Saturday
                ? "3,500 Flux Rec Tokens"
                : "10,000 Flux Rec Tokens";
        }
        catch
        {
            return "10,000 Flux Rec Tokens";
        }
    }

    private static void OnSetActive(GameObject __instance, bool value)
    {
        try
        {
            if (!value || __instance == null)
                return;
            var name = __instance.name ?? "";
            if (name.IndexOf("Plus", StringComparison.OrdinalIgnoreCase) < 0 &&
                name.IndexOf("Membership", StringComparison.OrdinalIgnoreCase) < 0)
                return;

            Plugin.Log.LogInfo($"[PLUS-PRICE] Plus page opened: {name}");
            if (!_priceFetched)
                EnsurePriceFetched();
            _activePlusPage = __instance;
            _rescanUntil = Time.time + RescanWindowSeconds;
            ReplacePriceText(__instance);
        }
        catch { }
    }

    // ------------------------------------------------------------------
    // Price text replacement
    // ------------------------------------------------------------------

    private static void ReplacePriceText(GameObject root)
    {
        if (root == null)
            return;

        int price;
        bool havePrice;
        lock (_priceLock)
        {
            price = _cachedPrice;
            havePrice = _priceFetched;
        }

        string priceText = havePrice ? FormatPrice(price) : "Loading price...";
        int replaced = 0;

        // uGUI Text path.
        // be.788: GetComponentsInChildren requires Il2CppSystem.Type, not System.Type.
        try
        {
            var textType = Il2CppSystem.Type.GetType(typeof(Text).AssemblyQualifiedName);
            var texts = root.GetComponentsInChildren(textType, true);
            if (texts != null)
            {
                foreach (var t in texts)
                {
                    var txt = t.TryCast<Text>();
                    if (txt == null)
                        continue;
                    if (IsPriceErrorText(txt.text))
                    {
                        txt.text = priceText;
                        replaced++;
                    }
                }
            }
        }
        catch (Exception e)
        {
            Plugin.Log.LogWarning($"[PLUS-PRICE] uGUI scan failed: {e.Message}");
        }

        // TMPro fallback (no compile-time dependency).
        try
        {
            var tmproAsm = AppDomain.CurrentDomain.GetAssemblies()
                .FirstOrDefault(a => a.GetName().Name == "Unity.TextMeshPro");
            var tmproType = tmproAsm?.GetType("TMPro.TextMeshProUGUI");
            if (tmproType != null)
            {
                var getTexts = typeof(GameObject).GetMethod("GetComponentsInChildren",
                    new[] { typeof(Type), typeof(bool) });
                var comps = (System.Collections.IEnumerable)getTexts.Invoke(root,
                    new object[] { tmproType, true });
                var textProp = tmproType.GetProperty("text");
                foreach (var c in comps)
                {
                    var cur = textProp.GetValue(c, null) as string;
                    if (IsPriceErrorText(cur))
                    {
                        textProp.SetValue(c, priceText, null);
                        replaced++;
                    }
                }
            }
        }
        catch (Exception e)
        {
            Plugin.Log.LogWarning($"[PLUS-PRICE] TMPro scan failed: {e.Message}");
        }

        if (replaced > 0)
            Plugin.Log.LogInfo($"[PLUS-PRICE] replaced {replaced} price text(s) with \"{priceText}\"");
    }

    private static bool IsPriceErrorText(string s)
    {
        if (string.IsNullOrEmpty(s))
            return false;
        var lower = s.ToLowerInvariant();
        return lower.Contains("error loading") ||
               (lower.Contains("membership") && lower.Contains("price"));
    }

    private static string FormatPrice(int tokens)
    {
        // "10,000 Flux Rec Tokens / 30 days" (N0 adds the thousands separator).
        try
        {
            return string.Format("{0:N0} Flux Rec Tokens / 30 days", tokens);
        }
        catch
        {
            return tokens + " Flux Rec Tokens / 30 days";
        }
    }

    // ------------------------------------------------------------------
    // Backend price fetch (safe BestHTTP pattern from FluxPairingPatch)
    // ------------------------------------------------------------------

    private static void EnsurePriceFetched()
    {
        lock (_priceLock)
        {
            if (_priceFetched || _fetchInFlight)
                return;
            _fetchInFlight = true;
        }

        try
        {
            var server = Plugin.ServerHostname.Value.TrimEnd('/');
            var url = server + PriceEndpoint;

            // Auth is optional for this endpoint, but include it when available.
            string auth = null;
            try { auth = SendRequestPatch.ConnectToRecNetPatch.LastAuthHeader; }
            catch { }

            // Delegate.CreateDelegate is used ONLY for our own fresh request
            // (the working FluxPairingPatch pattern) — never on the game's
            // callbacks, so the v0.1.30 hang cannot recur.
            Action<HTTPRequest, HTTPResponse> action = (req, resp) =>
            {
                try
                {
                    int status = resp != null ? resp.StatusCode : -1;
                    string body = resp != null ? (resp.DataAsText ?? "") : "";
                    lock (_queueLock) { _mainThreadQueue.Enqueue(() => OnPriceResponse(status, body)); }
                }
                catch (Exception e)
                {
                    Plugin.Log.LogWarning("[PLUS-PRICE] price callback failed: " + e.Message);
                    lock (_queueLock) { _mainThreadQueue.Enqueue(() => OnPriceResponse(-1, "")); }
                }
            };
            var cb = (OnRequestFinishedDelegate)Delegate.CreateDelegate(
                typeof(OnRequestFinishedDelegate), action.Target, action.Method);
            var httpReq = new HTTPRequest(new Il2CppSystem.Uri(url), cb);
            httpReq.MethodType = HTTPMethods.Get;
            if (!string.IsNullOrEmpty(auth))
                httpReq.SetHeader("Authorization", auth);

            HTTPManager.SendRequest(httpReq);
            Plugin.Log.LogInfo("[PLUS-PRICE] fetching token price from " + url);
        }
        catch (Exception e)
        {
            Plugin.Log.LogWarning("[PLUS-PRICE] price fetch failed: " + e.Message);
            lock (_priceLock)
            {
                _fetchInFlight = false;
                _cachedPrice = LocalSaturdayPrice();
                _priceFetched = true; // use fallback; don't retry aggressively
            }
        }
    }

    // Runs on the main thread (via the queue pump).
    private static void OnPriceResponse(int status, string body)
    {
        lock (_priceLock)
        {
            _fetchInFlight = false;
            if (status == 200 && !string.IsNullOrEmpty(body))
            {
                int price = ParseTokenPrice(body);
                if (price > 0)
                {
                    _cachedPrice = price;
                    _priceFetched = true;
                    Plugin.Log.LogInfo($"[PLUS-PRICE] token price = {_cachedPrice}");
                }
                else
                {
                    _cachedPrice = LocalSaturdayPrice();
                    _priceFetched = true;
                    Plugin.Log.LogWarning("[PLUS-PRICE] could not parse price, using fallback");
                }
            }
            else
            {
                _cachedPrice = LocalSaturdayPrice();
                _priceFetched = true;
                Plugin.Log.LogWarning($"[PLUS-PRICE] price fetch HTTP {status}, using fallback");
            }
        }

        // If a Plus page is open, refresh it now that we have the price.
        if (_activePlusPage != null)
            ReplacePriceText(_activePlusPage);
    }

    // Minimal parser for [{"SeasonId":1,...,"TokenPrice":10000,"TokenPriceSaturday":3500}]
    private static int ParseTokenPrice(string json)
    {
        try
        {
            const string key = "\"TokenPrice\":";
            int idx = json.IndexOf(key, StringComparison.Ordinal);
            if (idx < 0)
                return -1;
            idx += key.Length;
            var sb = new StringBuilder();
            while (idx < json.Length && (char.IsDigit(json[idx]) || json[idx] == '-'))
            {
                sb.Append(json[idx]);
                idx++;
            }
            if (int.TryParse(sb.ToString(), out int price) && price > 0)
                return price;
        }
        catch { }
        return -1;
    }

    private static int LocalSaturdayPrice()
    {
        try
        {
            return DateTime.UtcNow.DayOfWeek == DayOfWeek.Saturday
                ? FallbackSaturdayPrice
                : FallbackPrice;
        }
        catch
        {
            return FallbackPrice;
        }
    }

    // ------------------------------------------------------------------
    // Main-thread queue pump
    // ------------------------------------------------------------------

    private static void EnsureQueuePump()
    {
        if (_pumpCreated)
            return;
        try
        {
            if (!_typeRegistered)
            {
                ClassInjector.RegisterTypeInIl2Cpp<PriceQueuePump>();
                _typeRegistered = true;
            }
            var go = new GameObject("FluxPlusPricePump");
            go.hideFlags = HideFlags.HideAndDontSave;
            UnityEngine.Object.DontDestroyOnLoad(go);
            go.AddComponent<PriceQueuePump>();
            _pumpCreated = true;
        }
        catch (Exception e)
        {
            Plugin.Log.LogWarning("[PLUS-PRICE] queue pump failed: " + e.Message);
        }
    }

    private class PriceQueuePump : MonoBehaviour
    {
        // Throttle for the fallback Plus-page scan (the SetActive hook may
        // not fire under IL2CPP).
        private float _nextPageScanAt;

        void Update()
        {
            // Pump main-thread queue (HTTP callbacks arrive on background threads).
            try
            {
                while (true)
                {
                    Action act = null;
                    lock (_queueLock)
                    {
                        if (_mainThreadQueue.Count == 0)
                            break;
                        act = _mainThreadQueue.Dequeue();
                    }
                    try { act?.Invoke(); }
                    catch (Exception e)
                    {
                        Plugin.Log.LogWarning("[PLUS-PRICE] queued action failed: " + e.Message);
                    }
                }
            }
            catch { }

            // Fallback page detection (v0.1.36): the SetActive Harmony hook
            // may never fire under IL2CPP, so if no page is being re-scanned,
            // look for a visible Plus page every 2s and start the re-scan.
            try
            {
                if (_activePlusPage == null && Time.unscaledTime >= _nextPageScanAt)
                {
                    _nextPageScanAt = Time.unscaledTime + 2f;
                    var page = FindActivePlusPage();
                    if (page != null)
                    {
                        Plugin.Log.LogInfo($"[PLUS-PRICE] Plus page detected by fallback scan: {page.name}");
                        if (!_priceFetched)
                            EnsurePriceFetched();
                        _activePlusPage = page;
                        _rescanUntil = Time.time + RescanWindowSeconds;
                        ReplacePriceText(page);
                    }
                }
            }
            catch { }

            // Re-scan the active Plus page for the error text. This catches
            // async Steam price failures that land after the load method returns.
            try
            {
                if (_activePlusPage != null)
                {
                    if (Time.time < _rescanUntil)
                        ReplacePriceText(_activePlusPage);
                    else
                        _activePlusPage = null; // re-scan window expired
                }
            }
            catch { }
        }
    }

    // Finds a VISIBLE Plus/Membership page root for the fallback scan.
    private static GameObject FindActivePlusPage()
    {
        try
        {
            var find = typeof(UnityEngine.Object).GetMethod("FindObjectsOfType",
                new[] { typeof(Type) });
            if (find == null)
                return null;
            var all = (System.Collections.IEnumerable)find.Invoke(null,
                new object[] { typeof(GameObject) });
            if (all == null)
                return null;
            foreach (var o in all)
            {
                var go = ((UnityEngine.Object)o).TryCast<GameObject>();
                if (go == null || !go.activeInHierarchy)
                    continue;
                var name = go.name;
                if (string.IsNullOrEmpty(name))
                    continue;
                if (name.IndexOf("Plus", StringComparison.OrdinalIgnoreCase) < 0 &&
                    name.IndexOf("Membership", StringComparison.OrdinalIgnoreCase) < 0)
                    continue;
                if (name.Equals("FluxPlusPricePump", StringComparison.Ordinal))
                    continue;
                return go;
            }
        }
        catch { }
        return null;
    }
}
