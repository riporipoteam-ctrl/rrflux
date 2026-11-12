using System;
using System.Buffers.Binary;

namespace TachyonWire;

/// <summary>
/// Rabbit stream transform used by the 2026 Rec Room transport. Rabbit is
/// symmetric, so the same transform encrypts and decrypts a datagram.
/// Key and IV are the 16-byte key slice and 8-byte nonce supplied by the
/// client's Burst wrapper.
/// </summary>
internal sealed class RabbitCipher
{
    private static readonly uint[] CounterConstants =
    {
        0x4D34D34D, 0xD34D34D3, 0x34D34D34, 0x4D34D34D,
        0xD34D34D3, 0x34D34D34, 0x4D34D34D, 0xD34D34D3,
    };

    private readonly uint[] _x = new uint[8];
    private readonly uint[] _c = new uint[8];
    private uint _carry;

    public RabbitCipher(ReadOnlySpan<byte> key, ReadOnlySpan<byte> iv)
    {
        if (key.Length != 16) throw new ArgumentException("Rabbit key must be 16 bytes", nameof(key));
        if (iv.Length != 8) throw new ArgumentException("Rabbit IV must be 8 bytes", nameof(iv));

        uint k0 = BinaryPrimitives.ReadUInt32LittleEndian(key.Slice(0, 4));
        uint k1 = BinaryPrimitives.ReadUInt32LittleEndian(key.Slice(4, 4));
        uint k2 = BinaryPrimitives.ReadUInt32LittleEndian(key.Slice(8, 4));
        uint k3 = BinaryPrimitives.ReadUInt32LittleEndian(key.Slice(12, 4));

        _x[0] = k0;
        _x[1] = (k3 << 16) | (k2 >> 16);
        _x[2] = k1;
        _x[3] = (k0 << 16) | (k3 >> 16);
        _x[4] = k2;
        _x[5] = (k1 << 16) | (k0 >> 16);
        _x[6] = k3;
        _x[7] = (k2 << 16) | (k1 >> 16);

        _c[0] = RotateLeft(k2, 16);
        _c[1] = (k0 & 0xFFFF0000) | (k1 & 0x0000FFFF);
        _c[2] = RotateLeft(k3, 16);
        _c[3] = (k1 & 0xFFFF0000) | (k2 & 0x0000FFFF);
        _c[4] = RotateLeft(k0, 16);
        _c[5] = (k2 & 0xFFFF0000) | (k3 & 0x0000FFFF);
        _c[6] = RotateLeft(k1, 16);
        _c[7] = (k3 & 0xFFFF0000) | (k0 & 0x0000FFFF);

        for (int i = 0; i < 4; i++) NextState();
        for (int i = 0; i < 8; i++) _c[i] ^= _x[(i + 4) & 7];

        uint i0 = BinaryPrimitives.ReadUInt32LittleEndian(iv.Slice(0, 4));
        uint i2 = BinaryPrimitives.ReadUInt32LittleEndian(iv.Slice(4, 4));
        uint i1 = (i0 >> 16) | (i2 & 0xFFFF0000);
        uint i3 = (i2 << 16) | (i0 & 0x0000FFFF);
        _c[0] ^= i0; _c[1] ^= i1; _c[2] ^= i2; _c[3] ^= i3;
        _c[4] ^= i0; _c[5] ^= i1; _c[6] ^= i2; _c[7] ^= i3;

        for (int i = 0; i < 4; i++) NextState();
    }

    public void Transform(byte[] data, int offset, int length)
    {
        Span<byte> stream = stackalloc byte[16];
        int end = offset + length;
        while (offset < end)
        {
            NextState();
            uint s0 = _x[0] ^ (_x[5] >> 16) ^ (_x[3] << 16);
            uint s1 = _x[2] ^ (_x[7] >> 16) ^ (_x[5] << 16);
            uint s2 = _x[4] ^ (_x[1] >> 16) ^ (_x[7] << 16);
            uint s3 = _x[6] ^ (_x[3] >> 16) ^ (_x[1] << 16);
            BinaryPrimitives.WriteUInt32LittleEndian(stream.Slice(0, 4), s0);
            BinaryPrimitives.WriteUInt32LittleEndian(stream.Slice(4, 4), s1);
            BinaryPrimitives.WriteUInt32LittleEndian(stream.Slice(8, 4), s2);
            BinaryPrimitives.WriteUInt32LittleEndian(stream.Slice(12, 4), s3);
            int count = Math.Min(16, end - offset);
            for (int i = 0; i < count; i++) data[offset + i] ^= stream[i];
            offset += count;
        }
    }

    private void NextState()
    {
        uint carry = _carry;
        for (int i = 0; i < 8; i++)
        {
            ulong sum = (ulong)_c[i] + CounterConstants[i] + carry;
            _c[i] = (uint)sum;
            carry = (uint)(sum >> 32);
        }
        _carry = carry;

        Span<uint> g = stackalloc uint[8];
        for (int i = 0; i < 8; i++)
        {
            uint u = unchecked(_x[i] + _c[i]);
            ulong square = (ulong)u * u;
            g[i] = (uint)square ^ (uint)(square >> 32);
        }

        _x[0] = unchecked(g[0] + RotateLeft(g[7], 16) + RotateLeft(g[6], 16));
        _x[1] = unchecked(g[1] + RotateLeft(g[0], 8) + g[7]);
        _x[2] = unchecked(g[2] + RotateLeft(g[1], 16) + RotateLeft(g[0], 16));
        _x[3] = unchecked(g[3] + RotateLeft(g[2], 8) + g[1]);
        _x[4] = unchecked(g[4] + RotateLeft(g[3], 16) + RotateLeft(g[2], 16));
        _x[5] = unchecked(g[5] + RotateLeft(g[4], 8) + g[3]);
        _x[6] = unchecked(g[6] + RotateLeft(g[5], 16) + RotateLeft(g[4], 16));
        _x[7] = unchecked(g[7] + RotateLeft(g[6], 8) + g[5]);
    }

    private static uint RotateLeft(uint value, int count) =>
        (value << count) | (value >> (32 - count));
}
