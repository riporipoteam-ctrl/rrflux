using System;
using System.Collections.Generic;
using System.IO.Hashing;
using System.Text;

namespace TachyonWire;

public sealed class MessageVersionData
{
    public uint Hash;
    public int Version;
}

/// <summary>
/// Message-level framing for the forked NGO (docs: ngo-fork-batch-and-approval.md,
/// dissonance-named-message-framing.md). Inside a batch, each message starts with
/// its bit-packed type index (the sender's local order). Message hash =
/// XXHash32(UTF8(type name)).
/// </summary>
public static class NgoMessages
{
    /// <summary>Server build version (v6). Used in the startup self-identification line.</summary>
    public const string ServerBuildVersion = "v8";

    /// <summary>Sentinel named type for the stock-NGO u64-header fallback path (v6).</summary>
    public const ushort NamedStockU64Unknown = 0xFFFF;

    /// <summary>True when a u16 value is a known fork NamedMessageType (0..8).</summary>
    public static bool IsKnownNamedType(ushort v) => v <= NamedAIData;

    /// <summary>The seven internal message types of the trimmed NGO, per docs.</summary>
    public static readonly string[] InternalTypeNames =
    {
        "ConnectionRequestMessage",
        "ConnectionApprovedMessage",
        "DisconnectReasonMessage",
        "NamedMessage",
        "UnnamedMessage",
        "ServerLogMessage",
        "TimeSyncMessage",
    };

    // NamedMessageType values (fork enum, ushort): docs dissonance-named-message-framing.md.
    public const ushort NamedReservedDoNotUse = 0;
    public const ushort NamedDissonanceToServer = 1;
    public const ushort NamedDissonanceToClient = 2;
    public const ushort NamedRRClientToServerProxied = 3;
    public const ushort NamedRRServerToClientProxied = 4;
    public const ushort NamedRRClientToServer = 5;
    public const ushort NamedRRServerToClient = 6;
    public const ushort NamedAIManagement = 7;
    public const ushort NamedAIData = 8;

    public static uint HashTypeName(string typeName) =>
        XxHash32.HashToUInt32(Encoding.UTF8.GetBytes(typeName), 0);

    /// <summary>
    /// Find a message type's index in a peer's MessageVersions order, trying both
    /// the bare type name and the Unity.Netcode.-qualified name (the docs say
    /// "UTF8(type name)" without pinning which form the client registers).
    /// Returns -1 when no entry matches either candidate.
    /// </summary>
    public static int FindTypeIndex(IReadOnlyList<MessageVersionData> versions, string shortName)
    {
        uint bare = HashTypeName(shortName);
        uint qualified = HashTypeName("Unity.Netcode." + shortName);
        for (int i = 0; i < versions.Count; i++)
        {
            if (versions[i].Hash == bare || versions[i].Hash == qualified)
                return i;
        }
        return -1;
    }

    public static string DescribeHash(uint hash)
    {
        foreach (var name in InternalTypeNames)
        {
            if (HashTypeName(name) == hash) return name + " (bare)";
            if (HashTypeName("Unity.Netcode." + name) == hash) return name + " (Unity.Netcode.)";
        }
        return "unknown";
    }

    // ------------------------------------------------------------------ request

    public sealed class ConnectionRequest
    {
        public List<MessageVersionData> Versions = new();
        /// <summary>The extracted AuthPayload JSON text (null when not found).</summary>
        public string AuthJson;
        /// <summary>Everything after the versions array, for RE logging.</summary>
        public byte[] RawTail = Array.Empty<byte>();
    }

