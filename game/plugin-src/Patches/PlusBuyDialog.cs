using System;
using System.Collections.Generic;
using System.Reflection;
using BestHTTP;
using Il2CppInterop.Runtime.Injection;
using Il2CppInterop.Runtime.InteropTypes;
using UnityEngine;

namespace RecNetPlugin.Patches;

// In-game confirmation dialog for the Flux Rec+ token purchase, using the
// game's NATIVE RRUI dialog system (no IMGUI popup).
//
// Why this exists: FluxPlusPatch intercepts BuyRRPlusMembership() to block
// the Steam store. Firing the token purchase immediately with no
// confirmation is bad UX — the player should see the price and confirm
// first. This dialog shows the price (10,000 tokens, 3,500 on Saturdays
// UTC), the player's token balance, and Buy/Cancel buttons.
//
// How it works:
// 1. FluxPlusPatch.InterceptBuy calls PlusBuyDialog.Show() instead of
//    buying immediately.
// 2. Show() fetches the token balance, then builds the game's native
//    two-button dialog:
//      PRIMARY: RRUI.Dialogs.DialogListModel.TryFindDialogListModel(watchGO)
//      (static; verified in the 20230414 client's global-metadata.dat) finds
//      the dialog list under the Watch, then the instance method
//      AddTwoButtonMessageDialog(config) shows the dialog. NOTE: despite
//      what earlier research assumed, AddTwoButtonMessageDialog is NOT
//      static and does NOT take (title, message, labels, callbacks) — it
//      takes ONE dialog-config object.
//      The config type is the obfuscated (but unobfuscated-name-stable for
//      this build) global-namespace class DMGCKEOKIPC, whose settable
//      surface mirrors the dialog model hierarchy:
//        .AMFPEDOFIGE (base)            -> title string, dismiss flags
//        .FBNJPEHPKJA (one-button)      -> message string, primary
//                                         button text + click Action
//        .DMGCKEOKIPC (two-button)      -> secondary button text + click
//                                         Action
//      Property mapping is best-effort (names are obfuscated): string
//      setters are applied in declaration order matching the model's
//      (Title, Message, PrimaryButtonText, SecondaryButtonText) getters.
//      Buy wires to OnBuyClicked, Cancel wires to dialog dismissal.
//      FALLBACK: if no DialogListModel is found under the Watch,
//      WatchUI.get_Local().ShowRRUIConfirmationPage(title, message) shows
//      an informational page (Warning-logged: the fallback cannot wire
//      Buy/Cancel callbacks).
// 3. Buy -> FluxPlusPatch.DoTokenPurchaseBestHTTP (the fixed token purchase
//    flow) and the dialog is dismissed; purchase results are reported
//    through the BepInEx log (success / insufficient funds / already
//    owned / failure). Cancel dismisses the dialog.
//
// IL2CPP safety: button callbacks are built with
// DelegateSupport.ConvertDelegate<Il2CppSystem.Action>(new Action(...)) —
// never `new UnityAction(...)` / direct method-group construction (CS1503).
// Downcasts use TryCast<T>(). Delegate.CreateDelegate is used ONLY for our
// own fresh HTTPRequest objects (the working FluxPairingPatch/
// PlusBalancePatch pattern) — never on the game's callbacks. All
// Unity/engine calls happen on the main thread; the balance-fetch callback
// is marshalled through a queue drained in Update(). Everything is
// fail-soft.
public static class PlusBuyDialog
{
    private static bool _typeRegistered;
    private static GameObject _pumpObject;

    // The currently shown native dialog, for Cancel/dismiss wiring.
    private static Il2CppObjectBase _shownDialog;
    private static bool _fetchInFlight;

    private static readonly object _queueLock = new object();
    private static readonly Queue<Action> _mainThreadQueue = new Queue<Action>();

    internal static void EnqueueOnMainThread(Action a)
    {
        if (a == null) return;
        lock (_queueLock) { _mainThreadQueue.Enqueue(a); }
    }

    internal static void DrainMainThreadQueue()
    {
        while (true)
        {
            Action a = null;
            lock (_queueLock)
            {
                if (_mainThreadQueue.Count == 0) break;
                a = _mainThreadQueue.Dequeue();
            }
            try { a(); } catch (Exception e) { Plugin.Log.LogWarning($"[PLUS-DIALOG] queued action failed: {e.Message}"); }
        }
    }

    // Token price, mirroring the backend's currentPlusPrice()
    // (10,000 tokens, 3,500 on Saturdays UTC).
    internal static int CurrentPrice()
    {
        try { return DateTime.UtcNow.DayOfWeek == DayOfWeek.Saturday ? 3500 : 10000; }
        catch { return 10000; }
    }

    public static void Show()
    {
        try
        {
            EnsurePump();
            if (_fetchInFlight)
            {
                Plugin.Log.LogInfo("[PLUS-DIALOG] dialog already being prepared — ignoring duplicate Show()");
                return;
            }
            _fetchInFlight = true;
            FetchBalanceThenShow();
        }
        catch (Exception e)
        {
            _fetchInFlight = false;
            Plugin.Log.LogWarning($"[PLUS-DIALOG] show failed: {e.Message}");
        }
    }

