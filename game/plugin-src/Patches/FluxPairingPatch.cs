using System;
using System.Collections.Generic;
using System.Text;
using BestHTTP;
using Il2CppInterop.Runtime.Injection;
using UnityEngine;

namespace RecNetPlugin.Patches;

// Flux Connect (account pairing): links the in-game account to a Flux Social
// account through the OAuth 2.0 Device Authorization Grant (RFC 8628).
//
// Why this exists: the website and Flux Social need to know which game
// account belongs to which Flux account (profile linking, game captures,
// privacy switches). The game has no browser of its own, so the RFC 8628
// device flow is the right tool: the game shows a short user_code, the
// player approves it in a real browser, the game polls until approved.
//
// How it works:
// 1. Press F8 in-game (or click the Flux Connect button, which calls
//    FluxPairingPatch.ShowOverlay()) -> a draggable "Flux Connect" IMGUI
//    overlay opens.
//    (Standalone overlay on purpose: the Edit Profile page types are
//    obfuscated and re-rolled per game build, so hooking it would silently
//    break on every client update. This overlay touches zero game types.)
// 2. "Get pairing code" -> POST {auth}/connect/deviceauthorization
//    (form-encoded client_id=fluxrec-game&scope=openid) with the game's own
//    Authorization header, so the backend links the code to that account.
// 3. The overlay shows the 6-digit user_code big + the verification_uri.
// 4. The game polls POST /connect/token
//    (grant_type=urn:ietf:params:oauth:grant-type:device_code) every few
//    seconds, honoring the server's interval / slow_down, until the flow is
//    approved, denied, or expired. Polling continues even if the window is
//    closed.
// 5. On approval the linked Flux username is shown and persisted to the
//    [Pairing] config section. "Forget this pairing" clears it.
//
// Safety: BestHTTP only (never System.Net.Http); Delegate.CreateDelegate is
// used ONLY for our own fresh requests (the working FluxPlusPatch pattern) —
// we never wrap the game's callbacks, so the v0.1.30 hang cannot recur.
// Everything is fail-soft behind [Pairing] Enable Flux Pairing (default
// true). The F8 keybind is hardcoded and logged at startup.
//
// One knob, see [Pairing] in the .cfg:
//   Enable Flux Pairing -> THE FIX (default true).
internal static class FluxPairingPatch
{
    private static bool _overlayCreated;
    private static bool _typeRegistered;
    // Held so the visible Flux Connect button can open the overlay on demand.
    private static FluxPairingOverlay _overlayInstance;

    public static void Apply()
    {
        if (_overlayCreated || !Plugin.EnableFluxPairing.Value)
            return;

        try
        {
            if (!_typeRegistered)
            {
                ClassInjector.RegisterTypeInIl2Cpp<FluxPairingOverlay>();
                _typeRegistered = true;
            }

            var go = new GameObject("FluxPairingOverlay");
            go.hideFlags = HideFlags.HideAndDontSave;
            UnityEngine.Object.DontDestroyOnLoad(go);
            _overlayInstance = go.AddComponent<FluxPairingOverlay>();
            _overlayCreated = true;
            Plugin.Log.LogInfo("[PAIRING] Flux Connect overlay ready — press F8 in-game");
        }
        catch (Exception e)
        {
            Plugin.Log.LogWarning($"[PAIRING] overlay setup failed: {e.Message}");
        }
    }

    // Called by the visible Flux Connect button (or any other UI entry point)
    // to open the pairing overlay. Creates the overlay on demand if Apply()
    // never ran (e.g. plugin loaded before login). F8 still toggles it as a
    // fallback — see FluxPairingOverlay.Update.
    public static void ShowOverlay()
    {
        try
        {
            if (_overlayInstance == null)
            {
                // Not created yet (or was lost) — build it now.
                _overlayCreated = false;
                Apply();
            }
            _overlayInstance?.Show();
        }
        catch (Exception e)
        {
            Plugin.Log.LogWarning($"[PAIRING] ShowOverlay failed: {e.Message}");
        }
    }

    // Auth worker host. Derived from the RecNet NameServer host by swapping
    // ns. -> auth., unless [Pairing] Auth Host Override is set.
    internal static string AuthHost()
    {
        try
        {
            var ov = Plugin.PairingAuthHostOverride.Value;
            if (!string.IsNullOrWhiteSpace(ov))
                return ov.Trim().TrimEnd('/');
            var s = (Plugin.ServerHostname.Value ?? "").Trim().TrimEnd('/');
            if (s.Contains("://ns."))
                return s.Replace("://ns.", "://auth.");
            return s;
        }
        catch
        {
            return "";
        }
    }