    /// <summary>
    /// Parse a ConnectionRequestMessage body from the cursor (positioned just
    /// after the message index). Layout mirrors the documented approval layout:
    /// bit-packed ZigZag version count, then per entry a raw LE u32 hash and a
    /// bit-packed ZigZag version, then the connection data.
    ///
    /// The docs do NOT pin the exact framing of the connection-data byte block
    /// (packed length vs raw), so the JSON object is located by scanning for
    /// its opening brace and matching braces — the payload is self-delimiting.
    /// The request is the only message in its batch and consumes the rest of it.
    /// </summary>
    public static ConnectionRequest ParseConnectionRequest(ByteCursor cursor)
    {
        var req = new ConnectionRequest();
        int bodyStart = cursor.Pos;
        int count = cursor.ReadPackedSigned32();
        // The real 2026 client registers 1298 message types (observed live
        // 2026-11-08); the original 64 cap rejected its request outright and
        // stalled the client at 18% loading. The sanity bound is therefore
        // generous — the real protection is the cursor (every read throws on
        // overrun) plus the explicit per-entry minimum-size check below:
        // each entry needs a raw u32 hash + at least 1 packed-version byte.
        if (count < 0 || count > 100_000)
            throw new FormatException("implausible MessageVersions count: " + count);
        if (count > cursor.Remaining / 5)
            throw new FormatException(
                $"MessageVersions count {count} cannot fit in the {cursor.Remaining} remaining bytes");
        for (int i = 0; i < count; i++)
        {
            uint hash = cursor.ReadU32LE();
            int version = cursor.ReadPackedSigned32();
            req.Versions.Add(new MessageVersionData { Hash = hash, Version = version });
        }

        int tailStart = cursor.Pos;
        req.RawTail = cursor.ReadBytes(cursor.Remaining);

        int jsonStart = -1;
        for (int i = tailStart; i < cursor.End; i++)
        {
            if (cursor.Data[i] == (byte)'{') { jsonStart = i; break; }
        }
        if (jsonStart < 0)
        {
            // Fallback: if the versions walk consumed the wrong number of
            // bytes (a layout surprise), the AuthPayload JSON may begin
            // before the computed tail. Scan the whole message body.
            for (int i = bodyStart; i < cursor.End; i++)
            {
                if (cursor.Data[i] == (byte)'{') { jsonStart = i; break; }
            }
        }
        if (jsonStart >= 0)
        {
            int jsonEnd = FindJsonObjectEnd(cursor.Data, jsonStart, cursor.End);
            if (jsonEnd > jsonStart)
                req.AuthJson = Encoding.UTF8.GetString(cursor.Data, jsonStart, jsonEnd - jsonStart);
        }
        cursor.Pos = cursor.End;
        return req;
    }

    /// <summary>Index just past the closing brace of the JSON object starting at start, or -1.</summary>
    private static int FindJsonObjectEnd(byte[] data, int start, int end)
    {
        int depth = 0;
        bool inString = false, escape = false;
        for (int i = start; i < end; i++)
        {
            byte b = data[i];
            if (inString)
            {
                if (escape) escape = false;
                else if (b == (byte)'\\') escape = true;
                else if (b == (byte)'"') inString = false;
                continue;
            }
            if (b == (byte)'"') inString = true;
            else if (b == (byte)'{') depth++;
            else if (b == (byte)'}')
            {
                depth--;
                if (depth == 0) return i + 1;
            }
        }
        return -1;
    }

    // ------------------------------------------------------------------ approval

    /// <summary>
    /// Build a ConnectionApprovedMessage (message index prefix + body) exactly
    /// per the documented Serialize order: bit-packed ZigZag MessageVersions
    /// length; per entry raw LE u32 Hash + bit-packed ZigZag Version; bit-packed
    /// OwnerClientId (ulong overload, NO zigzag); bit-packed ZigZag NetworkTick;
    /// then (v8) TWO extra bit-packed unsigned scalars — the owner client id
    /// again, then 0 — and only then, if Payload != null, bit-packed
    /// Payload.Length followed by one bit-packed ushort per UTF-16 char. (The
    /// 2025 fork layout had no such pair: the fork dropped ConnectedClientIds
    /// and scene-object data — NGO here has no NetworkObject. The 2026 build's
    /// Deserialize, however, demonstrably consumes two more packed scalars
    /// before the Payload string than the 2025 layout carries — see the v8
    /// note in STATUS.md and the reconstruction comment at the write site.)
    /// </summary>
    /// <summary>Approval BODY only (no index/size prefix) — see BuildApprovalMessage.</summary>
    public static byte[] BuildApprovalBody(
        IReadOnlyList<MessageVersionData> versions,
        ulong ownerClientId,
        int networkTick,
        string payload)
    {
        var buf = new List<byte>();
        BytePacker.WritePackedSigned32(buf, versions.Count);
        foreach (var v in versions)
        {
            BytePacker.WriteU32LE(buf, v.Hash);
            BytePacker.WritePackedSigned32(buf, v.Version);
        }
        BytePacker.WritePackedUnsigned(buf, ownerClientId);
        BytePacker.WritePackedSigned32(buf, networkTick);
        // v8: two extra bit-packed unsigned scalars between NetworkTick and
        // the Payload (ownerClientId again, then 0). Evidence: the REAL 2026
        // client's log (attempt 2026-11-08 21:43 UTC against v7) shows a
        // Newtonsoft JSON parse exception INSIDE
        // TachyonClient.NetworkManager_OnClientConnectedCallback —
        // "unexpected character D at position 0" — followed by
        // FailedToConnectToVoice (8719). So ConnectionApproved.Handle DID
        // run (Deserialize completed) but the Payload string it received
        // started with 'D'. Our payload is {"DR":"Carrot","KT":...}: the
        // char at index 2 is 'D'. The only layout story fitting ALL of it:
        // the 2026 Deserialize consumes TWO extra bit-packed scalars here.
        // It read our payload-length (~365) as scalar #1, our '{' (123) as
        // scalar #2, our '"' (34) as ITS string length, then 34 chars
        // beginning at our index 2 → "DR\":\"Carrot\",..." → 'D' at
        // position 0. The pair (ownerClientId, 0) mirrors the stock-NGO-2.x
        // session-owner order and also parses as a (count=1, elem=0) list;
        // the callback only consumes DR/KT afterward, so exact field
        // semantics don't matter — the parse alignment does. HYPOTHESIS
        // fitted to the exception text; only a real-client run proves it.
        BytePacker.WritePackedUnsigned(buf, ownerClientId);
        BytePacker.WritePackedUnsigned(buf, 0UL);
        if (payload != null)
        {
            BytePacker.WritePackedUnsigned(buf, (ulong)payload.Length);
            foreach (char c in payload)
                BytePacker.WritePackedUnsigned(buf, c);
        }
        return buf.ToArray();
    }

