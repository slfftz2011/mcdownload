using System;
using System.Diagnostics;
using System.IO;
using System.Net;
using System.Net.Http;
using System.Net.Http.Headers;
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

    public async Task DownloadFileAsync(
        string url,
        string destPath,
        IProgress<DownloadProgress> progress,
        CancellationToken ct)
    {
        var dir = Path.GetDirectoryName(destPath);
        if (!string.IsNullOrEmpty(dir) && !Directory.Exists(dir))
            Directory.CreateDirectory(dir);

        var fileName = Path.GetFileName(destPath);
        var partPath = destPath + ".part";

        // 外层循环：处理 416 后需要重试的情况
        // 最多重试 1 次（416 → 删 .part → 重新请求）
        for (int attempt = 0; attempt < 2; attempt++)
        {
            ct.ThrowIfCancellationRequested();

            long existing = File.Exists(partPath) ? new FileInfo(partPath).Length : 0;

            // ── 构造请求，有 .part 就带 Range 头 ────────────
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
                // ── 416：.part 已经够了 ─────────────────────
                if (response.StatusCode == HttpStatusCode.RequestedRangeNotSatisfiable)
                {
                    var totalFromHeader = response.Content.Headers.ContentRange?.Length;

                    if (existing > 0 && totalFromHeader == existing)
                    {
                        // .part 已完整，直接改名
                        if (File.Exists(destPath)) File.Delete(destPath);
                        File.Move(partPath, destPath);
                        progress?.Report(new DownloadProgress
                        {
                            FileName = fileName,
                            CurrentBytes = existing,
                            TotalBytes = existing,
                            Percent = 100,
                            Speed = "0 B/s",
                            Status = "完成（续传校验通过）",
                        });
                        return;
                    }

                    // .part 比远端还大 / 远端变小 → 删掉重来
                    File.Delete(partPath);
                    continue;   // 重试
                }

                response.EnsureSuccessStatusCode();

                // ── 判断服务器是否真正接受了 Range ───────────
                bool resumed = response.StatusCode == HttpStatusCode.PartialContent;

                if (!resumed && existing > 0)
                {
                    // 服务器返回 200 → 忽略 Range，只能从头下
                    existing = 0;
                }

                // ── 计算总大小 ──────────────────────────────
                long total = -1;
                if (resumed)
                {
                    // 206 的 Content-Length 是「剩余部分」的长度
                    var remaining = response.Content.Headers.ContentLength;
                    if (remaining.HasValue)
                        total = existing + remaining.Value;
                }
                else
                {
                    total = response.Content.Headers.ContentLength ?? -1L;
                }

                // ── 打开文件流 ──────────────────────────────
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

                // ── 大小校验 ────────────────────────────────
                var actual = new FileInfo(partPath).Length;
                if (total > 0 && actual != total)
                    throw new IOException(
                        $"大小不匹配: 期望 {total} 字节，实际 {actual} 字节");

                // ── .part → 目标文件 ────────────────────────
                if (File.Exists(destPath)) File.Delete(destPath);
                File.Move(partPath, destPath);

                progress?.Report(new DownloadProgress
                {
                    FileName = fileName,
                    CurrentBytes = actual,
                    TotalBytes = actual,
                    Percent = 100,
                    Speed = "0 B/s",
                    Status = "完成",
                });
                return;
            }
        }

        // 循环跑完还没 return，说明 416 重试也没救回来
        throw new IOException("续传失败：.part 与远端不一致，已放弃");
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