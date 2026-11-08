using System;
using System.Diagnostics;
using System.IO;
using System.Net;
using System.Net.Http;
using System.Net.Http.Headers;
using System.Security.Cryptography;
using System.Threading;
using System.Threading.Tasks;
using MCDownloader.Models;

namespace MCDownloader.Services;

public class DownloadEngine
{
    private static readonly HttpClient Client = CreateClient();

    /// <summary>分块大小，默认 4 MB</summary>
    public long ChunkSize { get; set; } = 4 * 1024 * 1024;

    /// <summary>低于此大小不分块，默认 8 MB</summary>
    public long SegThreshold { get; set; } = 8 * 1024 * 1024;

    /// <summary>全局并发上限，默认 8（跨块、跨文件统一）</summary>
    public int MaxConcurrency { get; set; } = 8;

    private static HttpClient CreateClient()
    {
        var handler = new HttpClientHandler
        {
            AutomaticDecompression = DecompressionMethods.GZip | DecompressionMethods.Deflate,
        };
        var client = new HttpClient(handler)
        {
            Timeout = Timeout.InfiniteTimeSpan,
        };
        client.DefaultRequestHeaders.UserAgent.ParseAdd("MCDownloader/0.2");
        return client;
    }

    public async Task DownloadFileAsync(
        string url,
        string destPath,
        ExpectedHash expected,
        IProgress<DownloadProgress> progress,
        CancellationToken ct)
    {
        var dir = Path.GetDirectoryName(destPath);
        if (!string.IsNullOrEmpty(dir) && !Directory.Exists(dir))
            Directory.CreateDirectory(dir);

        var probe = await ProbeAsync(url, ct).ConfigureAwait(false);

        if (probe.SupportsRange
            && probe.Total.HasValue
            && probe.Total.Value >= SegThreshold
            && ChunkSize > 0)
        {
            await DoDownloadSegmentedAsync(url, destPath, expected, probe.Total.Value, progress, ct)
                .ConfigureAwait(false);
        }
        else
        {
            await DoDownloadSingleAsync(url, destPath, expected, progress, ct)
                .ConfigureAwait(false);
        }
    }

    // ─────────────────────────────────────────────────────
    // 探测
    // ─────────────────────────────────────────────────────

    private class ProbeResult
    {
        public bool SupportsRange;
        public long? Total;
    }

    private async Task<ProbeResult> ProbeAsync(string url, CancellationToken ct)
    {
        var result = new ProbeResult { SupportsRange = false, Total = null };
        try
        {
            var request = new HttpRequestMessage(HttpMethod.Get, url);
            request.Headers.Range = new RangeHeaderValue(0, 0);

            using (var response = await Client
                .SendAsync(request, HttpCompletionOption.ResponseHeadersRead, ct)
                .ConfigureAwait(false))
            {
                if (response.StatusCode == HttpStatusCode.PartialContent)
                {
                    result.SupportsRange = true;
                    result.Total = response.Content.Headers.ContentRange?.Length;
                }
                else if (response.IsSuccessStatusCode)
                {
                    result.SupportsRange = false;
                    result.Total = response.Content.Headers.ContentLength;
                }
            }
        }
        catch (OperationCanceledException) { throw; }
        catch { /* 探测失败一律走单线程 */ }

        return result;
    }

    // ─────────────────────────────────────────────────────
    // 分块下载
    // ─────────────────────────────────────────────────────

