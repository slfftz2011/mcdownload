namespace MCDownloader.Models;

public class DownloadProgress
{
    public string FileName { get; set; } = "";
    public double Percent { get; set; }
    public string Speed { get; set; } = "";
}