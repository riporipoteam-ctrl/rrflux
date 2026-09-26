using System;
using System.Collections.Generic;
using System.Linq;
using System.Reflection;
using BestHTTP;
using HarmonyLib;
using Il2CppInterop.Runtime.Injection;
using UnityEngine;
using UnityEngine.UI;

namespace RecNetPlugin.Patches;

// Shows the player's token balance next to the Buy button on the Flux Rec+
// membership page: "Balance: 12,345 tokens".
//
// Why this exists: the buy flow costs 10,000 tokens (3,500 on Saturdays),
// but the stock page never shows the player's balance — they have to guess
// whether they can afford it.
//
// How it works:
// 1. Watches GameObject.SetActive for the Plus page opening
//    (RecNetRRPlusMembershipPage / MembershipBenefitsScreen). The page types
//    are obfuscated, so detection is by GameObject name — the proven
//    PlusInspectorPatch pattern. Injection only proceeds when the
//    BuyRRPlusMembershipButtonImpl (unobfuscated name, confirmed in metadata)
//    is actually found inside, so random "Plus" popups are never touched.
// 2. Clones an existing Text label from the page (guaranteed style match —
//    same font/material/shaders), inserts it as a sibling right after the
//    Buy button, and strips any Button component from the clone so it can
//    never trigger a purchase.
// 3. Balance source: GET {server}/api/storefronts/v4/balance/2
//    (RecCenterTokens — the same bucket the purchase charges), with the
//    game's own Authorization header captured by
//    SendRequestPatch.ConnectToRecNetPatch.LastAuthHeader. The request goes
//    through the game's BestHTTP stack with the proven safe pattern:
//    Delegate.CreateDelegate ONLY for our own fresh request — never on the
//    game's callbacks (the v0.1.30 hang).
// 4. BestHTTP callbacks run on a background thread; UI updates are
//    marshalled to the main thread through a tiny IL2CPP dispatcher
//    MonoBehaviour (ClassInjector.RegisterTypeInIl2Cpp, the FluxPairingPatch
//    pattern). The dispatcher also retries label injection for ~0.5s in case
//    the Buy button is built a frame after the page activates.
//
// One knob, see [Plus] in the .cfg:
//   Show Token Balance -> THE FIX (default true).
internal static class PlusBalancePatch
{
    private const string LabelObjectName = "FluxBalanceLabel";
    private const string DispatcherObjectName = "FluxPlusBalanceDispatcher";
    private const string BuyButtonImplName = "BuyRRPlusMembershipButtonImpl";
    private const int MaxPageRetries = 30; // ~0.5s at 60fps

    private static bool _hookInstalled;
    private static bool _dispatcherReady;
    private static readonly Harmony _harmony = new Harmony("com.fluxrec.plusbalance");

    public static void Apply()
    {
        if (!Plugin.EnablePlusBalance.Value)
            return;

        try
        {
            if (!_dispatcherReady)
            {
                ClassInjector.RegisterTypeInIl2Cpp<PlusBalanceDispatcher>();
                var go = new GameObject(DispatcherObjectName);
                go.hideFlags = HideFlags.HideAndDontSave;
                UnityEngine.Object.DontDestroyOnLoad(go);
                go.AddComponent<PlusBalanceDispatcher>();
                _dispatcherReady = true;
                Plugin.Log.LogInfo("[PLUS-BALANCE] dispatcher ready");
            }

            if (!_hookInstalled)
            {
                var setActive = typeof(GameObject).GetMethod(nameof(GameObject.SetActive));
                var prefix = new HarmonyMethod(typeof(PlusBalancePatch).GetMethod(nameof(OnSetActive),
                    BindingFlags.Static | BindingFlags.NonPublic));
                _harmony.Patch(setActive, prefix: prefix);
                _hookInstalled = true;
                Plugin.Log.LogInfo("[PLUS-BALANCE] page watcher installed");
            }
        }
        catch (Exception e)
        {
            Plugin.Log.LogWarning($"[PLUS-BALANCE] setup failed: {e.Message}");
        }
    }

