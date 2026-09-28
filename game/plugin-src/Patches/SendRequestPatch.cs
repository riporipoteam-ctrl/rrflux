using System;
using System.Collections.Generic;
using BestHTTP;
using HarmonyLib;
using Il2CppInterop.Runtime;

namespace RecNetPlugin.Patches;

/**
    Intercept a variety of HTTP requests and rewrite them to point to our own custom server.
 */
public class SendRequestPatch
{
    // Official name server host to redirect away from, swapped for the custom server.
    private const string OfficialNameServer = "ns.rec.net";

    // Skip when HTTP-logging so we don't spam the logs.
    private static readonly string[] LogIgnoreSubstrings =
    {
        "/api/gamesight/event",
        "/data/heartbeat",
        "/identify",
        "/httpapi",
        "/data/event",
    };

    private static bool IsIgnoredForLogging(string url)
    {
        foreach (var s in LogIgnoreSubstrings)
            if (url.Contains(s, StringComparison.OrdinalIgnoreCase))
                return true;
        return false;
    }

    // Cap logged bodies so a large response/request doesn't flood the log.
    private const int MaxLoggedBodyLength = 10000;

    private static string Truncate(string s)
    {
        if (string.IsNullOrEmpty(s) || s.Length <= MaxLoggedBodyLength)
            return s;
        return s.Substring(0, MaxLoggedBodyLength) + $"... <truncated {s.Length - MaxLoggedBodyLength} chars>";
    }

    [HarmonyPatch(typeof(HTTPManager), "SendRequest", [typeof(HTTPRequest)])]
    public class ConnectToRecNetPatch
    {
        // Last seen Authorization header value, captured for reuse by
        // FluxPlusPatch when it needs to make authenticated backend calls.
        public static string LastAuthHeader { get; private set; }

        private static void Prefix(ref HTTPRequest request)
        {
            // Capture the auth header for reuse (FluxPlus token purchase flow).
            try
            {
                var auth = request.GetFirstHeaderValue("Authorization");
                if (!string.IsNullOrEmpty(auth))
                    LastAuthHeader = auth;
            }
            catch { }

            var debug = Plugin.Debug.Value && !IsIgnoredForLogging(request.Uri.AbsoluteUri);

            if (debug)
            {
                var entityBody = request.GetEntityBody();
                string body;
                if (entityBody == null)
                    body = "<none>";
                else if (IsBinaryContentType(request.GetFirstHeaderValue("content-type")) || LooksBinary(entityBody))
                    body = BinaryPreview(entityBody);
                else
                    body = System.Text.Encoding.UTF8.GetString(entityBody);
                Plugin.Log.LogInfo($"[HTTP] {request.MethodType} {request.Uri.AbsoluteUri} body={Truncate(body)}");
            }

            var host = request.Uri.Host;
            if (host == OfficialNameServer)
            {
                // Redirect the nameserver lookup to the custom server, swapping only the host.
                var newHost = new System.Uri(Plugin.ServerHostname.Value).Host;
                var builder = new Il2CppSystem.UriBuilder(request.Uri) { Host = newHost };
                request.Uri = builder.Uri;

                if (debug)
                    Plugin.Log.LogInfo($"[HTTP] intercepted {host} -> {newHost}");
            }

            // Storefront endpoint compatibility (comprehensive, 2026-09-26) —
            // see ApplyStorefrontFixes below for the full endpoint enumeration.
            ApplyStorefrontFixes(request);

            if (debug)
                LogResponseWhenDone(request);
        }
    }

