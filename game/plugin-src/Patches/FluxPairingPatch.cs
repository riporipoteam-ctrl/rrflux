using System;
using System.Collections.Generic;
using System.Reflection;
using System.Text;
using BestHTTP;
using Il2CppInterop.Runtime.Injection;
using UnityEngine;
using UnityEngine.Events;
using UnityEngine.UI;

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
//    FluxPairingPatch.ShowOverlay()) -> the "Flux Connect" uGUI panel opens
//    as a child of the real Rec Room Watch root (resolved via
//    WatchUI.get_Local(); the build is retried until the Watch exists).
//    (v2: the panel is part of the Watch now, not a standalone popup — the
//    player asked for UI that lives inside the real Watch.)
// 2. "Get pairing code" -> POST {auth}/connect/deviceauthorization
//    (form-encoded client_id=fluxrec-game&scope=openid) with the game's own
//    Authorization header, so the backend links the code to that account.
// 3. The panel shows the 6-digit user_code big + the verification_uri.
// 4. The game polls POST /connect/token
//    (grant_type=urn:ietf:params:oauth:grant-type:device_code) every few
//    seconds, honoring the server's interval / slow_down, until the flow is
//    approved, denied, or expired. Polling continues even if the panel is
//    hidden.
// 5. On approval the linked Flux username is shown and persisted to the
//    [Pairing] config section. "Forget this pairing" clears it.
//
// Safety: BestHTTP only (never System.Net.Http); Delegate.CreateDelegate is
// used ONLY for our own fresh requests (the working FluxPlusPatch pattern) —
// we never wrap the game's callbacks, so the v0.1.30 hang cannot recur.
// UI click handlers are OUR OWN fresh Actions converted through
// DelegateSupport.ConvertDelegate<UnityAction> (never `new UnityAction(...)`
// — that fails the build on be.788). Everything is fail-soft behind
// [Pairing] Enable Flux Pairing (default true). The F8 keybind is hardcoded
// and logged at startup.
//
// One knob, see [Pairing] in the .cfg:
//   Enable Flux Pairing -> THE FIX (default true).
internal static class FluxPairingPatch
{
    private static bool _overlayCreated;
    private static bool _typeRegistered;
    // Held so the visible Flux Connect button can open the panel on demand.
    private static FluxPairingPanel _panelInstance;

    public static void Apply()
    {
        if (_overlayCreated || !Plugin.EnableFluxPairing.Value)
            return;

        try
        {
            if (!_typeRegistered)
            {
                ClassInjector.RegisterTypeInIl2Cpp<FluxPairingPanel>();
                _typeRegistered = true;
            }

            var go = new GameObject("FluxPairingPanel");
            go.hideFlags = HideFlags.HideAndDontSave;
            UnityEngine.Object.DontDestroyOnLoad(go);
            _panelInstance = go.AddComponent<FluxPairingPanel>();
            _overlayCreated = true;
            Plugin.Log.LogInfo("[PAIRING] Flux Connect panel host ready — press F8 in-game");
        }
        catch (Exception e)
        {
            Plugin.Log.LogWarning($"[PAIRING] panel setup failed: {e.Message}");
        }
    }

