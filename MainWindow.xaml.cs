using System;
using System.Collections.ObjectModel;
using System.IO;
using System.Linq;
using System.Threading;
using System.Windows;
using System.Windows.Controls;
using MCDownloader.Models;
using MCDownloader.Services;

namespace MCDownloader;

public partial class MainWindow : Window
{
    private readonly ObservableCollection<NewsItem> _news = new();
    private FloatingBallWindow _ball;
    private readonly LocalHttpService _http = new();
    private CancellationTokenSource _cts;

    public MainWindow()
    {
        InitializeComponent();
        VersionList.ItemsSource = _news;
        _ = LoadDemoAsync();
        Loaded += (_, _) =>
        {
            try
            {
                _http.Start();
                StatusText.Text = "PCL 主页服务: http://127.0.0.1:1699/Custom.xaml";
            }
            catch
            {
                StatusText.Text = "PCL 主页服务启动失败（需管理员权限或注册 urlacl）";
            }
        };
        Closed += (_, _) => _http.Stop();
    }

    private async System.Threading.Tasks.Task LoadDemoAsync()
    {
        await System.Threading.Tasks.Task.Delay(100);
        _news.Add(new NewsItem { Id = "1.21.4", Kind = "Release", Title = "1.21.4 · Release", Summary = "发布于 2026-10-20" });
        _news.Add(new NewsItem { Id = "1.21.3", Kind = "Release", Title = "1.21.3 · Release", Summary = "发布于 2026-09-12" });
        StatusText.Text = "已加载演示版本列表";
    }

    private void VersionList_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (VersionList.SelectedItem is NewsItem item)
        {
            DetailTitle.Text = $"{item.Id} · {item.Kind}";
            DetailSub.Text = item.Summary;
            InstallBtn.IsEnabled = true;
        }
    }

    private async void InstallBtn_Click(object sender, RoutedEventArgs e)
    {
        if (VersionList.SelectedItem is not NewsItem item) return;
        if (_cts != null)
        {
            StatusText.Text = "已有下载任务在进行中";
            return;
        }

        // 骨架阶段仍用 manifest 测试，后续替换为完整 install 流程
        var testUrl = "https://launchermeta.mojang.com/mc/game/version_manifest_v2.json";
        var dest = Path.Combine("downloads", item.Id, "version_manifest.json");
        await RunDownloadAsync(testUrl, dest, null);
    }

    private async void CustomDownloadBtn_Click(object sender, RoutedEventArgs e)
    {
        if (_cts != null)
        {
            StatusText.Text = "已有下载任务在进行中";
            return;
        }

        var url = CustomUrlBox.Text?.Trim();
        if (string.IsNullOrWhiteSpace(url))
        {
            StatusText.Text = "请输入下载地址";
            return;
        }
        if (!Uri.TryCreate(url, UriKind.Absolute, out var uri)
            || (uri.Scheme != Uri.UriSchemeHttp && uri.Scheme != Uri.UriSchemeHttps))
        {
            StatusText.Text = "请输入有效的 http/https 地址";
            return;
        }

        ExpectedHash expected;
        try
        {
            expected = BuildExpectedHash();
        }
        catch (ArgumentException ex)
        {
            StatusText.Text = ex.Message;
            return;
        }

        var fileName = InferFileName(uri);
        var dest = Path.Combine("downloads", "custom", fileName);
        await RunDownloadAsync(url, dest, expected);
    }

    private async System.Threading.Tasks.Task RunDownloadAsync(string url, string destPath)
    {
        CustomDownloadBtn.IsEnabled = false;
        InstallBtn.IsEnabled = false;
        MainProgress.Value = 0;
        ProgressText.Text = "准备中...";
        _cts = new CancellationTokenSource();

        // Progress<T> 必须在 UI 线程创建，回调才会自动回到 UI 线程
        var progress = new Progress<DownloadProgress>(p =>
        {
            MainProgress.Value = p.Percent;
            ProgressText.Text = $"{p.FileName} · {p.Status} · {p.Speed}";
            StatusText.Text = $"{p.CurrentBytes:N0}/{p.TotalBytes:N0} 字节";
        });

        var engine = new DownloadEngine();
        try
        {
            await engine.DownloadFileAsync(url, destPath, expected, progress, _cts.Token);
            StatusText.Text = expected != null
                ? $"✓ 下载完成且校验通过: {destPath}"
                : $"✓ 下载完成: {destPath}";
        }
        catch (OperationCanceledException)
        {
            StatusText.Text = "已取消";
        }
        catch (Exception ex)
        {
            MessageBox.Show(ex.Message, "下载失败");
            StatusText.Text = "✗ 下载失败";
        }
        finally
        {
            CustomDownloadBtn.IsEnabled = true;
            InstallBtn.IsEnabled = VersionList.SelectedItem is NewsItem;
            _cts?.Dispose();
            _cts = null;
        }
    }

    private static string InferFileName(Uri uri)
    {
        var name = uri.Segments.LastOrDefault();
        if (string.IsNullOrEmpty(name) || name == "/")
            name = $"download_{DateTime.Now:yyyyMMdd_HHmmss}";

        name = Uri.UnescapeDataString(name);

        // 去掉可能残留的非法字符
        foreach (var c in Path.GetInvalidFileNameChars())
            name = name.Replace(c, '_');

        return name;
    }

    private void FloatBtn_Click(object sender, RoutedEventArgs e)
    {
        _ball ??= new FloatingBallWindow(this);
        _ball.Show();
        Hide();
    }

    public void ReturnFromBall()
    {
        Show();
        Activate();
        _ball?.Hide();
    }

    private void HashAlgoBox_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (HashHexBox == null) return;
        HashHexBox.IsEnabled = HashAlgoBox.SelectedIndex > 0;
    }

    private ExpectedHash BuildExpectedHash()
    {
        if (HashAlgoBox.SelectedIndex <= 0) return null;

        var hex = (HashHexBox.Text ?? "").Trim().ToLowerInvariant();
        if (string.IsNullOrEmpty(hex)) return null;

        var algo = HashAlgoBox.SelectedIndex == 1 ? HashAlgo.Sha1 : HashAlgo.Sha256;
        var expectedLen = algo == HashAlgo.Sha1 ? 40 : 64;

        if (hex.Length != expectedLen)
            throw new ArgumentException(
                $"{(algo == HashAlgo.Sha1 ? "SHA-1" : "SHA-256")} 应为 {expectedLen} 位十六进制，当前 {hex.Length} 位");

        return new ExpectedHash { Algo = algo, Hex = hex };
    }
}