using System;
using System.Collections.Generic;
using System.Linq;
using System.Reflection;
using HarmonyLib;
using UnityEngine;
using UnityEngine.UI;

namespace RecNetPlugin.Patches;

// Renders orange "DEV" and blue/green "MOD" badges above nametags.
//
// The DeveloperPatch already forces SessionManager.get_IsDeveloper() to true,
// which unlocks developer-gated UI. But there is no stock "DEV"/"MOD" badge
// above player heads — the only dev badge in the client is on the profile page
// (AccountModelController.HideDeveloperBadgeImpl). This patch creates both:
//
// BADGES-01 (nametag hooking layer):
// - Resolves the nametag type by unobfuscated name ("PlayerNameTag", from
//   player.get_PlayerNameTag()), with the old Contains("Nametag") scan as
//   fallback.
// - Harmony-postfixes get_PlayerNameTag so newly spawned players register
//   without waiting for a scene load.
// - Sweeps the live scene for PlayerNameTag components on every Apply()
//   (retried from Plugin.OnSceneLoaded via the standard retry-on-scene-load
//   pattern) and keeps them in the HookedNametags registry, pruning
//   destroyed entries. The registry is the handoff point for badge creation.
//
// DEV badge (BADGES-03):
// - Hook: Harmony postfix on the nametag's badge/title visibility methods
//   (nametag type found by unobfuscated type name; methods found by
//   badge/title keyword, max 5 — precise and minimal).
// - Role: resolved per nametag from thisPlayer -> IsDeveloper (property,
//   field, or get_IsDeveloper()), falling back to
//   playerNametagModel -> IsDeveloper. Fail-closed: no badge unless developer
//   status is confirmed for that player.
// - Badge: a child GameObject ("FluxDevBadge") under the nametag's
//   nameTagCanvas transform, created once per nametag. It carries a
//   TextMeshProUGUI with orange "DEV" text, positioned above the name label
//   (nametagHeightOffset is the fallback anchor, per spec).
// - Visibility: synced with the nametag's own get_IsNameTagVisible().
//
// MOD badge (BADGES-04): same shape, for community moderators.
// - Role: resolved per nametag from thisPlayer -> IsModerator (property,
//   field, or get_IsModerator()), then playerNametagModel -> IsModerator,
//   then the [Badges] Moderator Account IDs config list (explicit fallback).
//   Fail-closed: no badge unless moderator status is confirmed.
// - Badge: a child GameObject ("FluxModBadge") under the nametag's
//   nameTagCanvas transform, with blue/green "MOD" TextMeshProUGUI text.
// - Visibility: synced with the nametag's own visibility, same as DEV.
//
// IL2CPP rules honored (same as UltraGraphicsPatch):
// - Downcasts go through TryCast, never direct casts.
// - GetComponent(s)/AddComponent take Il2CppSystem.Type, not System.Type.
// - TMPro is resolved at runtime (no compile-time dependency) — the same
//   fallback pattern UltraGraphicsPatch.RelabelClone uses.
// - Everything is fail-soft in try/catch; no Delegate.CreateDelegate anywhere.
//
// Two knobs, see [Badges] in the .cfg:
//   Enable Dev Nametag Badge -> THE FIX (default true).
//   Enable Mod Badge -> THE FIX (default true).
internal static class NametagBadgePatch
{
    private const string BadgeObjectName = "FluxDevBadge";
    private const string ModBadgeObjectName = "FluxModBadge";
    private const float BadgeGapAboveName = 0.35f;
    private const float DefaultBadgeFontSize = 30f;

    private static int _attempts;
    private const int MaxAttempts = 10;

    // One-shot flags: badge-method patching and the spawn hook install once
    // per session; the live sweep keeps running on scene loads until attempts
    // run out (standard retry-on-scene-load pattern).
    private static bool _badgeMethodsPatched;
    private static bool _spawnHookInstalled;

    // Resolved nametag IL2CPP type (unobfuscated name: PlayerNameTag).
    private static Type _nametagType;

    private static Il2CppSystem.Type _tmproIl2CppType;

    // BADGES-01 registry: every hooked PlayerNameTag component, for badge
    // creation to consume.
    private static readonly List<Component> _hooked = new List<Component>();
    private static readonly HashSet<int> _hookedIds = new HashSet<int>();
    private static readonly object _hookLock = new object();

