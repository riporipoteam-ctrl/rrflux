using HarmonyLib;
using UnityEngine;

namespace RecNetPlugin.Patches;

// Fixes game freezing when minimized during loading.
//
// Root cause: Unity's default behavior pauses the game when it loses focus
// (Application.runInBackground = false). If the user minimizes during the
// loading screen, the load stalls and never recovers.
//
// Fix: Force runInBackground = true on startup so the game continues
// loading/rendering even when minimized or unfocused.
[HarmonyPatch]
public static class RunInBackgroundPatch
{
    public static void Apply(Harmony harmony)
    {
        try
        {
            // Set immediately on patch apply
            Application.runInBackground = true;

            // Also hook into scene loads to re-apply (in case something resets it)
            var sceneLoadedMethod = typeof(RunInBackgroundPatch).GetMethod(
                nameof(OnSceneLoaded),
                System.Reflection.BindingFlags.NonPublic | System.Reflection.BindingFlags.Static);
            // We don't have easy access to SceneManager.sceneLoaded here,
            // so we just set it once. The value persists for the session.
        }
        catch { /* Fail-soft */ }
    }

    // Called periodically to ensure runInBackground stays true
    // (some games reset it on scene changes)
    [HarmonyPatch(typeof(Application), "set_runInBackground")]
    private static class ForceRunInBackground
    {
        [HarmonyPrefix]
        private static bool Prefix(ref bool value)
        {
            // Always force to true, ignore attempts to set false
            value = true;
            return true; // Allow the original setter to run with value=true
        }
    }
}
