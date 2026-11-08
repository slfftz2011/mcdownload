using System;
using System.Diagnostics;
using System.IO;
using System.Net;
using System.Net.Http;
using System.Threading;
using System.Threading.Tasks;
using MCDownloader.Models;

namespace MCDownloader.Services;

public class DownloadEngine
{
    // ── 整个进程只创建一个 HttpClient ──────────────────────────
    // 反复 new HttpClient 会导致 socket 耗尽（TIME_WAIT 堆积），
    // 所以用 static 字段共享一个实例，它是线程安全的。
    private static readonly HttpClient Client = CreateClient();

    private static HttpClient CreateClient()
    {
        var handler = new HttpClientHandler
        {
            AutomaticDecompression = DecompressionMethods.GZip | DecompressionMethods.Deflate,
        };

        var client = new HttpClient(handler)
        {
            // 大文件下载可能持续很久，不能让 HttpClient 自己超时。
            // 什么时候放弃由 CancellationToken 控制。
            Timeout = Timeout.InfiniteTimeSpan,
        };

        client.DefaultRequestHeaders.UserAgent.ParseAdd("MCDownloader/0.1");
        return client;
    }

    /// <summary>
    /// 下载一个文件到 destPath。
    /// - 流式写入，不占内存
    /// - 通过 IProgress 报告进度（回调自动回到 UI 线程）
    /// - 支持 CancellationToken 取消
    /// - 先写 .part，校验大小后原子替换为目标文件
    /// </summary>
    public async Task DownloadFileAsync(
        string url,
        string destPath,
        IProgress<DownloadProgress> progress,
        CancellationToken ct)
    {
        // 1. 确保目标目录存在
        var dir = Path.GetDirectoryName(destPath);
        if (!string.IsNullOrEmpty(dir) && !Directory.Exists(dir))
            Directory.CreateDirectory(dir);

        var fileName = Path.GetFileName(destPath);
        var partPath = destPath + ".part";   // 临时文件，下完再改名

        long total = -1;   // 服务器返回的文件总大小，-1 表示未知

        // 2. 发请求。注意 ResponseHeadersRead：
        //    只等到响应头就返回，不把整个 body 读进内存。
        //    （如果用默认的 ResponseContentRead，HttpClient 会先把
        //      整个文件缓冲到内存里，几十 MB 的文件会直接卡死。）
        using (var response = await Client
            .GetAsync(url, HttpCompletionOption.ResponseHeadersRead, ct)
            .ConfigureAwait(false))
        {
            response.EnsureSuccessStatusCode();
            total = response.Content.Headers.ContentLength ?? -1L;

            // 3. 拿到网络流
            using (var src = await response.Content
                .ReadAsStreamAsync()
                .ConfigureAwait(false))
            // 4. 打开本地文件流（Create 模式：每次都从头写）
            using (var fs = new FileStream(
                partPath,
                FileMode.Create,
                FileAccess.Write,
                FileShare.None,
                bufferSize: 81920,
                useAsync: true))
            {
                var buffer = new byte[81920];
                long received = 0;

                // 用于计算速度
                var sw = Stopwatch.StartNew();
                long lastBytes = 0;
                var lastReport = TimeSpan.Zero;

                // 5. 循环：读一块 → 写一块 → 报告进度
                while (true)
                {
                    ct.ThrowIfCancellationRequested();

                    int read = await src
                        .ReadAsync(buffer, 0, buffer.Length, ct)
                        .ConfigureAwait(false);

                    if (read == 0) break;   // 读到 0 表示流结束

                    await fs
                        .WriteAsync(buffer, 0, read, ct)
                        .ConfigureAwait(false);

                    received += read;

                    // 6. 进度节流：每 120ms 报告一次
                    //    如果每次 read 都 Report，UI 线程会被每秒几千次的
                    //    回调淹没，界面反而卡。120ms 约等于 8 次/秒，足够流畅。
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
                            Status = "下载中",
                        });

                        lastReport = now;
                        lastBytes = received;
                    }
                }

                fs.Flush();
            }
        }

        // 7. 大小校验：服务器给了 Content-Length 就核对一下
        var actual = new FileInfo(partPath).Length;
        if (total > 0 && actual != total)
            throw new IOException($"大小不匹配: 期望 {total} 字节，实际 {actual} 字节");

        // 8. .part → 目标文件
        //    File.Move 在同一磁盘上是原子操作，中途崩溃不会留下半个目标文件
        if (File.Exists(destPath)) File.Delete(destPath);
        File.Move(partPath, destPath);

        // 9. 最后再报告一次 100%
        progress?.Report(new DownloadProgress
        {
            FileName = fileName,
            CurrentBytes = actual,
            TotalBytes = actual,
            Percent = 100,
            Speed = "0 B/s",
            Status = "完成",
        });
    }

    private static string HumanSpeed(double bytesPerSec)
    {
        string[] units = { "B/s", "KB/s", "MB/s", "GB/s" };
        var v = bytesPerSec;
        int i = 0;
        while (v >= 1024 && i < units.Length - 1)
        {
            v /= 1024;
            i++;
        }
        return $"{v:0.0} {units[i]}";
    }
}