    /// <summary>
    /// Live snapshot of every hooked PlayerNameTag component. Badge creation
    /// (DEV/MOD) reads this registry.
    /// </summary>
    public static IReadOnlyList<Component> HookedNametags
    {
        get { lock (_hookLock) return _hooked.ToList().AsReadOnly(); }
    }

    /// <summary>
    /// BADGES-02: re-runs badge logic for a nametag instance on the main
    /// thread. Called by RolesHelper when backend role data arrives for an
    /// account whose badge couldn't be decided from client-side data alone.
    /// </summary>
    public static void RefreshBadge(object nametagInstance)
    {
        try
        {
            if (nametagInstance == null) return;
            if (!Plugin.EnableDevNametagBadge.Value && !Plugin.EnableModBadge.Value) return;
            if (Plugin.EnableDevNametagBadge.Value)
            {
                try { EnsureDevBadge(nametagInstance); }
                catch { }
            }
            if (Plugin.EnableModBadge.Value)
            {
                try { EnsureModBadge(nametagInstance); }
                catch { }
            }
        }
        catch { }
    }

    public static void Apply()
    {
        if (_attempts >= MaxAttempts) return;
        _attempts++;

        try
        {
            // BADGES-01: resolve the nametag type by unobfuscated name.
            if (_nametagType == null)
                _nametagType = FindNametagType();

            if (_nametagType == null)
            {
                if (_attempts >= MaxAttempts)
                    Plugin.Log.LogWarning("[NAMETAG] nametag type not found after max attempts");
                return;
            }

            // One-shot: patch the nametag's badge/title visibility methods.
            if (!_badgeMethodsPatched)
                PatchBadgeMethods(_nametagType);

            // One-shot: hook get_PlayerNameTag so player spawns register
            // without waiting for a scene load.
            if (!_spawnHookInstalled)
                InstallSpawnHook();

            // Every scene load: sweep live nametags into the registry
            // (idempotent — skips already-registered ones).
            SweepLiveNametags();
        }
        catch (Exception e)
        {
            Plugin.Log.LogWarning($"[NAMETAG] attempt {_attempts} failed: {e.Message}");
        }
    }

    // BADGES-01: exact unobfuscated name first ("PlayerNameTag", from
    // player.get_PlayerNameTag()), Contains-scan as fallback.
    private static Type FindNametagType()
    {
        Type fallback = null;

        foreach (var asm in AppDomain.CurrentDomain.GetAssemblies())
        {
            Type[] types;
            try { types = asm.GetTypes(); }
            catch { continue; }

            foreach (var t in types)
            {
                if (t.Name == "PlayerNameTag")
                {
                    Plugin.Log.LogInfo($"[NAMETAG] found type: {t.FullName}");
                    return t;
                }

                if (fallback == null &&
                    (t.Name.Contains("Nametag") || t.Name.Contains("NameTag")))
                    fallback = t;
            }
        }

        if (fallback != null)
            Plugin.Log.LogInfo($"[NAMETAG] found type (fallback): {fallback.FullName}");
        return fallback;
    }

    private static void PatchBadgeMethods(Type nametagType)
    {
        // Find methods that control badge/title visibility
        // Look for methods with "badge", "title", or "developer" in name
        var methods = nametagType.GetMethods(BindingFlags.Public | BindingFlags.NonPublic | BindingFlags.Instance)
            .Where(m =>
            {
                var n = m.Name.ToLower();
                return (n.Contains("badge") || n.Contains("title")) &&
                       m.GetParameters().Length <= 1;
            })
            .Take(5) // Max 5 methods, not 30
            .ToArray();

        if (methods.Length == 0)
        {
            Plugin.Log.LogInfo("[NAMETAG] no badge methods found — nametag may use different pattern");
            _badgeMethodsPatched = true; // Don't retry forever
            return;
        }

        var harmony = new Harmony("com.fluxrec.nametagbadge");
        var postfix = new HarmonyMethod(typeof(NametagBadgePatch).GetMethod(nameof(EnsureBadgeVisible),
            BindingFlags.Static | BindingFlags.NonPublic));

        foreach (var m in methods)
        {
            try
            {
                harmony.Patch(m, postfix: postfix);
                Plugin.Log.LogInfo($"[NAMETAG] patched {m.Name}");
            }
            catch (Exception e)
            {
                Plugin.Log.LogWarning($"[NAMETAG] failed to patch {m.Name}: {e.Message}");
            }
        }

        _badgeMethodsPatched = true;
        Plugin.Log.LogInfo("[NAMETAG] badge patch applied");
    }