    // Second net, one layer down. Every SendRequest overload funnels into SendRequestImpl, and
    // IL2CPP is free to inline the tiny SendRequest(HTTPRequest) body into its callers — a hook on
    // it then never fires for those call sites (a Harmony patch that loads clean can still never
    // run). SendRequestImpl is the last managed-visible chokepoint before the connection, so the
    // host redirect and storefront rewrites are applied here too. The mutations are idempotent —
    // re-running on an already-rewritten URL is a no-op — so double-application via both hooks is
    // safe. Auth capture and debug logging stay only in the SendRequest hook to avoid duplicates.
    [HarmonyPatch(typeof(HTTPManager), "SendRequestImpl", [typeof(HTTPRequest)])]
    public class SendRequestImplPatch
    {
        private static void Prefix(ref HTTPRequest request)
        {
            var host = request.Uri.Host;
            if (host == OfficialNameServer)
            {
                var newHost = new System.Uri(Plugin.ServerHostname.Value).Host;
                var builder = new Il2CppSystem.UriBuilder(request.Uri) { Host = newHost };
                request.Uri = builder.Uri;
            }

            // STOREBUGLOG: record every Store-related outgoing request
            try
            {
                var absUrl = request.Uri.AbsoluteUri;
                if (IsStoreRequest(absUrl))
                    WriteStoreBugLog($"STORE REQ: {request.MethodType} {absUrl}");
            }
            catch { }

            ApplyStorefrontFixes(request);

            // Attach response logger here too — the SendRequest hook may be
            // inlined away, so this is the only place guaranteed to run.
            // Double-attach is safe (just logs twice) if both hooks fire.
            try { LogResponseWhenDone(request); } catch { }
        }
    }

    // ------------------------------------------------------------------
    // STOREBUGLOG: file-based Store crash diagnostics.
    // Writes to STOREBUGLOG.txt in the game folder so Armin can paste it back.
    private static readonly object _storeBugLogLock = new object();
    private static string _storeBugLogPath;

    private static string StoreBugLogPath
    {
        get
        {
            if (_storeBugLogPath == null)
            {
                try
                {
                    var dir = System.IO.Directory.GetCurrentDirectory();
                    _storeBugLogPath = System.IO.Path.Combine(dir, "STOREBUGLOG.txt");
                }
                catch { _storeBugLogPath = "STOREBUGLOG.txt"; }
            }
            return _storeBugLogPath;
        }
    }

    private static bool IsStoreRequest(string url)
    {
        if (string.IsNullOrEmpty(url)) return false;
        url = url.ToLowerInvariant();
        return url.Contains("storefront") || url.Contains("store") ||
               url.Contains("giftdropstore") || url.Contains("tokenbundle") ||
               url.Contains("purchasecampaign") || url.Contains("algorithmiclist") ||
               url.Contains("customavataritem");
    }

    private static void WriteStoreBugLog(string message)
    {
        try
        {
            lock (_storeBugLogLock)
            {
                var line = $"[{System.DateTime.Now:yyyy-MM-dd HH:mm:ss.fff}] {message}{System.Environment.NewLine}";
                System.IO.File.AppendAllText(StoreBugLogPath, line);
            }
        }
        catch { /* never crash the game over logging */ }
    }

