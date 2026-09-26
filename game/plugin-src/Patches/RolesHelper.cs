using System;
using System.Collections.Generic;
using System.Reflection;
using BestHTTP;
using Il2CppInterop.Runtime.Injection;
using UnityEngine;

namespace RecNetPlugin.Patches;

// BADGES-02: backend role resolution for nametag badges.
//
// The badge patch (NametagBadgePatch) already resolves roles from CLIENT
// sources: thisPlayer -> IsDeveloper/IsModerator, playerNametagModel, and the
// [Badges] Moderator Account IDs config list. This helper adds the BACKEND
// as the authoritative source, consulted when the client-side checks come up
// empty:
//
//   GET {auth}/role/developer?id=1&id=2 -> [1,2]     (bulk filter)
//   GET {auth}/role/moderator?id=1&id=2 -> [3]
//
// (auth worker, public endpoints — no auth header needed. The bulk form
// answers "which of these players hold the role" as an array of ints, so a
// room full of nametags costs 2 requests, not 2N. Ids are batched over a
// short window before the flush.)
//
// Backend state (all in server/packages/domain/src/accounts-db.ts):
//   - Account.isDeveloper / Account.isModerator live in the D1 JSON blob
//     (no separate columns, so NO migration is needed)
//   - accounts worker emits both flags in toAccountDto (public) and
//     toSelfAccountDto (private, via spread) — already shipped
//   - auth worker serves /role/developer[/:id], /role/moderator[/:id] —
//     already shipped
//   - operators grant via `runx admin grant-developer` / `grant-moderator`
// CONCLUSION: no backend change was required for this task.
//
// Results are cached per session (Dictionary<int accountId, PlayerRole>).
// A role granted mid-session won't appear until relaunch — documented
// limitation, keeps the hot path allocation-free.
//
// Threading: BestHTTP callbacks arrive on a background thread. They only
// touch the lock-protected cache; badge creation always happens on the main
// thread. When a fetch completes, the affected nametag instances are
// re-checked via NametagBadgePatch.RefreshBadge on the main thread through
// the RolesPump queue (same marshalling pattern as FluxPairingPatch —
// Delegate.CreateDelegate is used ONLY for our own fresh requests, never on
// the game's callbacks).
[Flags]
internal enum PlayerRole
{
    None = 0,
    Developer = 1,
    Moderator = 2,
}

internal static class RolesHelper
{
    private static readonly object _lock = new();
    private static readonly Dictionary<int, PlayerRole> _cache = new();
    private static readonly HashSet<int> _inflight = new(); // ids with a request in flight
    private static readonly HashSet<int> _batched = new();   // ids waiting for the next bulk flush
    private static readonly Dictionary<int, List<WeakReference>> _waiters = new(); // accountId -> nametag instances awaiting data

    private static readonly object _queueLock = new();
    private static readonly Queue<Action> _mainQueue = new();

    private static bool _pumpCreated;
    private static float _batchCountdown = -1f;
    private const float BatchWindowSeconds = 0.5f;

    private static readonly TimeSpan HttpConnectTimeout = TimeSpan.FromSeconds(10);
    private static readonly TimeSpan HttpReadWriteTimeout = TimeSpan.FromSeconds(15);

    // Reflection name candidates. The client's type names are NOT obfuscated,
    // but exact member names vary, so we try several.
    private static readonly string[] PlayerMemberNames =
        { "thisPlayer", "Player", "player", "get_thisPlayer" };
    private static readonly string[] AccountIdNames =
        { "AccountId", "accountId", "accountID", "PlayerId", "playerId", "UserId", "userId", "Id", "id" };

    // ------------------------------------------------------------------ API

    /// <summary>
    /// Resolves the account id for a nametag instance via
    /// PlayerNameTag.thisPlayer -> AccountId (reflection, several candidate
    /// names). Never throws.
    /// </summary>
    public static bool TryGetAccountId(object nametagInstance, out int accountId)
    {
        accountId = 0;
        try
        {
            if (nametagInstance == null) return false;
            var player = ReadMember(nametagInstance, PlayerMemberNames);
            if (player == null) return false;
            var idVal = ReadMember(player, AccountIdNames);
            if (idVal != null && TryParseInt(idVal, out var id) && id > 0)
            {
                accountId = id;
                return true;
            }
        }
        catch { }
        return false;
    }

    /// <summary>
    /// Backend role from the session cache. Null = not yet known (no fetch
    /// has completed for this account).
    /// </summary>
    public static PlayerRole? GetCachedBackendRole(int accountId)
    {
        lock (_lock)
        {
            if (_cache.TryGetValue(accountId, out var role))
                return role;
            return null;
        }
    }