    // BADGES-01: hook get_PlayerNameTag (unobfuscated) so newly spawned
    // players register without waiting for the next scene load. The postfix
    // binds __result positionally — the getter takes no parameters.
    private static void InstallSpawnHook()
    {
        MethodInfo getter = null;

        foreach (var asm in AppDomain.CurrentDomain.GetAssemblies())
        {
            Type[] types;
            try { types = asm.GetTypes(); }
            catch { continue; }

            foreach (var t in types)
            {
                MethodInfo m;
                try
                {
                    m = t.GetMethod("get_PlayerNameTag",
                        BindingFlags.Public | BindingFlags.NonPublic | BindingFlags.Instance,
                        null, Type.EmptyTypes, null);
                }
                catch { continue; }

                if (m != null)
                {
                    getter = m;
                    break;
                }
            }

            if (getter != null) break;
        }

        if (getter == null)
        {
            Plugin.Log.LogDebug("[NAMETAG] get_PlayerNameTag getter not found yet");
            return;
        }

        try
        {
            var harmony = new Harmony("com.fluxrec.nametagspawn");
            var postfix = new HarmonyMethod(typeof(NametagBadgePatch).GetMethod(nameof(OnPlayerNameTagSpawned),
                BindingFlags.Static | BindingFlags.NonPublic));
            harmony.Patch(getter, postfix: postfix);
            _spawnHookInstalled = true;
            Plugin.Log.LogInfo($"[NAMETAG] hooked {getter.DeclaringType?.Name}.get_PlayerNameTag for player spawns");
        }
        catch (Exception e)
        {
            Plugin.Log.LogWarning($"[NAMETAG] spawn hook install failed: {e.Message}");
        }
    }

    // BADGES-01: sweep the live scene for PlayerNameTag components using the
    // proven UltraGraphicsPatch pattern (FindObjectsOfType via reflection,
    // TryCast for downcasts — never a direct cast).
    private static void SweepLiveNametags()
    {
        if (_nametagType == null) return;

        var find = typeof(UnityEngine.Object).GetMethod("FindObjectsOfType",
            new[] { typeof(Type) });
        if (find == null)
        {
            Plugin.Log.LogWarning("[NAMETAG] FindObjectsOfType(Type) not found");
            return;
        }

        System.Collections.IEnumerable all;
        try
        {
            all = (System.Collections.IEnumerable)find.Invoke(null, new object[] { _nametagType });
        }
        catch (Exception e)
        {
            Plugin.Log.LogWarning($"[NAMETAG] sweep failed: {e.Message}");
            return;
        }

        if (all == null) return;

        PruneDestroyed();
        var found = 0;
        foreach (var o in all)
        {
            var uo = o as UnityEngine.Object;
            if (uo == null) continue;
            if (RegisterNametag(uo.TryCast<Component>())) found++;
        }

        if (found > 0)
            Plugin.Log.LogInfo($"[NAMETAG] sweep hooked {found} new nametag(s)");
    }

    // Postfix for get_PlayerNameTag: registers the nametag the moment a
    // player object exposes it (covers late joins without a scene load).
    private static void OnPlayerNameTagSpawned(object __result)
    {
        try
        {
            if (!(__result is UnityEngine.Object uo)) return;
            RegisterNametag(uo.TryCast<Component>());
        }
        catch { }
    }

    // BADGES-01: registers a nametag component for badge creation.
    // Idempotent: returns true only when the nametag was newly added.
    internal static bool RegisterNametag(Component comp)
    {
        if (comp == null) return false;
        // Unity fake-null: destroyed IL2CPP objects compare equal to null.
        if (comp == null) return false;

        int id;
        try { id = comp.GetInstanceID(); }
        catch { return false; }

        lock (_hookLock)
        {
            if (!_hookedIds.Add(id)) return false;
            _hooked.Add(comp);
            Plugin.Log.LogInfo($"[NAMETAG] registered nametag on '{comp.gameObject.name}' (total {_hooked.Count})");
            return true;
        }
    }

