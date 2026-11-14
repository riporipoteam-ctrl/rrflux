using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.IO;
using System.Linq;
using System.Security.Cryptography;
using System.Text;
using System.Text.Json;
using System.Threading;
using LiteNetLib;
using LiteNetLib.Utils;
using TachyonWire;

namespace TachyonTestClient;

/// <summary>
/// Throwaway self-test client (NOT shipped). Plays the role of the real
/// client's TachyonClient: LiteNetLib connect through the
/// RecRoomClientEncryptLayer (plaintext 0x11 framing), then an NGO
/// ConnectionRequestMessage carrying a genuine AuthPayload built exactly per
/// docs (RSA PKCS#1 v1.5 wrapping, AES-256-CBC auth token, CPK chained from
/// AT's last block), then verifies the Carrot approval and derives the
/// session key (CKA XOR SessionKeyB). Prints SESSION_KEY_SHA256 so the
/// harness can compare it against the server's independently computed key.
/// </summary>
internal static class Program
{
    private static int Main(string[] args)
    {
        string host = GetArg(args, "--host") ?? "127.0.0.1";
        int port = int.TryParse(GetArg(args, "--port"), out int p) ? p : 7777;
        string account = GetArg(args, "--account") ?? "424242";
        string keyXmlPath = GetArg(args, "--key-xml") ?? DefaultPublicKeyPath();
        int holdMs = int.TryParse(GetArg(args, "--hold-ms"), out int h) ? h : 0;
        int versionsCount = int.TryParse(GetArg(args, "--versions"), out int vn) ? vn : 7;
        bool expectRelay = args.Contains("--expect-relay");
        // v4: the wire framing is [index][size][body] (MessageHeader). The
        // --legacy-framing flag exercises the server's v1–v3 receive fallback.
        bool legacyFraming = args.Contains("--legacy-framing");

        try
        {
            return Run(host, port, account, keyXmlPath, holdMs, expectRelay, versionsCount, legacyFraming);
        }
        catch (Exception e)
        {
            Console.WriteLine("SELFTEST_RESULT=FAIL exception: " + e.GetType().Name + ": " + e.Message);
            return 1;
        }
    }