    public static void Hide()
    {
        try { DismissShownDialog(); }
        catch { }
    }

    private static void EnsurePump()
    {
        if (!_typeRegistered)
        {
            ClassInjector.RegisterTypeInIl2Cpp<PlusBuyDialogOverlay>();
            _typeRegistered = true;
        }

        if (_pumpObject == null)
        {
            _pumpObject = new GameObject("FluxPlusBuyDialogPump");
            _pumpObject.hideFlags = HideFlags.HideAndDontSave;
            UnityEngine.Object.DontDestroyOnLoad(_pumpObject);
            _pumpObject.AddComponent<PlusBuyDialogOverlay>();
        }
    }

    private static void OnBuyClicked()
    {
        // Dismiss first so the dialog is gone even if the purchase errors.
        try { DismissShownDialog(); } catch { }

        var auth = SendRequestPatch.ConnectToRecNetPatch.LastAuthHeader;
        if (string.IsNullOrEmpty(auth))
        {
            Plugin.Log.LogWarning("[PLUS-DIALOG] buy clicked with no auth token");
            return;
        }

        Plugin.Log.LogInfo("[PLUS-DIALOG] buy confirmed — starting token purchase");
        try
        {
            // The fixed purchase flow; results surface via the BepInEx log.
            FluxPlusPatch.DoTokenPurchaseBestHTTP(auth);
        }
        catch (Exception e)
        {
            Plugin.Log.LogWarning($"[PLUS-DIALOG] purchase start failed: {e.Message}");
        }
    }

    private static void OnCancelClicked()
    {
        Plugin.Log.LogInfo("[PLUS-DIALOG] purchase cancelled by player");
        try { DismissShownDialog(); } catch { }
    }

    private static void DismissShownDialog()
    {
        try
        {
            var dlg = _shownDialog?.TryCast<RRUI.Dialogs.BaseDialogModel>();
            _shownDialog = null;
            if (dlg != null)
            {
                dlg.Dismiss();
                Plugin.Log.LogInfo("[PLUS-DIALOG] native dialog dismissed");
            }
        }
        catch (Exception e)
        {
            Plugin.Log.LogWarning($"[PLUS-DIALOG] dismiss failed: {e.Message}");
        }
    }

    // Shows the native dialog on the main thread once the balance text is
    // known. Primary path: DialogListModel under the Watch. Fallback:
    // WatchUI.ShowRRUIConfirmationPage (informational only — no callbacks).
    private static void ShowNativeDialog(string balanceText)
    {
        _fetchInFlight = false;
        try
        {
            int price = CurrentPrice();
            string title = "Flux Rec +";
            string message = $"Buy Flux Rec+ for {price:N0} Flux Rec Tokens?\n30 days of membership.\n{balanceText}";

            var watch = GetWatchUI();
            var list = watch != null && watch.gameObject != null
                ? RRUI.Dialogs.DialogListModel.TryFindDialogListModel(watch.gameObject)
                : null;

            if (list != null)
            {
                ShowViaDialogList(list, title, message);
                Plugin.Log.LogInfo("[PLUS-DIALOG] native two-button dialog shown via DialogListModel");
                return;
            }

            if (watch != null)
            {
                // Fallback: try ShowRRUIConfirmationPage via reflection
                try
                {
                    var m = watch.GetType().GetMethod("ShowRRUIConfirmationPage",
                        BindingFlags.Public | BindingFlags.NonPublic | BindingFlags.Instance);
                    if (m != null)
                    {
                        m.Invoke(watch, new object[] { title, message });
                        Plugin.Log.LogWarning("[PLUS-DIALOG] DialogListModel not found under Watch — fallback ShowRRUIConfirmationPage shown (buy/cancel not wired)");
                    }
                    else
                    {
                        Plugin.Log.LogWarning("[PLUS-DIALOG] DialogListModel not found and ShowRRUIConfirmationPage not available");
                    }
                }
                catch (Exception ex)
                {
                    Plugin.Log.LogWarning($"[PLUS-DIALOG] fallback failed: {ex.Message}");
                }
                return;
            }

            Plugin.Log.LogWarning("[PLUS-DIALOG] WatchUI.get_Local() returned null — cannot show native dialog");
        }
        catch (Exception e)
        {
            Plugin.Log.LogWarning($"[PLUS-DIALOG] native dialog show failed: {e.Message}");
        }
    }

    private static Component GetWatchUI()
    {
        // Resolve WatchUI via reflection (get_Local is a property accessor in IL2CPP interop)
        foreach (var asm in AppDomain.CurrentDomain.GetAssemblies())
        {
            Type t;
            try { t = asm.GetType("WatchUI"); }
            catch { continue; }
            if (t == null) continue;

            var m = t.GetMethod("get_Local",
                BindingFlags.Public | BindingFlags.NonPublic | BindingFlags.Static);
            if (m == null || m.GetParameters().Length != 0) continue;

            try
            {
                var local = m.Invoke(null, null);
                if (local == null) return null;
                return ((UnityEngine.Object)local).TryCast<Component>();
            }
            catch { return null; }
        }
        return null;
    }

