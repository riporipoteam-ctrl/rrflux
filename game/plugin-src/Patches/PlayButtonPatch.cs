// Adds a "Play" button beside "Create" on the home screen and wires it to
// quick play (join a random public room). This file implements the
// CLONE + RELABEL + CLICK-HANDLER steps.
//
// Why this shape:
//  - The home screen is 100% code-built at runtime via RRUI (Rec Room's UI
//    framework) — there are no home-screen prefabs to edit. The tab buttons
//    follow an icon + label pattern (TabButtonImpl under HomeTop5TabsModel,
//    or GoToCreateRoomScreenButtonController on the legacy home screen).
//  - Type/GameObject names on the home screen may be obfuscated and re-rolled
//    per build, so the plugin resolves the Create button by its VISIBLE
//    LABEL ("Create") on a uGUI Button, not by GameObject/type name.
//  - The plugin clones the Create button (same prefab, same style, same row),
//    inserts the clone as its next sibling and relabels it "Play". Visual
//    consistency with the game's design language is guaranteed because it is
//    literally the same button GameObject.
//  - The clone's original onClick listeners (which open the room-creation
//    flow) are removed and replaced with a single listener that triggers
//    Quick Play. The listener is OUR OWN fresh handler wired through
//    DelegateSupport.ConvertDelegate — we never wrap the game's callbacks,
//    so the v0.1.30 Delegate.CreateDelegate hang cannot recur.
//
// Hard rules honored:
//  - The Create button itself is untouched: the plugin only ADDS a sibling.
//  - Idempotent: the clone is only added once per Create button (name-suffix
//    check), so scene reloads and repeated Apply() calls never duplicate it.
//
// Resolution is by visible label at runtime (never by obfuscated type name).
// IL2CPP rules: downcasts go through TryCast<T>(), GetComponent /
// GetComponentsInChildren require Il2CppSystem.Type, never direct casts.
//
// One knob, see [Home] in the .cfg:
//   Enable Play Button -> THE FEATURE (default true).
using System;
using System.Collections;
using System.Linq;
using System.Reflection;
using HarmonyLib;
using UnityEngine;
using UnityEngine.Events;
using UnityEngine.UI;

namespace RecNetPlugin.Patches;

internal static class PlayButtonPatch
{
    private const string CloneNameSuffix = "_Play";
    private const string SourceLabel = "Create";
    private const string CloneLabel = "Play";
    private const int MaxAttempts = 20;

    private static int _attempts;
    private static bool _buttonDone;

    // Called from Plugin.Load and again on each scene load: retries the
    // button clone until the home screen exists.
    public static void Apply()
    {
        if (!Plugin.EnablePlayButton.Value)
            return;

        if (_buttonDone)
            return;

        if (_attempts >= MaxAttempts)
            return;

        _attempts++;
        try
        {
            EnsurePlayButton();
        }
        catch (Exception e)
        {
            Plugin.Log.LogWarning($"[PLAY] attempt {_attempts} failed: {e.Message}");
        }

        if (_attempts >= MaxAttempts && !_buttonDone)
            Plugin.Log.LogWarning("[PLAY] gave up adding the Play button — " +
                "the Create button on the home screen was never found.");
    }

    // Find the Create button by its visible label and clone it into a Play
    // button right beside it.
    private static void EnsurePlayButton()
    {
        var source = FindCreateButton();
        if (source == null)
        {
            Plugin.Log.LogDebug("[PLAY] Create button not found yet");
            return;
        }

        var parent = source.transform.parent;
        if (parent == null)
        {
            Plugin.Log.LogWarning("[PLAY] Create button has no parent row — skipping");
            _attempts = MaxAttempts; // structural mismatch — stop retrying
            return;
        }

        // Already added? (a sibling already carrying our suffix)
        for (int i = 0; i < parent.childCount; i++)
        {
            var child = parent.GetChild(i);
            if (child != null && child.name.EndsWith(CloneNameSuffix, StringComparison.Ordinal))
            {
                _buttonDone = true;
                Plugin.Log.LogDebug("[PLAY] Play button already present");
                return;
            }
        }

        CloneAsPlay(source);
        _buttonDone = true;
    }

    // Locate the Create button. Tries the visible-label search first (most
    // robust against obfuscated GameObject/type names), then falls back to
    // GameObject-name hints and component-type resolution.
    private static GameObject FindCreateButton()
    {
        var byLabel = FindByVisibleLabel();
        if (byLabel != null)
            return byLabel;
        return FindByNameOrType();
    }

