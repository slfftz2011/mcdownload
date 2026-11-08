using System;
using System.Net;
using System.Windows;
using System.Windows.Threading;

namespace MCDownloader;

public partial class App : Application
{
    protected override void OnStartup(StartupEventArgs e)
    {
        // Win7 上 .NET Framework 4.8 默认可能不启用 TLS 1.2
        ServicePointManager.SecurityProtocol |= SecurityProtocolType.Tls12;
        ServicePointManager.DefaultConnectionLimit = 128;

        DispatcherUnhandledException += OnUnhandledException;
        base.OnStartup(e);
    }

    private void OnUnhandledException(object sender, DispatcherUnhandledExceptionEventArgs e)
    {
        MessageBox.Show($"未处理的异常:\n{e.Exception.Message}", "MCDownloader",
            MessageBoxButton.OK, MessageBoxImage.Error);
        e.Handled = true;
    }
}