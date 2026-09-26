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
// Root cause: the 2023 client queries Steam for localized subscription prices.
// With the Goldberg emulator (no real Steam client), the price query fails and
// the page shows a red error instead of a price.
//
// What this does:
// 1. Discovers price-loading methods at runtime (types with Plus/Membership/
//    Subscription/CampusCard in the name, methods with "Price" in the name)
//    and Harmony-patches them: a prefix warms the price cache, a postfix
//    replaces the red error text with the Flux Rec+ token price.
//    (Postfix, not skip-prefix: we don't know the methods' return types, so
//    skipping them could break callers. Replacing the text after the fact is
//    behavior-preserving and safe.)
// 2. The token price comes from GET /api/subscriptionseasons/v1/seasons/current
//    (the TokenPrice field, which already accounts for the Saturday discount).
//    Falls back to local Saturday logic (3,500 on Saturday UTC, 10,000 otherwise)
//    if the backend is unreachable.
// 3. A page watcher (GameObject.SetActive hook, the PlusInspectorPatch pattern)
//    detects when the Plus page opens and re-scans for the error text for ~15s,
//    catching async Steam failures that land after the load method returns.
//
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
    private static bool _pumpCreated;
    private static bool _typeRegistered;

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
        if ((_methodsPatched && _watcherPatched) || _attempts >= MaxAttempts)
            return;

        _attempts++;

        try
        {
            EnsureQueuePump();
            if (!_methodsPatched)
                PatchPriceMethods();
            if (!_watcherPatched)
                PatchPageWatcher();
            if (!_priceFetched)
                EnsurePriceFetched();
        }
        catch (Exception e)
        {
            Plugin.Log.LogWarning($"[PLUS-PRICE] attempt {_attempts} failed: {e.Message}");
            return;
        }

        if (_methodsPatched && _watcherPatched)
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
            go.hideFlags = HideFlags.HideAndDontDestroy;
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
}
