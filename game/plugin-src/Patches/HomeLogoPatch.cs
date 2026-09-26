// Inserts a "FLUX REC" logo above the home screen tab row.
//
// Why this shape:
//  - The 2023 client's home screen is the RRUI Watch home tab, built 100%
//    at runtime by AGUI.StackedUI.HomeScreenFlow (no prefabs): a
//    HomeTop5TabsModel row of 5 tab buttons plus a data-driven content feed.
//  - The plugin resolves the unobfuscated AGUI.StackedUI.HomeScreenFlow /
//    HomeTop5TabsModel type names at runtime, finds the live GameObject,
//    and inserts a new child (a UnityEngine.UI.Image) directly above the
//    tab-row container: SetSiblingIndex(tabRowIndex) when the row is found,
//    otherwise sibling index 0 (first child).
//  - The logo texture is generated procedurally (5x7 pixel wordmark
//    "FLUX REC" + teal underline accent) so no asset bundle, PNG file, or
//    addressable ship is needed. Swapping in a real logo PNG later is a
//    one-line change: return the loaded Texture2D from BuildLogoTexture().
//  - The plugin forces the new RRUI Watch UI on (WatchUIPatch), so the RRUI
//    home screen is the correct target; the legacy home screen is ignored.
//
// Hard rules honored (see UltraGraphicsPatch / NametagBadgePatch):
//  - Resolve by unobfuscated type name, never by obfuscated member names.
//  - TryCast for downcasts; FindObjectsOfType via the proven reflection
//    pattern; generic GetComponent<T>/AddComponent<T> only for Unity
//    built-in types (Image, RectTransform, LayoutElement).
//  - Idempotent: a child named "FluxHomeLogo" is never inserted twice.
//  - Fail-soft: every step is wrapped, retries stop after MaxAttempts.
//  - The logo is raycast-transparent (raycastTarget = false) so it never
//    eats clicks meant for the tab row.
//
// One knob, see [Home] in the .cfg:
//   Enable Flux Home Branding -> THE FEATURE (default true).
using System;
using System.Collections.Generic;
using System.Linq;
using System.Reflection;
using UnityEngine;
using UnityEngine.UI;

namespace RecNetPlugin.Patches;

internal static class HomeLogoPatch
{
    private const string LogoObjectName = "FluxHomeLogo";
    private const int MaxAttempts = 10;

    // Procedural logo texture dimensions.
    private const int TexW = 512;
    private const int TexH = 160;
    private const int GlyphScale = 10;

    private static int _attempts;
    private static bool _done;

    // Called from Plugin.Load and again on each scene load: retries until
    // the RRUI home screen exists, then inserts the logo once.
    public static void Apply()
    {
        if (!Plugin.EnableFluxHomeBranding.Value)
            return;

        if (_done || _attempts >= MaxAttempts)
            return;

        _attempts++;
        try { EnsureLogo(); }
        catch (Exception e)
        {
            Plugin.Log.LogWarning($"[HOMELOGO] attempt {_attempts} failed: {e.Message}");
        }

        if (_attempts >= MaxAttempts && !_done)
            Plugin.Log.LogWarning("[HOMELOGO] gave up — the RRUI home screen was never found.");
    }

    private static void EnsureLogo()
    {
        var root = FindHomeRoot(out var tabRow);
        if (root == null)
        {
            Plugin.Log.LogDebug("[HOMELOGO] home screen not found yet");
            return;
        }

        // Idempotent: never insert twice (scene reloads rebuild the UI).
        var existing = root.transform.Find(LogoObjectName);
        if (existing != null)
        {
            _done = true;
            return;
        }

        var logo = BuildLogo();
        logo.transform.SetParent(root.transform, false);

        // Directly above the tab row when we found it, else first child.
        int index = 0;
        if (tabRow != null)
            index = Math.Max(0, tabRow.transform.GetSiblingIndex());
        logo.transform.SetSiblingIndex(index);

        _done = true;
        Plugin.Log.LogInfo($"[HOMELOGO] inserted Flux logo above tab row (sibling index {index})");
    }

