using System;
using HarmonyLib;
using UnityEngine.Networking;

namespace RecNetPlugin.Patches;

/// <summary>
/// Rewrites dead Rec Room CDN URLs to our working image host.
/// The client loads Store images from https://rec.net/{ImageName} which has
/// been dead since the shutdown. This redirects those to our img host.
/// </summary>
public class RecNetImageRewritePatch
{
    private const string DeadHost = "https://rec.net/";
    private const string OurHost = "https://img.ripo-ripoteam.workers.dev/";

    [HarmonyPatch(typeof(UnityWebRequest), nameof(UnityWebRequest.Get))]
    public class RewriteGet
    {
        public static void Prefix(ref string uri)
        {
            try
            {
                if (!string.IsNullOrEmpty(uri) && uri.StartsWith(DeadHost, StringComparison.OrdinalIgnoreCase))
                {
                    var newUri = OurHost + uri.Substring(DeadHost.Length);
                    Plugin.StoreBugLog($"REWRITE {uri} -> {newUri}");
                    uri = newUri;
                }
            }
            catch { }
        }
    }

    [HarmonyPatch(typeof(UnityWebRequest))]
    [HarmonyPatch("set_url")]
    public class RewriteSetUrl
    {
        public static void Prefix(ref string value)
        {
            try
            {
                if (!string.IsNullOrEmpty(value) && value.StartsWith(DeadHost, StringComparison.OrdinalIgnoreCase))
                {
                    var newValue = OurHost + value.Substring(DeadHost.Length);
                    Plugin.StoreBugLog($"REWRITE {value} -> {newValue}");
                    value = newValue;
                }
            }
            catch { }
        }
    }
}
