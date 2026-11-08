namespace MCDownloader.Models;

public class DownloadProgress
{
    public string FileName { get; set; } = "";
    public long CurrentBytes { get; set; }
    public long TotalBytes { get; set; }
    public double Percent { get; set; }
    public string Speed { get; set; } = "";
    public string Status { get; set; } = "";
}