    private async Task DoDownloadSegmentedAsync(
        string url,
        string destPath,
        ExpectedHash expected,
        long total,
        IProgress<DownloadProgress> progress,
        CancellationToken ct)
    {
        var fileName = Path.GetFileName(destPath);
        var finalPart = destPath + ".part";
        long chunkSize = ChunkSize;
        long numChunks = (total + chunkSize - 1) / chunkSize;

        // 全局已写入字节数（含各块续传前缀），用于进度报告
        long totalWritten = 0;

        // 1. 统计已有块前缀
        for (long i = 0; i < numChunks; i++)
        {
            var cp = ChunkPath(destPath, i);
            if (File.Exists(cp))
            {
                var len = new FileInfo(cp).Length;
                var expectedLen = ChunkLenAt(i, total, chunkSize);
                totalWritten += Math.Min(len, expectedLen);
            }
        }
        Interlocked.Exchange(ref totalWritten, totalWritten);
        var globalWritten = new StrongBox<long>(totalWritten);

        progress?.Report(new DownloadProgress
        {
            FileName = fileName,
            CurrentBytes = globalWritten.Value,
            TotalBytes = total,
            Percent = (double)globalWritten.Value / total * 100.0,
            Speed = "",
            Status = totalWritten > 0 ? $"分块续传 ({numChunks} 块)" : $"分块下载 ({numChunks} 块)",
        });

        // 2. 节流报告任务
        using (var reportCts = CancellationTokenSource.CreateLinkedTokenSource(ct))
        {
            var sw = Stopwatch.StartNew();
            long lastBytes = globalWritten.Value;
            var lastReport = TimeSpan.Zero;

            var reporter = Task.Run(async () =>
            {
                while (!reportCts.IsCancellationRequested)
                {
                    try
                    {
                        await Task.Delay(120, reportCts.Token).ConfigureAwait(false);
                    }
                    catch (OperationCanceledException) { return; }

                    var now = sw.Elapsed;
                    var current = Interlocked.Read(ref globalWritten.Value);
                    var dt = (now - lastReport).TotalSeconds;
                    var speed = dt > 0 ? (current - lastBytes) / dt : 0;

                    progress?.Report(new DownloadProgress
                    {
                        FileName = fileName,
                        CurrentBytes = current,
                        TotalBytes = total,
                        Percent = total > 0 ? (double)current / total * 100.0 : 0,
                        Speed = HumanSpeed(speed),
                        Status = $"分块下载 ({numChunks} 块)",
                    });

                    lastReport = now;
                    lastBytes = current;
                }
            }, reportCts.Token);

            // 3. 并发下载所有块
            var sem = new SemaphoreSlim(MaxConcurrency, MaxConcurrency);
            var chunkTasks = new Task[numChunks];
            try
            {
                for (long i = 0; i < numChunks; i++)
                {
                    long idx = i;
                    chunkTasks[i] = Task.Run(async () =>
                    {
                        await sem.WaitAsync(ct).ConfigureAwait(false);
                        try
                        {
                            await DownloadChunkAsync(
                                url, destPath, idx, chunkSize, total, globalWritten, ct)
                                .ConfigureAwait(false);
                        }
                        finally
                        {
                            sem.Release();
                        }
                    }, ct);
                }

                await Task.WhenAll(chunkTasks).ConfigureAwait(false);
            }
            finally
            {
                reportCts.Cancel();
                try { await reporter.ConfigureAwait(false); } catch { }
                sem.Dispose();
            }
        }

        // 4. 校验每块大小
        for (long i = 0; i < numChunks; i++)
        {
            var cp = ChunkPath(destPath, i);
            var expectedLen = ChunkLenAt(i, total, chunkSize);
            var actual = new FileInfo(cp).Length;
            if (actual != expectedLen)
                throw new IOException($"块 {i} 大小不匹配: 期望 {expectedLen}，实际 {actual}");
        }

        // 5. 合并 + 哈希
        progress?.Report(new DownloadProgress
        {
            FileName = fileName,
            CurrentBytes = total,
            TotalBytes = total,
            Percent = 100,
            Speed = "",
            Status = "合并中...",
        });

        await MergeChunksAsync(destPath, finalPart, numChunks, expected, ct).ConfigureAwait(false);

        progress?.Report(new DownloadProgress
        {
            FileName = fileName,
            CurrentBytes = total,
            TotalBytes = total,
            Percent = 100,
            Speed = "0 B/s",
            Status = expected != null ? "完成（校验通过）" : "完成",
        });
    }

