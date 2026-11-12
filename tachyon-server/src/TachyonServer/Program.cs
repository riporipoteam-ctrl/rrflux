using System;
using System.Collections.Concurrent;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Net;
using System.Reflection;
using System.Security.Cryptography;
using System.Text;
using System.Threading;
using LiteNetLib;
using LiteNetLib.Utils;
using TachyonWire;

namespace TachyonServer;

/// <summary>
/// Tachyon — the self-hosted Rec Room (2026 client) room/voice session server.
///
/// Stack, per the reference docs in ../../docs:
///   LiteNetLib (stock 1.3.1, protocol 13) + RecRoomClientEncryptLayer on every
///   datagram -> forked-NGO batches (24-byte header, enforced XXHash) ->
///   Tachyon handshake (RSA -> AES key exchange, "Carrot" approval) ->
///   Dissonance packets relayed inside NGO named messages.
///
/// Milestone 1 scope: transport + handshake + approval + session roster,
/// single default instance. See STATUS.md for what is and is not established.
/// </summary>
internal static class Program
{
    /// <summary>Build version string for startup self-identification (v6).</summary>
    public const string BuildVersion = NgoMessages.ServerBuildVersion;

    private static readonly object LogLock = new();
    private static RSA _serverKey;
    private static RecRoomClientEncryptLayer _encryptLayer;
    private static string _defaultInstance = "default";
    private static int _nextClientId; // first Interlocked.Increment -> clientId 1 (0 = the server)
    private static ushort _nextDissonanceId = 1;
    // Stock Dissonance session id: one random nonzero u32 per server
    // process (BaseServer ctor: `while (_sessionId == 0) _sessionId =
    // (uint)random.Next();`). Carried by every post-handshake packet.
    private static uint _dissonanceSessionId;
    private static readonly ConcurrentDictionary<NetPeer, VoiceSession> Sessions = new();

    // v7 server tick clock. Stock NGO's NetworkTickSystem starts at tick 0
    // when the server starts and advances at the configured TickRate
    // (stock default 30). ONE clock feeds both the approval's NetworkTick
    // and the TimeSync stream, exactly as stock feeds both from the same
    // NetworkTickSystem — a TimeSync tick on a different base than the
    // approval tick would slam the client's time system backwards.
    private static readonly DateTime _serverStartUtc = DateTime.UtcNow;
    private const int ServerTickRate = 30;
    private static long _timeSyncSentTotal;
    private static int ServerTick() =>
        (int)((DateTime.UtcNow - _serverStartUtc).TotalSeconds * ServerTickRate);

    private sealed class VoiceSession
    {
        public NetPeer Peer;
        public bool Approved;
        /// <summary>True when this peer's messages carry the size prefix
        /// (MessageHeader framing); false for the legacy v1–v3 layout.
        /// Replies mirror the framing the peer used for its request.</summary>
        public bool Framed;
        public ulong ClientId;
        public ushort DissonancePlayerId;
        public string AccountId;
        public string InstanceId;
        public List<MessageVersionData> Versions;
        public int NamedMessageIndex = -1;
        public int TimeSyncIndex = -1;
        public int UnnamedIndex = -1;
        public int ServerLogIndex = -1;
        public int DisconnectReasonIndex = -1;
        public byte[] SessionKey;
        // Dissonance (stock) state, captured from this client's own
        // HandshakeRequest / ClientState packets (v5). The name + codec are
        // what the stock server echoes back in ClientState packets.
        public string DissonancePlayerName;
        public NgoMessages.DissonanceCodecSettings DissonanceCodec;
        public List<string> DissonanceRooms = new();
        public bool DissonanceHandshakeDone;
        // v7 TimeSync stream state (stock: server->client, 1/sec).
        public int TimeSyncSentCount;
        public bool TimeSyncFirstLogged;
        public DateTime ConnectedUtc = DateTime.UtcNow;
        // Ground-truth dump state (one full dump per peer + first length).
        public bool RequestDumped;
        public int FirstRequestLength = -1;
    }

    private static StreamWriter _logFile;
    private static string _logFilePath;

    private static void Log(string msg)
    {
        lock (LogLock)
        {
            Console.WriteLine($"[{DateTime.UtcNow:HH:mm:ss.fff}] {msg}");
            if (_logFile != null)
            {
                // Mirror every line to server.log (same dir as the key files)
                // with a full UTC timestamp, flushed per line: the launcher
                // spawns this process with stdio=null, so the file is the
                // only post-mortem evidence when a real client misbehaves.
                try
                {
                    _logFile.WriteLine($"[{DateTime.UtcNow:yyyy-MM-ddTHH:mm:ss.fffZ}] {msg}");
                    _logFile.Flush();
                }
                catch
                {
                    // File logging is best-effort — never let it kill the server.
                }
            }
        }
    }

    /// <summary>Open (append) server.log in the key dir. Console-only if it fails.</summary>
    private static void InitFileLog(string keyDir)
    {
        try
        {
            Directory.CreateDirectory(keyDir);
            _logFilePath = Path.Combine(keyDir, "server.log");
            var stream = new FileStream(_logFilePath, FileMode.Append, FileAccess.Write, FileShare.Read);
            _logFile = new StreamWriter(stream);
        }
        catch (Exception e)
        {
            _logFile = null;
            Console.WriteLine($"[log] WARNING: could not open file log in '{keyDir}': {e.Message} (console only)");
        }
    }