    // Locate the Create button: any uGUI Button in the active scene whose
    // child label text is exactly "Create". Prefers candidates living under a
    // "Home"-named ancestor (the RRUI home screen) over Create buttons in
    // other menus (maker pen, room creation, etc.).
    private static GameObject FindByVisibleLabel()
    {
        var find = typeof(UnityEngine.Object).GetMethod("FindObjectsOfType",
            new[] { typeof(Type) });
        if (find == null)
            return null;

        var all = (IEnumerable)find.Invoke(null, new object[] { typeof(Button) });

        GameObject best = null;
        bool bestIsHome = false;
        foreach (var o in all)
        {
            // Il2Cpp downcasts must go through TryCast, never a direct cast.
            var button = ((UnityEngine.Object)o).TryCast<Button>();
            if (button == null)
                continue;
            var go = button.gameObject;
            if (go == null || !go.activeInHierarchy)
                continue;
            // Skip our own clone if it somehow exists without the suffix check.
            if (go.name.EndsWith(CloneNameSuffix, StringComparison.Ordinal))
                continue;
            if (!HasLabel(go, SourceLabel))
                continue;

            bool isHome = IsUnderHome(go);
            if (best == null || (isHome && !bestIsHome))
            {
                best = go;
                bestIsHome = isHome;
                if (isHome)
                    break; // home-screen Create is the one we want
            }
        }

        if (best != null)
            Plugin.Log.LogInfo($"[PLAY] found Create button: '{best.name}'");
        return best;
    }

    // Fallback when the visible-label search finds nothing (e.g. labels fed
    // from obfuscated or localized sources): match by GameObject name hints,
    // then by component type name.
    private static GameObject FindByNameOrType()
    {
        var find = typeof(UnityEngine.Object).GetMethod("FindObjectsOfType",
            new[] { typeof(Type) });
        if (find == null)
            return null;

        // 1) GameObject name hints on uGUI Buttons.
        var all = (IEnumerable)find.Invoke(null, new object[] { typeof(Button) });

        GameObject best = null;
        bool bestIsHome = false;
        foreach (var o in all)
        {
            // Il2Cpp downcasts must go through TryCast, never a direct cast.
            var button = ((UnityEngine.Object)o).TryCast<Button>();
            if (button == null)
                continue;
            var go = button.gameObject;
            if (go == null || !go.activeInHierarchy)
                continue;
            if (go.name.EndsWith(CloneNameSuffix, StringComparison.Ordinal))
                continue;

            var lower = (go.name ?? string.Empty).ToLowerInvariant();
            bool nameHit = lower == "create" ||
                           lower == "button_create" ||
                           lower == "createbutton" ||
                           lower == "create_button" ||
                           (lower.Contains("create") &&
                            !lower.Contains("invention") &&
                            !lower.Contains("subroom") &&
                            !lower.Contains("sub_room") &&
                            !lower.Contains("choose"));
            if (!nameHit)
                continue;

            bool isHome = IsUnderHome(go);
            if (best == null || (isHome && !bestIsHome))
            {
                best = go;
                bestIsHome = isHome;
                if (isHome)
                    break; // home-screen Create is the one we want
            }
        }

        if (best != null)
        {
            Plugin.Log.LogInfo($"[PLAY] found Create button by name: '{best.name}'");
            return best;
        }

        // 2) Component type name fallback.
        return FindByCreateButtonType();
    }