    // Fires on the main thread (called by Unity UI code).
    private static void OnSetActive(GameObject __instance, bool value)
    {
        try
        {
            if (!Plugin.EnablePlusBalance.Value)
                return;
            if (!value || __instance == null)
                return;
            var name = __instance.name;
            if (string.IsNullOrEmpty(name))
                return;
            if (name.Contains(LabelObjectName) || name.Contains(DispatcherObjectName))
                return; // never our own objects
            if (!name.Contains("Plus") && !name.Contains("Membership"))
                return;

            Plugin.Log.LogInfo($"[PLUS-BALANCE] Plus page activated: {name}");
            PlusBalanceDispatcher.WatchPage(__instance);
        }
        catch { }
    }

    // Runs on the main thread via the dispatcher. Returns true when the page
    // is settled (label added or definitively impossible), false to retry.
    internal static bool TryAddBalanceLabel(GameObject page)
    {
        try
        {
            if (page == null)
                return true; // destroyed — stop retrying

            // Already labelled? Just refresh the balance and stop.
            var existing = FindDeep(page.transform, LabelObjectName);
            if (existing != null)
            {
                RefreshBalance(existing.gameObject);
                return true;
            }

            var buyButton = FindBuyButton(page.transform);
            if (buyButton == null)
                return false; // not built yet — retry

            var label = CreateLabel(page, buyButton);
            if (label == null)
                return true; // no text source on this page — stop retrying

            RefreshBalance(label);
            Plugin.Log.LogInfo($"[PLUS-BALANCE] balance label added next to '{buyButton.name}'");
            return true;
        }
        catch (Exception e)
        {
            Plugin.Log.LogWarning($"[PLUS-BALANCE] inject failed: {e.Message}");
            return true;
        }
    }

    private static Transform FindDeep(Transform root, string name)
    {
        if (root == null)
            return null;
        if (root.name == name)
            return root;
        for (int i = 0; i < root.childCount; i++)
        {
            var found = FindDeep(root.GetChild(i), name);
            if (found != null)
                return found;
        }
        return null;
    }

    // Precise first (BuyRRPlusMembershipButtonImpl — unobfuscated), then a
    // "Buy" GameObject fallback.
    private static GameObject FindBuyButton(Transform root)
    {
        GameObject buyByName = null;
        var stack = new Stack<Transform>();
        stack.Push(root);
        while (stack.Count > 0)
        {
            var t = stack.Pop();
            if (t == null)
                continue;

            try
            {
                foreach (var c in t.GetComponents<Component>())
                {
                    if (c == null)
                        continue;
                    var cn = c.GetType().Name;
                    if (cn != null && cn.Contains(BuyButtonImplName))
                        return t.gameObject;
                }
            }
            catch { }

            if (buyByName == null && t.name != null &&
                t.name.IndexOf("buy", StringComparison.OrdinalIgnoreCase) >= 0)
                buyByName = t.gameObject;

            for (int i = 0; i < t.childCount; i++)
                stack.Push(t.GetChild(i));
        }
        return buyByName;
    }