    // ------------------------------------------------------------------
    // Storefront endpoint compatibility (comprehensive, 2026-09-26,
    // re-verified live 2026-09-26 ~19:00 CEST against the deployed econ
    // worker at econ.recflare.net).
    //
    // Complete enumeration of the storefront endpoints baked into the 2023
    // client, extracted from GameAssembly's global-metadata.dat (2026-09-26).
    // The string table holds ONLY v1/v2 storefront routes (no v3/v4/v5/v6),
    // and the base const is literally "api/storefronts/" concatenated with
    // the versioned paths below — so this list is exhaustive, not sampled.
    //
    //   SERVED — no rewrite needed (live-verified):
    //     GET  /api/storefronts/v1/adcarouselitems -> 200, bare JSON array of
    //            StorefrontAdCarouselItem {AdCarouselItemId, ImageName, Title,
    //            Description, PurchasableItemIds, PurchaseReminderId?} — shape
    //            matches the client's DTO field-for-field.
    //     POST /api/storefronts/v2/buyItem      -> implemented (purchase)
    //     GET  /api/storefronts/v2/buyInvention  -> implemented (purchase)
    //     POST /api/ugcPurchasables/v1/items/bulk -> implemented (item resolve)
    //   REWRITTEN — see StorefrontRewrites:
    //     v2/balance -> v4/balance/2
    //   NO BACKEND EQUIVALENT — see UnmappedStorefrontPaths (telemetry only):
    //     v1/PurchaseRoomKeyWithCurrency, v1/buyForFreeGiftButton,
    //     v1/buyProgressionEventXpBoost, v1/buyPurchaseReminder, v1/buyRoomKey,
    //     v1/trialInvention, v1/trialInvention/duration, v2/buyElite, v2/buyTier,
    //     v1/toptoday, v1/objectives
    //
    // Notes:
    // - v1/toptoday and v1/objectives are PAGE-OPEN fetches (the Store page's
    //   item-list sources are Store/AdCarousel/Wishlist/TopToday, and the
    //   client fires adcarouselitems + toptoday as a paired async fetch).
    //   Both 404 live with `{"success":false,"error":{"message":"not found"}}`.
    //   They are the prime suspects for "Store opens EMPTY then crashes":
    //   the page renders with no items, and the faulted fetch pair ("Received
    //   null response from Storefront!") leaves the screen in a state the
    //   ~3s-later UI tick doesn't survive. No safe rewrite exists — their
    //   response DTO shapes are not recoverable from the dump, so per the
    //   no-synthesis rule they stay telemetry-only. The real fix is backend
    //   stubs returning `[]` (empty list reads as "nothing to show", a 404
    //   stalls the load) — same treatment adcarouselitems already got.
    // - v2/balance has NO native backend route (404 live); the v4/balance/2
    //   rewrite is load-bearing, not optional. v4/balance/:currencyType
    //   answers `[{CurrencyType, Platform, Balance}]` (single-entry array);
    //   the client wants List<BalanceResponseDTO> {Balance, CurrencyType,
    //   BalanceType} — BalanceType is absent and deserializes to default 0,
    //   which the balance-summing code tolerates.
    // - Response synthesis was considered for the 404 set and REJECTED:
    //   their shapes are not recoverable from any parsing code in the plugin
    //   or backend, and inventing one risks a worse crash than the 404.
    //   Fabricating a BestHTTP HTTPResponse was likewise rejected (no
    //   verifiable constructor contract; the v0.1.30 hang came from touching
    //   game callbacks). Balance is the one shape we CAN verify, and its
    //   rewrite target is a real, working endpoint, so no synthesis is needed.
    // - /api/items/purchaseInfos exists on the backend but the 2023 client
    //   NEVER calls it (zero occurrences in the client's string table) —
    //   not a crash factor.
    // ------------------------------------------------------------------
    private static readonly (string From, string To)[] StorefrontRewrites =
    {
        // The 2023 client fetches its token balance here (all-balances array
        // on the real server). The backend serves the per-currency v4 route
        // instead; the entry shape is identical ([{CurrencyType, Platform,
        // Balance}]). currencyType 2 = RecCenterTokens — the same bucket the
        // token purchase flow reads and charges (FluxPlusPatch,
        // PlusBuyDialog, PlusBalancePatch all use /api/storefronts/v4/balance/2).
        ("/api/storefronts/v2/balance", "/api/storefronts/v4/balance/2"),
        // Store page-open fetches: toptoday and objectives 404 on the live
        // backend (stubs exist in econ.app.ts but were never deployed). The
        // client pairs them with adcarouselitems in a WhenAll; a 404 faults
        // the pair and the page renders empty then crashes on the UI tick.
        // adcarouselitems is live and returns [] — the exact stub shape —
        // so rewriting here gives the identical result with no backend deploy.
        ("/api/storefronts/v1/toptoday", "/api/storefronts/v1/adcarouselitems"),
        ("/api/storefronts/v1/objectives", "/api/storefronts/v1/adcarouselitems"),
        ("/api/storefronts/v1/wishlist", "/api/storefronts/v1/adcarouselitems"),
    };

