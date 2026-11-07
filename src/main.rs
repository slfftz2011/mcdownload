mod assets;
mod downloader;
mod install;
mod manifest;
mod mirror;
mod version;

use anyhow::{bail, Context, Result};
use downloader::{Downloader, ExpectedHash, HashAlgo};
use manifest::VersionManifest;
use mirror::MirrorConfig;
use std::path::PathBuf;

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        print_usage();
        std::process::exit(1);
    }

    let (cmd, rest) = match args[0].as_str() {
        "url" | "manifest" | "install" => (args[0].as_str(), &args[1..]),
        _ => ("url", &args[..]),
    };

    let (positional, flags) = parse_flags(rest)?;
    let mirror = if flags.official {
        MirrorConfig::official()
    } else {
        MirrorConfig::default()
    };

    match cmd {
        "url" => cmd_url(positional, &flags).await,
        "manifest" => cmd_manifest(&mirror, &flags).await,
        "install" => cmd_install(positional, &mirror, &flags).await,
        _ => unreachable!(),
    }
}

async fn cmd_url(pos: Vec<String>, flags: &Flags) -> Result<()> {
    if pos.is_empty() {
        bail!("用法: mc-downloader url [--sha1 X|--sha256 X] <URL> [输出]");
    }
    let url = &pos[0];
    let dest = resolve_dest(url, pos.get(1))?;
    if dest.exists() {
        bail!("目标文件已存在: {}", dest.display());
    }
    if let Some(parent) = dest.parent() {
        if !parent.as_os_str().is_empty() {
            tokio::fs::create_dir_all(parent).await?;
        }
    }
    let expected = if let Some(h) = flags.sha1.as_ref() {
        Some(ExpectedHash { algo: HashAlgo::Sha1, hex: h.clone() })
    } else {
        flags.sha256.as_ref().map(|h| ExpectedHash {
            algo: HashAlgo::Sha256,
            hex: h.clone(),
        })
    };
    let dl = Downloader::new(1, false);
    dl.download(url, &dest, expected).await?;
    let size = tokio::fs::metadata(&dest).await?.len();
    println!("完成: {} ({})", dest.display(), human_size(size));
    Ok(())
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

async fn cmd_manifest(mirror: &MirrorConfig, flags: &Flags) -> Result<()> {
    let client = reqwest::Client::new();
    let resp = client
        .get(&mirror.version_manifest)
        .send()
        .await?
        .error_for_status()?;
    let mut json: serde_json::Value = resp.json().await?;
    mirror.rewrite_version_manifest(&mut json)?;
    let bytes = serde_json::to_vec(&json)?;
    let manifest = VersionManifest::from_slice(&bytes)?;

    println!("最新 release: {}", manifest.latest.release);
    println!("最新 snapshot: {}", manifest.latest.snapshot);
    println!("共 {} 个版本:", manifest.versions.len());
    let limit = flags.limit.unwrap_or(manifest.versions.len());
    for v in manifest.versions.iter().take(limit) {
        println!("  {:<16} {:<12} {}", v.id, v.kind, v.release_time);
    }
    if limit < manifest.versions.len() {
        println!("  ...（省略 {} 条，--limit 0 显示全部）", manifest.versions.len() - limit);
    }
    Ok(())
}

async fn cmd_install(pos: Vec<String>, mirror: &MirrorConfig, flags: &Flags) -> Result<()> {
    let version = pos.first().map(String::as_str).unwrap_or("latest");
    let base = flags
        .dir
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("minecraft"));
    install::install(version, &base, mirror, flags.concurrency).await
}

// ---------- 参数解析 ----------

struct Flags {
    official: bool,
    concurrency: usize,
    limit: Option<usize>,
    dir: Option<String>,
    sha1: Option<String>,
    sha256: Option<String>,
}

fn parse_flags(args: &[String]) -> Result<(Vec<String>, Flags)> {
    let mut pos = Vec::new();
    let mut flags = Flags {
        official: false,
        concurrency: 16,
        limit: None,
        dir: None,
        sha1: None,
        sha256: None,
    };
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        match a.as_str() {
            "--official" => flags.official = true,
            "-c" | "--concurrency" => {
                i += 1;
                flags.concurrency = args.get(i).context("-c 需要一个数值")?.parse()?;
            }
            "--limit" => {
                i += 1;
                flags.limit = Some(args.get(i).context("--limit 需要一个数值")?.parse()?);
            }
            "-d" | "--dir" => {
                i += 1;
                flags.dir = Some(args.get(i).context("-d 需要一个路径")?.clone());
            }
            "--sha1" => {
                i += 1;
                flags.sha1 = Some(args.get(i).context("--sha1 需要一个值")?.to_lowercase());
            }
            "--sha256" => {
                i += 1;
                flags.sha256 = Some(args.get(i).context("--sha256 需要一个值")?.to_lowercase());
            }
            "-h" | "--help" => {
                print_usage();
                std::process::exit(0);
            }
            s if s.starts_with('-') => bail!("未知参数: {s}"),
            s => pos.push(s.to_string()),
        }
        i += 1;
    }
    if flags.sha1.is_some() && flags.sha256.is_some() {
        bail!("--sha1 与 --sha256 不能同时指定");
    }
    Ok((pos, flags))
}

fn resolve_dest(url: &str, arg: Option<&String>) -> Result<PathBuf> {
    let name = url
        .split('/')
        .filter(|s| !s.is_empty())
        .last()
        .and_then(|s| s.split('?').next())
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    match arg {
        None => Ok(PathBuf::from(name.context("无法从 URL 推断文件名")?)),
        Some(p) => {
            let path = PathBuf::from(p);
            if p.ends_with('/') || path.is_dir() {
                Ok(path.join(name.context("目标为目录，但 URL 无文件名")?))
            } else {
                Ok(path)
            }
        }
    }
}

fn print_usage() {
    eprintln!("用法:");
    eprintln!("  mc-downloader url [--sha1 X|--sha256 X] <URL> [输出路径]");
    eprintln!("  mc-downloader manifest [--limit N] [--official]");
    eprintln!("  mc-downloader install [版本ID|latest] [-d 目录] [-c 并发数] [--official]");
    eprintln!();
    eprintln!("参数:");
    eprintln!("  --official      使用官方源（默认走 BMCLAPI 镜像）");
    eprintln!("  -c <N>          并发数，默认 16");
    eprintln!("  -d <DIR>        安装目录，默认 ./minecraft");
    eprintln!("  --limit <N>     manifest 列表显示条数，默认全部");
}