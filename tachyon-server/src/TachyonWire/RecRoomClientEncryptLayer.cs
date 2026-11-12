using System;
using System.Collections.Concurrent;
using System.Collections.Generic;
using System.Net;
using System.Net.Sockets;
using System.Threading;
using LiteNetLib.Layers;

namespace TachyonWire;

/// <summary>
/// Server-side twin of RecRoomClientEncryptLayer. It keeps the 32-byte
/// negotiated key per peer and decrypts the client's Rabbit-framed datagrams.
/// It identifies the client's key half only after an NGO batch validates.
/// Server datagrams use the client's documented plaintext receive branch.
/// </summary>
public sealed class RecRoomClientEncryptLayer : PacketLayerBase
{
    public const byte PlaintextMarker = 0x11;
    public const byte EncryptedMarker = 0x2A;
    public const int NonceSize = 8;
    public const int KeySize = 32;

    // LiteNetLib may hand the same remote peer back as either IPv4 or an
    // IPv4-mapped IPv6 endpoint, and IPEndPoint itself is not a stable map key
    // across all socket implementations. Store the normalized address value,
    // not the mutable endpoint object passed in by the transport.
    private readonly record struct EndpointKey(AddressFamily Family, string AddressHex, long ScopeId, int Port)
    {
        public static EndpointKey From(IPEndPoint endpoint)
        {
            var address = endpoint.Address;
            if (address.IsIPv4MappedToIPv6)
                address = address.MapToIPv4();

            long scopeId = address.AddressFamily == AddressFamily.InterNetworkV6
                ? address.ScopeId
                : 0;
            return new EndpointKey(
                address.AddressFamily,
                Convert.ToHexString(address.GetAddressBytes()),
                scopeId,
                endpoint.Port);
        }

        public override string ToString() =>
            $"{Family}:{AddressHex}%{ScopeId}:{Port}";
    }

    private sealed class EndpointCipher
    {
        public readonly object Sync = new();
        public readonly byte[] Key;
        public int InboundKeyOffset = -1;
        public long Decrypted;
        public long Rejected;
        public EndpointCipher(byte[] key) => Key = key;
    }

    private readonly ConcurrentDictionary<EndpointKey, EndpointCipher> _keys = new();
    private readonly ConcurrentDictionary<EndpointKey, byte> _firstMarkerByEndPoint = new();

    private sealed class EndpointStats
    {
        public long Total;
        public long EncryptedDrops;
        public long UnknownDrops;
        public byte LastMarker;
        public bool SawEncrypted;
    }

    private readonly ConcurrentDictionary<EndpointKey, EndpointStats> _statsByEndPoint = new();
    private EndpointStats StatsFor(EndpointKey endPoint) =>
        _statsByEndPoint.GetOrAdd(endPoint, _ => new EndpointStats());

    public Action<string> Log { get; set; } = msg => Console.WriteLine("[layer] " + msg);

    public RecRoomClientEncryptLayer() : base(9) { }

    public bool HasKey => !_keys.IsEmpty;

    /// <summary>Install the negotiated 32-byte session key for exactly this peer.</summary>
    public void SetKey(IPEndPoint endPoint, byte[] key)
    {
        if (endPoint == null) throw new ArgumentNullException(nameof(endPoint));
        if (key == null || key.Length != KeySize)
            throw new ArgumentException("Tachyon session key must be exactly 32 bytes", nameof(key));
        var endpointKey = EndpointKey.From(endPoint);
        _keys[endpointKey] = new EndpointCipher((byte[])key.Clone());
        Log($"RecRoomClientEncryptLayer session key installed for {endPoint} (normalized peer={endpointKey}; per-peer; 32 bytes)");
    }

    public void ClearKey(IPEndPoint endPoint)
    {
        if (endPoint == null) return;
        var endpointKey = EndpointKey.From(endPoint);
        _keys.TryRemove(endpointKey, out _);
        _firstMarkerByEndPoint.TryRemove(endpointKey, out _);
        _statsByEndPoint.TryRemove(endpointKey, out _);
        Log($"RecRoomClientEncryptLayer session key cleared for {endPoint}");
    }

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

    public override void ProcessOutBoundPacket(ref IPEndPoint endPoint, ref byte[] data, ref int offset, ref int length)
    {
        // The client explicitly accepts 0x11 on inbound packets even after
        // SetKey (it strips only the marker and performs no key check). Keep
        // server traffic on this documented, proven path; only client->server
        // datagrams require Rabbit decryption. This avoids guessing which
        // half the client uses for the opposite direction.
        data[offset + length] = PlaintextMarker;
        length++;
    }