    // Flat top-level JSON string extractor (no JSON lib on IL2CPP).
    // Handles "key":"value" and "key":123 / "key":true shapes.
    internal static string GetJsonString(string json, string key)
    {
        try
        {
            if (string.IsNullOrEmpty(json) || string.IsNullOrEmpty(key))
                return null;
            var qkey = "\"" + key + "\"";
            var i = json.IndexOf(qkey, StringComparison.Ordinal);
            if (i < 0) return null;
            i = json.IndexOf(':', i + qkey.Length);
            if (i < 0) return null;
            i++;
            while (i < json.Length && char.IsWhiteSpace(json[i])) i++;
            if (i >= json.Length) return null;
            var sb = new StringBuilder();
            if (json[i] == '"')
            {
                i++;
                while (i < json.Length && json[i] != '"')
                {
                    if (json[i] == '\\' && i + 1 < json.Length) { i++; sb.Append(json[i]); }
                    else sb.Append(json[i]);
                    i++;
                }
                return sb.ToString();
            }
            while (i < json.Length && json[i] != ',' && json[i] != '}' && json[i] != ']' && !char.IsWhiteSpace(json[i]))
            {
                sb.Append(json[i]);
                i++;
            }
            return sb.Length > 0 ? sb.ToString() : null;
        }
        catch
        {
            return null;
        }
    }

    internal static int GetJsonInt(string json, string key, int fallback)
    {
        var s = GetJsonString(json, key);
        return int.TryParse(s, out var v) ? v : fallback;
    }

    // The standalone overlay. Unity callbacks (Update/OnGUI) run on the main
    // thread; BestHTTP callbacks are marshalled here through a small queue.
    private class FluxPairingOverlay : MonoBehaviour
    {
        private enum PairState { Idle, RequestingCode, WaitingApproval, Paired, Error }

        private const int WindowId = 424242;
        private const string DeviceGrantType = "urn:ietf:params:oauth:grant-type:device_code";
        // Pre-encoded for application/x-www-form-urlencoded.
        private const string DeviceGrantTypeEncoded = "urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code";

        private static readonly TimeSpan HttpConnectTimeout = TimeSpan.FromSeconds(15);
        private static readonly TimeSpan HttpReadWriteTimeout = TimeSpan.FromSeconds(30);

        private bool _showWindow;
        private Rect _windowRect = new Rect(24, 24, 400, 320);
        private PairState _state = PairState.Idle;
        private string _status = "";
        private string _deviceCode = "";
        private string _userCode = "";
        private string _verifyUri = "";
        private string _verifyUriComplete = "";
        private string _pairedName = "";
        private int _pollIntervalSec = 5;
        private DateTime _deadlineUtc;
        private DateTime _nextPollUtc;
        private GUIStyle _bigCodeStyle;

        private readonly object _queueLock = new object();
        private readonly Queue<Action> _pending = new Queue<Action>();

        private void Start()
        {
            try
            {
                var saved = Plugin.PairedFluxAccount.Value;
                if (!string.IsNullOrWhiteSpace(saved))
                {
                    _pairedName = saved.Trim();
                    _state = PairState.Paired;
                }
            }
            catch { }
        }

        private void Update()
        {
            try
            {
                if (Input.GetKeyDown(KeyCode.F8))
                {
                    _showWindow = !_showWindow;
                    // v1: cursor visibility while the window is open depends on
                    // game state; force it visible on toggle and let the game
                    // restore its own behavior afterwards.
                    Cursor.visible = true;
                }

                // Drain HTTP callbacks onto the main thread.
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

                if (_state == PairState.WaitingApproval)
                {
                    var now = DateTime.UtcNow;
                    if (now >= _deadlineUtc)
                    {
                        Fail("Code expired — get a new one.");
                    }
                    else if (now >= _nextPollUtc)
                    {
                        _nextPollUtc = now.AddSeconds(_pollIntervalSec);
                        PollToken();
                    }
                }
            }
            catch (Exception e)
            {
                Plugin.Log.LogWarning($"[PAIRING] update failed: {e.Message}");
            }
        }

        // Opens the overlay — entry point for the Flux Connect button.
        // (F8 still toggles _showWindow in Update(); this only opens, never
        // closes, so a button click can never accidentally hide it.)
        public void Show()
        {
            _showWindow = true;
            Cursor.visible = true;
        }

        private void OnGUI()
        {
            if (!_showWindow) return;
            try
            {
                _windowRect = GUI.Window(WindowId, _windowRect, new GUI.WindowFunction(WindowFunc), "Flux Connect");
            }
            catch (Exception e)
            {
                Plugin.Log.LogWarning($"[PAIRING] window failed: {e.Message}");
                _showWindow = false;
            }
        }