    private static int Run(string host, int port, string account, string keyXmlPath, int holdMs, bool expectRelay, int versionsCount, bool legacyFraming)
    {
        if (!File.Exists(keyXmlPath))
        {
            Console.WriteLine("SELFTEST_RESULT=FAIL server public key file not found: " + keyXmlPath);
            return 1;
        }
        using var serverPubKey = RsaUtil.FromXml(File.ReadAllText(keyXmlPath));
        Console.WriteLine($"[client] server public key {RsaUtil.DescribeSize(serverPubKey)} loaded from {keyXmlPath}");

        // ------------------------------------------------ client key material
        using var clientRsa = RSA.Create(1024); // mirrors the real client's keypair size
        using var aes = Aes.Create();           // fresh AES-256 key + IV per connection (per docs)
        byte[] aesKey = aes.Key, aesIv = aes.IV;

        string authTokenJson = JsonSerializer.Serialize(new Dictionary<string, object>
        {
            ["accountId"] = account,
            ["environment"] = "selftest",
            ["accessToken"] = "selftest-access-token",
        });
        byte[] at = TachyonHandshake.AesCbcEncrypt(Encoding.UTF8.GetBytes(authTokenJson), aesKey, aesIv);

        // CPK: AES-CBC of the public CspBlob, CBC chain continuing from AT's last block.
        byte[] cpkBlob = RsaUtil.BuildPublicKeyCspBlob(clientRsa.ExportParameters(false));
        var chainedIv = new byte[16];
        Buffer.BlockCopy(at, at.Length - 16, chainedIv, 0, 16);
        byte[] cpk = TachyonHandshake.AesCbcEncrypt(cpkBlob, aesKey, chainedIv);

        byte[] vb = RsaUtil.EncryptPkcs1(serverPubKey, TachyonHandshake.ExpectedVerificationBlob);
        byte[] cka = RsaUtil.EncryptPkcs1(serverPubKey, aesKey);
        byte[] cia = RsaUtil.EncryptPkcs1(serverPubKey, aesIv);

        string authJson = JsonSerializer.Serialize(new Dictionary<string, object>
        {
            ["AI"] = account,
            ["AT"] = at,
            ["VB"] = vb,
            ["CKA"] = cka,
            ["CIA"] = cia,
            ["CPK"] = cpk,
        });
        Console.WriteLine($"[client] AuthPayload built ({authJson.Length} chars JSON): AI={account}, AT={at.Length}B, VB={vb.Length}B, CKA={cka.Length}B, CIA={cia.Length}B, CPK={cpk.Length}B");

        // ------------------------------------------------ message versions (client local order)
        string[] order =
        {
            "TimeSyncMessage",           // 0
            "ConnectionRequestMessage",  // 1 (per docs: ConnectionRequest sits at index 1)
            "ConnectionApprovedMessage", // 2
            "NamedMessage",              // 3
            "UnnamedMessage",            // 4
            "DisconnectReasonMessage",   // 5
            "ServerLogMessage",          // 6
        };
        var versions = order
            .Select(n => new MessageVersionData { Hash = NgoMessages.HashTypeName("Unity.Netcode." + n), Version = 1 })
            .ToList();
        // Regression coverage for the 18% stall: the real 2026 client sends
        // 1298 MessageVersions entries, which the server's original 64-entry
        // cap rejected. --versions N pads the list with plausible synthetic
        // entries (XXHash32 of invented type names, version 1) AFTER the 7
        // internal types, so the documented indices (request=1, approval=2,
        // named=3) are unchanged — the shape of the real client's list.
        for (int i = order.Length; i < versionsCount; i++)
        {
            versions.Add(new MessageVersionData
            {
                Hash = NgoMessages.HashTypeName($"Unity.Netcode.SelfTestPaddingMessage{i}"),
                Version = 1,
            });
        }
        Console.WriteLine($"[client] MessageVersions list: {versions.Count} entries (7 internal + {versions.Count - 7} synthetic)");

        // ------------------------------------------------ transport
        var listener = new EventBasedNetListener();
        var layer = new RecRoomClientEncryptLayer { Log = m => Console.WriteLine(m) };
        var net = new NetManager(listener, layer);
        net.Start();
        var connectWriter = new NetDataWriter();
        connectWriter.Put(new byte[] { 0x01, 0x00 }); // observed LiteNetLibTransport connect data (+ layer's 0x11 on the wire)
        NetPeer peer = net.Connect(host, port, connectWriter);
        Console.WriteLine($"[client] LiteNetLib connecting to {host}:{port} through RecRoomClientEncryptLayer...");

        bool approved = false;
        bool failed = false;
        bool probeSent = false;
        bool timeSyncSent = false;
        bool stockU64Sent = false;
        bool clientStateSent = false;
        bool roomProxiedSent = false;
        bool roomDataSent = false;
        bool joinSent = false;
        bool roomUpdateSent = false;
        bool joinResponseReceived = false;
        bool roomUpdateEchoReceived = false;
        ulong ownerClientId = 0;
        bool timeSyncReplyReceived = false;
        int timeSyncReplyTick = -1;
        bool handshakeSent = false;
        bool handshakeAnswered = false;
        bool clientStateReceived = false;
        uint dissonanceSession = 0;
        ushort assignedDissonanceId = 0;
        bool relayReceived = false;
        DateTime approvedAtUtc = DateTime.MinValue;
        string failReason = null;

        listener.PeerConnectedEvent += p =>
        {
            Console.WriteLine("[client] LiteNetLib peer connected (ConnectAccept received through the layer)");
            var body = new List<byte>();
            BytePacker.WritePackedSigned32(body, versions.Count);
            foreach (var v in versions)
            {
                BytePacker.WriteU32LE(body, v.Hash);
                BytePacker.WritePackedSigned32(body, v.Version);
            }
            byte[] jsonBytes = Encoding.UTF8.GetBytes(authJson);
            BytePacker.WritePackedUnsigned(body, (ulong)jsonBytes.Length);
            body.AddRange(jsonBytes);
            byte[] message = legacyFraming
                ? NgoMessages.WithIndex(1, body.ToArray())
                : NgoMessages.FrameMessage(1, body.ToArray());
            SendBatch(p, message);
            Console.WriteLine($"[client] ConnectionRequestMessage sent ({(legacyFraming ? "legacy" : "size-prefixed")} framing, AuthPayload as connection data)");
        };
        listener.PeerDisconnectedEvent += (p, info) =>
        {
            if (!approved) { failed = true; failReason = "disconnected before approval: " + info.Reason; }
        };
        listener.NetworkReceiveEvent += (p, reader, channel, method) =>
        {
            byte[] data = reader.GetRemainingBytes();
            if (!NgoBatch.TryParseHeader(data, data.Length, out int count, out string err))
            {
                failed = true; failReason = "bad batch: " + err; return;
            }
            var cursor = new ByteCursor(data, NgoBatch.HeaderSize, data.Length - NgoBatch.HeaderSize);
            int index = (int)cursor.ReadPackedUnsigned();
            if (!legacyFraming)
            {
                // Size-prefixed framing: bound the body cursor to the declared size.
                int size = (int)cursor.ReadPackedUnsigned();
                if (size < 0 || size > cursor.Remaining)
                {
                    failed = true; failReason = "bad framed message size " + size; return;
                }
                cursor = new ByteCursor(data, cursor.Pos, size);
            }

            if (approved)
            {
                // Post-approval traffic: relayed Dissonance packets (relay
                // test) and, since v5, the server's Dissonance handshake
                // answer (HandshakeResponse + ClientState burst).
                if (index == 3 && NgoMessages.TryParseNamedMessageBody(cursor, out ushort nType, out byte[] rest))
                {
                    if (nType == NgoMessages.NamedRRServerToClient && rest.Length > 0)
                    {
                        if (rest[0] == 101 && rest.Length >= 20 && rest[1] == 1 && rest[2] == 7 &&
                            BitConverter.ToUInt64(rest, 3) == ownerClientId && BitConverter.ToInt32(rest, 11) == 1 &&
                            rest[15] == 15 && BitConverter.ToUInt64(rest, 16) == ownerClientId)
                        {
                            joinResponseReceived = true;
                            Console.WriteLine("ROOM_JOIN_RESPONSE=PASS");
                        }
                        if (rest[0] == 102 && rest.Length == 7 && rest[1] == 4 && rest[2] == 52)
                        {
                            roomUpdateEchoReceived = true;
                            Console.WriteLine("ROOM_UPDATE_ECHO=PASS");
                        }
                        return;
                    }
                    byte[] packet = NgoMessages.UnwrapDissonancePayload(rest, out bool hadPrefix);
                    string text = Encoding.UTF8.GetString(packet);
                    Console.WriteLine($"[client] named message received: type={nType} rest={rest.Length}B packet={packet.Length}B lengthPrefix={hadPrefix} containsProbe={text.Contains("selftest-voice-probe")}");
                    if (nType == NgoMessages.NamedDissonanceToClient && text.Contains("selftest-voice-probe"))
                        relayReceived = true;
                    if (nType == NgoMessages.NamedDissonanceToClient &&
                        NgoMessages.TryGetDissonanceType(packet, out byte dType))
                    {
                        if (dType == 5 && NgoMessages.TryParseHandshakeResponse(packet, out uint sess, out ushort assignedId,
                                out int cc, out int rnc, out int rlc))
                        {
                            Console.WriteLine($"[client] Dissonance HandshakeResponse: session=0x{sess:X8} assignedId={assignedId} clients={cc} roomNames={rnc} roomLists={rlc}");
                            if (sess != 0 && assignedId != 0 && cc == 0 && rnc == 0 && rlc == 0)
                            {
                                dissonanceSession = sess;
                                assignedDissonanceId = assignedId;
                                handshakeAnswered = true;
                                Console.WriteLine("HANDSHAKE_RESPONSE=PASS");
                            }
                            else
                            {
                                Console.WriteLine("HANDSHAKE_RESPONSE=FAIL (unexpected session/id/list counts)");
                            }
                        }
                        else if (dType == 1 && NgoMessages.TryParseClientState(packet, out uint csSess, out string csName,
                                     out ushort csId, out var csCodec, out var csRooms))
                        {
                            Console.WriteLine($"[client] Dissonance ClientState: session=0x{csSess:X8} player='{csName}' id={csId} codec={csCodec.Codec}/{csCodec.FrameSize}/{csCodec.SampleRate} rooms={csRooms.Count}");
                            if (dissonanceSession != 0 && csSess == dissonanceSession && csName == account && csId == assignedDissonanceId &&
                                csCodec.Codec == 1 && csCodec.FrameSize == 960 && csCodec.SampleRate == 48000 && csRooms.Count == 0)
                            {
                                clientStateReceived = true;
                                Console.WriteLine("HANDSHAKE_CLIENTSTATE=PASS");
                            }
                            else
                            {
                                Console.WriteLine("HANDSHAKE_CLIENTSTATE=FAIL (state does not echo this client's handshake)");
                            }
                        }
                    }
                }
                else if (index == 0)
                {
                    // v7: the server's stock TimeSync stream (server->
                    // client only, 1/sec). Body must parse per the pinned
                    // layout: exactly one bit-packed signed tick.
                    if (NgoMessages.TryParseTimeSyncBody(cursor, out int tsTick) && cursor.Remaining == 0 && tsTick >= 0)
                    {
                        timeSyncReplyReceived = true;
                        timeSyncReplyTick = tsTick;
                        Console.WriteLine($"[client] server TimeSyncMessage received: tick={tsTick} (single bit-packed tick, parses clean)");
                        Console.WriteLine("TIMESYNC_REPLY=PASS");
                    }
                    else
                    {
                        Console.WriteLine($"[client] server TimeSyncMessage at index 0 did NOT parse as a single bit-packed tick (tick={tsTick}, remaining={cursor.Remaining})");
                        Console.WriteLine("TIMESYNC_REPLY=FAIL");
                    }
                }
                else
                {
                    Console.WriteLine($"[client] post-approval batch: index {index} (not a relayed named message)");
                }
                return;
            }

            Console.WriteLine($"[client] batch received: {count} message(s), first at index {index}");
            if (index != 2) { failed = true; failReason = "approval at unexpected index " + index; return; }

            var approval = NgoMessages.ParseApproval(cursor);
            Console.WriteLine($"[client] ConnectionApprovedMessage: ownerClientId={approval.OwnerClientId} tick={approval.NetworkTick} versions={approval.Versions.Count}");
            if (approval.Versions.Count != versions.Count ||
                approval.Versions.Where((v, i) => v.Hash != versions[i].Hash).Any())
            {
                failed = true; failReason = "approval MessageVersions do not echo the client's"; return;
            }
            if (approval.Payload == null) { failed = true; failReason = "approval has no Payload"; return; }

            // The live 2026 client's Deserialize reads Payload immediately
            // after NetworkTick. This parser follows that captured read
            // order; its PASS checks the encoded body and JSON, while the
            // captured client machine code is the independent layout proof.
            Console.WriteLine("APPROVAL_V10_LAYOUT=PASS");

            using var payloadDoc = JsonDocument.Parse(approval.Payload);
            string dr = payloadDoc.RootElement.GetProperty("DR").GetString();
            byte[] kt = Convert.FromBase64String(payloadDoc.RootElement.GetProperty("KT").GetString());
            Console.WriteLine($"[client] approval Payload: DR=\"{dr}\" KT={kt.Length}B");
            if (dr != "Carrot") { failed = true; failReason = "DR != \"Carrot\""; return; }

            byte[] sessionKeyB = RsaUtil.DecryptPkcs1(clientRsa, kt);
            var sessionKey = new byte[32];
            for (int i = 0; i < 32; i++) sessionKey[i] = (byte)(aesKey[i] ^ sessionKeyB[i]);
            Console.WriteLine($"SESSION_KEY_SHA256={Convert.ToHexString(SHA256.HashData(sessionKey)).ToLowerInvariant()}");
            approved = true;
            ownerClientId = approval.OwnerClientId;
            approvedAtUtc = DateTime.UtcNow;
        };

        var sw = Stopwatch.StartNew();
        while (sw.Elapsed < TimeSpan.FromSeconds(20))
        {
            net.PollEvents();
            if (failed) break;
            if (approved)
            {
                double sinceApprovalMs = (DateTime.UtcNow - approvedAtUtc).TotalMilliseconds;
                byte[] Frame(int idx, byte[] msgBody) => legacyFraming
                    ? NgoMessages.WithIndex(idx, msgBody)
                    : NgoMessages.FrameMessage(idx, msgBody);
                if (!probeSent && sinceApprovalMs > 250)
                {
                    // Voice DATA probe: stock Dissonance packet, magic 0x8BC7 + type 2 (VoiceData).
                    var packet = new List<byte> { 0x8B, 0xC7, 0x02 };
                    packet.AddRange(Encoding.UTF8.GetBytes("selftest-voice-probe"));
                    SendBatch(peer, Frame(3, NgoMessages.BuildNamedMessageBody(NgoMessages.NamedDissonanceToServer, packet.ToArray())));
                    probeSent = true;
                    Console.WriteLine("[client] DissonanceToServer VoiceData named message sent (" + packet.Count + "B stock packet)");
                }
                if (!timeSyncSent && sinceApprovalMs > 400)
                {
                    // Post-approval inbound TimeSync probe (index 0 in the
                    // client's local order). Since v7 the layout IS pinned
                    // (stock TimeSync is server->client only), so this is a
                    // NON-STOCK inbound message: the server must capture it
                    // with hex and survive. The actual stock mechanism —
                    // the server's own 1/sec TimeSync stream — is asserted
                    // separately in the receive handler (TIMESYNC_REPLY).
                    var body = new List<byte>();
                    BytePacker.WritePackedSigned32(body, 123456);
                    body.AddRange(BitConverter.GetBytes(0.001d));
                    SendBatch(peer, Frame(0, body.ToArray()));
                    timeSyncSent = true;
                    Console.WriteLine("[client] TimeSyncMessage probe sent (" + body.Count + "B body)");
                }
                if (!stockU64Sent && sinceApprovalMs > 475)
                {
                    // v6 probe: stock-NGO style named message — 8-byte u64
                    // name hash header (LE 0x1122334455667788; its low u16
                    // 0x7788 is NOT a known fork NamedMessageType) + payload.
                    // The server's dual parse must log the stock-u64 fallback
                    // path, capture only, and STAY ALIVE/APPROVED — proven by
                    // the handshake probe below still being answered after it.
                    var stockBody = new List<byte>();
                    ulong stockHash = 0x1122334455667788UL;
                    for (int i = 0; i < 8; i++) stockBody.Add((byte)(stockHash >> (8 * i)));
                    stockBody.AddRange(Encoding.UTF8.GetBytes("stock-u64-probe"));
                    SendBatch(peer, Frame(3, stockBody.ToArray()));
                    stockU64Sent = true;
                    Console.WriteLine("[client] stock-u64 named header probe sent (hash=0x1122334455667788, " + stockBody.Count + "B body)");
                    Console.WriteLine("STOCK_U64_PROBE_SENT=PASS");
                }
                if (!handshakeSent && sinceApprovalMs > 550)
                {
                    // Stock Dissonance HandshakeRequest (type 4): magic +
                    // CodecSettings + player name, NO session id (the one
                    // packet without one). Since v5 the server must ANSWER
                    // it (HandshakeResponse + ClientState burst, asserted in
                    // the receive handler above).
                    byte[] packet = NgoMessages.BuildHandshakeRequestPacket(account,
                        new NgoMessages.DissonanceCodecSettings { Codec = 1, FrameSize = 960, SampleRate = 48000 });
                    SendBatch(peer, Frame(3, NgoMessages.BuildNamedMessageBody(NgoMessages.NamedDissonanceToServer, packet)));
                    handshakeSent = true;
                    Console.WriteLine("[client] Dissonance HandshakeRequest probe sent (" + packet.Length + "B stock packet)");
                }
                if (!roomProxiedSent && sinceApprovalMs > 625)
                {
                    // v7 probe: kind-3 proxied room event. Payload per the
                    // measured client->server shape: [byte event][int32 LE
                    // cache index][tagged payload]. Sent RAW ([u16 type]
                    // [payload], no Dissonance i32 wrapper) via
                    // BuildRawNamedMessageBody. The server must log the
                    // kind 3->4 relay line (0 recipients single-client).
                    var proxiedPayload = new List<byte> { 42 }; // event byte
                    proxiedPayload.AddRange(BitConverter.GetBytes(777)); // cache index, int32 LE
                    proxiedPayload.AddRange(Encoding.UTF8.GetBytes("room-proxied-probe"));
                    SendBatch(peer, Frame(3, NgoMessages.BuildRawNamedMessageBody(NgoMessages.NamedRRClientToServerProxied, proxiedPayload.ToArray())));
                    roomProxiedSent = true;
                    Console.WriteLine("[client] kind-3 proxied room probe sent (event=42 cacheIndex=777, " + proxiedPayload.Count + "B payload)");
                    Console.WriteLine("ROOM_PROXIED_PROBE_SENT=PASS");
                }
                if (!roomDataSent && sinceApprovalMs > 700)
                {
                    // v7 probe: kind-5 room data, payload verbatim. The
                    // server must log the kind 5->6 relay line (0
                    // recipients single-client).
                    byte[] roomPayload = Encoding.UTF8.GetBytes("room-data-probe");
                    SendBatch(peer, Frame(3, NgoMessages.BuildRawNamedMessageBody(NgoMessages.NamedRRClientToServer, roomPayload)));
                    roomDataSent = true;
                    Console.WriteLine("[client] kind-5 room-data probe sent (" + roomPayload.Length + "B payload)");
                    Console.WriteLine("ROOM_DATA_PROBE_SENT=PASS");
                }
                if (!joinSent && sinceApprovalMs > 775)
                {
                    var join = new List<byte> { 100 };
                    foreach (string value in new[] { account, "selftest" })
                    {
                        join.AddRange(BitConverter.GetBytes(value.Length));
                        join.AddRange(Encoding.Unicode.GetBytes(value));
                    }
                    join.AddRange(new byte[] { 52, 0, 0, 0, 0 });
                    SendBatch(peer, Frame(3, NgoMessages.BuildRawNamedMessageBody(NgoMessages.NamedRRClientToServer, join.ToArray())));
                    joinSent = true;
                }
                if (!roomUpdateSent && joinResponseReceived && sinceApprovalMs > 850)
                {
                    byte[] update = { 102, 4, 52, 0, 0, 0, 0 };
                    SendBatch(peer, Frame(3, NgoMessages.BuildRawNamedMessageBody(NgoMessages.NamedRRClientToServer, update)));
                    roomUpdateSent = true;
                }
                if (!clientStateSent && handshakeAnswered && clientStateReceived)
                {
                    // v6 probe: send own stock ClientState (type 1) — the
                    // server must record it and verbatim-broadcast it to
                    // other sessions (0 others in this single-client run;
                    // the broadcast log line is asserted by the harness).
                    byte[] csPacket = NgoMessages.BuildClientStatePacket(dissonanceSession, account, assignedDissonanceId,
                        new NgoMessages.DissonanceCodecSettings { Codec = 1, FrameSize = 960, SampleRate = 48000 },
                        new List<string>());
                    SendBatch(peer, Frame(3, NgoMessages.BuildNamedMessageBody(NgoMessages.NamedDissonanceToServer, csPacket)));
                    clientStateSent = true;
                    Console.WriteLine("[client] Dissonance ClientState probe sent (" + csPacket.Length + "B stock packet)");
                    Console.WriteLine("CLIENTSTATE_PROBE_SENT=PASS");
                }
                bool holdDone = sinceApprovalMs >= Math.Max(holdMs, 1100);
                bool relayDone = !expectRelay || relayReceived;
                if (probeSent && timeSyncSent && stockU64Sent && roomProxiedSent && roomDataSent && joinResponseReceived && roomUpdateEchoReceived && timeSyncReplyReceived && clientStateSent && handshakeSent && handshakeAnswered && clientStateReceived && holdDone && relayDone) break;
            }
            Thread.Sleep(10);
        }
        net.Stop();

        if (failed)
        {
            Console.WriteLine("SELFTEST_RESULT=FAIL " + failReason);
            return 1;
        }
        if (!approved)
        {
            Console.WriteLine("SELFTEST_RESULT=FAIL timeout waiting for approval");
            return 1;
        }
        if (!handshakeAnswered || !clientStateReceived)
        {
            Console.WriteLine($"SELFTEST_RESULT=FAIL Dissonance handshake not answered (response={handshakeAnswered} clientState={clientStateReceived})");
            return 1;
        }
        if (!stockU64Sent || !clientStateSent)
        {
            Console.WriteLine($"SELFTEST_RESULT=FAIL v6 probes not sent (stockU64={stockU64Sent} clientState={clientStateSent})");
            return 1;
        }
        if (!roomProxiedSent || !roomDataSent || !timeSyncReplyReceived)
        {
            Console.WriteLine($"SELFTEST_RESULT=FAIL v7 checks incomplete (roomProxied={roomProxiedSent} roomData={roomDataSent} timeSyncReply={timeSyncReplyReceived} tick={timeSyncReplyTick})");
            return 1;
        }
        if (!joinResponseReceived || !roomUpdateEchoReceived)
        {
            Console.WriteLine($"SELFTEST_RESULT=FAIL room join/update (join={joinResponseReceived} echo={roomUpdateEchoReceived})");
            return 1;
        }
        if (expectRelay)
        {
            Console.WriteLine(relayReceived ? "RELAY_RECEIVED=PASS" : "RELAY_RECEIVED=FAIL");
            if (!relayReceived)
            {
                Console.WriteLine("SELFTEST_RESULT=FAIL no relayed voice packet received");
                return 1;
            }
        }
        Console.WriteLine("SELFTEST_RESULT=PASS");
        return 0;
    }

    private static void SendBatch(NetPeer peer, byte[] singleMessage)
    {
        byte[] batch = NgoBatch.Build(new[] { singleMessage });
        var writer = new NetDataWriter();
        writer.Put(batch);
        peer.Send(writer, DeliveryMethod.ReliableOrdered);
    }

    private static string GetArg(string[] args, string name)
    {
        for (int i = 0; i < args.Length - 1; i++)
            if (args[i] == name) return args[i + 1];
        return null;
    }

    private static string DefaultPublicKeyPath()
    {
        string env = Environment.GetEnvironmentVariable("TACHYON_KEY_DIR");
        if (!string.IsNullOrEmpty(env)) return Path.Combine(env, "voice-key-public.xml");
        string home = Environment.GetFolderPath(Environment.SpecialFolder.UserProfile);
        string workspace = Path.Combine(home, "workspace", "tachyon-server", "voice-key-public.xml");
        return File.Exists(workspace) ? workspace : Path.Combine(Environment.CurrentDirectory, "voice-key-public.xml");
    }
}
