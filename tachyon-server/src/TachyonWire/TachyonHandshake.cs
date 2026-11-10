using System;
using System.Collections.Generic;
using System.Security.Cryptography;
using System.Text;
using System.Text.Json;

namespace TachyonWire;

/// <summary>
/// The Tachyon connect handshake, server side
/// (docs: voice-handshake-key-exchange.md, voice-rsa-key-swap.md).
///
/// Client -> server AuthPayload JSON (Newtonsoft; byte[] = base64), all Required.Always:
///   AI  account id, plaintext decimal string
///   AT  AES-256-CBC/PKCS7(UTF8 auth JSON {"accountId","environment","accessToken"})
///   VB  RSA_Pkcs1(fixed 16-byte constant)          — built-in swap self-test
///   CKA RSA_Pkcs1(AES key)                          — RSA to the server's (swapped-in) key
///   CIA RSA_Pkcs1(AES IV)
///   CPK AES-256-CBC/PKCS7(RSA.ExportCspBlob(client keypair, false))
///       (2026 client notes: the AES-CBC chain for CPK continues from the
///        last block of the AT ciphertext, i.e. IV = last 16 bytes of AT)
///
/// Server -> client approval Payload JSON:
///   DR  Reason — must be the literal "Carrot"
///   KT  RSA_Pkcs1(SessionKeyB) under the client's CPK public key
///
/// Transport/session key = CKA(plaintext AES key) XOR SessionKeyB.
/// The server never has to validate the auth material to approve (docs), so
/// VB/AT/account mismatches are advisory warnings; only failures that make it
/// impossible to mint KT (undecryptable CKA/CIA/CPK) are fatal.
/// </summary>
public static class TachyonHandshake
{
    /// <summary>base64 "GTLFaIAoniLKqHEJFIhcGw==" — the fixed VB plaintext.</summary>
    public static readonly byte[] ExpectedVerificationBlob =
        Convert.FromBase64String("GTLFaIAoniLKqHEJFIhcGw==");

    public sealed class Outcome
    {
        public string FatalError;                 // non-null => cannot approve
        public List<string> Warnings = new();
        public string AccountId;                  // AI, plaintext
        public string TokenAccountId;             // accountId inside AT
        public string Environment;                // environment inside AT
        public int AccessTokenLength = -1;        // never log the token itself
        public bool VbSelfTestPassed;
        public bool AccountIdMatchesToken;
        public string ApprovalPayloadJson;        // {"DR":"Carrot","KT":"..."}
        public byte[] SessionKeyB;
        public byte[] SessionKey;                 // CKA XOR SessionKeyB (32 bytes)
    }

