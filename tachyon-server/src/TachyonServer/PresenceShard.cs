using System;
using System.Net.Http;
using System.Text.Json;
using System.Threading.Tasks;

namespace TachyonServer;

/// <summary>
/// Which voice/roster shard a peer belongs in. Without isolation, every client on one UDP
/// port shares the "default" instance and room-data/Dissonance relays cross between
/// unrelated dorm loads — the second concurrent login hangs around 18–25%.
/// </summary>
internal static class PresenceShard
{
    private static readonly HttpClient Http = new() { Timeout = TimeSpan.FromSeconds(4) };

    /// <summary>
    /// Match worker base URL (no trailing slash), e.g. https://match.example.com .
    /// When unset, isolation falls back to one shard per account id.
    /// </summary>
    private static string PresenceBase =>
        (Environment.GetEnvironmentVariable("TACHYON_PRESENCE_BASE") ?? "").TrimEnd('/');

    /// <summary>
    /// Resolves the relay instance id for an approved session.
    /// </summary>
    public static string Resolve(string accountId, Action<string> log)
    {
        if (!int.TryParse(accountId, out int id) || id <= 0)
        {
            string unknownShard = $"acc-unknown-{accountId.GetHashCode():x8}";
            log($"[presence] could not parse account id '{accountId}' — using solo shard '{unknownShard}'");
            return unknownShard;
        }

        if (PresenceBase.Length > 0)
        {
            try
            {
                string shard = FetchShardAsync(id).GetAwaiter().GetResult();
                if (!string.IsNullOrWhiteSpace(shard) &&
                    !string.Equals(shard, "default", StringComparison.OrdinalIgnoreCase))
                {
                    log($"[presence] shard '{shard}' for account {id}");
                    return shard;
                }
            }
            catch (Exception e)
            {
                log($"[presence] shard lookup failed for account {id}: {e.Message}");
            }

            try
            {
                int roomInstanceId = FetchLegacyRoomInstanceIdAsync(id).GetAwaiter().GetResult();
                if (roomInstanceId != 0)
                {
                    string legacy = $"a{id}-i{roomInstanceId}";
                    log($"[presence] shard legacy '{legacy}' for account {id}");
                    return legacy;
                }
            }
            catch (Exception e)
            {
                log($"[presence] legacy /tachyon lookup failed for account {id}: {e.Message}");
            }
        }

        string solo = $"acc-{id}";
        log($"[presence] shard solo '{solo}' for account {id} (no live match presence)");
        return solo;
    }

    private static async Task<string> FetchShardAsync(int accountId)
    {
        string url = $"{PresenceBase}/tachyon/shard?id={accountId}";
        using var res = await Http.GetAsync(url).ConfigureAwait(false);
        res.EnsureSuccessStatusCode();
        await using var stream = await res.Content.ReadAsStreamAsync().ConfigureAwait(false);
        using var doc = await JsonDocument.ParseAsync(stream).ConfigureAwait(false);
        if (doc.RootElement.ValueKind == JsonValueKind.Object &&
            doc.RootElement.TryGetProperty("shard", out var shardEl) &&
            shardEl.ValueKind == JsonValueKind.String)
        {
            return shardEl.GetString();
        }
        return null;
    }

    private static async Task<int> FetchLegacyRoomInstanceIdAsync(int accountId)
    {
        string url = $"{PresenceBase}/tachyon?id={accountId}";
        using var res = await Http.GetAsync(url).ConfigureAwait(false);
        res.EnsureSuccessStatusCode();
        await using var stream = await res.Content.ReadAsStreamAsync().ConfigureAwait(false);
        using var doc = await JsonDocument.ParseAsync(stream).ConfigureAwait(false);
        if (doc.RootElement.ValueKind == JsonValueKind.Number && doc.RootElement.TryGetInt32(out int n))
            return n;
        return 0;
    }
}