    private static int Main(string[] args)
    {
        // ------------------------------------------------------------ config
        int port = GetIntArg(args, "--port") ?? GetEnvInt("TACHYON_PORT") ?? 7777;
        string keyDir = GetArg(args, "--key-dir") ?? Environment.GetEnvironmentVariable("TACHYON_KEY_DIR") ?? DefaultKeyDir();
        _defaultInstance = GetArg(args, "--instance") ?? Environment.GetEnvironmentVariable("TACHYON_INSTANCE") ?? "default";
        int autoStopSeconds = GetEnvInt("TACHYON_AUTOSTOP_SECONDS") ?? 0;

        InitFileLog(keyDir);

        // v6 startup self-identification: build version + sha256 of the
        // OWN running executable (Environment.ProcessPath). Up to v5 the
        // log never said which binary was running, which is exactly how a
        // stale v4 process masqueraded as v5 on Armin's PC. Fail-soft:
        // log "unknown" if the path/hash cannot be obtained.
        string exePathForLog = "unknown";
        string exeShaForLog = "unknown";
        try
        {
            string pp = Environment.ProcessPath;
            if (!string.IsNullOrEmpty(pp))
            {
                exePathForLog = pp;
                if (File.Exists(pp))
                {
                    using var fs = File.OpenRead(pp);
                    exeShaForLog = Convert.ToHexString(SHA256.HashData(fs)).ToLowerInvariant();
                }
            }
        }
        catch
        {
            exeShaForLog = "unknown";
        }
        Log($"=== TachyonServer (Flux Rec) starting === build={BuildVersion} version={BuildVersion} exeSha256={exeShaForLog} exePath={exePathForLog}");
        if (_logFilePath != null) Log($"file log: {_logFilePath}");
        _dissonanceSessionId = (uint)RandomNumberGenerator.GetInt32(1, int.MaxValue);
        Log($"[voice] Dissonance session id: {_dissonanceSessionId} (0x{_dissonanceSessionId:X8}) — random nonzero u32 per process, per stock BaseServer");
        Log($"LiteNetLib protocol id: {LiteNetLibProtocolId()} (docs fingerprint: 13)");

        // ------------------------------------------------------------ RSA keypair
        _serverKey = LoadOrCreateServerKey(keyDir);
        var pubParams = _serverKey.ExportParameters(false);
        string modulusFingerprint = RsaUtil.Fingerprint(pubParams); // SHA256(modulus), first 6 bytes hex
        Log($"server RSA key: {RsaUtil.DescribeSize(_serverKey)}, modulus sha256 fingerprint {modulusFingerprint} (fp8 {modulusFingerprint[..8]})");
        Log($"public key for client config (VoiceKeyXml): {Path.Combine(keyDir, "voice-key-public.xml")}");

        // ------------------------------------------------------------ transport
        var listener = new EventBasedNetListener();
        _encryptLayer = new RecRoomClientEncryptLayer { Log = m => Log(m) };
        var layer = _encryptLayer;
        var net = new NetManager(listener, layer)
        {
            PingInterval = 1000,
            DisconnectTimeout = 30000,
            // Start(port) binds IPv4 — VoiceConnectionInfo endpoints are IPv4 literals.
        };

        listener.ConnectionRequestEvent += request =>
        {
            try
            {
                byte[] connectData = request.Data.GetRemainingBytes();
                Log($"[transport] connection request from {request.RemoteEndPoint}, connect data {connectData.Length}B: {Convert.ToHexString(connectData)}");
                request.Accept();
            }
            catch (Exception e)
            {
                Log($"[transport] EXCEPTION in ConnectionRequest handler for {request.RemoteEndPoint}: {e}");
            }
        };
        listener.PeerConnectedEvent += peer =>
        {
            try
            {
                Sessions[peer] = new VoiceSession { Peer = peer, InstanceId = _defaultInstance };
                Log($"[transport] LiteNetLib peer connected: id={peer.Id} endpoint={peer} (ConnectAccept flowed through RecRoomClientEncryptLayer)");
            }
            catch (Exception e)
            {
                Log($"[transport] EXCEPTION in PeerConnected handler for {peer}: {e}");
            }
        };
        listener.PeerDisconnectedEvent += (peer, info) =>
        {
            try
            {
                layer.ClearKey(new IPEndPoint(peer.Address, peer.Port));
                if (Sessions.TryRemove(peer, out var session) && session.Approved)
                {
                    Log($"[session] account {session.AccountId} (client {session.ClientId}, peer id={peer.Id} endpoint={peer}) left instance '{session.InstanceId}': {info.Reason}");
                    // Docs (voice-stale-playback-on-leave.md): clients cannot
                    // clean up a departed player's playback objects by
                    // themselves — the server must broadcast a stock
                    // Dissonance RemoveClient for the departed player id.
                    byte[] removePacket = NgoMessages.BuildRemoveClientPacket(session.DissonancePlayerId);
                    int sent = 0;
                    foreach (var other in Sessions.Values.Where(s => s.Approved && s.InstanceId == session.InstanceId).ToList())
                    {
                        SendMessage(other, NgoMessages.BuildNamedMessageBody(NgoMessages.NamedDissonanceToClient, removePacket));
                        sent++;
                    }
                    Log($"[voice] broadcast Dissonance RemoveClient for playerId {session.DissonancePlayerId} (account {session.AccountId}) to {sent} remaining client(s) — stale-playback cleanup per docs");
                    LogRoster();
                }
                else
                {
                    Log($"[transport] peer disconnected before approval: id={peer.Id} endpoint={peer} ({info.Reason})");
                }
            }
            catch (Exception e)
            {
                Log($"[transport] EXCEPTION in PeerDisconnected handler for {peer}: {e}");
            }
        };
        listener.NetworkReceiveEvent += (peer, reader, channel, method) =>
        {
            byte[] data = reader.GetRemainingBytes();
            try
            {
                HandleDatagram(peer, data);
            }
            catch (Exception e)
            {
                Log($"[ngo] EXCEPTION handling datagram from peer id={peer.Id} endpoint={peer}: {e}");
            }
        };
        listener.NetworkErrorEvent += (endPoint, error) =>
            Log($"[transport] network error from {endPoint}: {error}");

        if (!net.Start(port))
        {
            Log($"FATAL: could not bind UDP port {port}");
            _logFile?.Dispose();
            return 1;
        }
        Log($"LISTENING udp/{port} instance='{_defaultInstance}'");

        // ------------------------------------------------------------ main loop
        var stopAt = autoStopSeconds > 0 ? DateTime.UtcNow.AddSeconds(autoStopSeconds) : (DateTime?)null;
        var lastHeartbeat = DateTime.UtcNow;
        var lastTimeSync = DateTime.UtcNow;
        bool quit = false;
        Console.CancelKeyPress += (_, e) => { e.Cancel = true; quit = true; };
        while (!quit)
        {
            try
            {
                net.PollEvents();
            }
            catch (Exception e)
            {
                Log($"[server] EXCEPTION in PollEvents (continuing): {e}");
            }
            if (stopAt.HasValue && DateTime.UtcNow >= stopAt.Value) break;
            // v7: stock TimeSync cadence — 1 message per second to every
            // approved client (NetworkTimeSystem k_TimeSyncFrequency=1.0).
            if (DateTime.UtcNow - lastTimeSync >= TimeSpan.FromSeconds(1))
            {
                lastTimeSync = DateTime.UtcNow;
                try
                {
                    SendTimeSyncToApproved();
                }
                catch (Exception e)
                {
                    Log($"[ngo] EXCEPTION in TimeSync pump (continuing): {e}");
                }
            }
            if (DateTime.UtcNow - lastHeartbeat >= TimeSpan.FromSeconds(60))
            {
                lastHeartbeat = DateTime.UtcNow;
                string layerStats = layer.StatsSummary();
                Log($"HEARTBEAT peers={Sessions.Count} approved={Sessions.Values.Count(s => s.Approved)} timeSyncSent={Interlocked.Read(ref _timeSyncSentTotal)}" +
                    (layerStats.Length > 0 ? " | encrypt layer drops: " + layerStats : ""));
            }
            Thread.Sleep(15);
        }
        net.Stop();
        Log("server stopped");
        _logFile?.Dispose();
        return 0;
    }