    private static GameObject CreateLabel(GameObject page, GameObject buyButton)
    {
        // Clone an existing page Text so the style (font/material) matches.
        var sourceText = FindSourceText(page.transform);
        GameObject labelGo;
        if (sourceText != null)
        {
            var cloneObj = UnityEngine.Object.Instantiate(sourceText.gameObject);
            labelGo = cloneObj.TryCast<GameObject>();
            if (labelGo == null)
                return null;
            labelGo.name = LabelObjectName;

            // Safety: the clone must never be clickable. Strip any Button.
            try
            {
                var buttonType = Il2CppSystem.Type.GetType(typeof(Button).AssemblyQualifiedName);
                var btn = labelGo.GetComponent(buttonType);
                if (btn != null)
                    UnityEngine.Object.Destroy(btn);
            }
            catch { }
        }
        else
        {
            // Last resort: build a bare Text. Wrapped — never fatal.
            try
            {
                labelGo = new GameObject(LabelObjectName);
                var txt = labelGo.AddComponent<Text>();
                txt.fontSize = 14;
                txt.color = Color.white;
                txt.alignment = TextAnchor.MiddleCenter;
            }
            catch (Exception e)
            {
                Plugin.Log.LogWarning($"[PLUS-BALANCE] no text source and bare Text failed: {e.Message}");
                return null;
            }
        }

        var parent = buyButton.transform.parent ?? page.transform;
        labelGo.transform.SetParent(parent, false);
        labelGo.transform.SetSiblingIndex(buyButton.transform.GetSiblingIndex() + 1);
        SetLabelText(labelGo, "Balance: ...");
        return labelGo;
    }

    private static GameObject FindSourceText(Transform root)
    {
        try
        {
            // be.788: GetComponentsInChildren requires Il2CppSystem.Type.
            var textType = Il2CppSystem.Type.GetType(typeof(Text).AssemblyQualifiedName);
            var texts = root.GetComponentsInChildren(textType, true);
            if (texts != null)
            {
                foreach (var o in texts)
                {
                    var txt = ((UnityEngine.Object)o).TryCast<Text>();
                    if (txt != null && !string.IsNullOrWhiteSpace(txt.text))
                        return txt.gameObject;
                }
            }
        }
        catch { }
        return null;
    }

    private static void SetLabelText(GameObject labelGo, string text)
    {
        try
        {
            var label = labelGo.TryCast<GameObject>()?.GetComponent(
                Il2CppSystem.Type.GetType(typeof(Text).AssemblyQualifiedName))?.TryCast<Text>();
            if (label != null)
            {
                label.text = text;
                return;
            }
        }
        catch { }

        // TMPro fallback (no compile-time dependency).
        try
        {
            var tmproAsm = AppDomain.CurrentDomain.GetAssemblies()
                .FirstOrDefault(a => a.GetName().Name == "Unity.TextMeshPro");
            var tmproType = tmproAsm?.GetType("TMPro.TextMeshProUGUI");
            if (tmproType == null)
                return;
            var il2cppType = Il2CppSystem.Type.GetType(tmproType.AssemblyQualifiedName);
            var comp = labelGo.GetComponent(il2cppType);
            tmproType.GetProperty("text")?.SetValue(comp, text, null);
        }
        catch { }
    }

    private static void RefreshBalance(GameObject labelGo)
    {
        SetLabelText(labelGo, "Balance: ...");
        FetchBalance(balance =>
        {
            // Already on the main thread (FetchBalance marshals).
            var text = balance >= 0 ? $"Balance: {balance:N0} tokens" : "Balance: --";
            SetLabelText(labelGo, text);
            Plugin.Log.LogInfo($"[PLUS-BALANCE] {text}");
        });
    }