    // BADGES-01: drops registry entries whose GameObjects were destroyed
    // (scene unload / player despawn) so badge code never touches dead objects.
    private static void PruneDestroyed()
    {
        lock (_hookLock)
        {
            for (var i = _hooked.Count - 1; i >= 0; i--)
            {
                var comp = _hooked[i];
                bool dead;
                try { dead = comp == null || comp.gameObject == null; }
                catch { dead = true; }

                if (dead)
                {
                    try { _hookedIds.Remove(_hooked[i].GetInstanceID()); }
                    catch { }
                    _hooked.RemoveAt(i);
                }
            }
        }
    }

    // Postfix: after the nametag updates, ensure the DEV badge exists and is
    // visible iff the player is a developer and the nametag is visible.
    private static void EnsureBadgeVisible(object __instance)
    {
        try
        {
            if (__instance == null) return;

            // BADGES-01: the postfix fires on the nametag instance itself,
            // so register it for badge creation here too.
            if (__instance is UnityEngine.Object uo)
                RegisterNametag(uo.TryCast<Component>());

            if (!Plugin.EnableDevNametagBadge.Value && !Plugin.EnableModBadge.Value) return;

            var type = __instance.GetType();

            // Diagnostic: log badge/title fields the first time we see them
            // (kept from the original diagnostic pass).
            foreach (var field in type.GetFields(BindingFlags.Public | BindingFlags.NonPublic | BindingFlags.Instance))
            {
                var fieldName = field.Name.ToLower();
                if (!fieldName.Contains("badge") && !fieldName.Contains("title"))
                    continue;

                var val = field.GetValue(__instance);
                if (val is GameObject go && !go.activeSelf)
                {
                    Plugin.Log.LogInfo($"[NAMETAG] found badge object: {field.Name}");
                }
                else if (val is Component comp)
                {
                    var compGo = comp.gameObject;
                    if (compGo != null && !compGo.activeSelf)
                        Plugin.Log.LogInfo($"[NAMETAG] found badge component: {field.Name} on {compGo.name}");
                }
            }

            if (Plugin.EnableDevNametagBadge.Value)
            {
                try { EnsureDevBadge(__instance); }
                catch (Exception e)
                {
                    Plugin.Log.LogDebug($"[NAMETAG] EnsureDevBadge failed: {e.Message}");
                }
            }

            // MOD badge (BADGES-04): same hook, moderators get "MOD".
            if (Plugin.EnableModBadge.Value)
            {
                try { EnsureModBadge(__instance); }
                catch (Exception e)
                {
                    Plugin.Log.LogDebug($"[NAMETAG] EnsureModBadge failed: {e.Message}");
                }
            }
        }
        catch { }
    }

    // Creates (once per nametag) and syncs the orange "DEV" badge for
    // developer players.
    private static void EnsureDevBadge(object nametag)
    {
        try
        {
            // Only developers get the badge.
            if (!IsDeveloperNametag(nametag))
                return;

            // The badge lives under the nametag's own canvas.
            var canvasGo = ToGameObject(GetMemberValue(nametag, "nameTagCanvas"));
            if (canvasGo == null)
                return;
            var canvasTransform = canvasGo.transform;
            if (canvasTransform == null)
                return;

            // Create once per nametag; reuse on later updates.
            GameObject badgeGo = null;
            var existing = canvasTransform.Find(BadgeObjectName);
            if (existing != null)
                badgeGo = existing.gameObject;
            if (badgeGo == null)
            {
                badgeGo = CreateDevBadge(canvasGo, canvasTransform, nametag);
                if (badgeGo == null)
                    return;
            }

            // Visibility follows the nametag's own visibility.
            bool visible = GetIsNameTagVisible(nametag, badgeGo.activeSelf);
            if (badgeGo.activeSelf != visible)
                badgeGo.SetActive(visible);
        }
        catch (Exception e)
        {
            Plugin.Log.LogDebug($"[NAMETAG] EnsureDevBadge failed: {e.Message}");
        }
    }