    /// <summary>Legacy (v1–v3) approval message: bare index prefix + body, no size field.</summary>
    public static byte[] BuildApprovalMessage(
        int messageIndex,
        IReadOnlyList<MessageVersionData> versions,
        ulong ownerClientId,
        int networkTick,
        string payload) =>
        WithIndex(messageIndex, BuildApprovalBody(versions, ownerClientId, networkTick, payload));

    // ------------------------------------------------- per-message framing (v4)
    //
    // Live evidence (2026-11-08, server v3): the real client's connection
    // request decoded under the v1–v3 "index, then body-to-end" assumption as
    // a MessageVersions count of 1298 with exactly 2596 bytes remaining. That
    // pair is byte-identical to a packed-UNSIGNED value of 2596 read at the
    // same position (zigzag(2596) = 1298), and 2596 == the bytes remaining —
    // i.e. the field is a per-message SIZE prefix (stock NGO's MessageHeader
    // pattern: packed type index, packed size, then the body), with the one
    // message filling the batch. A genuine 1298-entry versions array cannot
    // fit in 2596 bytes (>= 5 bytes/entry), and the docs pin the fork's NGO
    // to 7 internal types with "the 8 hashes" in the captured request
    // (voice-stack-package-versions.md, ngo-fork-batch-and-approval.md). The
    // 24-byte batch header itself is proven by the client's own XXHash64
    // check passing on receive. v4 therefore frames messages as
    // [packed index][packed size][body] in both directions, with the legacy
    // unframed form kept as a receive fallback (see TrySegmentFramed).

    /// <summary>Frame one message: packed index + packed body size + body.</summary>
    public static byte[] FrameMessage(int messageIndex, byte[] body)
    {
        var buf = new List<byte>(body.Length + 8);
        BytePacker.WritePackedUnsigned(buf, (ulong)messageIndex);
        BytePacker.WritePackedUnsigned(buf, (ulong)body.Length);
        buf.AddRange(body);
        return buf.ToArray();
    }

    /// <summary>
    /// Validate + split a batch payload into size-prefixed messages. Succeeds
    /// only when repeated [index][size][body] records consume EXACTLY the
    /// given slice — a strict structural check, so the legacy unframed layout
    /// (which would need a freak coincidence to validate) falls through to
    /// the caller's legacy path. BodyOffset is absolute into data.
    /// </summary>
    public static bool TrySegmentFramed(
        byte[] data, int offset, int length,
        out List<(int Index, int BodyOffset, int BodyLength)> messages)
    {
        messages = new List<(int, int, int)>();
        var cursor = new ByteCursor(data, offset, length);
        try
        {
            while (cursor.Remaining > 0)
            {
                int index = (int)cursor.ReadPackedUnsigned();
                if (index > 65535) return false;
                ulong size = cursor.ReadPackedUnsigned();
                if (size > (ulong)cursor.Remaining || size > (1 << 21)) return false;
                int bodyOffset = cursor.Pos;
                cursor.Skip((int)size);
                messages.Add((index, bodyOffset, (int)size));
                if (messages.Count > 4096) return false;
            }
        }
        catch (FormatException)
        {
            return false;
        }
        return messages.Count > 0;
    }