    // ==================================================================== NGO

    private static string Hex(byte[] bytes, int max) =>
        bytes.Length <= max
            ? Convert.ToHexString(bytes)
            : Convert.ToHexString(bytes, 0, max) + $"…(+{bytes.Length - max}B more)";

    private static void HandleDatagram(NetPeer peer, byte[] data)
    {
        if (!NgoBatch.TryParseHeader(data, data.Length, out int batchCount, out string error))
        {
            Log($"[ngo] dropping malformed batch from {peer}: {error}");
            return;
        }
        if (!Sessions.TryGetValue(peer, out var session))
        {
            session = new VoiceSession { Peer = peer, InstanceId = _defaultInstance };
            Sessions[peer] = session;
        }

        // Ground truth first: dump the raw request batch BEFORE any parsing,
        // so even a parse throw leaves decodable evidence in server.log.
        // (The 24-byte header itself is not in doubt — TryParseHeader above
        // enforces magic + BatchSize + the client's XXHash64 over the
        // payload, so the body offset 24 is proven by the client's own hash.)
        if (!session.Approved)
            DumpRequestGroundTruth(peer, session, data, batchCount);

        if (NgoMessages.TrySegmentFramed(data, NgoBatch.HeaderSize, data.Length - NgoBatch.HeaderSize, out var segments))
        {
            // Size-prefixed (MessageHeader) framing: messages are
            // self-delimiting, so every message in the batch is routed and
            // unknown ones are skipped without desynchronising the rest.
            if (segments.Count != batchCount)
                Log($"[ngo] note: framed segmentation found {segments.Count} message(s), header BatchCount={batchCount} from {peer} — trusting segmentation");
            foreach (var seg in segments)
            {
                var cursor = new ByteCursor(data, seg.BodyOffset, seg.BodyLength);
                RouteMessage(peer, session, seg.Index, cursor, framed: true, data);
            }
        }
        else
        {
            // Legacy (v1–v3) layout: [packed index][body…] with no size
            // field. Kept as a receive fallback for peers that do not frame.
            var cursor = new ByteCursor(data, NgoBatch.HeaderSize, data.Length - NgoBatch.HeaderSize);
            for (int i = 0; i < batchCount; i++)
            {
                if (cursor.Remaining <= 0)
                {
                    Log($"[ngo] batch from {peer} truncated at message {i}/{batchCount}");
                    break;
                }
                int messageIndex = (int)cursor.ReadPackedUnsigned();
                if (!RouteMessage(peer, session, messageIndex, cursor, framed: false, data))
                    break;
            }
        }
    }