    private static GameObject CreateDevBadge(GameObject canvasGo, Transform parent, object nametag)
    {
        try
        {
            var tmproType = TmproIl2CppType();
            if (tmproType == null)
            {
                Plugin.Log.LogWarning("[NAMETAG] TextMeshProUGUI type not found — DEV badge skipped");
                return null;
            }

            var badgeGo = new GameObject(BadgeObjectName);
            // Parent under the canvas; worldPositionStays=false keeps local coords.
            badgeGo.transform.SetParent(parent, false);
            badgeGo.transform.localPosition = new Vector3(0f, ResolveBadgeY(nametag, canvasGo), 0f);
            badgeGo.transform.localScale = Vector3.one;

            // be.788: AddComponent requires Il2CppSystem.Type, not System.Type.
            var comp = badgeGo.AddComponent(tmproType);
            if (comp == null)
            {
                UnityEngine.Object.Destroy(badgeGo);
                return null;
            }

            ConfigureDevBadgeText(comp, canvasGo);
            Plugin.Log.LogInfo("[NAMETAG] DEV badge created");
            return badgeGo;
        }
        catch (Exception e)
        {
            Plugin.Log.LogWarning($"[NAMETAG] DEV badge creation failed: {e.Message}");
            return null;
        }
    }

    private static void ConfigureDevBadgeText(Component tmpro, GameObject canvasGo)
    {
        var t = tmpro.GetType();
        SetProp(t, tmpro, "text", "DEV");
        SetProp(t, tmpro, "color", new Color(1f, 0.55f, 0f, 1f)); // orange
        SetProp(t, tmpro, "fontSize", ResolveBadgeFontSize(canvasGo));
        SetProp(t, tmpro, "raycastTarget", false);
        // alignment is a TextAlignmentOptions enum — parse by name so we don't
        // hardcode the numeric value.
        try
        {
            var alignProp = t.GetProperty("alignment", BindingFlags.Public | BindingFlags.Instance);
            if (alignProp != null && alignProp.CanWrite)
                alignProp.SetValue(tmpro, Enum.Parse(alignProp.PropertyType, "Center"), null);
        }
        catch { }
    }

    // Creates (once per nametag) and syncs the blue/green "MOD" badge for
    // community moderators. Mirrors the DEV path (BADGES-04).
    private static void EnsureModBadge(object nametag)
    {
        try
        {
            // Only confirmed moderators get the badge (fail-closed).
            if (!IsModeratorNametag(nametag))
                return;

            // The badge lives under the nametag's own canvas.
            var canvasGo = ToGameObject(GetMemberValue(nametag, "nameTagCanvas"));
            if (canvasGo == null)
                return;
            var canvasTransform = canvasGo.transform;
            if (canvasTransform == null)
                return;

            // Create once per nametag; reuse on later updates.
            GameObject badgeGo = null;
            var existing = canvasTransform.Find(ModBadgeObjectName);
            if (existing != null)
                badgeGo = existing.gameObject;
            if (badgeGo == null)
            {
                badgeGo = CreateModBadge(canvasGo, canvasTransform, nametag);
                if (badgeGo == null)
                    return;
            }

            // Visibility follows the nametag's own visibility.
            bool visible = GetIsNameTagVisible(nametag, badgeGo.activeSelf);
            if (badgeGo.activeSelf != visible)
                badgeGo.SetActive(visible);
        }
        catch (Exception e)
        {
            Plugin.Log.LogDebug($"[NAMETAG] EnsureModBadge failed: {e.Message}");
        }
    }

    private static GameObject CreateModBadge(GameObject canvasGo, Transform parent, object nametag)
    {
        try
        {
            var tmproType = TmproIl2CppType();
            if (tmproType == null)
            {
                Plugin.Log.LogWarning("[NAMETAG] TextMeshProUGUI type not found — MOD badge skipped");
                return null;
            }

            var badgeGo = new GameObject(ModBadgeObjectName);
            // Parent under the canvas; worldPositionStays=false keeps local coords.
            badgeGo.transform.SetParent(parent, false);
            badgeGo.transform.localPosition = new Vector3(0f, ResolveBadgeY(nametag, canvasGo), 0f);
            badgeGo.transform.localScale = Vector3.one;

            // be.788: AddComponent requires Il2CppSystem.Type, not System.Type.
            var comp = badgeGo.AddComponent(tmproType);
            if (comp == null)
            {
                UnityEngine.Object.Destroy(badgeGo);
                return null;
            }

            ConfigureModBadgeText(comp, canvasGo);
            Plugin.Log.LogInfo("[NAMETAG] MOD badge created");
            return badgeGo;
        }
        catch (Exception e)
        {
            Plugin.Log.LogWarning($"[NAMETAG] MOD badge creation failed: {e.Message}");
            return null;
        }
    }