    // ------------------------------------------------------- Dissonance packets
    //
    // Stock Dissonance wire packet (docs: dissonance-named-message-framing.md):
    // magic 0x8BC7 (bytes 8B C7 on the wire) + 1 type byte + type-specific
    // body. The packet interior uses Dissonance's big-endian primitive order
    // (the magic itself is written big-endian).

    /// <summary>Dissonance MessageTypes value names (stock table, per docs).</summary>
    public static string DissonanceTypeName(byte type) => type switch
    {
        0 => "RemoveClient",
        1 => "ClientState",
        2 => "VoiceData",
        3 => "TextData",
        4 => "HandshakeRequest",
        5 => "HandshakeResponse",
        6 => "ErrorWrongSession",
        7 => "ServerRelayReliable",
        8 => "ServerRelayUnreliable",
        9 => "DeltaChannelState",
        10 => "HandshakeP2P",
        11 => "HeartbeatP2P",
        12 => "RemoveClientP2P",
        _ => "unknown",
    };

    /// <summary>Extract the Dissonance packet type byte; false when the magic is wrong.</summary>
    public static bool TryGetDissonanceType(byte[] packet, out byte type)
    {
        type = 0;
        if (packet == null || packet.Length < 3) return false;
        if (packet[0] != 0x8B || packet[1] != 0xC7) return false;
        type = packet[2];
        return true;
    }

    /// <summary>
    /// Stock Dissonance RemoveClient packet: magic + type 0 + the departed
    /// player's ushort id (big-endian, like the magic). Prescribed by
    /// voice-stale-playback-on-leave.md: clients cannot clean up playback
    /// objects themselves, so the server must originate this on leave.
    /// The id width (ushort) is pinned for the 2026 build by
    /// dissonance-named-message-framing.md.
    /// </summary>
    public static byte[] BuildRemoveClientPacket(ushort dissonancePlayerId) =>
        new[] { (byte)0x8B, (byte)0xC7, (byte)0, (byte)(dissonancePlayerId >> 8), (byte)dissonancePlayerId };

    /// <summary>Inverse of BuildApprovalMessage (used by the self-test client).</summary>
    public sealed class Approval
    {
        public List<MessageVersionData> Versions = new();
        public ulong OwnerClientId;
        public int NetworkTick;
        // v8 layout: the two extra bit-packed unsigned scalars the 2026
        // Deserialize consumes between NetworkTick and the Payload string.
        public ulong SessionScalar1;
        public ulong SessionScalar2;
        public string Payload;
    }

    public static Approval ParseApproval(ByteCursor cursor)
    {
        var a = new Approval();
        int count = cursor.ReadPackedSigned32();
        if (count < 0 || count > 100_000)
            throw new FormatException("implausible approval MessageVersions count: " + count);
        if (count > cursor.Remaining / 5)
            throw new FormatException(
                $"approval MessageVersions count {count} cannot fit in the {cursor.Remaining} remaining bytes");
        for (int i = 0; i < count; i++)
        {
            uint hash = cursor.ReadU32LE();
            int version = cursor.ReadPackedSigned32();
            a.Versions.Add(new MessageVersionData { Hash = hash, Version = version });
        }
        a.OwnerClientId = cursor.ReadPackedUnsigned();
        a.NetworkTick = cursor.ReadPackedSigned32();
        a.SessionScalar1 = cursor.ReadPackedUnsigned();
        a.SessionScalar2 = cursor.ReadPackedUnsigned();
        if (cursor.Remaining > 0)
        {
            int len = (int)cursor.ReadPackedUnsigned();
            var chars = new char[len];
            for (int i = 0; i < len; i++)
                chars[i] = (char)cursor.ReadPackedUnsigned();
            a.Payload = new string(chars);
        }
        return a;
    }

    // ------------------------------------------------------------------ named messages

    /// <summary>
    /// Build a NamedMessage body: u16 NamedMessageType (the fork replaced stock
    /// NGO's ulong hash with this ushort), then i32 length + Dissonance packet
    /// bytes (the WriteValueSafe(byte[]) framing the NFGO integration uses).
    /// </summary>
    public static byte[] BuildNamedMessageBody(ushort namedType, byte[] packet)
    {
        var buf = new List<byte>(2 + 4 + packet.Length);
        BytePacker.WriteU16LE(buf, namedType);
        BytePacker.WriteI32LE(buf, packet.Length);
        buf.AddRange(packet);
        return buf.ToArray();
    }