    /// <summary>
    /// Route one message. Returns false when batch parsing must stop
    /// (legacy framing: an unmodelled message owns the rest of the batch).
    /// </summary>
    private static bool RouteMessage(NetPeer peer, VoiceSession session, int messageIndex, ByteCursor cursor, bool framed, byte[] datagram)
    {
        if (!session.Approved)
        {
            HandleConnectionRequest(peer, session, messageIndex, cursor, framed, datagram);
            return framed; // a legacy request consumes the rest of its batch
        }

        if (messageIndex == session.NamedMessageIndex && session.NamedMessageIndex >= 0)
        {
            // v5 full capture FIRST (before any handling): the named type +
            // the FULL body hex (≤512B), parsed or not. v1–v4 logged no hex
            // on the named-parse failure path, which is exactly where the
            // real client's first post-approval message died invisibly.
            byte[] rawBody = cursor.ReadRemaining();
            string typeDesc = rawBody.Length >= 2
                ? $"type={(ushort)(rawBody[0] | (rawBody[1] << 8))} ({NamedTypeName((ushort)(rawBody[0] | (rawBody[1] << 8)))})"
                : "type=<none: body shorter than the u16 type>";
            Log($"[ngo] NamedMessage from client {session.ClientId} (account {session.AccountId}): {typeDesc} body {rawBody.Length}B hex={Hex(rawBody, 512)}");
            var bodyCursor = new ByteCursor(rawBody, 0, rawBody.Length);
            if (NgoMessages.TryParseNamedMessageBodyDual(bodyCursor, out ushort namedType, out byte[] rest, out bool isStockU64, out ulong stockHash))
            {
                if (isStockU64)
                {
                    // v6 dual-parse fallback: u16 was not a known fork type
                    // and the body carried >=8 bytes — interpreted as a
                    // stock-NGO u64 name hash. Capture only: do not crash,
                    // do not relay blindly.
                    Log($"[ngo] stock-u64 named header fallback, hash=0x{stockHash:X16} from client {session.ClientId} (account {session.AccountId}): payload {rest.Length}B hex={Hex(rest, 512)} — capture only, not relayed (fork u16 parse is primary per docs; this path settles the stock-u64 claim in code)");
                }
                else
                {
                    HandleNamedMessage(session, namedType, rest);
                }
            }
            else
                Log($"[ngo] NamedMessage body from client {session.ClientId} too short to carry a type ({rawBody.Length}B) — dropped, hex above");
            return true;
        }

        byte[] body = cursor.ReadRemaining();
        if (messageIndex == session.TimeSyncIndex && session.TimeSyncIndex >= 0)
        {
            // v7, per the stock source (TimeSyncMessage.cs): TimeSync is
            // SERVER->CLIENT ONLY — the client's Deserialize rejects any
            // owner that isn't a client, and stock clients never send one.
            // So an inbound TimeSync-index message is non-stock (the fork
            // may differ, or it's a probe): capture it with hex, fabricate
            // no reply — the stock mechanism is our own outbound 1/sec
            // stream (SendTimeSyncToApproved), not a response.
            bool parses = NgoMessages.TryParseTimeSyncBody(new ByteCursor(body, 0, body.Length), out int inboundTick);
            Log($"[ngo] TimeSyncMessage from client {session.ClientId} (index {messageIndex}, body {body.Length}B: {Hex(body, 64)}) — stock TimeSync is server->client only, so an inbound one is non-stock; captured, no reply fabricated (parsedTick={(parses ? inboundTick.ToString() : "unparseable as a single packed tick")}); the server's own 1/sec TimeSync stream is the stock mechanism");
            return true;
        }

        string resolved = ResolveIndexName(session, messageIndex);
        Log($"[ngo] unhandled message index {messageIndex} ({resolved}) from client {session.ClientId}: body {body.Length}B first-bytes={Hex(body, 32)}" +
            (framed ? " — skipped (framed batch, parsing continues)" : " — stopping batch parse here (legacy framing)"));
        return framed;
    }

    private static string NamedTypeName(ushort namedType) => namedType switch
    {
        NgoMessages.NamedDissonanceToServer => "DissonanceToServer",
        NgoMessages.NamedDissonanceToClient => "DissonanceToClient",
        NgoMessages.NamedRRClientToServerProxied => "RRClientToServerProxied",
        NgoMessages.NamedRRServerToClientProxied => "RRServerToClientProxied",
        NgoMessages.NamedRRClientToServer => "RRClientToServer",
        NgoMessages.NamedRRServerToClient => "RRServerToClient",
        NgoMessages.NamedAIManagement => "AIManagement",
        NgoMessages.NamedAIData => "AIData",
        _ => "unknown",
    };

    private static string ResolveIndexName(VoiceSession session, int index)
    {
        if (session.Versions == null) return "no version table for this session";
        if (index < 0 || index >= session.Versions.Count) return "index outside the session's version table";
        return NgoMessages.DescribeHash(session.Versions[index].Hash);
    }

    /// <summary>
    /// Raw-evidence dump of a pre-approval batch: parsed header fields, the
    /// computed body offset, candidate decodings of the first body fields,
    /// and hex. Full dump on the first attempt per peer, and again whenever
    /// the datagram length changes (identical retries add nothing).
    /// </summary>
    private static void DumpRequestGroundTruth(NetPeer peer, VoiceSession session, byte[] data, int batchCount)
    {
        if (session.RequestDumped)
        {
            if (data.Length == session.FirstRequestLength) return;
            Log($"[reqdump] {peer} request retry with a DIFFERENT length ({session.FirstRequestLength}B -> {data.Length}B) — dumping again");
        }
        session.RequestDumped = true;
        session.FirstRequestLength = data.Length;

        NgoBatch.ReadHeaderFields(data, out ushort magic, out uint batchSize, out ulong batchHash, out _);
        Log($"[reqdump] {peer} batch: len={data.Length}B header: magic=0x{magic:X4} batchSize={batchSize} batchHash=0x{batchHash:X16} batchCount={batchCount} bodyOffset={NgoBatch.HeaderSize}");

        string candidates;
        try
        {
            var p1 = new ByteCursor(data, NgoBatch.HeaderSize, data.Length - NgoBatch.HeaderSize);
            int index = (int)p1.ReadPackedUnsigned();
            int afterIndex = p1.Pos;
            var p2 = new ByteCursor(data, afterIndex, data.Length - afterIndex);
            ulong asUnsigned = p2.ReadPackedUnsigned();
            var p3 = new ByteCursor(data, afterIndex, data.Length - afterIndex);
            int asSigned = p3.ReadPackedSigned32();
            int bytesAfterField = data.Length - p3.Pos;
            candidates = $"index={index} nextField: packedUnsigned={asUnsigned} packedSigned(zigzag)={asSigned} bytesAfterField={bytesAfterField}" +
                (asUnsigned == (ulong)bytesAfterField
                    ? " — packedUnsigned EQUALS the remaining bytes: the field is a message SIZE prefix (MessageHeader framing)"
                    : "");
        }
        catch (Exception e)
        {
            candidates = "field probe failed: " + e.Message;
        }
        Log($"[reqdump] decoded: {candidates}");
        Log($"[reqdump] datagram[0..128]={Hex(data, 128)}");
        int bodyHexLen = Math.Min(96, data.Length - NgoBatch.HeaderSize);
        Log($"[reqdump] body[0..{bodyHexLen}]={Convert.ToHexString(data, NgoBatch.HeaderSize, bodyHexLen)}");
    }