    private async Task DownloadChunkAsync(
        string url,
        string destPath,
        long index,
        long chunkSize,
        long total,
        StrongBox<long> globalWritten,
        CancellationToken ct)
    {
        long start = index * chunkSize;
        long endExclusive = Math.Min(start + chunkSize, total);
        long chunkLen = endExclusive - start;
        var chunkPath = ChunkPath(destPath, index);

        long existing = File.Exists(chunkPath) ? new FileInfo(chunkPath).Length : 0;
        if (existing > chunkLen) existing = chunkLen;
        if (existing == chunkLen) return;   // 已完成

        long rangeStart = start + existing;
        long rangeEnd = endExclusive - 1;

        var request = new HttpRequestMessage(HttpMethod.Get, url);
        request.Headers.Range = new RangeHeaderValue(rangeStart, rangeEnd);

        using (var response = await Client
            .SendAsync(request, HttpCompletionOption.ResponseHeadersRead, ct)
            .ConfigureAwait(false))
        {
            if (response.StatusCode != HttpStatusCode.PartialContent)
                throw new IOException(
                    $"块 {index} 未返回 206（实际 {(int)response.StatusCode}），服务器可能中途不支持 Range");

            using (var src = await response.Content.ReadAsStreamAsync().ConfigureAwait(false))
            using (var fs = new FileStream(
                chunkPath,
                existing > 0 ? FileMode.Append : FileMode.Create,
                FileAccess.Write,
                FileShare.None,
                bufferSize: 81920,
                useAsync: true))
            {
                var buffer = new byte[81920];
                long written = existing;

                while (true)
                {
                    ct.ThrowIfCancellationRequested();

                    int read = await src
                        .ReadAsync(buffer, 0, buffer.Length, ct)
                        .ConfigureAwait(false);
                    if (read == 0) break;

                    await fs
                        .WriteAsync(buffer, 0, read, ct)
                        .ConfigureAwait(false);

                    written += read;
                    Interlocked.Add(ref globalWritten.Value, read);
                }

                fs.Flush();

                if (written != chunkLen)
                    throw new IOException(
                        $"块 {index} 大小不匹配: 期望 {chunkLen}，实际 {written}");
            }
        }
    }

    private async Task MergeChunksAsync(
        string destPath,
        string finalPart,
        long numChunks,
        ExpectedHash expected,
        CancellationToken ct)
    {
        HashAlgorithm hasher = expected != null ? CreateHasher(expected.Algo) : null;
        try
        {
            using (var outFs = new FileStream(
                finalPart, FileMode.Create, FileAccess.Write, FileShare.None, 81920, useAsync: true))
            {
                var buffer = new byte[81920];

                for (long i = 0; i < numChunks; i++)
                {
                    var cp = ChunkPath(destPath, i);
                    using (var inFs = new FileStream(
                        cp, FileMode.Open, FileAccess.Read, FileShare.Read, 81920, useAsync: true))
                    {
                        while (true)
                        {
                            ct.ThrowIfCancellationRequested();

                            int read = await inFs
                                .ReadAsync(buffer, 0, buffer.Length, ct)
                                .ConfigureAwait(false);
                            if (read == 0) break;

                            await outFs
                                .WriteAsync(buffer, 0, read, ct)
                                .ConfigureAwait(false);

                            hasher?.TransformBlock(buffer, 0, read, buffer, 0);
                        }
                    }
                }

                outFs.Flush();
            }

            // 哈希校验
            if (hasher != null)
            {
                var actualHex = FinishHasher(hasher);
                if (!actualHex.Equals(expected.Hex, StringComparison.OrdinalIgnoreCase))
                {
                    File.Delete(finalPart);
                    for (long i = 0; i < numChunks; i++)
                        File.Delete(ChunkPath(destPath, i));
                    throw new IOException(
                        $"哈希校验失败: 期望 {expected.Hex}，实际 {actualHex}");
                }
            }

            // rename
            if (File.Exists(destPath)) File.Delete(destPath);
            File.Move(finalPart, destPath);

            // 清理块
            for (long i = 0; i < numChunks; i++)
            {
                try { File.Delete(ChunkPath(destPath, i)); } catch { }
            }
        }
        finally
        {
            hasher?.Dispose();
        }
    }

    // ─────────────────────────────────────────────────────
    // 单线程下载（保留原断点续传 + 前缀哈希逻辑）
    // ─────────────────────────────────────────────────────