    // Client-called storefront paths with no backend equivalent. The buy*/trial*
    // entries are all purchase/action endpoints (room keys, try-on, elite
    // tiers, gift button, …), never page-open fetches — EXCEPT v1/toptoday and
    // v1/objectives, which ARE page-open fetches (paired with adcarouselitems)
    // and the prime suspects for the empty-then-crash Store page. The 404
    // passes through untouched; each is logged ONCE per session so the next
    // Store crash report names the exact endpoint the client tried.
    private static readonly string[] UnmappedStorefrontPaths =
    {
        "/api/storefronts/v1/PurchaseRoomKeyWithCurrency",
        "/api/storefronts/v1/buyForFreeGiftButton",
        "/api/storefronts/v1/buyProgressionEventXpBoost",
        "/api/storefronts/v1/buyPurchaseReminder",
        "/api/storefronts/v1/buyRoomKey",
        "/api/storefronts/v1/trialInvention",
        "/api/storefronts/v1/trialInvention/duration",
        "/api/storefronts/v2/buyElite",
        "/api/storefronts/v2/buyTier",
    };

    private static readonly HashSet<string> _loggedUnmappedStorefronts =
        new HashSet<string>(StringComparer.OrdinalIgnoreCase);

    private static void ApplyStorefrontFixes(HTTPRequest request)
    {
        string path;
        try { path = request.Uri.AbsolutePath; }
        catch { return; }
        if (string.IsNullOrEmpty(path))
            return;

        // Trailing-slash tolerance: the client's metadata strings carry no
        // trailing slash, but be lenient in case a call site adds one.
        var normPath = path.EndsWith("/", StringComparison.Ordinal) && path.Length > 1
            ? path.Substring(0, path.Length - 1)
            : path;

        foreach (var (from, to) in StorefrontRewrites)
        {
            if (!normPath.Equals(from, StringComparison.OrdinalIgnoreCase))
                continue;
            try
            {
                var builder = new Il2CppSystem.UriBuilder(request.Uri) { Path = to };
                request.Uri = builder.Uri;
                Plugin.Log.LogInfo($"[HTTP] rewrote {from} -> {to} (storefront compat)");
                WriteStoreBugLog($"STORE REWRITE: {from} -> {to}");
            }
            catch (Exception e)
            {
                Plugin.Log.LogWarning($"[HTTP] storefront rewrite {from} -> {to} failed: {e.Message}");
            }
            return;
        }

        foreach (var unmapped in UnmappedStorefrontPaths)
        {
            if (!normPath.Equals(unmapped, StringComparison.OrdinalIgnoreCase))
                continue;
            lock (_loggedUnmappedStorefronts)
            {
                if (_loggedUnmappedStorefronts.Add(normPath))
                    Plugin.Log.LogWarning(
                        $"[HTTP] storefront endpoint {normPath} has no backend equivalent " +
                        "(404 expected, no rewrite applied) — include this line with any Store crash report.");
            }
            return;
        }
    }

    // Wraps the request's completion callback so we log the response (status + body) when it
    // finishes, then forwards to the game's original callback. This is how we see *which*
    // request comes back empty (RecNet throws "Response was empty" on a blank body).
    private static void LogResponseWhenDone(HTTPRequest request)
    {
        try
        {
            var original = request.Callback;
            var url = request.Uri.AbsoluteUri;

            // NOTE: Was DelegateSupport.ConvertDelegate<OnRequestFinishedDelegate>(action).
            // See DeviceIdResponsePatch.cs for why CreateDelegate is used here.
            var logAction = (Action<HTTPRequest, HTTPResponse>)((req, resp) =>
                {
                    // STOREBUGLOG: record Store-related responses
                    try
                    {
                        if (IsStoreRequest(url))
                        {
                            if (resp == null)
                                WriteStoreBugLog($"STORE RESP: {url} -> NO RESPONSE (state={req.State})");
                            else
                            {
                                string rtext;
                                try
                                {
                                    rtext = resp.DataAsText;
                                    if (string.IsNullOrEmpty(rtext)) rtext = "<empty>";
                                    if (rtext.Length > 2000) rtext = rtext.Substring(0, 2000) + "... <truncated>";
                                }
                                catch { rtext = "<unreadable>"; }
                                WriteStoreBugLog($"STORE RESP: {url} -> {resp.StatusCode} body={rtext}");
                            }
                        }
                    }
                    catch { }

                    if (resp == null)
                        Plugin.Log.LogWarning($"[HTTP] <- {url} NO RESPONSE (state={req.State})");
                    else
                    {
                        string text;
                        if (IsBinaryContentType(resp.GetFirstHeaderValue("content-type")))
                            text = "<binary>";
                        else
                        {
                            text = resp.DataAsText;
                            if (string.IsNullOrEmpty(text)) text = "<empty>";
                        }
                        var msg = $"[HTTP] <- {resp.StatusCode} {url} body={Truncate(text)}";
                        if (resp.StatusCode is >= 200 and < 300)
                            Plugin.Log.LogInfo(msg);
                        else
                            Plugin.Log.LogError(msg);
                    }

                    original?.Invoke(req, resp);
                });
            request.Callback = (OnRequestFinishedDelegate)Delegate.CreateDelegate(
                typeof(OnRequestFinishedDelegate), logAction.Target, logAction.Method);
        }
        catch (Exception e)
        {
            Plugin.Log.LogError($"[HTTP] failed to attach response logger: {e}");
        }
    }

