using System;
using System.Collections.Generic;
using System.Linq;
using System.Reflection;
using HarmonyLib;
using UnityEngine;

namespace RecNetPlugin.Patches;

// Fixes chat bubbles not appearing above avatars.
//
// Root cause: The game client does not set PhotonNetwork.AuthValues.UserId,
// so Photon assigns random GUIDs. When ReceiveRoomChat tries to map the
// sender's account ID (from the RPC) to an avatar GameObject, the lookup
// fails because the avatar registry uses a different ID scheme.
//
// Fix: Harmony patch that intercepts incoming chat messages, finds the
// sender's avatar by matching account ID via reflection (using the same
// technique as RolesHelper), and manually invokes RunShowChatMessage.
//
// The patch hooks into the chat message flow by patching methods that
// contain "Chat" in their name and take a message parameter. It uses
// signature-based resolution to survive obfuscation.
public static class ChatBubblePatch
{
    private static readonly string[] ChatMethodKeywords =
        { "ReceiveRoomChat", "ReceiveBroadcastRoomChat", "OnChatMessage", "HandleChatMessage" };

    private static readonly string[] AvatarIdNames =
        { "AccountId", "accountId", "accountID", "PlayerId", "playerId", "UserId", "userId", "Id", "id" };

    private static readonly string[] BubbleMethodNames =
        { "RunShowChatMessage", "ShowChatMessage", "ShowBubble", "DisplayChatBubble" };

    private static readonly string[] BubbleFieldNames =
        { "chatBubble", "ChatBubble", "chatBubbleText", "bubbleText" };

    public static void Apply()
    {
        try
        {
            var harmony = new Harmony("com.fluxrec.chatbubble");
            Apply(harmony);
        }
        catch { /* Fail-soft */ }
    }

    public static void Apply(Harmony harmony)
    {
        try
        {
            // Find the chat receive method by scanning for methods that:
            // 1. Have "Chat" in the name (or "RpcChatMessage")
            // 2. Take a string parameter (the message)
            // 3. Take an int/long parameter (the sender ID)
            var assembly = typeof(Harmony).Assembly; // Start with a known assembly
            // Actually, we need Assembly-CSharp. Find it via the NametagBadgePatch's approach.
            var csharpAssembly = AppDomain.CurrentDomain.GetAssemblies()
                .FirstOrDefault(a => a.GetName().Name == "Assembly-CSharp");
            if (csharpAssembly == null) return;

            foreach (var type in csharpAssembly.GetTypes())
            {
                foreach (var method in type.GetMethods(BindingFlags.Public | BindingFlags.NonPublic | BindingFlags.Instance | BindingFlags.Static))
                {
                    var name = method.Name;
                    if (!ChatMethodKeywords.Any(k => name.Contains(k)) &&
                        !name.Contains("RpcChatMessage"))
                        continue;

                    var parameters = method.GetParameters();
                    // Look for (int/long senderId, string message) or similar
                    if (parameters.Length >= 2 &&
                        parameters.Any(p => p.ParameterType == typeof(string)))
                    {
                        try
                        {
                            var patchMethod = typeof(ChatBubblePatch).GetMethod(
                                nameof(OnChatMessageReceived),
                                BindingFlags.NonPublic | BindingFlags.Static);
                            harmony.Patch(method,
                                postfix: new HarmonyMethod(patchMethod));
                        }
                        catch { /* Skip if patch fails */ }
                    }
                }
            }
        }
        catch { /* Fail-soft */ }
    }

    // Postfix called after a chat message is received.
    // Tries to find the sender's avatar and show a bubble.
    private static void OnChatMessageReceived(MethodBase __originalMethod, object[] __args)
    {
        try
        {
            if (__args == null || __args.Length < 2) return;

            // Extract sender ID and message from args
            // (order varies; find by type)
            int? senderId = null;
            string message = null;
            foreach (var arg in __args)
            {
                if (arg is string s && message == null)
                    message = s;
                else if (arg is int i && senderId == null)
                    senderId = i;
                else if (arg is long l && senderId == null)
                    senderId = (int)l;
            }
            if (senderId == null || string.IsNullOrEmpty(message)) return;

            ShowBubbleForSender(senderId.Value, message);
        }
        catch { /* Fail-soft: chat log already handled it */ }
    }

