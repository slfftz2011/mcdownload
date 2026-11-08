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
        client.DefaultRequestHeaders.UserAgent.ParseAdd("MCDownloader/0.1");
        return client;
    }

    /// <summary>
    /// 下载文件。expected 为 null 时跳过哈希校验。
    /// </summary>
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
                // ── 416 分支：.part 已经够了 ─────────────────
                if (response.StatusCode == HttpStatusCode.RequestedRangeNotSatisfiable)
                {
                    var totalFromHeader = response.Content.Headers.ContentRange?.Length;

                    if (existing > 0 && totalFromHeader == existing)
                    {
                        // .part 已完整，校验哈希后改名
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
                if (!resumed && existing > 0)
                    existing = 0;   // 服务器忽略 Range，从头下

                long total = -1;
                if (resumed)
                {
                    var remaining = response.Content.Headers.ContentLength;
                    if (remaining.HasValue)
                        total = existing + remaining.Value;
                }
                else
                {
                    total = response.Content.Headers.ContentLength ?? -1L;
                }

                // ── 构建 hasher ──────────────────────────────
                // 续传：先把已有 .part 内容喂给 hasher
                // 从头：空 hasher
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
                    using (var src = await response.Content
                        .ReadAsStreamAsync()
                        .ConfigureAwait(false))
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

                            // 边写边算哈希
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

                    // ── 大小校验 ────────────────────────────
                    var actual = new FileInfo(partPath).Length;
                    if (total > 0 && actual != total)
                        throw new IOException(
                            $"大小不匹配: 期望 {total} 字节，实际 {actual} 字节");

                    // ── 哈希校验 ────────────────────────────
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

                    // ── .part → 目标文件 ────────────────────
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
                    // 无论成功失败，释放 hasher 资源
                    hasher?.Dispose();
                }
            }
        }

        throw new IOException("续传失败：.part 与远端不一致，已放弃");
    }

    // ── 哈希工具 ───────────────────────────────────────

    private static HashAlgorithm CreateHasher(HashAlgo algo)
    {
        return algo == HashAlgo.Sha1
            ? (HashAlgorithm)SHA1.Create()
            : SHA256.Create();
    }

    /// <summary>
    /// 读已有 .part 全部内容喂给 hasher，得到「前缀状态」。
    /// 用 Task.Run 包一层，避免续传大文件时阻塞 UI 线程。
    /// </summary>
    private static Task<HashAlgorithm> HashExistingPartAsync(
        string path, HashAlgo algo, CancellationToken ct)
    {
        return Task.Run(() =>
        {
            var hasher = CreateHasher(algo);
            using (var fs = new FileStream(
                path, FileMode.Open, FileAccess.Read, FileShare.Read, 81920))
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

    /// <summary>
    /// 收尾：TransformFinalBlock + 十六进制小写。
    /// </summary>
    private static string FinishHasher(HashAlgorithm hasher)
    {
        hasher.TransformFinalBlock(Array.Empty<byte>(), 0, 0);
        return BitConverter.ToString(hasher.Hash!)
            .Replace("-", "")
            .ToLowerInvariant();
    }

    /// <summary>
    /// 独立校验一个完整文件的哈希（用于 416 分支）。
    /// </summary>
    private static Task<bool> VerifyFileHashAsync(
        string path, ExpectedHash expected, CancellationToken ct)
    {
        return Task.Run(() =>
        {
            using (var hasher = CreateHasher(expected.Algo))
            using (var fs = new FileStream(
                path, FileMode.Open, FileAccess.Read, FileShare.Read, 81920))
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

    // ── 格式化工具 ─────────────────────────────────────

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