    // Balance source: the econ worker's storefront balance endpoint, same
    // bucket the token purchase charges (currencyType 2 = RecCenterTokens).
    // Response shape: [{"CurrencyType":2,"Platform":..,"Balance":12345}]
    private static void FetchBalance(Action<int> onDone)
    {
        try
        {
            var auth = SendRequestPatch.ConnectToRecNetPatch.LastAuthHeader;
            if (string.IsNullOrEmpty(auth))
            {
                Plugin.Log.LogWarning("[PLUS-BALANCE] no auth token yet — balance unavailable");
                onDone(-1);
                return;
            }

            var server = Plugin.ServerHostname.Value.TrimEnd('/');
            var url = $"{server}/api/storefronts/v4/balance/2";

            // The callback runs on a background thread: marshal the result
            // back to the main thread through the dispatcher queue.
            // Delegate.CreateDelegate is used ONLY for our own fresh request
            // (the working FluxPairingPatch pattern) — never on the game's
            // callbacks.
            Action<HTTPRequest, HTTPResponse> action = (req, resp) =>
            {
                int balance = -1;
                try
                {
                    if (resp != null && resp.StatusCode == 200)
                        balance = ParseBalance(resp.DataAsText ?? "");
                    else
                        Plugin.Log.LogWarning($"[PLUS-BALANCE] balance request failed: {resp?.StatusCode}");
                }
                catch (Exception e)
                {
                    Plugin.Log.LogWarning($"[PLUS-BALANCE] callback failed: {e.Message}");
                }
                int b = balance;
                PlusBalanceDispatcher.Enqueue(() =>
                {
                    try { onDone(b); } catch { }
                });
            };
            var cb = (OnRequestFinishedDelegate)Delegate.CreateDelegate(
                typeof(OnRequestFinishedDelegate), action.Target, action.Method);
            var request = new HTTPRequest(new Il2CppSystem.Uri(url), cb);
            request.MethodType = HTTPMethods.Get;
            request.SetHeader("Authorization", auth);
            HTTPManager.SendRequest(request);
        }
        catch (Exception e)
        {
            Plugin.Log.LogWarning($"[PLUS-BALANCE] fetch failed: {e.Message}");
            onDone(-1);
        }
    }

    private static int ParseBalance(string json)
    {
        // Minimal scan for the "Balance" key; -1 = unparseable.
        try
        {
            int idx = json.IndexOf("\"Balance\"", StringComparison.OrdinalIgnoreCase);
            if (idx < 0)
                return -1;
            int colon = json.IndexOf(':', idx);
            if (colon < 0)
                return -1;
            int i = colon + 1;
            while (i < json.Length && (json[i] == ' ' || json[i] == '"'))
                i++;
            int start = i;
            while (i < json.Length && (char.IsDigit(json[i]) || json[i] == '-'))
                i++;
            if (i > start && int.TryParse(json.Substring(start, i - start), out var v))
                return v;
        }
        catch { }
        return -1;
    }

    // Main-thread dispatcher: drains one-shot actions and retries pending
    // Plus pages until their Buy button exists (or attempts run out).
    private class PlusBalanceDispatcher : MonoBehaviour
    {
        private static readonly object _lock = new object();
        private static readonly Queue<Action> _queue = new Queue<Action>();
        private static readonly List<PendingPage> _pending = new List<PendingPage>();

        public static void Enqueue(Action a)
        {
            lock (_lock) { _queue.Enqueue(a); }
        }

        public static void WatchPage(GameObject page)
        {
            lock (_lock) { _pending.Add(new PendingPage { Page = page, Attempts = 0 }); }
        }

        private void Update()
        {
            try
            {
                while (true)
                {
                    Action a = null;
                    lock (_lock)
                    {
                        if (_queue.Count == 0)
                            break;
                        a = _queue.Dequeue();
                    }
                    try { a?.Invoke(); }
                    catch (Exception e) { Plugin.Log.LogWarning($"[PLUS-BALANCE] action failed: {e.Message}"); }
                }

                lock (_lock)
                {
                    for (int i = _pending.Count - 1; i >= 0; i--)
                    {
                        var p = _pending[i];
                        bool done;
                        try { done = TryAddBalanceLabel(p.Page); }
                        catch
                        {
                            done = true; // destroyed page or worse — stop
                        }
                        p.Attempts++;
                        if (done || p.Attempts >= MaxPageRetries)
                        {
                            _pending.RemoveAt(i);
                            if (!done)
                                Plugin.Log.LogWarning("[PLUS-BALANCE] gave up — Buy button never appeared on a Plus page");
                        }
                        else
                        {
                            _pending[i] = p;
                        }
                    }
                }
            }
            catch { }
        }

        private struct PendingPage
        {
            public GameObject Page;
            public int Attempts;
        }
    }
}