    // Called by the visible Flux Connect button (or any other UI entry point)
    // to open the pairing panel. Creates the host on demand if Apply()
    // never ran (e.g. plugin loaded before login). F8 still toggles it as a
    // fallback — see FluxPairingPanel.Update.
    public static void ShowOverlay()
    {
        try
        {
            if (_panelInstance == null)
            {
                // Not created yet (or was lost) — build it now.
                _overlayCreated = false;
                Apply();
            }
            _panelInstance?.ShowWindow();
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

    // The pairing panel host. Unity callbacks (Update) run on the main
    // thread; BestHTTP callbacks are marshalled here through a small queue.
    // The visible UI is a real uGUI panel built as a child of the Watch
    // root — no IMGUI anywhere in this file.
    private class FluxPairingPanel : MonoBehaviour
    {
        private enum PairState { Idle, RequestingCode, WaitingApproval, Paired, Error }

        private const string DeviceGrantType = "urn:ietf:params:oauth:grant-type:device_code";
        // Pre-encoded for application/x-www-form-urlencoded.
        private const string DeviceGrantTypeEncoded = "urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code";

        private static readonly TimeSpan HttpConnectTimeout = TimeSpan.FromSeconds(15);
        private static readonly TimeSpan HttpReadWriteTimeout = TimeSpan.FromSeconds(30);

        // Watch-root resolution: retried (throttled) until the Watch exists.
        private const int MaxBuildAttempts = 20;
        private const float BuildRetryIntervalSec = 2f;

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

        private readonly object _queueLock = new object();
        private readonly Queue<Action> _pending = new Queue<Action>();

        // uGUI panel pieces (built once under the Watch root).
        private GameObject _panelRoot;
        private Text _titleText;
        private Text _codeText;
        private Text _statusText;
        private Button _primaryButton;
        private Text _primaryLabel;
        private Button _secondaryButton;
        private Text _secondaryLabel;
        private bool _panelBuilt;
        private bool _buildGaveUp;
        private int _buildAttempts;
        private float _nextBuildAttemptTime;
        private float _nextCountdownRefresh;
        private bool _uiDirty = true;

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
            _uiDirty = true;
        }

        private void Update()
        {
            try
            {
                if (Input.GetKeyDown(KeyCode.F8))
                {
                    TogglePanel();
                    // Cursor visibility while the panel is open depends on
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

                // The panel dies with the Watch root on scene changes —
                // rebuild it when that happens.
                if (_panelBuilt && _panelRoot == null)
                {
                    _panelBuilt = false;
                    _buildAttempts = 0;
                    _buildGaveUp = false;
                    _nextBuildAttemptTime = 0f;
                    Plugin.Log.LogInfo("[PAIRING] panel root lost — will rebuild under the Watch UI");
                }

                // Build the panel under the Watch root (throttled retries).
                if (!_panelBuilt && !_buildGaveUp && Time.realtimeSinceStartup >= _nextBuildAttemptTime)
                {
                    _nextBuildAttemptTime = Time.realtimeSinceStartup + BuildRetryIntervalSec;
                    TryBuildPanel();
                }

                // Re-render once per second while waiting, so the expiry
                // countdown ticks.
                if (_panelBuilt && _panelRoot != null && _panelRoot.activeSelf &&
                    _state == PairState.WaitingApproval &&
                    Time.realtimeSinceStartup >= _nextCountdownRefresh)
                {
                    _nextCountdownRefresh = Time.realtimeSinceStartup + 1f;
                    _uiDirty = true;
                }

                // Refresh visible content when the state machine changed it.
                if (_uiDirty)
                {
                    _uiDirty = false;
                    if (_panelBuilt && _panelRoot != null)
                        RefreshUI();
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

        // Opens the panel — entry point for the Flux Connect button.
        // (F8 still toggles in Update(); this only opens, never closes, so
        // a button click can never accidentally hide it.)
        public void ShowWindow()
        {
            try
            {
                if (!_panelBuilt || _panelRoot == null)
                {
                    // Try once immediately — the Watch may have appeared
                    // since the last throttled tick.
                    _nextBuildAttemptTime = 0f;
                    TryBuildPanel();
                }

                if (_panelRoot != null)
                {
                    _panelRoot.SetActive(true);
                    _uiDirty = true;
                    Cursor.visible = true;
                }
                else
                {
                    Plugin.Log.LogWarning("[PAIRING] cannot show the Flux Connect panel — Watch UI not found yet");
                }
            }
            catch (Exception e)
            {
                Plugin.Log.LogWarning($"[PAIRING] show panel failed: {e.Message}");
            }
        }

        public void Hide()
        {
            try
            {
                if (_panelRoot != null)
                    _panelRoot.SetActive(false);
            }
            catch { }
        }

        private void TogglePanel()
        {
            if (!_panelBuilt || _panelRoot == null)
            {
                Plugin.Log.LogWarning("[PAIRING] Flux Connect panel not ready yet (Watch UI not found)");
                return;
            }
            if (_panelRoot.activeSelf)
                Hide();
            else
                ShowWindow();
        }

        // --- Panel construction (uGUI, child of the Watch root) ---

        private void TryBuildPanel()
        {
            if (_panelBuilt)
                return;

            _buildAttempts++;
            GameObject watchRoot = null;
            try
            {
                watchRoot = ResolveWatchRoot();
            }
            catch (Exception e)
            {
                Plugin.Log.LogDebug($"[PAIRING] watch resolve failed: {e.Message}");
            }

            if (watchRoot == null)
            {
                if (_buildAttempts >= MaxBuildAttempts)
                {
                    _buildGaveUp = true;
                    Plugin.Log.LogWarning("[PAIRING] gave up building the Flux Connect panel — " +
                        "WatchUI.get_Local() never resolved after 20 attempts.");
                }
                return;
            }

            try
            {
                BuildPanel(watchRoot);
                _panelBuilt = true;
                Plugin.Log.LogInfo("[PAIRING] Flux Connect panel built under the Watch UI");
            }
            catch (Exception e)
            {
                Plugin.Log.LogWarning($"[PAIRING] panel build failed: {e.Message}");
                if (_buildAttempts >= MaxBuildAttempts)
                {
                    _buildGaveUp = true;
                    Plugin.Log.LogWarning("[PAIRING] gave up building the Flux Connect panel after 20 attempts.");
                }
            }
        }

        // WatchUI is unobfuscated, but resolve it by name at runtime anyway:
        // a compile-time reference would break the build if the interop ever
        // regenerates without it.
        private static GameObject ResolveWatchRoot()
        {
            foreach (var asm in AppDomain.CurrentDomain.GetAssemblies())
            {
                Type t;
                try { t = asm.GetType("WatchUI"); }
                catch { continue; }
                if (t == null)
                    continue;

                var m = t.GetMethod("get_Local",
                    BindingFlags.Public | BindingFlags.NonPublic | BindingFlags.Static);
                if (m == null || m.GetParameters().Length != 0)
                    continue;

                object local;
                try { local = m.Invoke(null, null); }
                catch { return null; }
                if (local == null)
                    return null;

                try
                {
                    var comp = ((UnityEngine.Object)local).TryCast<Component>();
                    return comp?.gameObject;
                }
                catch
                {
                    return null;
                }
            }
            return null;
        }

        // Builds a minimal dialog: dark panel + vertical stack of
        // title / big code / status / button row. No new Canvas — the Watch
        // already provides one; this is a plain child of the Watch root.
        private void BuildPanel(GameObject watchRoot)
        {
            var root = new GameObject("FluxConnectPanel");
            root.transform.SetParent(watchRoot.transform, false);

            var rootRt = root.AddComponent<RectTransform>();
            rootRt.anchorMin = new Vector2(0.5f, 0.5f);
            rootRt.anchorMax = new Vector2(0.5f, 0.5f);
            rootRt.pivot = new Vector2(0.5f, 0.5f);
            rootRt.sizeDelta = new Vector2(520f, 470f);
            rootRt.anchoredPosition = Vector2.zero;

            var bg = root.AddComponent<Image>();
            bg.color = new Color(0.03f, 0.03f, 0.06f, 0.97f);

            var layout = root.AddComponent<VerticalLayoutGroup>();
            layout.padding = new RectOffset(28, 28, 24, 24);
            layout.spacing = 12f;
            layout.childAlignment = TextAnchor.UpperCenter;
            layout.childControlWidth = true;
            layout.childControlHeight = true;
            layout.childForceExpandWidth = true;
            layout.childForceExpandHeight = false;

            // Steal the Watch's own font so the text matches the game.
            var font = ResolveFont(watchRoot);

            _titleText = MakeText(root.transform, "Title", font, 30, FontStyle.Bold,
                TextAnchor.MiddleCenter, Color.white);
            _titleText.text = "Flux Connect";

            _codeText = MakeText(root.transform, "Code", font, 52, FontStyle.Bold,
                TextAnchor.MiddleCenter, new Color(0.45f, 0.95f, 0.55f));
            _codeText.gameObject.SetActive(false);

            _statusText = MakeText(root.transform, "Status", font, 18, FontStyle.Normal,
                TextAnchor.UpperCenter, Color.white);

            var row = new GameObject("ButtonRow");
            row.transform.SetParent(root.transform, false);
            row.AddComponent<RectTransform>();
            var hlg = row.AddComponent<HorizontalLayoutGroup>();
            hlg.spacing = 12f;
            hlg.childAlignment = TextAnchor.MiddleCenter;
            hlg.childControlWidth = true;
            hlg.childControlHeight = true;
            hlg.childForceExpandWidth = true;
            hlg.childForceExpandHeight = false;
            var rowLe = row.AddComponent<LayoutElement>();
            rowLe.preferredHeight = 52f;

            (_primaryButton, _primaryLabel) = MakeButton(row.transform, "Primary", font);
            _primaryButton.onClick.AddListener(
                Il2CppInterop.Runtime.DelegateSupport.ConvertDelegate<UnityAction>(
                    new Action(OnPrimaryClicked)));
            (_secondaryButton, _secondaryLabel) = MakeButton(row.transform, "Secondary", font);
            _secondaryButton.onClick.AddListener(
                Il2CppInterop.Runtime.DelegateSupport.ConvertDelegate<UnityAction>(
                    new Action(OnSecondaryClicked)));
            var (closeButton, closeLabel) = MakeButton(row.transform, "Close", font);
            closeLabel.text = "Close (F8)";
            closeButton.onClick.AddListener(
                Il2CppInterop.Runtime.DelegateSupport.ConvertDelegate<UnityAction>(
                    new Action(OnCloseClicked)));

            root.transform.SetAsLastSibling();
            root.SetActive(false);
            _panelRoot = root;
            _uiDirty = true;
        }

        private static Text MakeText(Transform parent, string name, Font font, int size,
            FontStyle style, TextAnchor align, Color color)
        {
            var go = new GameObject(name);
            go.transform.SetParent(parent, false);
            var txt = go.AddComponent<Text>();
            if (font != null)
                txt.font = font;
            txt.fontSize = size;
            txt.fontStyle = style;
            txt.alignment = align;
            txt.color = color;
            txt.raycastTarget = false;
            var le = go.AddComponent<LayoutElement>();
            le.flexibleWidth = 1f;
            return txt;
        }

        private static (Button, Text) MakeButton(Transform parent, string name, Font font)
        {
            var go = new GameObject(name);
            go.transform.SetParent(parent, false);
            go.AddComponent<RectTransform>();
            var img = go.AddComponent<Image>();
            img.color = new Color(0.16f, 0.38f, 0.78f, 1f);
            var btn = go.AddComponent<Button>();
            var colors = btn.colors;
            colors.normalColor = new Color(0.16f, 0.38f, 0.78f, 1f);
            colors.highlightedColor = new Color(0.22f, 0.48f, 0.9f, 1f);
            colors.pressedColor = new Color(0.1f, 0.28f, 0.6f, 1f);
            colors.disabledColor = new Color(0.25f, 0.25f, 0.3f, 0.6f);
            btn.colors = colors;
            var le = go.AddComponent<LayoutElement>();
            le.preferredHeight = 52f;
            le.flexibleWidth = 1f;
            var label = MakeText(go.transform, "Label", font, 20, FontStyle.Bold,
                TextAnchor.MiddleCenter, Color.white);
            return (btn, label);
        }

        // Borrow the Watch's own Text font so our labels match the game.
        // Falls back to the built-in Arial if the Watch has no Text yet.
        private static Font ResolveFont(GameObject watchRoot)
        {
            try
            {
                // be.788: GetComponentsInChildren requires Il2CppSystem.Type.
                var textType = Il2CppSystem.Type.GetType(typeof(Text).AssemblyQualifiedName);
                var texts = watchRoot.GetComponentsInChildren(textType, true);
                if (texts != null)
                {
                    foreach (var o in texts)
                    {
                        var t = ((UnityEngine.Object)o).TryCast<Text>();
                        if (t != null && t.font != null)
                            return t.font;
                    }
                }
            }
            catch { }

            try
            {
                var fontType = Il2CppSystem.Type.GetType(typeof(Font).AssemblyQualifiedName);
                var builtin = Resources.GetBuiltinResource(fontType, "Arial.ttf");
                return builtin?.TryCast<Font>();
            }
            catch { }

            return null;
        }

        // --- Panel content (mirrors the old overlay's status strings) ---

        private void RefreshUI()
        {
            try
            {
                _codeText.gameObject.SetActive(false);
                _primaryButton.gameObject.SetActive(true);
                _primaryButton.interactable = true;
                _secondaryButton.gameObject.SetActive(false);

                switch (_state)
                {
                    case PairState.Idle:
                    {
                        var auth = SendRequestPatch.ConnectToRecNetPatch.LastAuthHeader;
                        var hasAuth = !string.IsNullOrEmpty(auth);
                        _statusText.text = hasAuth
                            ? "Link this game to your Flux account."
                            : "Log in to the game first, then press F8.";
                        _primaryLabel.text = "Get pairing code";
                        _primaryButton.interactable = hasAuth;
                        break;
                    }
                    case PairState.RequestingCode:
                        _statusText.text = "Contacting pairing server…";
                        _primaryButton.gameObject.SetActive(false);
                        break;
                    case PairState.WaitingApproval:
                    {
                        _codeText.gameObject.SetActive(true);
                        _codeText.text = _userCode;
                        var line = !string.IsNullOrEmpty(_verifyUriComplete)
                            ? "Or open: " + _verifyUriComplete
                            : (!string.IsNullOrEmpty(_verifyUri) ? "At: " + _verifyUri : "");
                        var remaining = _deadlineUtc - DateTime.UtcNow;
                        if (remaining < TimeSpan.Zero) remaining = TimeSpan.Zero;
                        _statusText.text = "Enter this code on the website:\n" + line +
                            $"\n\nWaiting for approval… (expires in {remaining:mm\\:ss})";
                        _primaryLabel.text = "Copy code";
                        _secondaryLabel.text = "Cancel";
                        _secondaryButton.gameObject.SetActive(true);
                        break;
                    }
                    case PairState.Paired:
                        _statusText.text = "Paired as: " + _pairedName +
                            "\nYour game and Flux accounts are linked.";
                        _primaryLabel.text = "Forget this pairing";
                        break;
                    case PairState.Error:
                        _statusText.text = "Pairing failed:\n" + _status;
                        _primaryLabel.text = "Back";
                        break;
                }
            }
            catch (Exception e)
            {
                Plugin.Log.LogWarning($"[PAIRING] panel refresh failed: {e.Message}");
            }
        }

        // --- Button handlers (wired via DelegateSupport, never IMGUI) ---

        private void OnPrimaryClicked()
        {
            try
            {
                switch (_state)
                {
                    case PairState.Idle:
                    {
                        var auth = SendRequestPatch.ConnectToRecNetPatch.LastAuthHeader;
                        if (string.IsNullOrEmpty(auth))
                        {
                            Plugin.Log.LogWarning("[PAIRING] Get pairing code pressed with no auth token");
                            return;
                        }
                        RequestCode(auth);
                        break;
                    }
                    case PairState.WaitingApproval:
                        try { GUIUtility.systemCopyBuffer = _userCode; } catch { }
                        Plugin.Log.LogInfo("[PAIRING] pairing code copied to clipboard");
                        break;
                    case PairState.Paired:
                        try
                        {
                            Plugin.PairedFluxAccount.Value = "";
                            Plugin.Log.LogInfo("[PAIRING] pairing forgotten");
                        }
                        catch { }
                        _pairedName = "";
                        _state = PairState.Idle;
                        _uiDirty = true;
                        break;
                    case PairState.Error:
                        _state = PairState.Idle;
                        _status = "";
                        _uiDirty = true;
                        break;
                    case PairState.RequestingCode:
                        break;
                }
            }
            catch (Exception e)
            {
                Plugin.Log.LogWarning($"[PAIRING] button click failed: {e.Message}");
            }
        }

        private void OnSecondaryClicked()
        {
            // WaitingApproval -> Cancel.
            if (_state == PairState.WaitingApproval)
            {
                _state = PairState.Idle;
                _status = "";
                _uiDirty = true;
            }
        }

        private void OnCloseClicked()
        {
            Hide();
        }

        // --- State machine (unchanged behavior) ---

        private void Fail(string message)
        {
            _status = message;
            _state = PairState.Error;
            _deviceCode = "";
            _userCode = "";
            _uiDirty = true;
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
            _uiDirty = true;
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
                    _uiDirty = true;
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
                        _uiDirty = true;
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