    private async Task DoDownloadSingleAsync(
        string url,
        string destPath,
        ExpectedHash expected,
        IProgress<DownloadProgress> progress,
        CancellationToken ct)
    {
        var fileName = Path.GetFileName(destPath);
        var partPath = destPath + ".part";

        for (int attempt = 0; attempt < 2; attempt++)
        {
            ct.ThrowIfCancellationRequested();

            long existing = File.Exists(partPath) ? new FileInfo(partPath).Length : 0;

            var request = new HttpRequestMessage(HttpMethod.Get, url);
            if (existing > 0)
            {
                request.Headers.Range = new RangeHeaderValue(existing, null);
                progress?.Report(new DownloadProgress
                {
                    FileName = fileName,
                    CurrentBytes = existing,
                    TotalBytes = 0,
                    Percent = 0,
                    Speed = "",
                    Status = $"检测到未完成下载 ({FormatSize(existing)})，尝试续传...",
                });
            }

            using (var response = await Client
                .SendAsync(request, HttpCompletionOption.ResponseHeadersRead, ct)
                .ConfigureAwait(false))
            {
                // 416
                if (response.StatusCode == HttpStatusCode.RequestedRangeNotSatisfiable)
                {
                    var totalFromHeader = response.Content.Headers.ContentRange?.Length;
                    if (existing > 0 && totalFromHeader == existing)
                    {
                        if (expected != null)
                        {
                            progress?.Report(new DownloadProgress
                            {
                                FileName = fileName,
                                CurrentBytes = existing,
                                TotalBytes = existing,
                                Percent = 100,
                                Speed = "",
                                Status = "校验中...",
                            });
                            var ok = await VerifyFileHashAsync(partPath, expected, ct)
                                .ConfigureAwait(false);
                            if (!ok)
                            {
                                File.Delete(partPath);
                                throw new IOException("哈希校验失败，已删除 .part 文件");
                            }
                        }
                        if (File.Exists(destPath)) File.Delete(destPath);
                        File.Move(partPath, destPath);
                        progress?.Report(new DownloadProgress
                        {
                            FileName = fileName,
                            CurrentBytes = existing,
                            TotalBytes = existing,
                            Percent = 100,
                            Speed = "0 B/s",
                            Status = expected != null ? "完成（校验通过）" : "完成",
                        });
                        return;
                    }
                    File.Delete(partPath);
                    continue;
                }

                response.EnsureSuccessStatusCode();

                bool resumed = response.StatusCode == HttpStatusCode.PartialContent;
                if (!resumed && existing > 0) existing = 0;

                long total = -1;
                if (resumed)
                {
                    var remaining = response.Content.Headers.ContentLength;
                    if (remaining.HasValue) total = existing + remaining.Value;
                }
                else
                {
                    total = response.Content.Headers.ContentLength ?? -1L;
                }

                HashAlgorithm hasher = null;
                if (expected != null)
                {
                    hasher = resumed && existing > 0
                        ? await HashExistingPartAsync(partPath, expected.Algo, ct)
                            .ConfigureAwait(false)
                        : CreateHasher(expected.Algo);
                }

                try
                {
                    using (var src = await response.Content.ReadAsStreamAsync().ConfigureAwait(false))
                    using (var fs = new FileStream(
                        partPath,
                        resumed ? FileMode.Append : FileMode.Create,
                        FileAccess.Write,
                        FileShare.None,
                        bufferSize: 81920,
                        useAsync: true))
                    {
                        var buffer = new byte[81920];
                        long received = existing;

                        var sw = Stopwatch.StartNew();
                        long lastBytes = existing;
                        var lastReport = TimeSpan.Zero;

                        while (true)
                        {
                            ct.ThrowIfCancellationRequested();

                            int read = await src
                                .ReadAsync(buffer, 0, buffer.Length, ct)
                                .ConfigureAwait(false);
                            if (read == 0) break;

                            await fs
                                .WriteAsync(buffer, 0, read, ct)
                                .ConfigureAwait(false);

                            hasher?.TransformBlock(buffer, 0, read, buffer, 0);
                            received += read;

                            var now = sw.Elapsed;
                            if (now - lastReport >= TimeSpan.FromMilliseconds(120)
                                || (total > 0 && received == total))
                            {
                                var dt = (now - lastReport).TotalSeconds;
                                var speed = dt > 0 ? (received - lastBytes) / dt : 0;

                                progress?.Report(new DownloadProgress
                                {
                                    FileName = fileName,
                                    CurrentBytes = received,
                                    TotalBytes = total,
                                    Percent = total > 0 ? (double)received / total * 100.0 : 0,
                                    Speed = HumanSpeed(speed),
                                    Status = resumed ? "续传中" : "下载中",
                                });

                                lastReport = now;
                                lastBytes = received;
                            }
                        }

                        fs.Flush();
                    }

                    var actual = new FileInfo(partPath).Length;
                    if (total > 0 && actual != total)
                        throw new IOException(
                            $"大小不匹配: 期望 {total} 字节，实际 {actual} 字节");

                    if (hasher != null)
                    {
                        progress?.Report(new DownloadProgress
                        {
                            FileName = fileName,
                            CurrentBytes = actual,
                            TotalBytes = actual,
                            Percent = 100,
                            Speed = "",
                            Status = "校验中...",
                        });

                        var actualHex = FinishHasher(hasher);
                        if (!actualHex.Equals(expected.Hex, StringComparison.OrdinalIgnoreCase))
                        {
                            File.Delete(partPath);
                            throw new IOException(
                                $"哈希校验失败: 期望 {expected.Hex}，实际 {actualHex}");
                        }
                    }

                    if (File.Exists(destPath)) File.Delete(destPath);
                    File.Move(partPath, destPath);

                    progress?.Report(new DownloadProgress
                    {
                        FileName = fileName,
                        CurrentBytes = actual,
                        TotalBytes = actual,
                        Percent = 100,
                        Speed = "0 B/s",
                        Status = expected != null ? "完成（校验通过）" : "完成",
                    });
                    return;
                }
                finally
                {
                    hasher?.Dispose();
                }
            }
        }

        throw new IOException("续传失败：.part 与远端不一致，已放弃");
    }

