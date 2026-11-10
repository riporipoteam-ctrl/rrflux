using System;
using System.Globalization;
using System.Security.Cryptography;
using System.Text;
using System.Xml;

namespace TachyonWire;

/// <summary>
/// RSA helpers for the Tachyon key exchange (docs: voice-rsa-key-swap.md,
/// voice-handshake-key-exchange.md). All RSA encryption in the handshake is
/// PKCS#1 v1.5 (the client calls RSACryptoServiceProvider.Encrypt with
/// fOAEP: false at every call site). Keys move around as .NET &lt;RSAKeyValue&gt;
/// XML (the client's hardcoded key and the VoiceKeyXml config key use exactly
/// that form) and, for the client's own keypair, as a CAPI PUBLICKEYBLOB
/// (RSA.ExportCspBlob(false)).
/// </summary>
public static class RsaUtil
{
    // ---------------------------------------------------------------- XML form

    public static string ToXml(RSA rsa, bool includePrivate)
    {
        RSAParameters p = rsa.ExportParameters(includePrivate);
        var sb = new StringBuilder();
        sb.Append("<RSAKeyValue>");
        AppendElement(sb, "Modulus", p.Modulus);
        AppendElement(sb, "Exponent", p.Exponent);
        if (includePrivate)
        {
            AppendElement(sb, "P", p.P);
            AppendElement(sb, "Q", p.Q);
            AppendElement(sb, "DP", p.DP);
            AppendElement(sb, "DQ", p.DQ);
            AppendElement(sb, "InverseQ", p.InverseQ);
            AppendElement(sb, "D", p.D);
        }
        sb.Append("</RSAKeyValue>");
        return sb.ToString();
    }

    private static void AppendElement(StringBuilder sb, string name, byte[] value)
    {
        if (value == null) return;
        sb.Append('<').Append(name).Append('>');
        sb.Append(Convert.ToBase64String(value));
        sb.Append("</").Append(name).Append('>');
    }

    public static RSA FromXml(string xml)
    {
        var doc = new XmlDocument();
        doc.LoadXml(xml);
        var root = doc.DocumentElement;
        if (root == null || root.Name != "RSAKeyValue")
            throw new FormatException("not an <RSAKeyValue> document");

        byte[] Get(string name)
        {
            var node = root.SelectSingleNode(name);
            return node == null ? null : Convert.FromBase64String(node.InnerText.Trim());
        }

        var p = new RSAParameters
        {
            Modulus = Get("Modulus"),
            Exponent = Get("Exponent"),
            P = Get("P"),
            Q = Get("Q"),
            DP = Get("DP"),
            DQ = Get("DQ"),
            InverseQ = Get("InverseQ"),
            D = Get("D"),
        };
        if (p.Modulus == null || p.Exponent == null)
            throw new FormatException("RSA XML is missing Modulus/Exponent (private fields belong on the server only)");
        var rsa = RSA.Create();
        rsa.ImportParameters(p);
        return rsa;
    }

    // ---------------------------------------------------------------- CspBlob form

    /// <summary>
    /// Parse a CAPI PUBLICKEYBLOB (what RSA.ExportCspBlob(false) emits and what
    /// the client sends, AES-wrapped, as CPK). Layout: BLOBHEADER (8 bytes:
    /// bType 0x06, bVersion, reserved, aiKeyAlg), then RSAPUBKEY (magic "RSA1",
    /// bitlen u32 LE, pubexp u32 LE), then the modulus little-endian.
    /// Parsed manually so it works identically on every platform.
    /// </summary>
    public static RSAParameters ParsePublicKeyCspBlob(byte[] blob)
    {
        if (blob == null || blob.Length < 20)
            throw new FormatException("CspBlob too short");
        if (blob[0] != 0x06)
            throw new FormatException("CspBlob is not a PUBLICKEYBLOB (bType " + blob[0] + ")");
        uint magic = BitConverter.ToUInt32(blob, 8);
        if (magic != 0x31415352) // "RSA1"
            throw new FormatException($"CspBlob RSAPUBKEY magic 0x{magic:X8} != RSA1");
        int bitlen = (int)BitConverter.ToUInt32(blob, 12);
        uint pubexp = BitConverter.ToUInt32(blob, 16);
        int modBytes = bitlen / 8;
        if (blob.Length < 20 + modBytes)
            throw new FormatException("CspBlob truncated before end of modulus");

        var modulus = new byte[modBytes];
        for (int i = 0; i < modBytes; i++)
            modulus[i] = blob[20 + modBytes - 1 - i]; // little-endian -> big-endian
        modulus = TrimLeadingZeros(modulus);

        var exponent = new byte[4];
        exponent[0] = (byte)(pubexp >> 24);
        exponent[1] = (byte)(pubexp >> 16);
        exponent[2] = (byte)(pubexp >> 8);
        exponent[3] = (byte)pubexp;
        exponent = TrimLeadingZeros(exponent);

        return new RSAParameters { Modulus = modulus, Exponent = exponent };
    }

    /// <summary>Inverse of ParsePublicKeyCspBlob (used by the self-test client).</summary>
    public static byte[] BuildPublicKeyCspBlob(RSAParameters pub)
    {
        int bitlen = pub.Modulus.Length * 8;
        uint pubexp = 0;
        foreach (byte b in pub.Exponent) pubexp = (pubexp << 8) | b;

        var blob = new byte[20 + pub.Modulus.Length];
        blob[0] = 0x06; // PUBLICKEYBLOB
        blob[1] = 0x02; // CUR_BLOB_VERSION
        BitConverter.GetBytes(0x0000A400u).CopyTo(blob, 4); // CALG_RSA_KEYX
        BitConverter.GetBytes(0x31415352u).CopyTo(blob, 8); // "RSA1"
        BitConverter.GetBytes((uint)bitlen).CopyTo(blob, 12);
        BitConverter.GetBytes(pubexp).CopyTo(blob, 16);
        for (int i = 0; i < pub.Modulus.Length; i++)
            blob[20 + i] = pub.Modulus[pub.Modulus.Length - 1 - i]; // -> little-endian
        return blob;
    }

    private static byte[] TrimLeadingZeros(byte[] bytes)
    {
        int i = 0;
        while (i < bytes.Length - 1 && bytes[i] == 0) i++;
        if (i == 0) return bytes;
        var trimmed = new byte[bytes.Length - i];
        Buffer.BlockCopy(bytes, i, trimmed, 0, trimmed.Length);
        return trimmed;
    }

    // ---------------------------------------------------------------- operations

    public static byte[] EncryptPkcs1(RSA rsa, byte[] data) =>
        rsa.Encrypt(data, RSAEncryptionPadding.Pkcs1);

    public static byte[] DecryptPkcs1(RSA rsa, byte[] data) =>
        rsa.Decrypt(data, RSAEncryptionPadding.Pkcs1);

    /// <summary>Short non-secret fingerprint of a public key, for logs.</summary>
    public static string Fingerprint(RSAParameters pub)
    {
        using var sha = SHA256.Create();
        byte[] hash = sha.ComputeHash(pub.Modulus);
        return Convert.ToHexString(hash, 0, 6);
    }

    public static string DescribeSize(RSA rsa) =>
        rsa.KeySize.ToString(CultureInfo.InvariantCulture) + "-bit";
}
