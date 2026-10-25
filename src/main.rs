mod downloader;

use anyhow::{bail, Context, Result};
use std::path::PathBuf;

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("用法: mc-downloader <URL> [输出路径]");
        eprintln!("示例: mc-downloader https://example.com/file.jar ./file.jar");
        eprintln!("       mc-downloader https://example.com/file.jar ./downloads/");
        std::process::exit(1);
    }

    let url = &args[0];
    let dest = resolve_dest(url, args.get(1))?;

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

    println!("下载: {}", url);
    println!("保存至: {}", dest.display());

    downloader::download(url, &dest).await?;

    let size = tokio::fs::metadata(&dest).await?.len();
    println!("完成: {} ({})", dest.display(), human_size(size));
    Ok(())
}

/// 若第二个参数是已存在的目录或以 / 结尾，则把 URL 最后一段作为文件名
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