using System;
using System.Collections.Concurrent;
using System.Collections.Generic;
using System.Net;
using System.Security.Cryptography;
using System.Threading;
using LiteNetLib.Layers;

namespace TachyonWire;

/// <summary>
/// Server-side twin of the client's RecRoomClientEncryptLayer
/// (docs: recroom-encrypt-layer-format.md, litenetlib-fork-encrypt-layer.md).
///
/// Wire format — a TRAILING marker byte, variable length:
///   key == null : append a single 0x11 byte (plaintext, +1). No transform.
///   key != null : 8 random bytes (the nonce/IV), transform, then 0x2A (+9 total).
/// ExtraPacketSizeForLayer = 9 is the worst-case RESERVATION, not the framed size.
///
/// Inbound: marker is the LAST byte. 0x11 -> strip only (no validation).
/// 0x2A -> drop when length &lt;= 9 or no key; otherwise decrypt.
/// Anything else -> log "unknown marker" and drop.
///
/// MILESTONE STATE: the transform itself is NOT established by the docs (the
/// client's copy is Burst-compiled native code; the 2026 client notes in
/// p2026_AGENTS.md observe a "Rabbit IV plus marker 2A" but do not pin the
/// 32-byte key schedule). This implementation is therefore doc-exact for the
/// plaintext path — which the docs prove is accepted through the whole
/// LiteNetLib + NGO handshake — and implements the 0x2A branch's framing/drop
/// semantics with the cipher deliberately unimplemented: SetKey stores the
/// key and logs, but outbound encryption refuses loudly instead of emitting
/// fake ciphertext, and inbound 0x2A datagrams are dropped while no verified
/// cipher is configured. See STATUS.md.
/// </summary>
public sealed class RecRoomClientEncryptLayer : PacketLayerBase
{
    public const byte PlaintextMarker = 0x11;
    public const byte EncryptedMarker = 0x2A;
    public const int NonceSize = 8;
    public const int KeySize = 32;

    private byte[] _key; // exactly 32 bytes when set (copied, like the client's SetKey)

    // First framing marker seen per remote endpoint — the key wire diagnostic:
    // it tells us whether a client speaks plaintext (0x11) or the encrypted
    // path (0x2A) from its very first datagram. Endpoint-keyed and concurrent
    // because LiteNetLib may call the layer from its receive thread(s).
    private readonly ConcurrentDictionary<IPEndPoint, byte> _firstMarkerByEndPoint = new();

    // Per-endpoint drop accounting (v4). The interesting live event is the
    // marker FLIP 0x11 -> 0x2A: the client arms its encrypt layer the moment
    // it processes our approval (docs: ngo-fork-batch-and-approval.md), so
    // from then on every client datagram arrives 0x2A-framed and — the cipher
    // being unpinned — is dropped here. Counting + first-capture turns that
    // deafness into hard log evidence instead of a silent stall.
    private sealed class EndpointStats
    {
        public long Total;
        public long EncryptedDrops;
        public long UnknownDrops;
        public byte LastMarker;
        public bool SawEncrypted;
    }

    private readonly ConcurrentDictionary<IPEndPoint, EndpointStats> _statsByEndPoint = new();

    private EndpointStats StatsFor(IPEndPoint endPoint) =>
        _statsByEndPoint.GetOrAdd(endPoint, _ => new EndpointStats());

    /// <summary>One-line-per-endpoint drop summary for the server heartbeat; "" when nothing was dropped.</summary>
    public string StatsSummary()
    {
        var parts = new List<string>();
        foreach (var kv in _statsByEndPoint)
        {
            var s = kv.Value;
            if (s.EncryptedDrops > 0 || s.UnknownDrops > 0)
                parts.Add($"{kv.Key}: datagrams={s.Total} encryptedDropped={s.EncryptedDrops} unknownDropped={s.UnknownDrops}");
        }
        return string.Join("; ", parts);
    }

    public Action<string> Log { get; set; } = msg => Console.WriteLine("[layer] " + msg);

    public RecRoomClientEncryptLayer() : base(9)
    {
    }

    public bool HasKey => _key != null;

    /// <summary>Sink for the derived session key. Copies exactly 32 bytes.</summary>
    public void SetKey(byte[] key)
    {
        if (key == null) throw new ArgumentNullException(nameof(key));
        var copy = new byte[KeySize];
        Buffer.BlockCopy(key, 0, copy, 0, KeySize);
        _key = copy;
        Log("RecRoomClientEncryptLayer Encryption Enabled (key set; cipher not established — see STATUS.md)");
    }