        private void WindowFunc(int id)
        {
            try
            {
                switch (_state)
                {
                    case PairState.Idle: DrawIdle(); break;
                    case PairState.RequestingCode: DrawBusy("Contacting pairing server…"); break;
                    case PairState.WaitingApproval: DrawWaiting(); break;
                    case PairState.Paired: DrawPaired(); break;
                    case PairState.Error: DrawError(); break;
                }
                GUILayout.Space(4);
                if (GUILayout.Button("Close (F8)")) _showWindow = false;
                GUI.DragWindow();
            }
            catch { }
        }

        private void DrawIdle()
        {
            GUILayout.Label("Link this game to your Flux account.");
            GUILayout.Space(4);
            var auth = SendRequestPatch.ConnectToRecNetPatch.LastAuthHeader;
            if (string.IsNullOrEmpty(auth))
            {
                GUILayout.Label("Log in to the game first, then press F8.");
            }
            else if (GUILayout.Button("Get pairing code", GUILayout.Height(36)))
            {
                RequestCode(auth);
            }
        }

        private void DrawBusy(string text)
        {
            GUILayout.Space(12);
            GUILayout.Label(text);
        }

        private void DrawWaiting()
        {
            try
            {
                if (_bigCodeStyle == null)
                {
                    _bigCodeStyle = new GUIStyle(GUI.skin.label);
                    _bigCodeStyle.fontSize = 34;
                    _bigCodeStyle.fontStyle = FontStyle.Bold;
                    _bigCodeStyle.alignment = TextAnchor.MiddleCenter;
                }
                GUILayout.Label("Enter this code on the website:", _bigCodeStyle);
                GUILayout.Label(_userCode, _bigCodeStyle);
                GUILayout.Space(4);
                if (!string.IsNullOrEmpty(_verifyUriComplete))
                    GUILayout.Label("Or open: " + _verifyUriComplete);
                else if (!string.IsNullOrEmpty(_verifyUri))
                    GUILayout.Label("At: " + _verifyUri);
                GUILayout.Space(4);
                if (GUILayout.Button("Copy code"))
                {
                    try { GUIUtility.systemCopyBuffer = _userCode; } catch { }
                }
                GUILayout.Space(4);
                var remaining = _deadlineUtc - DateTime.UtcNow;
                if (remaining < TimeSpan.Zero) remaining = TimeSpan.Zero;
                GUILayout.Label($"Waiting for approval… (expires in {remaining:mm\\:ss})");
                GUILayout.Space(4);
                if (GUILayout.Button("Cancel"))
                {
                    _state = PairState.Idle;
                    _status = "";
                }
            }
            catch (Exception e)
            {
                Plugin.Log.LogWarning($"[PAIRING] draw waiting failed: {e.Message}");
            }
        }

        private void DrawPaired()
        {
            GUILayout.Space(8);
            GUILayout.Label("Paired as: " + _pairedName);
            GUILayout.Space(4);
            GUILayout.Label("Your game and Flux accounts are linked.");
            GUILayout.Space(8);
            if (GUILayout.Button("Forget this pairing"))
            {
                try
                {
                    Plugin.PairedFluxAccount.Value = "";
                    Plugin.Log.LogInfo("[PAIRING] pairing forgotten");
                }
                catch { }
                _pairedName = "";
                _state = PairState.Idle;
            }
        }

        private void DrawError()
        {
            GUILayout.Space(8);
            GUILayout.Label("Pairing failed:");
            GUILayout.Label(_status);
            GUILayout.Space(8);
            if (GUILayout.Button("Back"))
            {
                _state = PairState.Idle;
                _status = "";
            }
        }

        private void Fail(string message)
        {
            _status = message;
            _state = PairState.Error;
            _deviceCode = "";
            _userCode = "";
            Plugin.Log.LogWarning("[PAIRING] " + message);
        }

        private void RequestCode(string auth)
        {
            var host = AuthHost();
            if (string.IsNullOrEmpty(host))
            {
                Fail("No server host configured.");
                return;
            }

            _state = PairState.RequestingCode;
            _status = "";
            var url = host + "/connect/deviceauthorization";
            var body = "client_id=fluxrec-game&scope=openid";
            Plugin.Log.LogInfo("[PAIRING] requesting device code");
            SendFormPost(url, body, auth, (status, respBody) =>
            {
                try
                {
                    if (status != 200)
                    {
                        Fail($"Pairing server error (HTTP {status}). Try again later.");
                        return;
                    }

                    var deviceCode = GetJsonString(respBody, "device_code");
                    var userCode = GetJsonString(respBody, "user_code");
                    if (string.IsNullOrEmpty(deviceCode) || string.IsNullOrEmpty(userCode))
                    {
                        Fail("Bad response from pairing server.");
                        return;
                    }

                    _deviceCode = deviceCode;
                    _userCode = userCode;
                    _verifyUri = GetJsonString(respBody, "verification_uri") ?? "";
                    _verifyUriComplete = GetJsonString(respBody, "verification_uri_complete") ?? "";
                    var expiresIn = GetJsonInt(respBody, "expires_in", 600);
                    _pollIntervalSec = Math.Max(1, GetJsonInt(respBody, "interval", 5));
                    var now = DateTime.UtcNow;
                    _deadlineUtc = now.AddSeconds(Math.Max(30, expiresIn));
                    _nextPollUtc = now.AddSeconds(_pollIntervalSec);
                    _state = PairState.WaitingApproval;
                    Plugin.Log.LogInfo($"[PAIRING] code issued, waiting for approval (expires in {expiresIn}s)");
                }
                catch (Exception e)
                {
                    Fail("Failed to read pairing response: " + e.Message);
                }
            });
        }

