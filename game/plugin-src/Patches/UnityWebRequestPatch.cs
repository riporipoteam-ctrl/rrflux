using System;
using HarmonyLib;
using UnityEngine.Networking;

namespace RecNetPlugin.Patches;

/// <summary>
/// Logs every UnityWebRequest to STOREBUGLOG.txt. The Store page might use
/// UnityWebRequest (not BestHTTP) for image/data loads — those are invisible
/// to SendRequestPatch. This catches them.
/// </summary>
public class UnityWebRequestPatch
{
    [HarmonyPatch(typeof(UnityWebRequest), nameof(UnityWebRequest.SendWebRequest))]
    public class LogSendWebRequest
    {
        public static void Prefix(UnityWebRequest __instance)
        {
            try
            {
                var url = __instance?.url ?? "<null>";
                // Log all requests, but mark storefront/image ones clearly.
                var tag = "UWR";
                if (url.Contains("storefront", StringComparison.OrdinalIgnoreCase))
                    tag = "UWR-STOREFRONT";
                else if (url.Contains(".png", StringComparison.OrdinalIgnoreCase) ||
                         url.Contains(".jpg", StringComparison.OrdinalIgnoreCase) ||
                         url.Contains("img.", StringComparison.OrdinalIgnoreCase))
                    tag = "UWR-IMAGE";
                Plugin.StoreBugLog($"{tag} {__instance.method} {url}");
            }
            catch { }
        }
    }
}
