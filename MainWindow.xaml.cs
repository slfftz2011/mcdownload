using System.Collections.ObjectModel;
using System.Windows;
using System.Windows.Controls;
using MCDownloader.Models;
using MCDownloader.Services;

namespace MCDownloader;

public partial class MainWindow : Window
{
    private readonly ObservableCollection<NewsItem> _news = new();
    private FloatingBallWindow? _ball;
    private readonly LocalHttpService _http = new();

    public MainWindow()
    {
        InitializeComponent();
        VersionList.ItemsSource = _news;
        _ = LoadDemoAsync();
        Loaded += (_, _) =>
        {
            try { _http.Start(); StatusText.Text = "PCL 主页服务: http://127.0.0.1:1699/Custom.xaml"; }
            catch { StatusText.Text = "PCL 主页服务启动失败（需管理员权限或 netsh 注册前缀）"; }
        };
        Closed += (_, _) => _http.Stop();
    }

    private async System.Threading.Tasks.Task LoadDemoAsync()
    {
        // 骨架阶段先用假数据占位，后续替换为 Manifest 解析
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

    private void InstallBtn_Click(object sender, RoutedEventArgs e)
    {
        if (VersionList.SelectedItem is not NewsItem item) return;
        StatusText.Text = $"开始安装 {item.Id}（下载引擎待接入）";
        // 后续：调用 DownloadEngine，通过 IProgress<DownloadProgress> 更新 MainProgress
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
}