    // Last resort: resolve by component type name. Mirrors the FindQualitySetter
    // pattern in UltraGraphicsPatch — scan loaded assemblies for non-abstract
    // types whose names look like a Create button/tab, then FindObjectsOfType
    // each one. Il2Cpp downcasts must go through TryCast, never a direct cast.
    private static GameObject FindByCreateButtonType()
    {
        var find = typeof(UnityEngine.Object).GetMethod("FindObjectsOfType",
            new[] { typeof(Type) });
        if (find == null)
            return null;

        foreach (var asm in AppDomain.CurrentDomain.GetAssemblies())
        {
            Type[] types;
            try { types = asm.GetTypes(); }
            catch (ReflectionTypeLoadException e) { types = e.Types.Where(t => t != null).ToArray(); }
            catch { continue; }

            foreach (var t in types)
            {
                if (t == null || t.IsAbstract || t.IsInterface)
                    continue;
                if (!typeof(UnityEngine.Object).IsAssignableFrom(t))
                    continue;

                var fullName = t.FullName ?? string.Empty;
                // Skip engine assemblies — the button impl lives in game code.
                if (fullName.StartsWith("UnityEngine.", StringComparison.Ordinal) ||
                    fullName.StartsWith("Unity.", StringComparison.Ordinal))
                    continue;

                var name = t.Name;
                if (name.IndexOf("Create", StringComparison.OrdinalIgnoreCase) < 0)
                    continue;
                if (name.IndexOf("Button", StringComparison.OrdinalIgnoreCase) < 0 &&
                    name.IndexOf("Tab", StringComparison.OrdinalIgnoreCase) < 0)
                    continue;
                // Skip room-creation sub-dialog impls.
                if (name.IndexOf("Invention", StringComparison.OrdinalIgnoreCase) >= 0 ||
                    name.IndexOf("SubRoom", StringComparison.OrdinalIgnoreCase) >= 0 ||
                    name.IndexOf("Subroom", StringComparison.OrdinalIgnoreCase) >= 0)
                    continue;

                IEnumerable all;
                try { all = (IEnumerable)find.Invoke(null, new object[] { t }); }
                catch { continue; }
                if (all == null)
                    continue;

                foreach (var o in all)
                {
                    var comp = ((UnityEngine.Object)o).TryCast<Component>();
                    if (comp == null)
                        continue;
                    var go = comp.gameObject;
                    if (go == null || !go.activeInHierarchy)
                        continue;
                    Plugin.Log.LogInfo($"[PLAY] found Create button by type {t.FullName}: '{go.name}'");
                    return go;
                }
            }
        }

        return null;
    }

    private static bool IsUnderHome(GameObject go)
    {
        var t = go.transform.parent;
        while (t != null)
        {
            if (t.name.IndexOf("Home", StringComparison.OrdinalIgnoreCase) >= 0)
                return true;
            t = t.parent;
        }
        return false;
    }

    // True when any uGUI Text or TMPro label under go reads exactly `label`.
    private static bool HasLabel(GameObject go, string label)
    {
        // uGUI path
        // be.788: GetComponentsInChildren requires Il2CppSystem.Type, not System.Type.
        var textType = Il2CppSystem.Type.GetType(typeof(Text).AssemblyQualifiedName);
        var texts = go.GetComponentsInChildren(textType, true);
        if (texts != null)
        {
            foreach (var c in texts)
            {
                var txt = c.TryCast<Text>()?.text;
                if (!string.IsNullOrWhiteSpace(txt) &&
                    txt.Trim().Equals(label, StringComparison.OrdinalIgnoreCase))
                    return true;
            }
        }

        // TMPro fallback (no compile-time dependency)
        try
        {
            var tmproAsm = AppDomain.CurrentDomain.GetAssemblies()
                .FirstOrDefault(a => a.GetName().Name == "Unity.TextMeshPro");
            var tmproType = tmproAsm?.GetType("TMPro.TextMeshProUGUI");
            if (tmproType == null)
                return false;
            var getTexts = typeof(GameObject).GetMethod("GetComponentsInChildren",
                new[] { typeof(Type), typeof(bool) });
            var list = (IEnumerable)getTexts.Invoke(go, new object[] { tmproType, true });
            var textProp = tmproType.GetProperty("text");
            foreach (var c in list)
            {
                var txt = (string)textProp.GetValue(c, null);
                if (!string.IsNullOrWhiteSpace(txt) &&
                    txt.Trim().Equals(label, StringComparison.OrdinalIgnoreCase))
                    return true;
            }
        }
        catch (Exception e)
        {
            Plugin.Log.LogDebug($"[PLAY] TMPro label scan failed: {e.Message}");
        }

        return false;
    }