    /// <summary>
    /// Build a NamedMessage body WITHOUT the Dissonance i32 length wrapper:
    /// just the u16 NamedMessageType + the payload bytes verbatim (v7).
    /// This is the room-lane form (kinds 3/4 proxied, 5/6 room data): the
    /// fork's NamedMessage.Deserialize reads only the u16 type and treats
    /// the rest as receive data, and v5+ parses non-Dissonance bodies as
    /// [u16][whole rest] — so room kinds must be SENT the same way. The
    /// i32 WriteValueSafe wrapper in BuildNamedMessageBody exists only on
    /// the Dissonance send path and stays Dissonance-only (type 2).
    /// </summary>
    public static byte[] BuildRawNamedMessageBody(ushort namedType, byte[] payload)
    {
        var buf = new List<byte>(2 + payload.Length);
        BytePacker.WriteU16LE(buf, namedType);
        buf.AddRange(payload);
        return buf.ToArray();
    }

    // ------------------------------------------------------------- TimeSync
    //
    // Stock Unity Netcode TimeSyncMessage, pinned from the actual source
    // (Unity-Technologies/com.unity.netcode.gameobjects, develop branch,
    // fetched 2026-11-08):
    //   Runtime/Messaging/Messages/TimeSyncMessage.cs —
    //     internal struct TimeSyncMessage : INetworkMessage {
    //       public int Tick;
    //       Serialize:  BytePacker.WriteValueBitPacked(writer, Tick);
    //       Deserialize: returns FALSE unless the owner IsClient, then
    //                    ByteUnpacker.ReadValueBitPacked(reader, out Tick);
    //       Handle: client Sync()s its NetworkTimeSystem to the tick. }
    //   Runtime/Timing/NetworkTimeSystem.cs —
    //     ONLY the server sends it: Initialize() hooks OnTickSyncTime
    //     solely when LocalClient.IsServer; OnTickSyncTime sends
    //     `new TimeSyncMessage { Tick = ServerTime.Tick }` to all
    //     ConnectedClientIds whenever ServerTime.Tick %
    //     m_TimeSyncFrequencyTicks == 0, with k_TimeSyncFrequency = 1.0s
    //     ("1 synchronization message per second"), delivery Unreliable.
    // So the body is exactly ONE bit-packed (ZigZag) int — the server's
    // current tick — and the direction is server->client ONLY. A client
    // never sends TimeSyncMessage; there is no request to "reply" to.
    // WriteValueBitPacked for int is the same ZigZag bit-packing as this
    // fork's BytePacker.WritePackedSigned32 / ReadPackedSigned32 (the
    // fork's packing mirrors stock, per ngo-fork-batch-and-approval.md).

    /// <summary>Build a TimeSyncMessage body: one bit-packed signed tick.</summary>
    public static byte[] BuildTimeSyncBody(int tick)
    {
        var buf = new List<byte>(5);
        BytePacker.WritePackedSigned32(buf, tick);
        return buf.ToArray();
    }

    /// <summary>Parse a TimeSyncMessage body (one bit-packed signed tick).</summary>
    public static bool TryParseTimeSyncBody(ByteCursor cursor, out int tick)
    {
        tick = 0;
        try
        {
            tick = cursor.ReadPackedSigned32();
            return true;
        }
        catch (FormatException)
        {
            return false;
        }
    }

    /// <summary>Wrap a body with its bit-packed message index.</summary>
    public static byte[] WithIndex(int messageIndex, byte[] body)
    {
        var buf = new List<byte>(body.Length + 4);
        BytePacker.WritePackedUnsigned(buf, (ulong)messageIndex);
        buf.AddRange(body);
        return buf.ToArray();
    }

    /// <summary>
    /// Parse a NamedMessage body (v5, per dissonance-named-message-framing.md).
    /// The fork's NamedMessage.Deserialize reads ONLY the u16 type (Hash);
    /// everything after it is receive data — there is NO universal length
    /// field. The i32 length prefix exists only inside the Dissonance send
    /// path (NfgoCommsNetwork packs the Dissonance byte[] with
    /// WriteValueSafe), so it is unwrapped later, per named type, by
    /// <see cref="UnwrapDissonancePayload"/>. v1–v4 demanded
    /// [u16][i32 length][packet] for EVERY named type and hard-failed the
    /// rest — which is why the real client's first post-approval
    /// NamedMessage (a non-Dissonance room type) was dropped as
    /// "unparseable" and the client hung at 18%.
    /// On success, payload = ALL bytes after the u16 type.
    /// </summary>
    public static bool TryParseNamedMessageBody(ByteCursor cursor, out ushort namedType, out byte[] payload)
    {
        return TryParseNamedMessageBodyDual(cursor, out namedType, out payload, out _, out _);
    }