    public void ClearKey()
    {
        _key = null;
        Log("RecRoomClientEncryptLayer Endpoint Disconnected (key cleared)");
    }

    public override void ProcessOutBoundPacket(ref IPEndPoint endPoint, ref byte[] data, ref int offset, ref int length)
    {
        if (_key == null)
        {
            data[offset + length] = PlaintextMarker;
            length += 1;
            return;
        }
        // The 0x2A framing is known (8-byte nonce trailer + marker) but the
        // cipher/key schedule is not established by the docs. Refuse loudly
        // rather than emit bytes the client cannot decrypt.
        throw new InvalidOperationException(
            "Encrypted datagram path (marker 0x2A) is not implemented: the layer cipher " +
            "is not established by the reference docs. Run plaintext (no SetKey) — see STATUS.md.");
    }

    public override void ProcessInboundPacket(ref IPEndPoint endPoint, ref byte[] data, ref int length)
    {
        // Never let an exception escape into LiteNetLib's receive path: log it
        // (full stack) and drop the datagram instead of deafening the server.
        try
        {
            ProcessInboundPacketCore(ref endPoint, ref data, ref length);
        }
        catch (Exception e)
        {
            Log($"EXCEPTION processing inbound datagram from {endPoint} (dropping it): {e}");
            length = 0;
        }
    }

    private void ProcessInboundPacketCore(ref IPEndPoint endPoint, ref byte[] data, ref int length)
    {
        if (length <= 0)
            return;
        byte marker = data[length - 1];
        var stats = StatsFor(endPoint);
        Interlocked.Increment(ref stats.Total);
        if (_firstMarkerByEndPoint.TryAdd(endPoint, marker))
        {
            string kind = marker == PlaintextMarker ? "plaintext"
                : marker == EncryptedMarker ? "ENCRYPTED"
                : "UNKNOWN";
            Log($"first inbound datagram from {endPoint}: framing marker 0x{marker:X2} ({kind})" +
                (marker == PlaintextMarker ? " — client starts on the plaintext path"
                 : marker == EncryptedMarker ? " — client starts on the ENCRYPTED path"
                 : " — unrecognised framing, will be dropped"));
        }
        else if (marker == EncryptedMarker && stats.LastMarker == PlaintextMarker && !stats.SawEncrypted)
        {
            // The approval just armed the client's encrypt layer: from here
            // on it encrypts EVERY outbound datagram (docs). Capture the
            // transition + the first ciphertext verbatim for offline work.
            stats.SawEncrypted = true;
            int headLen = Math.Min(48, length);
            string head = Convert.ToHexString(data, 0, headLen);
            string nonce = length > 9
                ? Convert.ToHexString(data, length - 9, 8)
                : "n/a";
            Log($"endpoint {endPoint} switched to ENCRYPTED framing (0x11 -> 0x2A): the approval armed its session key. " +
                $"First encrypted datagram: {length}B head={head} nonceTrailer={nonce}. " +
                "Cipher not established by the docs — datagram dropped (see STATUS.md gap #1).");
        }
        stats.LastMarker = marker;
        if (marker == PlaintextMarker)
        {
            length -= 1; // strip-only, no key check, no validation (per doc)
            return;
        }
        if (marker == EncryptedMarker)
        {
            long drops = Interlocked.Increment(ref stats.EncryptedDrops);
            // Rate-limit the per-drop lines (a live client retransmits and
            // pings ~1/s; the heartbeat carries the running totals).
            if (length <= 9)
            {
                if (drops <= 3)
                    Log($"dropping encrypted datagram from {endPoint}: {length}B is too short for the 8B nonce trailer + marker (per doc)");
                length = 0; // drop (per doc)
                return;
            }
            if (_key == null)
            {
                if (drops <= 3)
                    Log($"dropping encrypted datagram from {endPoint}: no session key set on this layer (marker 0x2A requires one, per doc)");
                length = 0; // drop (per doc)
                return;
            }
            // A key is set but no verified cipher exists in this milestone.
            if (drops <= 3)
                Log($"dropping encrypted datagram from {endPoint}: cipher not established (see STATUS.md)");
            length = 0;
            return;
        }
        Interlocked.Increment(ref stats.UnknownDrops);
        Log($"Discarding packet from {endPoint} with unknown marker 0x{marker:X2}");
        length = 0;
    }

    /// <summary>Nonce generation for the 0x2A trailer (kept for the cipher milestone).</summary>
    internal static byte[] NewNonce() => RandomNumberGenerator.GetBytes(NonceSize);
}