    private static void ConfigureModBadgeText(Component tmpro, GameObject canvasGo)
    {
        var t = tmpro.GetType();
        SetProp(t, tmpro, "text", "MOD");
        SetProp(t, tmpro, "color", new Color(0.25f, 0.75f, 1f, 1f)); // blue/cyan
        SetProp(t, tmpro, "fontSize", ResolveBadgeFontSize(canvasGo));
        SetProp(t, tmpro, "raycastTarget", false);
        // alignment is a TextAlignmentOptions enum — parse by name so we don't
        // hardcode the numeric value.
        try
        {
            var alignProp = t.GetProperty("alignment", BindingFlags.Public | BindingFlags.Instance);
            if (alignProp != null && alignProp.CanWrite)
                alignProp.SetValue(tmpro, Enum.Parse(alignProp.PropertyType, "Center"), null);
        }
        catch { }
    }

    // Per-player moderator check. Tries thisPlayer -> IsModerator, then
    // playerNametagModel -> IsModerator, then the [Badges] Moderator Account
    // IDs config list (explicit fallback). Fail-closed: no badge unless
    // moderator status is confirmed.
    private static bool IsModeratorNametag(object nametag)
    {
        try
        {
            var player = GetMemberValue(nametag, "thisPlayer");
            if (player != null)
            {
                var mod = ResolveBoolMember(player, "IsModerator", "isModerator");
                if (mod.HasValue) return mod.Value;

                var acct = GetMemberValue(player, "Account")
                    ?? GetMemberValue(player, "account")
                    ?? GetMemberValue(player, "PlayerAccount");
                if (acct != null)
                {
                    var amod = ResolveBoolMember(acct, "IsModerator", "isModerator");
                    if (amod.HasValue) return amod.Value;
                }

                // Explicit config fallback: match account ID or username.
                if (ModeratorListMatches(player, acct))
                    return true;
            }

            var model = GetMemberValue(nametag, "playerNametagModel")
                ?? GetMemberValue(nametag, "PlayerNametagModel");
            if (model != null)
            {
                var mmod = ResolveBoolMember(model, "IsModerator", "isModerator");
                if (mmod.HasValue) return mmod.Value;
            }

            // BADGES-02: backend authoritative source (auth worker role
            // filters). On cache miss this kicks off a batched fetch;
            // RefreshBadge re-runs this check on the main thread when the
            // data lands.
            if (RolesHelper.TryGetAccountId(nametag, out var modAccountId))
            {
                var cached = RolesHelper.GetCachedBackendRole(modAccountId);
                if (cached.HasValue)
                    return cached.Value.HasFlag(PlayerRole.Moderator);
                RolesHelper.EnsureBackendFetch(nametag, modAccountId);
            }
        }
        catch { }
        return false;
    }

    // Checks the player's account ID / username against the [Badges]
    // "Moderator Account IDs" config list (comma-separated).
    private static bool ModeratorListMatches(object player, object account)
    {
        string list;
        try { list = Plugin.ModeratorAccountIds.Value; }
        catch { return false; }
        if (string.IsNullOrWhiteSpace(list)) return false;

        var entries = list.Split(new[] { ',' }, StringSplitOptions.RemoveEmptyEntries)
            .Select(s => s.Trim())
            .Where(s => s.Length > 0)
            .ToArray();
        if (entries.Length == 0) return false;

        var identities = new HashSet<string>(StringComparer.OrdinalIgnoreCase);
        foreach (var source in new[] { player, account })
        {
            if (source == null) continue;
            foreach (var name in new[] { "AccountId", "accountId", "Id", "id", "Username", "username", "DisplayName", "displayName", "Name" })
            {
                try
                {
                    var v = GetMemberValue(source, name);
                    if (v == null) continue;
                    var s = v.ToString();
                    if (!string.IsNullOrWhiteSpace(s)) identities.Add(s.Trim());
                }
                catch { }
            }
        }

        return entries.Any(e => identities.Contains(e));
    }