    public override void ProcessInboundPacket(ref IPEndPoint endPoint, ref byte[] data, ref int length)
    {
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
        if (length <= 0) return;
        byte marker = data[length - 1];
        var endpointKey = EndpointKey.From(endPoint);
        var stats = StatsFor(endpointKey);
        Interlocked.Increment(ref stats.Total);
        if (_firstMarkerByEndPoint.TryAdd(endpointKey, marker))
        {
            string kind = marker == PlaintextMarker ? "plaintext" : marker == EncryptedMarker ? "ENCRYPTED" : "UNKNOWN";
            Log($"first inbound datagram from {endPoint}: framing marker 0x{marker:X2} ({kind})");
        }
        else if (marker == EncryptedMarker && stats.LastMarker == PlaintextMarker && !stats.SawEncrypted)
        {
            stats.SawEncrypted = true;
            Log($"endpoint {endPoint} switched to ENCRYPTED framing (0x11 -> 0x2A)");
        }
        stats.LastMarker = marker;

        if (marker == PlaintextMarker)
        {
            length--;
            return;
        }
        if (marker != EncryptedMarker)
        {
            Interlocked.Increment(ref stats.UnknownDrops);
            Log($"Discarding packet from {endPoint} with unknown marker 0x{marker:X2}");
            length = 0;
            return;
        }

        long drops = Interlocked.Increment(ref stats.EncryptedDrops);
        if (length <= NonceSize + 1)
        {
            if (drops <= 3) Log($"dropping encrypted datagram from {endPoint}: too short for nonce and marker");
            length = 0;
            return;
        }
        if (!_keys.TryGetValue(endpointKey, out var state))
        {
            if (drops <= 3) Log($"dropping encrypted datagram from {endPoint} (normalized peer={endpointKey}): no per-peer session key installed");
            length = 0;
            return;
        }

        int cipherLength = length - NonceSize - 1;
        int nonceOffset = cipherLength;
        byte[] nonce = new byte[NonceSize];
        Buffer.BlockCopy(data, nonceOffset, nonce, 0, NonceSize);

        lock (state.Sync)
        {
            int chosenOffset = -1;
            byte[] clear = null;
            int batchOffset = -1;

            // Try both halves until one yields a self-consistent NGO batch
            // (magic, exact length, and XXHash64). This pins the client->server
            // Rabbit key direction from live bytes and avoids guessing based
            // on the 32-byte session-key size.
            foreach (int keyOffset in new[] { 0, 16 })
            {
                byte[] candidate = new byte[cipherLength];
                Buffer.BlockCopy(data, 0, candidate, 0, cipherLength);
                new RabbitCipher(state.Key.AsSpan(keyOffset, 16), nonce).Transform(candidate, 0, candidate.Length);
                if (TryFindNgoBatch(candidate, candidate.Length, out batchOffset))
                {
                    chosenOffset = keyOffset;
                    clear = candidate;
                    break;
                }
            }

            if (clear == null)
            {
                Interlocked.Increment(ref state.Rejected);
                if (drops <= 6)
                    Log($"dropping encrypted datagram from {endPoint}: neither Rabbit key slice produced a valid NGO batch (length={cipherLength}); waiting for a verifiable client batch");
                length = 0;
                return;
            }

            if (state.InboundKeyOffset != chosenOffset)
            {
                state.InboundKeyOffset = chosenOffset;
                Log($"Rabbit decrypt verified for {endPoint}: client->server key half={chosenOffset / 16}, NGO batch offset={batchOffset}");
            }
            Buffer.BlockCopy(clear, 0, data, 0, cipherLength);
            length = cipherLength;
            Interlocked.Increment(ref state.Decrypted);
        }
    }

    private static bool TryFindNgoBatch(byte[] data, int length, out int batchOffset)
    {
        batchOffset = -1;
        int lastOffset = Math.Min(16, length - NgoBatch.HeaderSize);
        for (int offset = 0; offset <= lastOffset; offset++)
        {
            if (data[offset] != unchecked((byte)NgoBatch.Magic) || data[offset + 1] != (byte)(NgoBatch.Magic >> 8))
                continue;
            int remaining = length - offset;
            var batch = new byte[remaining];
            Buffer.BlockCopy(data, offset, batch, 0, remaining);
            if (NgoBatch.TryParseHeader(batch, remaining, out _, out _))
            {
                batchOffset = offset;
                return true;
            }
        }
        return false;
    }
}