    private static void ShowViaDialogList(RRUI.Dialogs.DialogListModel list, string title, string message)
    {
        // DMGCKEOKIPC: the game's two-button dialog config.
        // Use reflection for property sets (obfuscated names, compile-time
        // names unverified). Property mapping from build errors:
        //   AMFPEDOFIGE.LJJGMJNNJKN = title, CBGMHAODOKL = dismiss flag
        //   FBNJPEHPKJA.AJGKDMIDAJA = message, GMFABKOCIOO = primary text,
        //     EHOOLNCFNCO = primary callback, EFCGLEKHKBP = flag
        //   DMGCKEOKIPC.CBFPNPEBBJH = secondary text, CGHFEEPNEMJ = secondary
        //     callback, JFGDGEIJCJD = flag
        var cfg = new DMGCKEOKIPC();
        var t = cfg.GetType();

        void SetProp(string name, object value)
        {
            var p = t.GetProperty(name, BindingFlags.Public | BindingFlags.NonPublic | BindingFlags.Instance);
            if (p != null && p.CanWrite)
            {
                try { p.SetValue(cfg, value, null); }
                catch (Exception e) { Plugin.Log.LogWarning($"[PLUS-DIALOG] set {name} failed: {e.Message}"); }
            }
            else
            {
                Plugin.Log.LogWarning($"[PLUS-DIALOG] property {name} not found");
            }
        }

        var buyCallback = Il2CppInterop.Runtime.DelegateSupport.ConvertDelegate<Il2CppSystem.Action>(
            new Action(OnBuyClicked));
        var cancelCallback = Il2CppInterop.Runtime.DelegateSupport.ConvertDelegate<Il2CppSystem.Action>(
            new Action(OnCancelClicked));

        SetProp("LJJGMJNNJKN", title);
        SetProp("CBGMHAODOKL", true);
        SetProp("AJGKDMIDAJA", message);
        SetProp("GMFABKOCIOO", "Buy");
        SetProp("EHOOLNCFNCO", buyCallback);
        SetProp("EFCGLEKHKBP", true);
        SetProp("CBFPNPEBBJH", "Cancel");
        SetProp("CGHFEEPNEMJ", cancelCallback);
        SetProp("JFGDGEIJCJD", true);

        var shown = list.AddTwoButtonMessageDialog(cfg);
        _shownDialog = shown;
    }

    private static void FetchBalanceThenShow()
    {
        try
        {
            var auth = SendRequestPatch.ConnectToRecNetPatch.LastAuthHeader;
            var server = Plugin.ServerHostname.Value.TrimEnd('/');
            if (string.IsNullOrEmpty(auth) || string.IsNullOrEmpty(server))
            {
                EnqueueOnMainThread(() => ShowNativeDialog("Balance: unavailable (not logged in)"));
                return;
            }

            var url = $"{server}/api/storefronts/v4/balance/2";
            int price = CurrentPrice();
            Action<HTTPRequest, HTTPResponse> action = (req, resp) =>
            {
                int balance = -1;
                try
                {
                    if (resp != null && resp.StatusCode == 200)
                        balance = ParseBalance(resp.DataAsText ?? "");
                }
                catch { }
                int b = balance;
                string text = b >= 0
                    ? $"Your balance: {b:N0} tokens" + (b < price ? " (not enough)" : "")
                    : "Balance: unavailable";
                EnqueueOnMainThread(() => ShowNativeDialog(text));
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
            Plugin.Log.LogWarning($"[PLUS-DIALOG] balance fetch failed: {e.Message}");
            EnqueueOnMainThread(() => ShowNativeDialog("Balance: unavailable"));
        }
    }

    private static int ParseBalance(string json)
    {
        // Minimal scan for the "Balance" key; -1 = unparseable.
        try
        {
            int idx = json.IndexOf("\"Balance\"", StringComparison.OrdinalIgnoreCase);
            if (idx < 0) return -1;
            int colon = json.IndexOf(':', idx);
            if (colon < 0) return -1;
            int i = colon + 1;
            while (i < json.Length && (json[i] == ' ' || json[i] == '"')) i++;
            int start = i;
            while (i < json.Length && (char.IsDigit(json[i]) || json[i] == '-')) i++;
            if (i > start && int.TryParse(json.Substring(start, i - start), out var v)) return v;
        }
        catch { }
        return -1;
    }

    // Main-thread pump for the balance-callback queue. No IMGUI here —
    // dialog presentation is 100% native RRUI.
    private class PlusBuyDialogOverlay : MonoBehaviour
    {
        private void Update()
        {
            try { DrainMainThreadQueue(); }
            catch { }
        }
    }
}