    private static void SetProp(Type t, object obj, string name, object value)
    {
        try
        {
            var p = t.GetProperty(name, BindingFlags.Public | BindingFlags.Instance);
            if (p != null && p.CanWrite)
                p.SetValue(obj, value, null);
        }
        catch { }
    }

    // Anchor: just above the name label inside the canvas. Falls back to the
    // nametag's own height offset (per spec), clamped to a sane in-canvas
    // range — the NametagInspectorPatch runtime dump should confirm the scale.
    private static float ResolveBadgeY(object nametag, GameObject canvasGo)
    {
        var label = FindNameLabel(canvasGo);
        if (label != null)
        {
            try { return label.transform.localPosition.y + BadgeGapAboveName; }
            catch { }
        }

        var h = GetMemberValue(nametag, "nametagHeightOffset");
        float f = 0f;
        if (h is float hf) f = hf;
        else if (h is double hd) f = (float)hd;
        else if (h is int hi) f = hi;
        if (f > 0f && f <= 5f)
            return f;
        return 0.5f;
    }

    // Match the name label's font size so the badge looks native.
    private static float ResolveBadgeFontSize(GameObject canvasGo)
    {
        var label = FindNameLabel(canvasGo);
        if (label != null)
        {
            try
            {
                var fs = label.GetType().GetProperty("fontSize", BindingFlags.Public | BindingFlags.Instance);
                if (fs != null)
                {
                    var v = fs.GetValue(label, null);
                    if (v is float f && f > 0f) return f;
                    if (v is int i && i > 0) return i;
                }
            }
            catch { }
        }
        return DefaultBadgeFontSize;
    }

    // First non-empty text label under the canvas (uGUI Text, then TMPro).
    // Skips our own badges so repeated scans stay stable.
    private static Component FindNameLabel(GameObject canvasGo)
    {
        try
        {
            // be.788: GetComponentsInChildren requires Il2CppSystem.Type.
            var textType = Il2CppSystem.Type.GetType(typeof(Text).AssemblyQualifiedName);
            var texts = canvasGo.GetComponentsInChildren(textType, true);
            if (texts != null)
            {
                foreach (var c in texts)
                {
                    // Il2Cpp downcasts must go through TryCast, never a direct cast.
                    var t = c.TryCast<Text>();
                    if (t != null && !string.IsNullOrWhiteSpace(t.text) && t.text != "DEV" && t.text != "MOD")
                        return t;
                }
            }

            // TMPro fallback (no compile-time dependency).
            var tmproType = TmproIl2CppType();
            if (tmproType != null)
            {
                var comps = canvasGo.GetComponentsInChildren(tmproType, true);
                if (comps != null)
                {
                    foreach (var c in comps)
                    {
                        var txt = c.GetType()
                            .GetProperty("text", BindingFlags.Public | BindingFlags.Instance)
                            ?.GetValue(c, null) as string;
                        if (!string.IsNullOrWhiteSpace(txt) && txt != "DEV" && txt != "MOD")
                            return c;
                    }
                }
            }
        }
        catch { }
        return null;
    }