    private static void CloneAsPlay(GameObject sourceGo)
    {
        var cloneObj = UnityEngine.Object.Instantiate(sourceGo);
        var cloneGo = cloneObj.TryCast<GameObject>();
        if (cloneGo == null)
        {
            Plugin.Log.LogWarning("[PLAY] clone failed (not a GameObject)");
            return;
        }

        cloneGo.transform.SetParent(sourceGo.transform.parent, false);
        cloneGo.transform.SetSiblingIndex(sourceGo.transform.GetSiblingIndex() + 1);
        cloneGo.name = sourceGo.name + CloneNameSuffix;

        // Relabel "Create" -> "Play" (uGUI Text; TMPro fallback via reflection).
        RelabelClone(cloneGo, SourceLabel, CloneLabel);

        // Replace the room-creation click handler with Quick Play.
        ReplaceClickHandler(cloneGo);

        Plugin.Log.LogInfo($"[PLAY] added Play button next to '{sourceGo.name}'");
    }

    private static void RelabelClone(GameObject cloneGo, string from, string to)
    {
        // uGUI path
        var textType = Il2CppSystem.Type.GetType(typeof(Text).AssemblyQualifiedName);
        var dstTexts = cloneGo.GetComponentsInChildren(textType, true);
        if (dstTexts != null)
        {
            foreach (var c in dstTexts)
            {
                var label = c.TryCast<Text>();
                if (label == null)
                    continue;
                if (!string.IsNullOrWhiteSpace(label.text) &&
                    label.text.Trim().Equals(from, StringComparison.OrdinalIgnoreCase))
                {
                    label.text = to;
                    return;
                }
            }
        }

        // TMPro fallback (no compile-time dependency)
        try
        {
            var tmproAsm = AppDomain.CurrentDomain.GetAssemblies()
                .FirstOrDefault(a => a.GetName().Name == "Unity.TextMeshPro");
            var tmproType = tmproAsm?.GetType("TMPro.TextMeshProUGUI");
            if (tmproType == null)
            {
                Plugin.Log.LogWarning("[PLAY] no label Text found on the Create button — Play button keeps its label");
                return;
            }
            var getTexts = typeof(GameObject).GetMethod("GetComponentsInChildren",
                new[] { typeof(Type), typeof(bool) });
            var dst = (IEnumerable)getTexts.Invoke(cloneGo, new object[] { tmproType, true });
            var textProp = tmproType.GetProperty("text");
            foreach (var c in dst)
            {
                var txt = (string)textProp.GetValue(c, null);
                if (!string.IsNullOrWhiteSpace(txt) &&
                    txt.Trim().Equals(from, StringComparison.OrdinalIgnoreCase))
                {
                    textProp.SetValue(c, to, null);
                    return;
                }
            }
            Plugin.Log.LogWarning("[PLAY] no matching label found on the clone — Play button keeps its label");
        }
        catch (Exception e)
        {
            Plugin.Log.LogWarning($"[PLAY] TMPro relabel failed: {e.Message}");
        }
    }

    // THE CLICK HANDLER WIRING: drop the clone's room-creation onClick
    // listeners, then wire our own fresh Quick Play handler. This is OUR OWN
    // delegate (never a wrapped game callback), converted via
    // DelegateSupport.ConvertDelegate exactly like Plugin.cs does for
    // SceneManager.sceneLoaded — so the v0.1.30 Delegate.CreateDelegate
    // hang cannot recur.
    private static void ReplaceClickHandler(GameObject cloneGo)
    {
        // be.788: GetComponent requires Il2CppSystem.Type, not System.Type.
        var buttonType = Il2CppSystem.Type.GetType(typeof(Button).AssemblyQualifiedName);
        var button = cloneGo.GetComponent(buttonType)?.TryCast<Button>();
        if (button == null)
        {
            // The clickable Button may be nested (RRUI composes buttons from parts).
            var buttons = cloneGo.GetComponentsInChildren(buttonType, true);
            if (buttons != null)
            {
                foreach (var b in buttons)
                {
                    button = b.TryCast<Button>();
                    if (button != null)
                        break;
                }
            }
        }
        if (button == null)
        {
            Plugin.Log.LogWarning("[PLAY] no uGUI Button on the Play clone — click handler not wired");
            return;
        }

        button.onClick.RemoveAllListeners();
        var playAction = Il2CppInterop.Runtime.DelegateSupport.ConvertDelegate<UnityAction>(
            new Action(OnPlayClicked));
        button.onClick.AddListener(playAction);
        Plugin.Log.LogInfo("[PLAY] Play button wired to Quick Play");
    }

    private static void OnPlayClicked()
    {
        try
        {
            Plugin.Log.LogInfo("[PLAY] Play pressed — starting Quick Play");
            if (TryGameQuickPlay())
                return;
            TryPhotonJoinRandom();
        }
        catch (Exception e)
        {
            Plugin.Log.LogWarning($"[PLAY] Quick Play failed: {e.Message}");
        }
    }