    /// <summary>
    /// Enqueues a backend role fetch for the account (batched with other
    /// pending ids into one bulk request per role). When the data lands, the
    /// nametag instance is re-checked on the main thread via
    /// NametagBadgePatch.RefreshBadge. Idempotent and never throws.
    /// </summary>
    public static void EnsureBackendFetch(object nametagInstance, int accountId)
    {
        try
        {
            EnsurePump();
            lock (_lock)
            {
                if (_cache.ContainsKey(accountId) || _inflight.Contains(accountId))
                    return; // known, or a fetch is already covering it
                _batched.Add(accountId);
                TrackWaiter(accountId, nametagInstance);
                if (_batchCountdown < 0f)
                    _batchCountdown = BatchWindowSeconds;
            }
        }
        catch { }
    }

    // ------------------------------------------------------------- internals

    private static void TrackWaiter(int accountId, object nametagInstance)
    {
        if (nametagInstance == null) return;
        if (!_waiters.TryGetValue(accountId, out var list))
        {
            list = new List<WeakReference>();
            _waiters[accountId] = list;
        }
        foreach (var w in list)
        {
            if (w.IsAlive && ReferenceEquals(w.Target, nametagInstance))
                return;
        }
        list.Add(new WeakReference(nametagInstance));
    }

    private static void EnsurePump()
    {
        if (_pumpCreated) return;
        try
        {
            ClassInjector.RegisterTypeInIl2Cpp<RolesPump>();
            var go = new GameObject("FluxRolesPump");
            go.hideFlags = HideFlags.HideAndDontSave;
            UnityEngine.Object.DontDestroyOnLoad(go);
            go.AddComponent<RolesPump>();
            _pumpCreated = true;
            Plugin.Log.LogInfo("[ROLES] pump created");
        }
        catch (Exception e)
        {
            Plugin.Log.LogWarning("[ROLES] pump setup failed: " + e.Message);
        }
    }

    /// <summary>Main-thread drain: runs queued actions and flushes the role batch timer.</summary>
    internal static void Drain(float deltaTime)
    {
        // Flush batched role requests.
        lock (_lock)
        {
            if (_batchCountdown >= 0f)
            {
                _batchCountdown -= deltaTime;
                if (_batchCountdown < 0f && _batched.Count > 0)
                {
                    var ids = new List<int>(_batched);
                    _batched.Clear();
                    foreach (var id in ids) _inflight.Add(id);
                    FlushBatch(ids);
                }
            }
        }

        // Run main-thread callbacks.
        while (true)
        {
            Action a = null;
            lock (_queueLock)
            {
                if (_mainQueue.Count == 0) break;
                a = _mainQueue.Dequeue();
            }
            try { a?.Invoke(); }
            catch { }
        }
    }

    private static void FlushBatch(List<int> ids)
    {
        var host = FluxPairingPatch.AuthHost();
        if (string.IsNullOrEmpty(host))
        {
            Plugin.Log.LogWarning("[ROLES] no auth host configured — skipping backend role fetch");
            lock (_lock)
            {
                foreach (var id in ids) _inflight.Remove(id);
            }
            return;
        }

        var query = BuildIdQuery(ids);
        // One bulk request per role; each updates the cache and re-checks waiters.
        SendGet(host + "/role/developer" + query, (status, body) =>
            OnRoleResponse(status, body, ids, PlayerRole.Developer));
        SendGet(host + "/role/moderator" + query, (status, body) =>
            OnRoleResponse(status, body, ids, PlayerRole.Moderator));
    }

    private static string BuildIdQuery(List<int> ids)
    {
        // ?id=1&id=2&id=3 (matches the auth worker's bulk filter form)
        var sb = new System.Text.StringBuilder("?");
        for (var i = 0; i < ids.Count; i++)
        {
            if (i > 0) sb.Append('&');
            sb.Append("id=").Append(ids[i]);
        }
        return sb.ToString();
    }

    private static void OnRoleResponse(int status, string body, List<int> ids, PlayerRole flag)
    {
        try
        {
            var holders = (status == 200) ? ParseIntArray(body) : new HashSet<int>();
            var affected = new List<object>();
            lock (_lock)
            {
                foreach (var id in ids)
                {
                    _inflight.Remove(id);
                    if (holders.Contains(id))
                    {
                        _cache.TryGetValue(id, out var cur);
                        _cache[id] = cur | flag;
                    }
                    else if (!_cache.ContainsKey(id))
                    {
                        // Explicit None so we don't refetch every frame.
                        _cache[id] = PlayerRole.None;
                    }

                    if (_waiters.TryGetValue(id, out var list))
                    {
                        foreach (var w in list)
                        {
                            if (w.IsAlive && w.Target != null)
                                affected.Add(w.Target);
                        }
                        _waiters.Remove(id);
                    }
                }
            }

            if (affected.Count == 0) return;

            // Back on the main thread: re-run badge logic now that roles are known.
            lock (_queueLock)
            {
                _mainQueue.Enqueue(() =>
                {
                    foreach (var nametag in affected)
                    {
                        try { NametagBadgePatch.RefreshBadge(nametag); }
                        catch { }
                    }
                });
            }
        }
        catch (Exception e)
        {
            Plugin.Log.LogWarning("[ROLES] role response handling failed: " + e.Message);
        }
    }