    private static void HandleConnectionRequest(NetPeer peer, VoiceSession session, int messageIndex, ByteCursor cursor, bool framed, byte[] datagram)
    {
        NgoMessages.ConnectionRequest req;
        try
        {
            req = NgoMessages.ParseConnectionRequest(cursor);
        }
        catch (Exception e)
        {
            if (!framed)
            {
                Log($"[handshake] unparseable ConnectionRequestMessage from {peer}: {e.Message}");
                return;
            }
            // The size-prefixed slice did not parse as a request — retry the
            // v1–v3 whole-remainder parse before giving up (the raw bytes are
            // already preserved by the reqdump lines).
            Log($"[handshake] framed parse of ConnectionRequestMessage from {peer} failed ({e.Message}) — retrying legacy whole-batch parse");
            try
            {
                var legacy = new ByteCursor(datagram, NgoBatch.HeaderSize, datagram.Length - NgoBatch.HeaderSize);
                messageIndex = (int)legacy.ReadPackedUnsigned();
                req = NgoMessages.ParseConnectionRequest(legacy);
                framed = false;
            }
            catch (Exception e2)
            {
                Log($"[handshake] unparseable ConnectionRequestMessage from {peer}: {e2.Message}");
                return;
            }
        }
        session.Framed = framed;

        Log($"[handshake] ConnectionRequestMessage at index {messageIndex} from peer id={peer.Id} endpoint={peer}: " +
            $"framing={(framed ? "size-prefixed (MessageHeader)" : "legacy (no size prefix)")} " +
            $"{req.Versions.Count} message versions, tail {req.RawTail.Length}B " +
            $"(first bytes: {Convert.ToHexString(req.RawTail.Take(24).ToArray())})");
        // The real client sends ~1298 version entries; itemising every one
        // would flood server.log (each line is flushed) on every connect.
        // Itemise only entries that resolve to known internal types — those
        // are the indices this server actually acts on.
        int knownLogged = 0;
        for (int i = 0; i < req.Versions.Count; i++)
        {
            string desc = NgoMessages.DescribeHash(req.Versions[i].Hash);
            if (desc == "unknown") continue;
            Log($"[handshake]   versions[{i}] hash=0x{req.Versions[i].Hash:X8} ({desc}) version={req.Versions[i].Version}");
            knownLogged++;
        }
        if (req.Versions.Count > knownLogged)
            Log($"[handshake]   (+{req.Versions.Count - knownLogged} non-internal version entries, not itemised)");

        if (req.AuthJson == null)
        {
            Log("[handshake] no JSON AuthPayload found in connection data — cannot run key exchange");
            return;
        }

        var outcome = TachyonHandshake.Process(req.AuthJson, _serverKey, m => Log(m));
        if (outcome.FatalError != null)
        {
            Log($"[handshake] FAILED for peer id={peer.Id} endpoint={peer} account={outcome.AccountId ?? "?"}: {outcome.FatalError}");
            return;
        }
        foreach (var w in outcome.Warnings)
            Log($"[handshake] warning: {w}");

        session.ClientId = (ulong)Interlocked.Increment(ref _nextClientId); // first client = 1 (0 is the server)
        session.DissonancePlayerId = _nextDissonanceId++;
        session.AccountId = outcome.AccountId;
        session.Versions = req.Versions;
        session.NamedMessageIndex = NgoMessages.FindTypeIndex(req.Versions, "NamedMessage");
        session.TimeSyncIndex = NgoMessages.FindTypeIndex(req.Versions, "TimeSyncMessage");
        session.UnnamedIndex = NgoMessages.FindTypeIndex(req.Versions, "UnnamedMessage");
        session.ServerLogIndex = NgoMessages.FindTypeIndex(req.Versions, "ServerLogMessage");
        session.DisconnectReasonIndex = NgoMessages.FindTypeIndex(req.Versions, "DisconnectReasonMessage");
        session.SessionKey = outcome.SessionKey;
        // The client arms its encrypt layer as soon as it reads approval.
        // Install this peer's key before sending approval so the immediate
        // encrypted response can be decrypted on the first receive callback.
        _encryptLayer.SetKey(new IPEndPoint(peer.Address, peer.Port), outcome.SessionKey);
        session.Approved = true;

        int approvalIndex = NgoMessages.FindTypeIndex(req.Versions, "ConnectionApprovedMessage");
        if (approvalIndex < 0)
        {
            approvalIndex = 2;
            Log("[handshake] WARNING: ConnectionApprovedMessage hash not found in client versions — falling back to index 2");
        }

        // Reply in the framing the peer used for its request. The
        // approval's NetworkTick comes from the server tick clock (v7) —
        // the same clock the TimeSync stream reports, as in stock NGO.
        // The body is built ONCE and then framed/wrapped, so the hex
        // logged below is exactly the body that went on the wire in both
        // framing modes (v7's legacy path rebuilt the body with a second
        // ServerTick() call — same format, potentially a tick apart).
        byte[] approvalBody = NgoMessages.BuildApprovalBody(
            req.Versions, session.ClientId, ServerTick(), outcome.ApprovalPayloadJson);
        byte[] approval = framed
            ? NgoMessages.FrameMessage(approvalIndex, approvalBody)
            : NgoMessages.WithIndex(approvalIndex, approvalBody);
        SendBatch(peer, approval);

        Log($"[handshake] APPROVED clientId={session.ClientId} account={outcome.AccountId} " +
            $"vbSelfTest={(outcome.VbSelfTestPassed ? "PASS" : "FAIL")} " +
            $"tokenAccount={(outcome.TokenAccountId ?? "?")} env={(outcome.Environment ?? "?")} " +
            $"accessTokenLen={outcome.AccessTokenLength} approvalIndex={approvalIndex} namedIndex={session.NamedMessageIndex}");
        // Ground truth for the approval body; this makes the exact sent
        // bytes available for comparison with a live-client capture.
        int hexBytes = Math.Min(128, approvalBody.Length);
        Log($"[ngo] approval body sent to client {session.ClientId}: {approvalBody.Length}B, " +
            $"payload {(outcome.ApprovalPayloadJson?.Length ?? 0)} chars, " +
            $"first {hexBytes}B hex: {Convert.ToHexString(approvalBody.AsSpan(0, hexBytes))}");
        Log($"[ngo] client {session.ClientId} message indices: request={messageIndex} approval={approvalIndex} named={session.NamedMessageIndex} timesync={session.TimeSyncIndex} unnamed={session.UnnamedIndex} serverlog={session.ServerLogIndex} disconnect={session.DisconnectReasonIndex}");
        Log($"SESSION_KEY_SHA256={Convert.ToHexString(SHA256.HashData(outcome.SessionKey)).ToLowerInvariant()}");
        LogRoster();
    }