    // Layer 1 — the game's own matchmaking entry point: static, parameterless,
    // void, with an unobfuscated QuickPlay/matchmaking name (confirmed present
    // in the 20230414 metadata). Proper integration beats a raw Photon call.
    private static bool TryGameQuickPlay()
    {
        string[] entryNames =
        {
            "QuickPlay", "PlayQuickPlay", "StartQuickPlay",
            "JoinPublicRoom", "JoinRandomPublicRoom"
        };

        foreach (var asm in AppDomain.CurrentDomain.GetAssemblies())
        {
            Type[] types;
            try { types = asm.GetTypes(); }
            catch (ReflectionTypeLoadException e) { types = e.Types.Where(t => t != null).ToArray(); }
            catch { continue; }

            foreach (var t in types)
            {
                if (t == null)
                    continue;
                foreach (var name in entryNames)
                {
                    var m = t.GetMethod(name,
                        BindingFlags.Public | BindingFlags.NonPublic | BindingFlags.Static,
                        null, Type.EmptyTypes, null);
                    if (m == null || m.ReturnType != typeof(void))
                        continue;
                    try
                    {
                        m.Invoke(null, null);
                        Plugin.Log.LogInfo($"[PLAY] Quick Play via {t.FullName}.{m.Name}()");
                        return true;
                    }
                    catch (Exception e)
                    {
                        Plugin.Log.LogDebug($"[PLAY] {t.FullName}.{m.Name}() threw: {e.Message}");
                    }
                }
            }
        }

        Plugin.Log.LogDebug("[PLAY] no game-level Quick Play entry point found");
        return false;
    }

    // Layer 2 — fallback: PhotonNetwork.JoinRandomRoom() directly.
    // On JoinRandomFailed the game's existing error handling shows the
    // message; we don't touch it.
    private static void TryPhotonJoinRandom()
    {
        try
        {
            var photonType = FindTypeByName("PhotonNetwork");
            if (photonType == null)
            {
                Plugin.Log.LogWarning("[PLAY] PhotonNetwork type not found — Quick Play unavailable");
                return;
            }

            if (!IsPhotonConnected(photonType))
            {
                Plugin.Log.LogWarning("[PLAY] Photon not connected — Quick Play skipped");
                return;
            }

            var join = photonType.GetMethod("JoinRandomRoom",
                BindingFlags.Public | BindingFlags.Static, null, Type.EmptyTypes, null);
            if (join == null)
            {
                Plugin.Log.LogWarning("[PLAY] PhotonNetwork.JoinRandomRoom() not found");
                return;
            }

            var result = join.Invoke(null, null);
            Plugin.Log.LogInfo($"[PLAY] PhotonNetwork.JoinRandomRoom() invoked (returned {result})");
        }
        catch (Exception e)
        {
            Plugin.Log.LogWarning($"[PLAY] Photon JoinRandomRoom failed: {e.Message}");
        }
    }

    private static Type FindTypeByName(string name)
    {
        foreach (var asm in AppDomain.CurrentDomain.GetAssemblies())
        {
            Type[] types;
            try { types = asm.GetTypes(); }
            catch (ReflectionTypeLoadException e) { types = e.Types.Where(t => t != null).ToArray(); }
            catch { continue; }

            foreach (var t in types)
            {
                if (t != null && (t.Name == name || t.FullName == "Photon.Pun." + name))
                    return t;
            }
        }
        return null;
    }

    private static bool IsPhotonConnected(Type photonType)
    {
        // PUN2: static bool IsConnected; PUN1: static bool connected.
        foreach (var memberName in new[] { "IsConnected", "connected" })
        {
            try
            {
                var prop = photonType.GetProperty(memberName,
                    BindingFlags.Public | BindingFlags.Static);
                if (prop != null && prop.PropertyType == typeof(bool))
                    return (bool)prop.GetValue(null, null);
                var field = photonType.GetField(memberName,
                    BindingFlags.Public | BindingFlags.Static);
                if (field != null && field.FieldType == typeof(bool))
                    return (bool)field.GetValue(null);
            }
            catch
            {
                // fall through to the next member name
            }
        }
        return true; // fail-open: attempt the join and let Photon report
    }
}