    // Per-player developer check. Tries thisPlayer -> IsDeveloper, then
    // thisPlayer -> Account -> IsDeveloper, then playerNametagModel ->
    // IsDeveloper. Fail-closed: no badge unless dev status is confirmed.
    private static bool IsDeveloperNametag(object nametag)
    {
        try
        {
            var player = GetMemberValue(nametag, "thisPlayer");
            if (player != null)
            {
                var dev = ResolveBoolMember(player, "IsDeveloper", "isDeveloper");
                if (dev.HasValue) return dev.Value;

                var acct = GetMemberValue(player, "Account")
                    ?? GetMemberValue(player, "account")
                    ?? GetMemberValue(player, "PlayerAccount");
                if (acct != null)
                {
                    var adev = ResolveBoolMember(acct, "IsDeveloper", "isDeveloper");
                    if (adev.HasValue) return adev.Value;
                }
            }

            var model = GetMemberValue(nametag, "playerNametagModel")
                ?? GetMemberValue(nametag, "PlayerNametagModel");
            if (model != null)
            {
                var mdev = ResolveBoolMember(model, "IsDeveloper", "isDeveloper");
                if (mdev.HasValue) return mdev.Value;
            }

            // BADGES-02: backend authoritative source (auth worker role
            // filters). On cache miss this kicks off a batched fetch;
            // RefreshBadge re-runs this check on the main thread when the
            // data lands.
            if (RolesHelper.TryGetAccountId(nametag, out var devAccountId))
            {
                var cached = RolesHelper.GetCachedBackendRole(devAccountId);
                if (cached.HasValue)
                    return cached.Value.HasFlag(PlayerRole.Developer);
                RolesHelper.EnsureBackendFetch(nametag, devAccountId);
            }
        }
        catch { }
        return false;
    }

    // Reads a bool from a property, field, or explicit get_ method, trying
    // each candidate name in order. Returns null when nothing resolves.
    private static bool? ResolveBoolMember(object obj, params string[] names)
    {
        foreach (var name in names)
        {
            try
            {
                var t = obj.GetType();
                var p = t.GetProperty(name, BindingFlags.Public | BindingFlags.NonPublic | BindingFlags.Instance);
                if (p != null && p.PropertyType == typeof(bool) && p.GetGetMethod(true) != null)
                    return (bool)p.GetValue(obj, null);
                var f = t.GetField(name, BindingFlags.Public | BindingFlags.NonPublic | BindingFlags.Instance);
                if (f != null && f.FieldType == typeof(bool))
                    return (bool)f.GetValue(obj);
                var m = t.GetMethod("get_" + name, BindingFlags.Public | BindingFlags.NonPublic | BindingFlags.Instance);
                if (m != null && m.ReturnType == typeof(bool) && m.GetParameters().Length == 0)
                    return (bool)m.Invoke(obj, null);
            }
            catch { }
        }
        return null;
    }

    private static bool GetIsNameTagVisible(object nametag, bool current)
    {
        try
        {
            var v = ResolveBoolMember(nametag, "IsNameTagVisible", "IsNametagVisible");
            if (v.HasValue) return v.Value;
        }
        catch { }
        return current; // fail-open: keep current state when unreadable
    }

    private static object GetMemberValue(object obj, string name)
    {
        if (obj == null) return null;
        try
        {
            var t = obj.GetType();
            var f = t.GetField(name, BindingFlags.Public | BindingFlags.NonPublic | BindingFlags.Instance);
            if (f != null) return f.GetValue(obj);
            var p = t.GetProperty(name, BindingFlags.Public | BindingFlags.NonPublic | BindingFlags.Instance);
            if (p != null && p.GetGetMethod(true) != null) return p.GetValue(obj, null);
        }
        catch { }
        return null;
    }

    private static GameObject ToGameObject(object val)
    {
        if (val == null) return null;
        if (val is GameObject go) return go;
        if (val is Transform tr) return tr.gameObject;
        if (val is Component c) return c.gameObject;
        // Last resort: IL2CPP proxies expose gameObject.
        try
        {
            var p = val.GetType().GetProperty("gameObject", BindingFlags.Public | BindingFlags.Instance);
            return p?.GetValue(val, null) as GameObject;
        }
        catch { return null; }
    }

    // TMPro.TextMeshProUGUI as Il2CppSystem.Type, cached. Runtime-resolved so
    // there is no compile-time TMPro dependency in this file's logic.
    private static Il2CppSystem.Type TmproIl2CppType()
    {
        if (_tmproIl2CppType != null) return _tmproIl2CppType;
        try
        {
            var asm = AppDomain.CurrentDomain.GetAssemblies()
                .FirstOrDefault(a => a.GetName().Name == "Unity.TextMeshPro");
            var sysType = asm?.GetType("TMPro.TextMeshProUGUI");
            if (sysType != null)
                _tmproIl2CppType = Il2CppSystem.Type.GetType(sysType.AssemblyQualifiedName);
        }
        catch { }
        return _tmproIl2CppType;
    }
}