    public static Outcome Process(string authJson, RSA serverKey, Action<string> log = null)
    {
        var outcome = new Outcome();
        void Warn(string w)
        {
            outcome.Warnings.Add(w);
            log?.Invoke("[handshake] WARNING: " + w);
        }

        JsonDocument doc;
        try
        {
            doc = JsonDocument.Parse(authJson);
        }
        catch (Exception e)
        {
            outcome.FatalError = "AuthPayload is not valid JSON: " + e.Message;
            return outcome;
        }

        using (doc)
        {
            var root = doc.RootElement;
            byte[] Field(string name)
            {
                if (!root.TryGetProperty(name, out var el))
                    throw new FormatException("AuthPayload missing required field " + name);
                return Convert.FromBase64String(el.GetString());
            }

            if (!root.TryGetProperty("AI", out var aiEl))
            {
                outcome.FatalError = "AuthPayload missing required field AI";
                return outcome;
            }
            outcome.AccountId = aiEl.GetString();

            byte[] at, vb, cka, cia, cpk;
            try
            {
                at = Field("AT"); vb = Field("VB"); cka = Field("CKA"); cia = Field("CIA"); cpk = Field("CPK");
            }
            catch (Exception e)
            {
                outcome.FatalError = e.Message;
                return outcome;
            }

            // --- unwrap the client's AES key/IV with our (swapped-in) RSA key ---
            byte[] aesKey, aesIv;
            try
            {
                aesKey = RsaUtil.DecryptPkcs1(serverKey, cka);
                aesIv = RsaUtil.DecryptPkcs1(serverKey, cia);
            }
            catch (Exception e)
            {
                outcome.FatalError = "RSA decrypt of CKA/CIA failed (client not using our key?): " + e.Message;
                return outcome;
            }
            if (aesKey.Length != 32 || aesIv.Length != 16)
                Warn($"unexpected AES material sizes: key {aesKey.Length}B, iv {aesIv.Length}B (expected 32/16)");

            // --- VB self-test: proves the client encrypted to OUR key ---
            try
            {
                byte[] vbPlain = RsaUtil.DecryptPkcs1(serverKey, vb);
                outcome.VbSelfTestPassed = vbPlain.AsSpan().SequenceEqual(ExpectedVerificationBlob);
                if (!outcome.VbSelfTestPassed)
                    Warn("VB decrypted but does not match the fixed constant — key swap suspect");
            }
            catch (Exception e)
            {
                Warn("VB decrypt failed: " + e.Message);
            }

            // --- AT: the auth JSON (advisory only) ---
            try
            {
                byte[] atPlain = AesCbcDecrypt(at, aesKey, aesIv);
                using var atDoc = JsonDocument.Parse(Encoding.UTF8.GetString(atPlain));
                var atRoot = atDoc.RootElement;
                if (atRoot.TryGetProperty("accountId", out var idEl)) outcome.TokenAccountId = idEl.GetString();
                if (atRoot.TryGetProperty("environment", out var envEl)) outcome.Environment = envEl.GetString();
                if (atRoot.TryGetProperty("accessToken", out var tokEl)) outcome.AccessTokenLength = tokEl.GetString()?.Length ?? 0;
                outcome.AccountIdMatchesToken = outcome.TokenAccountId != null && outcome.TokenAccountId == outcome.AccountId;
                if (!outcome.AccountIdMatchesToken)
                    Warn($"AT accountId '{outcome.TokenAccountId}' != plaintext AI '{outcome.AccountId}'");
            }
            catch (Exception e)
            {
                Warn("AT decrypt/parse failed (auth continues unvalidated, per docs): " + e.Message);
            }

            // --- CPK: the client's RSA public key, AES-wrapped ---
            RSAParameters clientPub;
            try
            {
                clientPub = DecryptClientPublicKey(cpk, aesKey, aesIv, at);
            }
            catch (Exception e)
            {
                outcome.FatalError = "CPK decrypt/parse failed, cannot mint KT: " + e.Message;
                return outcome;
            }

            // --- mint SessionKeyB and the Carrot approval payload ---
            byte[] sessionKeyB = RandomNumberGenerator.GetBytes(32);
            byte[] kt;
            using (var clientRsa = RSA.Create())
            {
                clientRsa.ImportParameters(clientPub);
                kt = RsaUtil.EncryptPkcs1(clientRsa, sessionKeyB);
            }
            outcome.SessionKeyB = sessionKeyB;
            outcome.ApprovalPayloadJson = JsonSerializer.Serialize(new Dictionary<string, object>
            {
                ["DR"] = "Carrot",
                ["KT"] = kt,
            });

            var sessionKey = new byte[32];
            for (int i = 0; i < 32; i++)
                sessionKey[i] = (byte)(aesKey[i] ^ sessionKeyB[i]);
            outcome.SessionKey = sessionKey;
            return outcome;
        }
    }

    /// <summary>
    /// CPK arrives AES-CBC encrypted. Per the 2026 client notes the CBC chain
    /// continues from the encrypted token: IV = last 16 bytes of the AT
    /// ciphertext. Falls back to the CKA/CIA IV when the chained form fails.
    /// </summary>
    private static RSAParameters DecryptClientPublicKey(byte[] cpkCt, byte[] aesKey, byte[] aesIv, byte[] atCt)
    {
        Exception chainedError = null;
        if (atCt.Length >= 16)
        {
            try
            {
                var chainedIv = new byte[16];
                Buffer.BlockCopy(atCt, atCt.Length - 16, chainedIv, 0, 16);
                byte[] blob = AesCbcDecrypt(cpkCt, aesKey, chainedIv);
                return RsaUtil.ParsePublicKeyCspBlob(blob);
            }
            catch (Exception e)
            {
                chainedError = e;
            }
        }
        try
        {
            byte[] blob = AesCbcDecrypt(cpkCt, aesKey, aesIv);
            return RsaUtil.ParsePublicKeyCspBlob(blob);
        }
        catch (Exception)
        {
            throw chainedError ?? new FormatException("CPK decrypt failed");
        }
    }

    public static byte[] AesCbcEncrypt(byte[] plain, byte[] key, byte[] iv)
    {
        using var aes = Aes.Create();
        aes.Mode = CipherMode.CBC;
        aes.Padding = PaddingMode.PKCS7;
        using var enc = aes.CreateEncryptor(key, iv);
        return enc.TransformFinalBlock(plain, 0, plain.Length);
    }

    public static byte[] AesCbcDecrypt(byte[] cipher, byte[] key, byte[] iv)
    {
        using var aes = Aes.Create();
        aes.Mode = CipherMode.CBC;
        aes.Padding = PaddingMode.PKCS7;
        using var dec = aes.CreateDecryptor(key, iv);
        return dec.TransformFinalBlock(cipher, 0, cipher.Length);
    }
}