    // ─────────────────────────────────────────────────────
    // 工具
    // ─────────────────────────────────────────────────────

    private static string ChunkPath(string destPath, long index)
        => destPath + ".part." + index.ToString();

    private static long ChunkLenAt(long index, long total, long chunkSize)
    {
        long start = index * chunkSize;
        return Math.Min(start + chunkSize, total) - start;
    }

    private static HashAlgorithm CreateHasher(HashAlgo algo)
        => algo == HashAlgo.Sha1 ? (HashAlgorithm)SHA1.Create() : SHA256.Create();

    private static Task<HashAlgorithm> HashExistingPartAsync(
        string path, HashAlgo algo, CancellationToken ct)
    {
        return Task.Run(() =>
        {
            var hasher = CreateHasher(algo);
            using (var fs = new FileStream(path, FileMode.Open, FileAccess.Read, FileShare.Read, 81920))
            {
                var buffer = new byte[81920];
                int read;
                while ((read = fs.Read(buffer, 0, buffer.Length)) > 0)
                {
                    ct.ThrowIfCancellationRequested();
                    hasher.TransformBlock(buffer, 0, read, buffer, 0);
                }
            }
            return hasher;
        }, ct);
    }

    private static string FinishHasher(HashAlgorithm hasher)
    {
        hasher.TransformFinalBlock(Array.Empty<byte>(), 0, 0);
        return BitConverter.ToString(hasher.Hash!)
            .Replace("-", "")
            .ToLowerInvariant();
    }

    private static Task<bool> VerifyFileHashAsync(
        string path, ExpectedHash expected, CancellationToken ct)
    {
        return Task.Run(() =>
        {
            using (var hasher = CreateHasher(expected.Algo))
            using (var fs = new FileStream(path, FileMode.Open, FileAccess.Read, FileShare.Read, 81920))
            {
                var buffer = new byte[81920];
                int read;
                while ((read = fs.Read(buffer, 0, buffer.Length)) > 0)
                {
                    ct.ThrowIfCancellationRequested();
                    hasher.TransformBlock(buffer, 0, read, buffer, 0);
                }
                hasher.TransformFinalBlock(Array.Empty<byte>(), 0, 0);
                var hex = BitConverter.ToString(hasher.Hash!)
                    .Replace("-", "")
                    .ToLowerInvariant();
                return hex.Equals(expected.Hex, StringComparison.OrdinalIgnoreCase);
            }
        }, ct);
    }

    private static string FormatSize(long bytes)
    {
        string[] units = { "B", "KB", "MB", "GB" };
        double v = bytes;
        int i = 0;
        while (v >= 1024 && i < units.Length - 1) { v /= 1024; i++; }
        return $"{v:0.0} {units[i]}";
    }

    private static string HumanSpeed(double bytesPerSec)
    {
        string[] units = { "B/s", "KB/s", "MB/s", "GB/s" };
        var v = bytesPerSec;
        int i = 0;
        while (v >= 1024 && i < units.Length - 1) { v /= 1024; i++; }
        return $"{v:0.0} {units[i]}";
    }
}