    // Builds the logo GameObject: a UnityEngine.UI.Image with a generated
    // "FLUX REC" texture. Adding the first UI component converts the plain
    // Transform into a RectTransform automatically.
    private static GameObject BuildLogo()
    {
        var go = new GameObject(LogoObjectName);

        var img = go.AddComponent<Image>(); // auto-adds CanvasRenderer
        var rt = go.GetComponent<RectTransform>();

        var tex = BuildLogoTexture();
        img.sprite = Sprite.Create(tex,
            new Rect(0, 0, tex.width, tex.height), new Vector2(0.5f, 0.5f));
        img.preserveAspect = true;
        img.raycastTarget = false;

        // Top-stretched anchor for plain canvases; the LayoutElement covers
        // parents driven by a VerticalLayoutGroup (anchoredPosition is then
        // ignored and height comes from preferredHeight).
        rt.anchorMin = new Vector2(0f, 1f);
        rt.anchorMax = new Vector2(1f, 1f);
        rt.pivot = new Vector2(0.5f, 1f);
        rt.sizeDelta = new Vector2(0f, 110f);
        rt.anchoredPosition = new Vector2(0f, -10f);

        var le = go.AddComponent<LayoutElement>();
        le.preferredHeight = 110f;
        le.flexibleWidth = 1f;

        return go;
    }

    // --- Home screen discovery (unobfuscated type names only) ---

    private static GameObject FindHomeRoot(out GameObject tabRow)
    {
        tabRow = null;

        // Path 1: the home screen impl type itself.
        var implType = FindTypeByName("RRUIHomeScreenImpl")
                    ?? FindTypeByName("RRUIHomeScreen");
        if (implType != null)
        {
            var go = FirstGameObjectOfType(implType);
            if (go != null)
            {
                tabRow = FindTabRow(go);
                return go;
            }
        }

        // Path 2: the tab row type directly — its parent is the home root.
        var tabsType = FindTypeByName("HomeTop5TabsModel");
        if (tabsType != null)
        {
            var go = FirstGameObjectOfType(tabsType);
            if (go != null)
            {
                tabRow = go;
                var parent = go.transform.parent;
                return parent != null ? parent.gameObject : go;
            }
        }

        // Path 3: plain GameObject name lookup (active objects only).
        foreach (var name in new[] { "RRUIHomeScreen", "HomeScreen" })
        {
            var go = GameObject.Find(name);
            if (go != null)
            {
                tabRow = FindTabRow(go);
                return go;
            }
        }

        return null;
    }

    private static Type FindTypeByName(string name)
    {
        foreach (var asm in AppDomain.CurrentDomain.GetAssemblies())
        {
            Type[] types;
            try { types = asm.GetTypes(); }
            catch { continue; }
            foreach (var t in types)
            {
                if (t == null)
                    continue;
                if (t.Name == name)
                    return t;
            }
        }
        return null;
    }

    // Proven UltraGraphicsPatch pattern: FindObjectsOfType(Type) via
    // reflection, downcast with TryCast (never a direct cast).
    private static GameObject FirstGameObjectOfType(Type type)
    {
        var find = typeof(UnityEngine.Object).GetMethod("FindObjectsOfType",
            new[] { typeof(Type) });
        if (find == null)
            return null;

        var all = find.Invoke(null, new object[] { type })
            as System.Collections.IEnumerable;
        if (all == null)
            return null;

        foreach (var o in all)
        {
            var comp = ((UnityEngine.Object)o).TryCast<Component>();
            var go = comp != null ? comp.gameObject : null;
            if (go != null)
                return go;
        }
        return null;
    }