    /// <summary>
    /// Dual named-header parse (v6). PRIMARY: fork u16 NamedMessageType
    /// (docs/dissonance-named-message-framing.md PROVES NamedMessage.Deserialize
    /// reads exactly 2 bytes; stock NGO's ulong is explicitly a trap).
    /// FALLBACK: if the u16 value is NOT a known NamedMessageType (0..8)
    /// AND the body is >= 8 bytes, interpret the first 8 bytes as a
    /// stock-NGO u64 name hash (little-endian, matching the LE wire order
    /// used for the u16), set isStockU64Fallback=true, stockHash to that
    /// value, namedType to NamedStockU64Unknown, and payload to the bytes
    /// after those 8 bytes. Callers must log the fallback distinctly and
    /// treat it as capture-only (do not crash, do not relay blindly).
    /// Known-u16 behaviour is unchanged.
    /// </summary>
    public static bool TryParseNamedMessageBodyDual(ByteCursor cursor, out ushort namedType, out byte[] payload,
        out bool isStockU64Fallback, out ulong stockHash)
    {
        namedType = 0;
        payload = null;
        isStockU64Fallback = false;
        stockHash = 0;
        if (cursor.Remaining < 2) return false;
        int startPos = cursor.Pos;
        int totalRemaining = cursor.Remaining;
        ushort u16val = (ushort)(cursor.Data[startPos] | (cursor.Data[startPos + 1] << 8));
        if (!IsKnownNamedType(u16val) && totalRemaining >= 8)
        {
            // Stock-NGO u64 header fallback.
            ulong h = 0;
            for (int i = 0; i < 8; i++)
                h |= (ulong)cursor.Data[startPos + i] << (8 * i);
            stockHash = h;
            isStockU64Fallback = true;
            namedType = NamedStockU64Unknown;
            cursor.Pos = startPos + 8;
            payload = cursor.ReadRemaining();
            return true;
        }
        namedType = cursor.ReadU16LE();
        payload = cursor.ReadRemaining();
        return true;
    }

    /// <summary>
    /// Undo the Dissonance send-path framing inside a DissonanceToServer /
    /// DissonanceToClient body: NfgoCommsNetwork packs the Dissonance byte[]
    /// with FastBufferWriter.WriteValueSafe, prefixing an i32 LE length.
    /// The prefix is accepted ONLY when it fits EXACTLY (len == remaining-4)
    /// and the following bytes start with the stock Dissonance magic
    /// 0x8BC7; otherwise the whole rest is the packet. Never fails.
    /// </summary>
    public static byte[] UnwrapDissonancePayload(byte[] rest, out bool hadLengthPrefix)
    {
        hadLengthPrefix = false;
        if (rest != null && rest.Length >= 6)
        {
            int len = rest[0] | (rest[1] << 8) | (rest[2] << 16) | (rest[3] << 24);
            if (len == rest.Length - 4 && rest[4] == 0x8B && rest[5] == 0xC7)
            {
                hadLengthPrefix = true;
                var packet = new byte[len];
                Buffer.BlockCopy(rest, 4, packet, 0, len);
                return packet;
            }
        }
        return rest;
    }

    // ------------------------------------- stock Dissonance packets (v5)
    //
    // Layouts pinned from the actual Dissonance source (Placeholder
    // Software's GitHub repo hosts docs only — "This repository does not
    // contain the source code" — so the reference copies are the asset
    // source / decompilations vendored in public game projects; two
    // independent decompilations of the current generation —
    // nomodev-xd/SCPSL-Decompilation and xenoveni/LethalMic — agree
    // byte-for-byte on every method below):
    //   PacketWriter.cs / PacketReader.cs — primitives + packet layouts
    //   CodecSettings.cs — (Codec u8, FrameSize u32, SampleRate int32)
    //   Server/BroadcastingClientCollection.cs — server handshake flow
    //   BaseServer.cs — session id = random nonzero u32 per server
    //
    // Primitives (PacketWriter.Write / PacketReader.Read*): every
    // multi-byte value is BIG-ENDIAN ("network order" — ushort writes MSB
    // then LSB; the magic 35783 = 0x8BC7 therefore lands as bytes 8B C7,
    // which cross-checks against the observed wire bytes).
    // Strings: u16 BE length = UTF8 byte count + 1, then the raw UTF8
    // bytes (no terminator); length 0 encodes null. The +1 is real — the
    // reader does num-- before reading (verified in both decompilations
    // AND in the older asset source, oshaikh13/Boxy PacketWriter.Write).
    // Byte segments: u16 BE count + bytes.