        private void PollToken()
        {
            var host = AuthHost();
            if (string.IsNullOrEmpty(host) || string.IsNullOrEmpty(_deviceCode))
                return;

            var url = host + "/connect/token";
            var body = "grant_type=" + DeviceGrantTypeEncoded
                + "&device_code=" + _deviceCode
                + "&client_id=fluxrec-game";
            var auth = SendRequestPatch.ConnectToRecNetPatch.LastAuthHeader;
            SendFormPost(url, body, auth, (status, respBody) =>
            {
                try
                {
                    if (status == 200)
                    {
                        var accessToken = GetJsonString(respBody, "access_token");
                        if (string.IsNullOrEmpty(accessToken))
                        {
                            Fail("Pairing approved but no token returned.");
                            return;
                        }

                        var name = GetJsonString(respBody, "username")
                            ?? GetJsonString(respBody, "display_name")
                            ?? GetJsonString(respBody, "displayName")
                            ?? GetJsonString(respBody, "account_name")
                            ?? "Linked account";
                        _pairedName = name;
                        try
                        {
                            // BepInEx auto-saves on set (SaveOnConfigSet).
                            Plugin.PairedFluxAccount.Value = name;
                        }
                        catch { }
                        _deviceCode = "";
                        _userCode = "";
                        _state = PairState.Paired;
                        Plugin.Log.LogInfo("[PAIRING] paired as " + name);
                        return;
                    }

                    // RFC 8628 §3.5 error responses (usually HTTP 400).
                    var error = (GetJsonString(respBody, "error") ?? "").Trim().ToLowerInvariant();
                    if (error == "authorization_pending")
                    {
                        // Keep waiting; next poll already scheduled.
                        return;
                    }
                    if (error == "slow_down")
                    {
                        _pollIntervalSec += 5;
                        Plugin.Log.LogInfo("[PAIRING] server asked to slow down, interval now " + _pollIntervalSec + "s");
                        return;
                    }
                    if (error == "access_denied")
                    {
                        Fail("Denied on the website.");
                        return;
                    }
                    if (error == "expired_token")
                    {
                        Fail("Code expired — get a new one.");
                        return;
                    }

                    Fail($"Pairing poll failed (HTTP {status}).");
                }
                catch (Exception e)
                {
                    Fail("Pairing poll failed: " + e.Message);
                }
            });
        }

        // POST a form body with the game's BestHTTP stack. The callback is
        // marshalled back onto the main thread through _pending.
        // Delegate.CreateDelegate is used ONLY for our own fresh request (the
        // working FluxPlusPatch pattern) — never on the game's callbacks.
        private void SendFormPost(string url, string formBody, string auth, Action<int, string> onDone)
        {
            try
            {
                Action<HTTPRequest, HTTPResponse> action = (req, resp) =>
                {
                    try
                    {
                        int status = resp != null ? resp.StatusCode : -1;
                        string body = resp != null ? (resp.DataAsText ?? "") : "";
                        lock (_queueLock) { _pending.Enqueue(() => onDone(status, body)); }
                    }
                    catch (Exception e)
                    {
                        Plugin.Log.LogWarning("[PAIRING] callback failed: " + e.Message);
                    }
                };
                var cb = (OnRequestFinishedDelegate)Delegate.CreateDelegate(
                    typeof(OnRequestFinishedDelegate), action.Target, action.Method);
                var req = new HTTPRequest(new Il2CppSystem.Uri(url), cb);
                req.MethodType = HTTPMethods.Post;
                req.ConnectTimeout = new Il2CppSystem.TimeSpan(HttpConnectTimeout.Ticks);
                req.SetHeader("Content-Type", "application/x-www-form-urlencoded");
                if (!string.IsNullOrEmpty(auth))
                    req.SetHeader("Authorization", auth);
                req.RawData = Encoding.UTF8.GetBytes(formBody ?? "");
                HTTPManager.SendRequest(req);
            }
            catch (Exception e)
            {
                Plugin.Log.LogWarning("[PAIRING] request failed: " + e.Message);
                lock (_queueLock) { _pending.Enqueue(() => onDone(-1, "")); }
            }
        }
    }
}
