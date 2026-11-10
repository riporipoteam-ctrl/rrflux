using System;
using System.Buffers.Binary;
using System.Collections.Generic;
using System.IO.Hashing;

namespace TachyonWire;

/// <summary>
/// The forked-NGO batch framing (docs: ngo-fork-batch-and-approval.md).
/// NetworkBatchHeader is 24 bytes: Magic u16 @0 (0x1160), BatchSize u32 @4,
/// BatchHash u64 @8 = XXHash64(bytes after the header, seed 0), BatchCount
/// u16 @16 (MOVED from stock NGO's offset 2 — the one real fork change).
/// Bytes [2:4] and [18:24] are padding. The hash IS enforced by the client.
/// </summary>
public static class NgoBatch
{
    public const ushort Magic = 0x1160;
    public const int HeaderSize = 24;

    public static byte[] Build(IReadOnlyList<byte[]> messages)
    {
        int payloadLength = 0;
        foreach (var m in messages) payloadLength += m.Length;

        var buf = new byte[HeaderSize + payloadLength];
        BinaryPrimitives.WriteUInt16LittleEndian(buf.AsSpan(0), Magic);
        BinaryPrimitives.WriteUInt32LittleEndian(buf.AsSpan(4), (uint)buf.Length);
        int pos = HeaderSize;
        foreach (var m in messages)
        {
            Buffer.BlockCopy(m, 0, buf, pos, m.Length);
            pos += m.Length;
        }
        ulong hash = XxHash64.HashToUInt64(new ReadOnlySpan<byte>(buf, HeaderSize, payloadLength), 0);
        BinaryPrimitives.WriteUInt64LittleEndian(buf.AsSpan(8), hash);
        BinaryPrimitives.WriteUInt16LittleEndian(buf.AsSpan(16), (ushort)messages.Count);
        return buf;
    }

    public static bool TryParseHeader(byte[] data, int length, out int batchCount, out string error)
    {
        batchCount = 0;
        error = null;
        if (length < HeaderSize)
        {
            error = "datagram shorter than the 24-byte batch header";
            return false;
        }
        ushort magic = BinaryPrimitives.ReadUInt16LittleEndian(data.AsSpan(0));
        if (magic != Magic)
        {
            error = $"bad batch magic 0x{magic:X4} (expected 0x{Magic:X4})";
            return false;
        }
        uint batchSize = BinaryPrimitives.ReadUInt32LittleEndian(data.AsSpan(4));
        if (batchSize != (uint)length)
        {
            error = $"BatchSize {batchSize} != datagram length {length}";
            return false;
        }
        ulong expected = XxHash64.HashToUInt64(new ReadOnlySpan<byte>(data, HeaderSize, length - HeaderSize), 0);
        ulong actual = BinaryPrimitives.ReadUInt64LittleEndian(data.AsSpan(8));
        if (expected != actual)
        {
            error = "BatchHash mismatch (XXHash64 of payload, seed 0)";
            return false;
        }
        batchCount = BinaryPrimitives.ReadUInt16LittleEndian(data.AsSpan(16));
        return true;
    }

    /// <summary>
    /// Raw header fields for ground-truth logging (no validation). Caller must
    /// guarantee data.Length &gt;= HeaderSize.
    /// </summary>
    public static void ReadHeaderFields(byte[] data, out ushort magic, out uint batchSize, out ulong batchHash, out int batchCount)
    {
        magic = BinaryPrimitives.ReadUInt16LittleEndian(data.AsSpan(0));
        batchSize = BinaryPrimitives.ReadUInt32LittleEndian(data.AsSpan(4));
        batchHash = BinaryPrimitives.ReadUInt64LittleEndian(data.AsSpan(8));
        batchCount = BinaryPrimitives.ReadUInt16LittleEndian(data.AsSpan(16));
    }
}