    /// <summary>Dissonance CodecSettings (9 bytes on the wire).</summary>
    public sealed class DissonanceCodecSettings
    {
        public byte Codec;
        public uint FrameSize;
        public uint SampleRate;
    }

    private sealed class BeReader
    {
        private readonly byte[] _d;
        public int Pos;
        public BeReader(byte[] data, int pos) { _d = data; Pos = pos; }
        public int Remaining => _d.Length - Pos;
        public byte ReadByteE()
        {
            if (Remaining < 1) throw new FormatException("BE read past end");
            return _d[Pos++];
        }
        public ushort ReadU16BE()
        {
            if (Remaining < 2) throw new FormatException("truncated BE u16");
            ushort v = (ushort)((_d[Pos] << 8) | _d[Pos + 1]);
            Pos += 2;
            return v;
        }
        public uint ReadU32BE()
        {
            if (Remaining < 4) throw new FormatException("truncated BE u32");
            uint v = (uint)((_d[Pos] << 24) | (_d[Pos + 1] << 16) | (_d[Pos + 2] << 8) | _d[Pos + 3]);
            Pos += 4;
            return v;
        }
        public string ReadString()
        {
            int len = ReadU16BE();
            if (len == 0) return null;
            len--; // the writer stores UTF8 byte count + 1 (PacketWriter.Write(string))
            if (len > Remaining) throw new FormatException("truncated BE string");
            string s = Encoding.UTF8.GetString(_d, Pos, len);
            Pos += len;
            return s;
        }
        public DissonanceCodecSettings ReadCodecSettings() => new()
        {
            Codec = ReadByteE(),
            FrameSize = ReadU32BE(),
            SampleRate = ReadU32BE(),
        };
    }

    private static void WriteU16BE(List<byte> buf, ushort v)
    {
        buf.Add((byte)(v >> 8));
        buf.Add((byte)v);
    }

    private static void WriteU32BE(List<byte> buf, uint v)
    {
        buf.Add((byte)(v >> 24));
        buf.Add((byte)(v >> 16));
        buf.Add((byte)(v >> 8));
        buf.Add((byte)v);
    }

    private static void WriteDissonanceString(List<byte> buf, string s)
    {
        if (s == null) { WriteU16BE(buf, 0); return; }
        byte[] bytes = Encoding.UTF8.GetBytes(s);
        WriteU16BE(buf, (ushort)(bytes.Length + 1)); // +1 per PacketWriter.Write(string)
        buf.AddRange(bytes);
    }

    private static void WriteCodecSettings(List<byte> buf, DissonanceCodecSettings codec)
    {
        buf.Add(codec.Codec);
        WriteU32BE(buf, codec.FrameSize);
        WriteU32BE(buf, codec.SampleRate);
    }

    private static void WriteClientInfo(List<byte> buf, string name, ushort id, DissonanceCodecSettings codec)
    {
        WriteDissonanceString(buf, name);
        WriteU16BE(buf, id);
        WriteCodecSettings(buf, codec);
    }

    private static List<byte> DissonancePacketHead(byte type)
    {
        var buf = new List<byte> { 0x8B, 0xC7, type };
        return buf;
    }

    /// <summary>
    /// Parse a stock HandshakeRequest packet (magic + type 4, NO session
    /// id — it is the one packet without one): CodecSettings, then the
    /// player name string (PacketReader.ReadHandshakeRequest).
    /// </summary>
    public static bool TryParseHandshakeRequest(byte[] packet, out DissonanceCodecSettings codec, out string playerName)
    {
        codec = null;
        playerName = null;
        if (!TryGetDissonanceType(packet, out byte type) || type != 4) return false;
        try
        {
            var r = new BeReader(packet, 3);
            codec = r.ReadCodecSettings();
            playerName = r.ReadString();
            return true;
        }
        catch (FormatException)
        {
            return false;
        }
    }

    /// <summary>
    /// Parse a stock ClientState packet (magic + type 1 + session u32):
    /// ClientInfo (name, id, codec), then a u16 room count + room name
    /// strings (PacketReader.ReadClientStateHeader / ReadClientStateRooms).
    /// </summary>
    public static bool TryParseClientState(byte[] packet, out uint sessionId, out string playerName,
        out ushort playerId, out DissonanceCodecSettings codec, out List<string> rooms)
    {
        sessionId = 0;
        playerName = null;
        playerId = 0;
        codec = null;
        rooms = null;
        if (!TryGetDissonanceType(packet, out byte type) || type != 1) return false;
        try
        {
            var r = new BeReader(packet, 3);
            sessionId = r.ReadU32BE();
            playerName = r.ReadString();
            playerId = r.ReadU16BE();
            codec = r.ReadCodecSettings();
            int count = r.ReadU16BE();
            rooms = new List<string>(count);
            for (int i = 0; i < count; i++)
                rooms.Add(r.ReadString());
            return true;
        }
        catch (FormatException)
        {
            return false;
        }
    }

