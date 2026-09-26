using System;
using System.Collections.Generic;
using BestHTTP;
using Il2CppInterop.Runtime.Injection;
using UnityEngine;

namespace RecNetPlugin.Patches;

// In-game confirmation dialog for the Flux Rec+ token purchase.
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
// 2. Show() registers an IMGUI overlay (the proven FluxPairingPatch
//    pattern: ClassInjector.RegisterTypeInIl2Cpp + GameObject + OnGUI).
//    IMGUI is used deliberately: constructing the game's native RRUI
//    TwoButtonMessageDialogModel from IL2CPP would need obfuscated nested
//    types and IL2CPP delegates for button callbacks — fragile. IMGUI
//    touches zero game types. (RRUI.Dialogs.DialogListModel.AddTwoButton-
//    MessageDialog was researched and rejected for this reason.)
// 3. Buy -> FluxPlusPatch.DoTokenPurchaseBestHTTP (the fixed token purchase
//    flow) and the dialog closes; purchase results are reported to the
//    player by the existing PlusToastOverlay toasts (success / insufficient
//    funds / already owned / failure). Cancel just closes the dialog.
//
// IL2CPP safety: Delegate.CreateDelegate is used ONLY for our own fresh
// HTTPRequest objects (the working FluxPairingPatch/PlusBalancePatch
// pattern) — never on the game's callbacks. All Unity/engine calls happen
// on the main thread; the balance-fetch callback is marshalled through a
// queue drained in Update(). Everything is fail-soft.
public static class PlusBuyDialog
{
    private const int WindowId = 424243;

    private static bool _typeRegistered;
    private static GameObject _dialogObject;
    private static PlusBuyDialogOverlay _overlay;

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
            if (!_typeRegistered)
            {
                ClassInjector.RegisterTypeInIl2Cpp<PlusBuyDialogOverlay>();
                _typeRegistered = true;
            }

            if (_dialogObject == null)
            {
                _dialogObject = new GameObject("FluxPlusBuyDialog");
                _dialogObject.hideFlags = HideFlags.HideAndDontSave;
                UnityEngine.Object.DontDestroyOnLoad(_dialogObject);
                _overlay = _dialogObject.AddComponent<PlusBuyDialogOverlay>();
            }

            _overlay?.Show();
            Plugin.Log.LogInfo("[PLUS-DIALOG] confirmation dialog shown");
        }
        catch (Exception e)
        {
            Plugin.Log.LogWarning($"[PLUS-DIALOG] show failed: {e.Message}");
        }
    }

    public static void Hide()
    {
        try { _overlay?.Hide(); }
        catch { }
    }

    private class PlusBuyDialogOverlay : MonoBehaviour
    {
        private bool _showWindow;
        private Rect _windowRect = new Rect(100, 100, 420, 250);
        private string _balanceText = "Checking balance…";
        private int _price = 10000;

        public void Show()
        {
            _balanceText = "Checking balance…";
            _price = CurrentPrice();
            _showWindow = true;
            try
            {
                Cursor.visible = true;
                var w = Math.Min(420, Screen.width - 40);
                var h = 250;
                _windowRect = new Rect((Screen.width - w) / 2, (Screen.height - h) / 2, w, h);
            }
            catch { }
            FetchBalance();
        }

        public void Hide()
        {
            _showWindow = false;
        }

        private void Update()
        {
            try { DrainMainThreadQueue(); }
            catch { }
        }

        private void OnGUI()
        {
            if (!_showWindow) return;
            try
            {
                Action<int> windowAction = WindowFunc;
                var windowFunc = (GUI.WindowFunction)Delegate.CreateDelegate(
                    typeof(GUI.WindowFunction), windowAction.Target, windowAction.Method);
                _windowRect = GUI.Window(WindowId, _windowRect, windowFunc, "Flux Rec +");
            }
            catch (Exception e)
            {
                Plugin.Log.LogWarning($"[PLUS-DIALOG] window failed: {e.Message}");
                _showWindow = false;
            }
        }

        private void WindowFunc(int id)
        {
            try
            {
                GUILayout.Space(8);
                GUILayout.Label($"Buy Flux Rec+ for {_price:N0} Flux Rec Tokens?");
                GUILayout.Label("30 days of membership.");
                GUILayout.Space(6);
                GUILayout.Label(_balanceText);
                GUILayout.Space(8);
                GUILayout.BeginHorizontal();
                if (GUILayout.Button("Buy", GUILayout.Height(36)))
                {
                    OnBuyClicked();
                }
                if (GUILayout.Button("Cancel", GUILayout.Height(36)))
                {
                    _showWindow = false;
                }
                GUILayout.EndHorizontal();
                GUI.DragWindow();
            }
            catch { }
        }

        private void OnBuyClicked()
        {
            var auth = SendRequestPatch.ConnectToRecNetPatch.LastAuthHeader;
            if (string.IsNullOrEmpty(auth))
            {
                Plugin.Log.LogWarning("[PLUS-DIALOG] buy clicked with no auth token");
                _showWindow = false;
                return;
            }

            Plugin.Log.LogInfo("[PLUS-DIALOG] buy confirmed — starting token purchase");
            _showWindow = false;
            try
            {
                // The fixed purchase flow; results surface via PlusToastOverlay.
                FluxPlusPatch.DoTokenPurchaseBestHTTP(auth);
            }
            catch (Exception e)
            {
                Plugin.Log.LogWarning($"[PLUS-DIALOG] purchase start failed: {e.Message}");
            }
        }

        private void FetchBalance()
        {
            try
            {
                var auth = SendRequestPatch.ConnectToRecNetPatch.LastAuthHeader;
                var server = Plugin.ServerHostname.Value.TrimEnd('/');
                if (string.IsNullOrEmpty(auth) || string.IsNullOrEmpty(server))
                {
                    EnqueueOnMainThread(() => { _balanceText = "Balance: unavailable (not logged in)"; });
                    return;
                }

                var url = $"{server}/api/storefronts/v4/balance/2";
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
                    EnqueueOnMainThread(() =>
                    {
                        _balanceText = b >= 0
                            ? $"Your balance: {b:N0} tokens" + (b < _price ? " (not enough)" : "")
                            : "Balance: unavailable";
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
                Plugin.Log.LogWarning($"[PLUS-DIALOG] balance fetch failed: {e.Message}");
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
    }
}