    private static void ShowBubbleForSender(int senderId, string message)
    {
        try
        {
            // Find all GameObjects with chat bubble components
            // (using the PlayerNameTag registry pattern from NametagBadgePatch)
            var nametags = UnityEngine.Object.FindObjectsOfType<MonoBehaviour>()
                .Where(mb => mb.GetType().Name.Contains("NameTag") ||
                             mb.GetType().Name.Contains("PlayerName"));

            foreach (var nametag in nametags)
            {
                try
                {
                    // Get account ID via reflection (same as RolesHelper)
                    if (!TryGetAccountId(nametag, out int accountId))
                        continue;
                    if (accountId != senderId)
                        continue;

                    // Found the sender's avatar! Try to show the bubble.
                    // The bubble is on the avatar/puppet, which is the parent
                    // or a related component. Try the nametag's GameObject
                    // and its parents.
                    var go = nametag.gameObject;
                    for (int i = 0; i < 5 && go != null; i++)
                    {
                        if (TryShowBubble(go, message))
                            return;
                        go = go.transform.parent?.gameObject;
                    }
                }
                catch { continue; }
            }
        }
        catch { /* Fail-soft */ }
    }

    private static bool TryGetAccountId(object instance, out int accountId)
    {
        accountId = 0;
        try
        {
            // Try direct fields/properties first
            foreach (var name in AvatarIdNames)
            {
                var field = instance.GetType().GetField(name,
                    BindingFlags.Public | BindingFlags.NonPublic | BindingFlags.Instance);
                if (field != null)
                {
                    var val = field.GetValue(instance);
                    if (TryParseInt(val, out int id) && id > 0)
                    {
                        accountId = id;
                        return true;
                    }
                }
                var prop = instance.GetType().GetProperty(name,
                    BindingFlags.Public | BindingFlags.NonPublic | BindingFlags.Instance);
                if (prop != null && prop.CanRead)
                {
                    var val = prop.GetValue(instance);
                    if (TryParseInt(val, out int id) && id > 0)
                    {
                        accountId = id;
                        return true;
                    }
                }
            }

            // Try via player object (like RolesHelper)
            var player = ReadMember(instance, new[] { "thisPlayer", "Player", "player" });
            if (player != null)
            {
                foreach (var name in AvatarIdNames)
                {
                    var val = ReadMember(player, new[] { name });
                    if (TryParseInt(val, out int id) && id > 0)
                    {
                        accountId = id;
                        return true;
                    }
                }
            }
        }
        catch { }
        return false;
    }

    private static bool TryShowBubble(GameObject go, string message)
    {
        try
        {
            // Find the component with the chatBubble field
            foreach (var comp in go.GetComponents<MonoBehaviour>())
            {
                var type = comp.GetType();
                // Check if this component has bubble fields
                bool hasBubble = BubbleFieldNames.Any(fn =>
                    type.GetField(fn, BindingFlags.Public | BindingFlags.NonPublic | BindingFlags.Instance) != null);
                if (!hasBubble) continue;

                // Try to invoke RunShowChatMessage or similar
                foreach (var methodName in BubbleMethodNames)
                {
                    var method = type.GetMethod(methodName,
                        BindingFlags.Public | BindingFlags.NonPublic | BindingFlags.Instance,
                        null, new[] { typeof(string) }, null);
                    if (method != null)
                    {
                        method.Invoke(comp, new object[] { message });
                        return true;
                    }
                }
            }
        }
        catch { }
        return false;
    }

    private static object ReadMember(object instance, string[] names)
    {
        if (instance == null) return null;
        var type = instance.GetType();
        foreach (var name in names)
        {
            var field = type.GetField(name,
                BindingFlags.Public | BindingFlags.NonPublic | BindingFlags.Instance);
            if (field != null) return field.GetValue(instance);
            var prop = type.GetProperty(name,
                BindingFlags.Public | BindingFlags.NonPublic | BindingFlags.Instance);
            if (prop != null && prop.CanRead) return prop.GetValue(instance);
        }
        return null;
    }

    private static bool TryParseInt(object val, out int result)
    {
        result = 0;
        if (val == null) return false;
        if (val is int i) { result = i; return true; }
        if (val is long l) { result = (int)l; return true; }
        if (val is string s) return int.TryParse(s, out result);
        return int.TryParse(val.ToString(), out result);
    }
}