    private static void HandleNamedMessage(VoiceSession session, ushort namedType, byte[] rest)
    {
        switch (namedType)
        {
            case NgoMessages.NamedDissonanceToServer:
            {
                // v5: `rest` is everything after the u16 named type. The i32
                // WriteValueSafe length prefix exists only on this Dissonance
                // send path (docs) — unwrap it when it fits exactly and the
                // stock magic follows; otherwise the whole rest is the packet.
                byte[] packet = NgoMessages.UnwrapDissonancePayload(rest, out bool hadPrefix);
                string magic = packet.Length >= 2
                    ? $"0x{(packet[0] << 8) | packet[1]:X4}" : "n/a";
                bool hasType = NgoMessages.TryGetDissonanceType(packet, out byte dType);
                string dName = hasType ? NgoMessages.DissonanceTypeName(dType) : "no-type-byte";

                // Server-directed Dissonance control plane: per the stock
                // type table (docs: dissonance-named-message-framing.md),
                // HandshakeRequest / ErrorWrongSession / DeltaChannelState
                // are addressed TO the server. v1–v3 relayed them to other
                // peers like voice data — wrong, and it hid the voice-session
                // setup entirely. Consume them here with full capture.
                if (hasType && dType == 4) // HandshakeRequest
                {
                    HandleDissonanceHandshakeRequest(session, packet);
                    break;
                }
                if (hasType && dType == 1) // ClientState (client -> server state update)
                {
                    // Stock: the server records the sender's state and
                    // broadcasts the packet verbatim to all other clients
                    // (v6 implements the broadcast v5 left out).
                    if (NgoMessages.TryParseClientState(packet, out uint pktSession, out string csName,
                            out ushort csId, out var csCodec, out var csRooms))
                    {
                        session.DissonancePlayerName = csName;
                        session.DissonanceCodec = csCodec;
                        session.DissonanceRooms = csRooms ?? new List<string>();
                        // v6: stock verbatim broadcast — forward the packet
                        // verbatim to every OTHER approved session in the
                        // same instance, wrapped as DissonanceToClient
                        // (stock ProcessClientState behaviour v5 left out).
                        int csBroadcast = 0;
                        foreach (var other in Sessions.Values.Where(s => s.Approved && s.InstanceId == session.InstanceId && !ReferenceEquals(s, session)).ToList())
                        {
                            SendMessage(other, NgoMessages.BuildNamedMessageBody(NgoMessages.NamedDissonanceToClient, packet));
                            csBroadcast++;
                        }
                        Log($"[voice] Dissonance ClientState from account {session.AccountId} ({packet.Length}B): player='{csName}' id={csId} codec={csCodec.Codec}/{csCodec.FrameSize}/{csCodec.SampleRate} rooms=[{string.Join(",", session.DissonanceRooms)}] session=0x{pktSession:X8}{(pktSession == _dissonanceSessionId ? "" : $" (MISMATCH vs ours 0x{_dissonanceSessionId:X8})")} — state recorded, verbatim broadcast to {csBroadcast} other client(s)");
                    }
                    else
                    {
                        Log($"[voice] Dissonance ClientState from account {session.AccountId} ({packet.Length}B): {Hex(packet, 512)} — stock parse failed, consumed (not relayed)");
                    }
                    break;
                }
                if (hasType && (dType == 6 || dType == 9)) // ErrorWrongSession, DeltaChannelState
                {
                    Log($"[voice] Dissonance {dName} (type {dType}) from account {session.AccountId} ({packet.Length}B): {Hex(packet, 64)} — server-directed control packet, consumed (not relayed)");
                    break;
                }
                if (hasType && (dType == 0 || dType == 5 || dType == 7 || dType == 8))
                {
                    // Server->client types arriving FROM a client: not part of
                    // the data plane; capture rather than relay nonsense.
                    Log($"[voice] Dissonance {dName} (type {dType}) from account {session.AccountId} ({packet.Length}B): {Hex(packet, 64)} — server-to-client type received from a client; consumed (not relayed)");
                    break;
                }

                int relayed = 0;
                foreach (var other in Sessions.Values.Where(s => s.Approved && s.InstanceId == session.InstanceId && !ReferenceEquals(s, session)).ToList())
                {
                    SendMessage(other, NgoMessages.BuildNamedMessageBody(NgoMessages.NamedDissonanceToClient, packet));
                    relayed++;
                }
                Log($"[voice] Dissonance packet from account {session.AccountId}: {packet.Length}B magic={magic} (stock=0x8BC7) inner={dName}({(hasType ? dType.ToString() : "?")}) lengthPrefix={(hadPrefix ? "present (unwrapped)" : "absent")} relayed to {relayed} peer(s) in instance '{session.InstanceId}'");
                break;
            }
            case NgoMessages.NamedRRClientToServerProxied:
            {
                // v7 proxied relay (kind 3 -> 4), per the patch-2026
                // maintainers' measured contract (their AGENTS.md,
                // "Transport fields and approval observations"): kinds 3/4
                // carry proxied room events, and the measured
                // server->client proxied header is [int32 sender][byte
                // event][int32 cache index] (reader 0x3D33C10) then the
                // unchanged tagged payload. The client->server payload
                // already begins with [byte event][int32 cache index]
                // [tagged payload] — which is exactly why the
                // server->client reader sees sender, then event, then
                // cache index — so the forwarded content is the sender's
                // NGO clientId (int32 LE) prepended to the original
                // payload, byte-for-byte unchanged after that. Room kinds
                // are sent RAW ([u16 type][payload], no Dissonance i32
                // length wrapper — see BuildRawNamedMessageBody).
                byte eventByte = rest.Length >= 1 ? rest[0] : (byte)0;
                int cacheIndex = rest.Length >= 5
                    ? rest[1] | (rest[2] << 8) | (rest[3] << 16) | (rest[4] << 24)
                    : -1;
                var fwd = new List<byte>(4 + rest.Length);
                BytePacker.WriteI32LE(fwd, (int)session.ClientId);
                fwd.AddRange(rest);
                byte[] fwdBody = NgoMessages.BuildRawNamedMessageBody(
                    NgoMessages.NamedRRServerToClientProxied, fwd.ToArray());
                int proxiedSent = 0;
                foreach (var other in Sessions.Values.Where(s => s.Approved && s.InstanceId == session.InstanceId && !ReferenceEquals(s, session)).ToList())
                {
                    SendMessage(other, fwdBody);
                    proxiedSent++;
                }
                Log($"[room] proxied relay (kind 3->4) from account {session.AccountId} (client {session.ClientId}): event={eventByte} cacheIndex={cacheIndex} payload {rest.Length}B, sender {session.ClientId} prepended -> {proxiedSent} recipient(s) in instance '{session.InstanceId}'");
                break;
            }
            case NgoMessages.NamedRRClientToServer:
            {
                // v7 room-data relay (kind 5 -> 6): forwarded VERBATIM —
                // no header is measured for kinds 5/6, so none is added —
                // to every OTHER approved session in the instance, as the
                // server->client kind 6, raw named framing (no i32 wrapper).
                byte[] roomBody = NgoMessages.BuildRawNamedMessageBody(
                    NgoMessages.NamedRRServerToClient, rest);
                int roomSent = 0;
                foreach (var other in Sessions.Values.Where(s => s.Approved && s.InstanceId == session.InstanceId && !ReferenceEquals(s, session)).ToList())
                {
                    SendMessage(other, roomBody);
                    roomSent++;
                }
                Log($"[room] room-data relay (kind 5->6) from account {session.AccountId} (client {session.ClientId}): {rest.Length}B first-bytes={Hex(rest, 64)} -> {roomSent} recipient(s) in instance '{session.InstanceId}'");
                break;
            }
            case NgoMessages.NamedRRServerToClientProxied:
            case NgoMessages.NamedRRServerToClient:
                // Server->client kinds arriving FROM a client: not part of
                // the client->server lanes; capture rather than relay.
                Log($"[room] server->client room kind (type {namedType}) received from account {session.AccountId}: {rest.Length}B first-bytes={Hex(rest, 64)} — consumed, not relayed");
                break;
            default:
                Log($"[ngo] named message type {namedType} from account {session.AccountId}: {rest.Length}B — logged only");
                break;
        }
    }

