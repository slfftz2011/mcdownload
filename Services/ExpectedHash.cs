namespace MCDownloader.Models;

public enum HashAlgo
{
    Sha1,
    Sha256,
}

public class ExpectedHash
{
    public HashAlgo Algo { get; set; }
    public string Hex { get; set; } = "";
}