    // GET with the game's BestHTTP stack.
    // Delegate.CreateDelegate is used ONLY for our own fresh request (the
    // proven FluxPairingPatch pattern) — never on the game's callbacks.
    private static void SendGet(string url, Action<int, string> onDone)
    {
        try
        {
            Action<HTTPRequest, HTTPResponse> action = (req, resp) =>
            {
                try
                {
                    int status = resp != null ? resp.StatusCode : -1;
                    string body = resp != null ? (resp.DataAsText ?? "") : "";
                    lock (_queueLock) { _mainQueue.Enqueue(() => onDone(status, body)); }
                }
                catch (Exception e)
                {
                    Plugin.Log.LogWarning("[ROLES] callback failed: " + e.Message);
                }
            };
            var cb = (OnRequestFinishedDelegate)Delegate.CreateDelegate(
                typeof(OnRequestFinishedDelegate), action.Target, action.Method);
            var req = new HTTPRequest(new Il2CppSystem.Uri(url), cb);
            req.MethodType = HTTPMethods.Get;
            req.ConnectTimeout = new Il2CppSystem.TimeSpan(HttpConnectTimeout.Ticks);
            HTTPManager.SendRequest(req);
        }
        catch (Exception e)
        {
            Plugin.Log.LogWarning("[ROLES] request failed: " + e.Message);
            lock (_queueLock) { _mainQueue.Enqueue(() => onDone(-1, "")); }
        }
    }

    /// <summary>Parses a flat JSON int array like "[3, 17]". No JSON lib on IL2CPP.</summary>
    private static HashSet<int> ParseIntArray(string json)
    {
        var set = new HashSet<int>();
        try
        {
            if (string.IsNullOrEmpty(json)) return set;
            var sb = new System.Text.StringBuilder();
            foreach (var c in json)
            {
                if (char.IsDigit(c) || (c == '-' && sb.Length == 0))
                {
                    sb.Append(c);
                }
                else if (sb.Length > 0)
                {
                    if (int.TryParse(sb.ToString(), out var v) && v > 0) set.Add(v);
                    sb.Length = 0;
                }
            }
            if (sb.Length > 0 && int.TryParse(sb.ToString(), out var last) && last > 0)
                set.Add(last);
        }
        catch { }
        return set;
    }

    // ------------------------------------------------------- reflection util

    private static object ReadMember(object obj, string[] names)
    {
        if (obj == null) return null;
        try
        {
            var type = obj.GetType();
            foreach (var name in names)
            {
                var prop = type.GetProperty(name,
                    BindingFlags.Public | BindingFlags.NonPublic | BindingFlags.Instance);
                if (prop != null && prop.CanRead)
                {
                    try
                    {
                        var v = prop.GetValue(obj, null);
                        if (v != null) return v;
                    }
                    catch { }
                }

                var field = type.GetField(name,
                    BindingFlags.Public | BindingFlags.NonPublic | BindingFlags.Instance);
                if (field != null)
                {
                    try
                    {
                        var v = field.GetValue(obj);
                        if (v != null) return v;
                    }
                    catch { }
                }

                if (name.StartsWith("get_"))
                {
                    var m = type.GetMethod(name,
                        BindingFlags.Public | BindingFlags.NonPublic | BindingFlags.Instance,
                        null, Type.EmptyTypes, null);
                    if (m != null)
                    {
                        try
                        {
                            var v = m.Invoke(obj, null);
                            if (v != null) return v;
                        }
                        catch { }
                    }
                }
            }
        }
        catch { }
        return null;
    }

    private static bool TryParseInt(object v, out int result)
    {
        result = 0;
        try
        {
            if (v is int i) { result = i; return true; }
            if (v is long l) { result = (int)l; return true; }
            if (v is short s) { result = s; return true; }
            if (v != null && int.TryParse(v.ToString(), out result)) return true;
        }
        catch { }
        return false;
    }

    // ------------------------------------------------------- pump component

    private class RolesPump : MonoBehaviour
    {
        private float _lastTime;

        private void Start()
        {
            _lastTime = Time.realtimeSinceStartup;
        }

        private void Update()
        {
            try
            {
                var now = Time.realtimeSinceStartup;
                var dt = now - _lastTime;
                _lastTime = now;
                if (dt < 0f || dt > 5f) dt = 0.016f; // clamp hiccups
                RolesHelper.Drain(dt);
            }
            catch { }
        }
    }
}