    /// <summary>
    /// Answer a stock Dissonance HandshakeRequest exactly as the stock
    /// server does (BroadcastingClientCollection.ProcessHandshakeRequest,
    /// current-generation Dissonance source — see NgoMessages):
    ///   1. HandshakeResponse to the joiner: session id + the joiner's
    ///      assigned ushort id, with the client/room lists EMPTY (stock
    ///      passes its cleared buffer; the protocol doc explicitly blesses
    ///      sending state as ClientState packets right after instead).
    ///   2. One ClientState packet per handshaken member of the instance —
    ///      INCLUDING the joiner itself (stock's GetOrCreate registers the
    ///      joiner before GetClients runs) — each sent to the JOINER only,
    ///      reliable, in that order.
    /// The player name + codec are parsed out of the request with the stock
    /// reader layout (CodecSettings first, then the name string). A request
    /// whose name cannot be parsed (or is null — stock ignores those) is
    /// logged with full hex and NOT answered; nothing is fabricated.
    /// </summary>
    private static void HandleDissonanceHandshakeRequest(VoiceSession session, byte[] packet)
    {
        if (!NgoMessages.TryParseHandshakeRequest(packet, out var codec, out string playerName) || playerName == null)
        {
            Log($"[voice] Dissonance HandshakeRequest from account {session.AccountId} ({packet.Length}B): {Hex(packet, 512)} — stock parse failed or null player name; NOT answered (stock server ignores null-name handshakes)");
            return;
        }

        session.DissonancePlayerName = playerName;
        session.DissonanceCodec = codec;
        session.DissonanceHandshakeDone = true;
        Log($"[voice] Dissonance HandshakeRequest from account {session.AccountId}: player='{playerName}' codec={codec.Codec} frameSize={codec.FrameSize} sampleRate={codec.SampleRate} assignedDissonanceId={session.DissonancePlayerId} ({packet.Length}B): {Hex(packet, 96)}");

        byte[] response = NgoMessages.BuildHandshakeResponsePacket(_dissonanceSessionId, session.DissonancePlayerId);
        SendMessage(session, NgoMessages.BuildNamedMessageBody(NgoMessages.NamedDissonanceToClient, response));
        Log($"[voice] sent Dissonance HandshakeResponse to account {session.AccountId}: session=0x{_dissonanceSessionId:X8} clientId={session.DissonancePlayerId} clients=0 rooms=0 (stock on-join form; state burst follows)");

        var members = Sessions.Values
            .Where(s => s.Approved && s.InstanceId == session.InstanceId && s.DissonanceHandshakeDone)
            .OrderBy(s => s.DissonancePlayerId)
            .ToList();
        foreach (var member in members)
        {
            byte[] state = NgoMessages.BuildClientStatePacket(
                _dissonanceSessionId, member.DissonancePlayerName, member.DissonancePlayerId,
                member.DissonanceCodec, member.DissonanceRooms);
            SendMessage(session, NgoMessages.BuildNamedMessageBody(NgoMessages.NamedDissonanceToClient, state));
        }
        Log($"[voice] sent {members.Count} Dissonance ClientState packet(s) to account {session.AccountId} (stock on-join state burst: one per handshaken member incl. self, joiner only)");
    }