    private static GameObject FindTabRow(GameObject root)
    {
        var t = FindChildRecursive(root.transform, tr =>
        {
            var n = tr.name ?? "";
            return n.IndexOf("top5tabs", StringComparison.OrdinalIgnoreCase) >= 0
                || n.IndexOf("tabsmodel", StringComparison.OrdinalIgnoreCase) >= 0
                || n.IndexOf("tabrow", StringComparison.OrdinalIgnoreCase) >= 0
                || n.IndexOf("watchtabs", StringComparison.OrdinalIgnoreCase) >= 0;
        });
        return t != null ? t.gameObject : null;
    }

    private static Transform FindChildRecursive(Transform parent, Func<Transform, bool> match)
    {
        for (int i = 0; i < parent.childCount; i++)
        {
            var child = parent.GetChild(i);
            if (match(child))
                return child;
            var deep = FindChildRecursive(child, match);
            if (deep != null)
                return deep;
        }
        return null;
    }

    // --- Procedural logo texture ("FLUX REC" pixel wordmark) ---

    // 5x7 pixel font, rows top-to-bottom, '#' = filled.
    private static readonly Dictionary<char, string[]> Font = new Dictionary<char, string[]>
    {
        { 'F', new[] { "#####", "#....", "#....", "####.", "#....", "#....", "#...." } },
        { 'L', new[] { "#....", "#....", "#....", "#....", "#....", "#....", "#####" } },
        { 'U', new[] { "#...#", "#...#", "#...#", "#...#", "#...#", "#...#", ".###." } },
        { 'X', new[] { "#...#", "#...#", ".#.#.", "..#..", ".#.#.", "#...#", "#...#" } },
        { 'R', new[] { "####.", "#...#", "#...#", "####.", "#.#..", "#..#.", "#...#" } },
        { 'E', new[] { "#####", "#....", "#....", "####.", "#....", "#....", "#####" } },
        { 'C', new[] { ".####", "#....", "#....", "#....", "#....", "#....", ".####" } },
        { ' ', new[] { ".....", ".....", ".....", ".....", ".....", ".....", "....." } },
    };

    private static Texture2D BuildLogoTexture()
    {
        var tex = new Texture2D(TexW, TexH);
        tex.filterMode = FilterMode.Point; // crisp pixel edges

        var clear = new Color(0f, 0f, 0f, 0f);
        for (int y = 0; y < TexH; y++)
            for (int x = 0; x < TexW; x++)
                tex.SetPixel(x, y, clear);

        const string word = "FLUX REC";
        int advance = 6 * GlyphScale;
        int textW = word.Length * advance - GlyphScale;
        int ox = (TexW - textW) / 2;
        int oy = 52; // text bottom; leaves room for the underline below

        // Drop shadow first, then the white wordmark on top.
        DrawWord(tex, word, ox + 4, oy - 4, new Color(0f, 0f, 0f, 0.45f));
        DrawWord(tex, word, ox, oy, new Color(1f, 1f, 1f, 1f));

        // Teal underline accent.
        var accent = new Color(0.18f, 0.78f, 0.92f, 1f);
        for (int y = oy - 22; y < oy - 14; y++)
            for (int x = ox; x < ox + textW; x++)
                tex.SetPixel(x, y, accent);

        tex.Apply();
        return tex;
    }

    private static void DrawWord(Texture2D tex, string word, int ox, int oy, Color color)
    {
        int advance = 6 * GlyphScale;
        for (int gi = 0; gi < word.Length; gi++)
        {
            if (!Font.TryGetValue(word[gi], out var glyph))
                continue;
            int gx = ox + gi * advance;
            for (int r = 0; r < 7; r++)
            {
                var row = glyph[r];
                for (int c = 0; c < 5; c++)
                {
                    if (row[c] != '#')
                        continue;
                    int px = gx + c * GlyphScale;
                    int py = oy + (6 - r) * GlyphScale;
                    for (int dy = 0; dy < GlyphScale; dy++)
                        for (int dx = 0; dx < GlyphScale; dx++)
                            tex.SetPixel(px + dx, py + dy, color);
                }
            }
        }
    }
}
