using System;
using System.Net;
using System.Text;
using System.Threading.Tasks;
using System.Windows;

namespace MCDownloader.Services;

public class LocalHttpService
{
    private readonly HttpListener _listener = new();
    private readonly XamlHomepageGenerator _generator = new();
    private bool _running;

    public void Start()
    {
        _listener.Prefixes.Clear();
        _listener.Prefixes.Add("http://127.0.0.1:1699/");
        _listener.Start();
        _running = true;
        _ = Task.Run(LoopAsync);
    }

    public void Stop()
    {
        _running = false;
        try { _listener.Stop(); _listener.Close(); } catch { }
    }

    private async Task LoopAsync()
    {
        while (_running)
        {
            HttpListenerContext ctx;
            try { ctx = await _listener.GetContextAsync(); }
            catch { if (!_running) return; continue; }

            _ = Task.Run(() => Handle(ctx));
        }
    }

    private void Handle(HttpListenerContext ctx)
    {
        try
        {
            var path = ctx.Request.Url?.AbsolutePath ?? "/";
            if (path == "/Custom.xaml")
            {
                var xaml = _generator.Build();
                var bytes = Encoding.UTF8.GetBytes(xaml);
                ctx.Response.ContentType = "application/xml; charset=utf-8";
                ctx.Response.ContentLength64 = bytes.Length;
                ctx.Response.OutputStream.Write(bytes, 0, bytes.Length);
            }
            else
            {
                ctx.Response.StatusCode = 404;
            }
        }
        catch { }
        finally { try { ctx.Response.Close(); } catch { } }
    }
}