    // Content-Type prefixes/keywords we treat as textual; anything else is logged as <binary> so we
    // don't dump image/asset bytes into the log.
    private static readonly string[] TextContentTypes =
    {
        "text/", "application/json", "application/xml", "application/javascript",
        "application/x-www-form-urlencoded", "+json", "+xml",
    };

    // True if the body is (probably) binary and shouldn't be logged as text. Defaults to text when
    // there's no Content-Type, so we err toward logging rather than hiding.
    private static bool IsBinaryContentType(string contentType)
    {
        if (string.IsNullOrEmpty(contentType)) return false;

        foreach (var t in TextContentTypes)
            if (contentType.Contains(t, StringComparison.OrdinalIgnoreCase))
                return false;
        return true;
    }

    // Render the leading bytes of a binary body as text so structured framing (e.g. multipart form
    // boundaries and part headers) stays readable, while raw bytes are shown as \xNN escapes. Capped
    // at MaxLoggedBodyLength since the interesting framing is at the front.
    private static string BinaryPreview(byte[] data)
    {
        if (data.Length == 0) return "<binary empty>";

        var sb = new System.Text.StringBuilder(MaxLoggedBodyLength + 32);
        sb.Append("<binary ").Append(data.Length).Append(" bytes> ");
        var i = 0;
        // Cap on rendered length, not byte count: escapes expand a byte to 4 chars, so this keeps the
        // preview near MaxLoggedBodyLength and avoids a second pass by Truncate at the log site.
        for (; i < data.Length && sb.Length < MaxLoggedBodyLength; i++)
        {
            var b = data[i];
            if (b == 0x09 || b == 0x0A || b == 0x0D || (b >= 0x20 && b < 0x7F))
                sb.Append((char)b);
            else
                sb.Append("\\x").Append(b.ToString("x2"));
        }
        if (i < data.Length)
            sb.Append($"... <truncated {data.Length - i} bytes>");
        return sb.ToString();
    }

    // Content sniff for raw request bytes — the Content-Type header isn't reliably set at
    // SendRequest time (e.g. multipart form bodies set it lazily, and the body still embeds the
    // raw image), so look at the bytes: a NUL byte, or a high ratio of non-text control bytes in
    // the first chunk, means it's binary (or binary-mixed like a multipart upload).
    private static bool LooksBinary(byte[] data)
    {
        if (data.Length == 0) return false;

        var sample = Math.Min(data.Length, 4096);
        var nonText = 0;
        for (var i = 0; i < sample; i++)
        {
            var b = data[i];
            if (b == 0) return true;
            // Control chars other than tab/newline/carriage-return.
            if (b < 0x20 && b != 0x09 && b != 0x0A && b != 0x0D) nonText++;
        }
        return nonText * 100 / sample > 10;
    }
}