    /// <summary>Send a NamedMessage body to a session, framed the way that peer frames.</summary>
    private static void SendMessage(VoiceSession recipient, byte[] namedBody)
    {
        byte[] msg = recipient.Framed
            ? NgoMessages.FrameMessage(recipient.NamedMessageIndex, namedBody)
            : NgoMessages.WithIndex(recipient.NamedMessageIndex, namedBody);
        SendBatch(recipient.Peer, msg);
    }

    private static void SendBatch(NetPeer peer, byte[] singleMessage)
    {
        byte[] batch = NgoBatch.Build(new[] { singleMessage });
        var writer = new NetDataWriter();
        writer.Put(batch);
        peer.Send(writer, DeliveryMethod.ReliableOrdered);
    }

    /// <summary>
    /// The stock TimeSync stream (v7). Stock NGO's NetworkTimeSystem sends
    /// TimeSyncMessage ONLY from the server: OnTickSyncTime fires once per
    /// second (k_TimeSyncFrequency = 1.0) with
    /// `Tick = NetworkTickSystem.ServerTime.Tick` to every connected
    /// client, and the client's Handle() syncs its local time system to
    /// that tick. Body = one bit-packed tick (NgoMessages.BuildTimeSyncBody),
    /// sent on the session's own TimeSync index in the peer's framing mode
    /// (same rule as approvals). Stock delivery is Unreliable; this server
    /// sends all batches ReliableOrdered (approval, named) and keeps that
    /// here — a 1/sec small message, ordering harmless.
    /// Logging: first send per session is logged; the running total rides
    /// the 60s HEARTBEAT (timeSyncSent=) instead of a line per second.
    /// </summary>
    private static void SendTimeSyncToApproved()
    {
        int tick = ServerTick();
        byte[] body = NgoMessages.BuildTimeSyncBody(tick);
        foreach (var session in Sessions.Values.Where(s => s.Approved && s.TimeSyncIndex >= 0).ToList())
        {
            byte[] msg = session.Framed
                ? NgoMessages.FrameMessage(session.TimeSyncIndex, body)
                : NgoMessages.WithIndex(session.TimeSyncIndex, body);
            SendBatch(session.Peer, msg);
            session.TimeSyncSentCount++;
            Interlocked.Increment(ref _timeSyncSentTotal);
            if (!session.TimeSyncFirstLogged)
            {
                session.TimeSyncFirstLogged = true;
                Log($"[ngo] TimeSync -> client {session.ClientId} (account {session.AccountId}): tick={tick} — first of the stock 1/sec server->client TimeSync stream (body = one bit-packed tick, per stock TimeSyncMessage.cs / NetworkTimeSystem.cs)");
            }
        }
    }

    private static void LogRoster()
    {
        var approved = Sessions.Values.Where(s => s.Approved).ToList();
        if (approved.Count == 0)
        {
            Log("[roster] (empty)");
            return;
        }
        foreach (var group in approved.GroupBy(s => s.InstanceId))
        {
            string members = string.Join(", ", group.Select(s => $"{s.AccountId} (client {s.ClientId}, dissonance {s.DissonancePlayerId})"));
            Log($"[roster] instance '{group.Key}': {members}");
        }
    }

    // ==================================================================== keys & config

    private static RSA LoadOrCreateServerKey(string keyDir)
    {
        Directory.CreateDirectory(keyDir);
        string privatePath = Path.Combine(keyDir, "server-key.xml");
        string publicPath = Path.Combine(keyDir, "voice-key-public.xml");

        RSA rsa;
        if (File.Exists(privatePath))
        {
            rsa = RsaUtil.FromXml(File.ReadAllText(privatePath));
            Log($"loaded server RSA key from {privatePath}");
        }
        else
        {
            rsa = RSA.Create(2048);
            File.WriteAllText(privatePath, RsaUtil.ToXml(rsa, includePrivate: true));
            TryRestrictPrivateKeyFile(privatePath);
            Log($"generated new 2048-bit server RSA key -> {privatePath}");
        }
        // The public half is what ships to clients as VoiceKeyXml; always (re)emit it.
        File.WriteAllText(publicPath, RsaUtil.ToXml(rsa, includePrivate: false));
        return rsa;
    }

    private static void TryRestrictPrivateKeyFile(string path)
    {
        try
        {
            if (OperatingSystem.IsLinux() || OperatingSystem.IsMacOS())
                File.SetUnixFileMode(path, UnixFileMode.UserRead | UnixFileMode.UserWrite);
        }
        catch
        {
            // best effort only
        }
    }

    private static string DefaultKeyDir()
    {
        string home = Environment.GetFolderPath(Environment.SpecialFolder.UserProfile);
        string workspace = Path.Combine(home, "workspace", "tachyon-server");
        return Directory.Exists(workspace) ? workspace : Environment.CurrentDirectory;
    }

    private static string GetArg(string[] args, string name)
    {
        for (int i = 0; i < args.Length - 1; i++)
            if (args[i] == name) return args[i + 1];
        return null;
    }

    private static int? GetIntArg(string[] args, string name)
    {
        string v = GetArg(args, name);
        return v != null && int.TryParse(v, out int n) ? n : null;
    }

    private static int? GetEnvInt(string name)
    {
        string v = Environment.GetEnvironmentVariable(name);
        return v != null && int.TryParse(v, out int n) ? n : null;
    }

    private static string LiteNetLibProtocolId()
    {
        var field = typeof(NetConstants).GetField("ProtocolId",
            BindingFlags.Public | BindingFlags.NonPublic | BindingFlags.Static);
        return field?.GetValue(null)?.ToString() ?? "unknown";
    }
}
