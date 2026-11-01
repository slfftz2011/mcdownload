mod downloader;

use anyhow::{bail, Context, Result};
use std::path::PathBuf;

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();

    let mut url: Option<String> = None;
    let mut out: Option<String> = None;
    let mut sha1_hex: Option<String> = None;
    let mut sha256_hex: Option<String> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--sha1" => {
                i += 1;
                sha1_hex = Some(
                    args.get(i)
                        .context("--sha1 需要一个 40 位十六进制参数")?
                        .to_lowercase(),
                );
            }
            "--sha256" => {
                i += 1;
                sha256_hex = Some(
                    args.get(i)
                        .context("--sha256 需要一个 64 位十六进制参数")?
                        .to_lowercase(),
                );
            }
            "-h" | "--help" => {
                print_usage();
                return Ok(());
            }
            s if s.starts_with("--") => bail!("未知参数: {s}"),
            s => {
                if url.is_none() {
                    url = Some(s.to_string());
                } else if out.is_none() {
                    out = Some(s.to_string());
                } else {
                    bail!("多余的参数: {s}");
                }
            }
        }
        i += 1;
    }

    let url = match url {
        Some(u) => u,
        None => {
            print_usage();
            std::process::exit(1);
        }
    };

    if sha1_hex.is_some() && sha256_hex.is_some() {
        bail!("--sha1 与 --sha256 不能同时指定");
    }

    let dest = resolve_dest(&url, out.as_ref())?;
    if dest.exists() {
        bail!("目标文件已存在: {}", dest.display());
    }
    if let Some(parent) = dest.parent() {
        if !parent.as_os_str().is_empty() {
            tokio::fs::create_dir_all(parent)
                .await
                .with_context(|| format!("创建目录失败: {}", parent.display()))?;
        }
    }

    let expected = if let Some(h) = sha1_hex.as_deref() {
        Some(downloader::ExpectedHash {
            algo: downloader::HashAlgo::Sha1,
            hex: h,
        })
    } else {
        sha256_hex.as_deref().map(|h| downloader::ExpectedHash {
            algo: downloader::HashAlgo::Sha256,
            hex: h,
        })
    };

    println!("下载: {}", url);
    println!("保存至: {}", dest.display());
    if let Some(exp) = expected {
        println!("校验: {:?} {}", exp.algo, exp.hex);
    }

    downloader::download(&url, &dest, expected).await?;

    let size = tokio::fs::metadata(&dest).await?.len();
    println!("完成: {} ({})", dest.display(), human_size(size));
    Ok(())
}

fn resolve_dest(url: &str, arg: Option<&String>) -> Result<PathBuf> {
    let name_from_url = url
        .split('/')
        .filter(|s| !s.is_empty())
        .last()
        .and_then(|s| s.split('?').next())
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    match arg {
        None => {
            let name = name_from_url.context("无法从 URL 推断文件名，请显式指定输出路径")?;
            Ok(PathBuf::from(name))
        }
        Some(p) => {
            let path = PathBuf::from(p);
            let is_dir = p.ends_with('/') || path.is_dir();
            if is_dir {
                let name = name_from_url.context("目标为目录，但无法从 URL 推断文件名")?;
                Ok(path.join(name))
            } else {
                Ok(path)
            }
        }
    }
}

fn print_usage() {
    eprintln!("用法: mc-downloader [--sha1 <hex>|--sha256 <hex>] <URL> [输出路径]");
    eprintln!();
    eprintln!("示例:");
    eprintln!("  mc-downloader https://example.com/foo.jar");
    eprintln!("  mc-downloader https://example.com/foo.jar ./downloads/");
    eprintln!("  mc-downloader --sha1 0123abcd... https://example.com/foo.jar ./foo.jar");
}

fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut size = bytes as f64;
    let mut idx = 0;
    while size >= 1024.0 && idx < UNITS.len() - 1 {
        size /= 1024.0;
        idx += 1;
    }
    format!("{:.2} {}", size, UNITS[idx])
}