    /// <summary>
    /// Build a stock HandshakeRequest packet (test client + completeness):
    /// magic + type 4 + CodecSettings + player name (no session id).
    /// </summary>
    public static byte[] BuildHandshakeRequestPacket(string playerName, DissonanceCodecSettings codec)
    {
        var buf = DissonancePacketHead(4);
        WriteCodecSettings(buf, codec);
        WriteDissonanceString(buf, playerName);
        return buf.ToArray();
    }

    /// <summary>
    /// Build a stock HandshakeResponse packet in the form the stock server
    /// actually sends on join (BroadcastingClientCollection.
    /// ProcessHandshakeRequest passes its CLEARED client buffer, so the
    /// response carries ZERO clients and ZERO rooms; the full state follows
    /// immediately as individual ClientState packets — explicitly blessed
    /// by docs/Reference/Networking/Network-Protocol.md). Layout
    /// (PacketWriter.WriteHandshakeResponse): magic + type 5 + session u32
    /// BE + assigned clientId u16 BE + clients u16 count + per-client
    /// ClientInfo + room-names u16 count + names + room-lists u16 count +
    /// per-room (roomId u16 BE, memberCount u8, member ids u16 BE).
    /// </summary>
    public static byte[] BuildHandshakeResponsePacket(uint sessionId, ushort assignedClientId) =>
        BuildHandshakeResponsePacket(sessionId, assignedClientId,
            Array.Empty<(string Name, ushort Id, DissonanceCodecSettings Codec)>());

    public static byte[] BuildHandshakeResponsePacket(
        uint sessionId, ushort assignedClientId,
        IReadOnlyList<(string Name, ushort Id, DissonanceCodecSettings Codec)> clients)
    {
        var buf = DissonancePacketHead(5);
        WriteU32BE(buf, sessionId);
        WriteU16BE(buf, assignedClientId);
        WriteU16BE(buf, (ushort)clients.Count);
        foreach (var c in clients)
            WriteClientInfo(buf, c.Name, c.Id, c.Codec);
        WriteU16BE(buf, 0); // room names (state is delivered via ClientState burst)
        WriteU16BE(buf, 0); // room -> member lists
        return buf.ToArray();
    }

    /// <summary>
    /// Build a stock ClientState packet (PacketWriter.WriteClientState):
    /// magic + type 1 + session u32 BE + ClientInfo (name, id, codec) +
    /// u16 room count + room name strings.
    /// </summary>
    public static byte[] BuildClientStatePacket(uint sessionId, string playerName, ushort playerId,
        DissonanceCodecSettings codec, IReadOnlyList<string> rooms)
    {
        var buf = DissonancePacketHead(1);
        WriteU32BE(buf, sessionId);
        WriteClientInfo(buf, playerName, playerId, codec);
        WriteU16BE(buf, (ushort)(rooms?.Count ?? 0));
        if (rooms != null)
            foreach (var room in rooms)
                WriteDissonanceString(buf, room);
        return buf.ToArray();
    }

    /// <summary>Parse a HandshakeResponse packet (used by the self-test client).</summary>
    public static bool TryParseHandshakeResponse(byte[] packet, out uint sessionId, out ushort clientId,
        out int clientCount, out int roomNameCount, out int roomListCount)
    {
        sessionId = 0;
        clientId = 0;
        clientCount = -1;
        roomNameCount = -1;
        roomListCount = -1;
        if (!TryGetDissonanceType(packet, out byte type) || type != 5) return false;
        try
        {
            var r = new BeReader(packet, 3);
            sessionId = r.ReadU32BE();
            clientId = r.ReadU16BE();
            clientCount = r.ReadU16BE();
            for (int i = 0; i < clientCount; i++)
            {
                r.ReadString();
                r.ReadU16BE();
                r.ReadCodecSettings();
            }
            roomNameCount = r.ReadU16BE();
            var names = new List<string>(roomNameCount);
            for (int i = 0; i < roomNameCount; i++)
                names.Add(r.ReadString());
            roomListCount = r.ReadU16BE();
            for (int i = 0; i < roomListCount; i++)
            {
                r.ReadU16BE();
                int members = r.ReadByteE();
                for (int j = 0; j < members; j++)
                    r.ReadU16BE();
            }
            return true;
        }
        catch (FormatException)
        {
            return false;
        }
    }
}
