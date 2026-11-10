using System;
using System.Collections.Generic;

namespace TachyonWire;

/// <summary>
/// The forked-NGO bit-packing scheme (docs: ngo-fork-batch-and-approval.md).
/// encoded = (value &lt;&lt; 3) | numBytes, written little-endian in numBytes bytes;
/// numBytes = firstByte &amp; 7. For numBytes == 5 the value is the raw LE u32 in
/// the following 4 bytes (no shift). Signed callers ZigZag first; the packer
/// itself is unsigned and rejects values &gt; 0x1FFFFFFF for the shifted forms.
/// </summary>
public static class BytePacker
{
    public static void WritePackedUnsigned(List<byte> buf, ulong value)
    {
        if (value <= 0x1FFFFFFFUL)
        {
            int numBytes = value < 32 ? 1 : value < 8192 ? 2 : value < 2097152 ? 3 : 4;
            ulong encoded = (value << 3) | (uint)numBytes;
            for (int i = 0; i < numBytes; i++)
                buf.Add((byte)(encoded >> (8 * i)));
        }
        else if (value <= 0xFFFFFFFFUL)
        {
            buf.Add(5);
            buf.Add((byte)value);
            buf.Add((byte)(value >> 8));
            buf.Add((byte)(value >> 16));
            buf.Add((byte)(value >> 24));
        }
        else
        {
            throw new NotSupportedException(
                "value exceeds the documented 5-byte packed form: " + value);
        }
    }

    /// <summary>ZigZag for 64-bit signed values: (n &lt;&lt; 1) ^ (n &gt;&gt; 63).</summary>
    public static void WritePackedSigned(List<byte> buf, long value)
    {
        long zz = (value << 1) ^ (value >> 63);
        WritePackedUnsigned(buf, (ulong)zz);
    }

    /// <summary>ZigZag for 32-bit signed values: (n &lt;&lt; 1) ^ (n &gt;&gt; 31).</summary>
    public static void WritePackedSigned32(List<byte> buf, int value)
    {
        int zz = (value << 1) ^ (value >> 31);
        WritePackedUnsigned(buf, (uint)zz);
    }

    public static void WriteU16LE(List<byte> buf, ushort value)
    {
        buf.Add((byte)value);
        buf.Add((byte)(value >> 8));
    }

    public static void WriteU32LE(List<byte> buf, uint value)
    {
        buf.Add((byte)value);
        buf.Add((byte)(value >> 8));
        buf.Add((byte)(value >> 16));
        buf.Add((byte)(value >> 24));
    }

    public static void WriteI32LE(List<byte> buf, int value) => WriteU32LE(buf, (uint)value);
}

/// <summary>Forward-only reader over a datagram slice, companion to BytePacker.</summary>
public sealed class ByteCursor
{
    public byte[] Data;
    public int Pos;
    public int End;

    public ByteCursor(byte[] data, int pos, int length)
    {
        Data = data;
        Pos = pos;
        End = pos + length;
    }

    public int Remaining => End - Pos;

    public byte ReadByte()
    {
        if (Pos >= End) throw new FormatException("read past end of batch");
        return Data[Pos++];
    }

    public ushort ReadU16LE()
    {
        if (Remaining < 2) throw new FormatException("truncated u16");
        ushort v = (ushort)(Data[Pos] | (Data[Pos + 1] << 8));
        Pos += 2;
        return v;
    }

    public uint ReadU32LE()
    {
        if (Remaining < 4) throw new FormatException("truncated u32");
        uint v = (uint)(Data[Pos] | (Data[Pos + 1] << 8) | (Data[Pos + 2] << 16) | (Data[Pos + 3] << 24));
        Pos += 4;
        return v;
    }

    public int ReadI32LE() => (int)ReadU32LE();

    public ulong ReadPackedUnsigned()
    {
        byte b0 = ReadByte();
        int n = b0 & 7;
        if (n == 5)
            return ReadU32LE(); // raw LE u32, no shift (per doc)
        if (n < 1 || n > 4)
            throw new FormatException("invalid packed numBytes: " + n);
        ulong encoded = b0;
        for (int i = 1; i < n; i++)
            encoded |= (ulong)ReadByte() << (8 * i);
        return encoded >> 3;
    }

    public long ReadPackedSigned()
    {
        ulong v = ReadPackedUnsigned();
        return (long)(v >> 1) ^ -(long)(v & 1);
    }

    public int ReadPackedSigned32()
    {
        ulong v = ReadPackedUnsigned();
        long s = (long)(v >> 1) ^ -(long)(v & 1);
        return (int)s;
    }

    public byte[] ReadBytes(int count)
    {
        if (count < 0 || count > Remaining) throw new FormatException("truncated byte block");
        var result = new byte[count];
        Buffer.BlockCopy(Data, Pos, result, 0, count);
        Pos += count;
        return result;
    }

    public void Skip(int count)
    {
        if (count < 0 || count > Remaining) throw new FormatException("skip past end of buffer");
        Pos += count;
    }

    /// <summary>All bytes from the current position to the end of the slice.</summary>
    public byte[] ReadRemaining()
    {
        var result = new byte[Remaining];
        Buffer.BlockCopy(Data, Pos, result, 0, result.Length);
        Pos = End;
        return result;
    }
}
