use anyhow::{bail, Context, Result};
use futures_util::StreamExt;
use indicatif::{ProgressBar, ProgressStyle};
use sha2::Digest as _;
use std::path::{Path, PathBuf};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[derive(Clone, Copy, Debug)]
pub enum HashAlgo {
    Sha1,
    Sha256,
}

#[derive(Clone, Copy, Debug)]
pub struct ExpectedHash<'a> {
    pub algo: HashAlgo,
    pub hex: &'a str,
}

enum Hasher {
    Sha1(sha1::Sha1),
    Sha256(sha2::Sha256),
}

impl Hasher {
    fn new(algo: HashAlgo) -> Self {
        match algo {
            HashAlgo::Sha1 => Hasher::Sha1(sha1::Sha1::new()),
            HashAlgo::Sha256 => Hasher::Sha256(sha2::Sha256::new()),
        }
    }
    fn update(&mut self, data: &[u8]) {
        match self {
            Hasher::Sha1(h) => h.update(data),
            Hasher::Sha256(h) => h.update(data),
        }
    }
    fn finalize_hex(self) -> String {
        match self {
            Hasher::Sha1(h) => hex_encode(&h.finalize()),
            Hasher::Sha256(h) => hex_encode(&h.finalize()),
        }
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

pub async fn download(
    url: &str,
    dest: &Path,
    expected: Option<ExpectedHash<'_>>,
) -> Result<()> {
    let client = reqwest::Client::builder()
        .user_agent("mc-downloader/0.1")
        .build()
        .context("创建 HTTP 客户端失败")?;

    let part = part_path(dest);
    let mut announced = false;

    loop {
        let existing = file_len(&part).await;

        // 续传时先把已有 .part 喂给 hasher 得到前缀状态
        let mut hasher: Option<Hasher> = match expected {
            Some(e) if existing > 0 => Some(hash_prefix(&part, e.algo).await?),
            Some(e) => Some(Hasher::new(e.algo)),
            None => None,
        };

        let mut req = client.get(url);
        if existing > 0 {
            if !announced {
                println!("检测到未完成下载 ({} 字节)，尝试续传...", existing);
                announced = true;
            }
            req = req.header(reqwest::header::RANGE, format!("bytes={}-", existing));
        }

        let resp = req
            .send()
            .await
            .with_context(|| format!("请求失败: {url}"))?;
        let status = resp.status();

        // 416: .part 已经够长（可能刚好完整）
        if status == reqwest::StatusCode::RANGE_NOT_SATISFIABLE {
            let total = parse_content_range_total(resp.headers());
            if existing > 0 && total == Some(existing) {
                if let (Some(exp), Some(h)) = (expected, hasher) {
                    let actual = h.finalize_hex();
                    if !actual.eq_ignore_ascii_case(exp.hex) {
                        tokio::fs::remove_file(&part).await.ok();
                        bail!("哈希校验失败: 期望 {}, 实际 {}", exp.hex, actual);
                    }
                }
                tokio::fs::rename(&part, dest)
                    .await
                    .with_context(|| format!("重命名失败: {}", part.display()))?;
                println!("本地片段已完整，直接使用: {}", dest.display());
                return Ok(());
            }
            // .part 比远端还大 / 远端变小 → 删掉重来
            tokio::fs::remove_file(&part).await.ok();
            announced = false;
            continue;
        }

        let resp = resp
            .error_for_status()
            .with_context(|| format!("服务器返回错误状态: {url}"))?;

        let resume = status == reqwest::StatusCode::PARTIAL_CONTENT;

        let (total, start) = if resume {
            (resp.content_length().map(|len| existing + len), existing)
        } else {
            (resp.content_length(), 0)
        };

        // 服务器忽略 Range → 从头重写，前缀哈希作废，重置 hasher
        if !resume {
            if let Some(e) = expected {
                hasher = Some(Hasher::new(e.algo));
            }
        }

        let mut file = if resume && start > 0 {
            tokio::fs::OpenOptions::new()
                .append(true)
                .open(&part)
                .await
                .with_context(|| format!("打开片段失败: {}", part.display()))?
        } else {
            tokio::fs::File::create(&part)
                .await
                .with_context(|| format!("创建片段失败: {}", part.display()))?
        };

        let pb = new_progress_bar(total, url, start);
        let mut stream = resp.bytes_stream();
        let mut written = start;

        while let Some(chunk) = stream.next().await {
            let chunk = chunk.with_context(|| format!("下载中断: {url}"))?;
            file.write_all(&chunk)
                .await
                .with_context(|| format!("写入失败: {}", part.display()))?;
            if let Some(h) = hasher.as_mut() {
                h.update(&chunk);
            }
            written += chunk.len() as u64;
            pb.set_position(written);
        }

        file.flush().await?;
        drop(file);

        if let Some(expected_total) = total {
            if written != expected_total {
                bail!(
                    "文件大小不匹配: 期望 {expected_total} 字节，实际 {written} 字节 (片段保留在 {})",
                    part.display()
                );
            }
        }

        if let (Some(exp), Some(h)) = (expected, hasher.take()) {
            let actual = h.finalize_hex();
            if !actual.eq_ignore_ascii_case(exp.hex) {
                tokio::fs::remove_file(&part).await.ok();
                bail!("哈希校验失败: 期望 {}, 实际 {}", exp.hex, actual);
            }
        }

        tokio::fs::rename(&part, dest)
            .await
            .with_context(|| format!("重命名失败: {}", part.display()))?;
        pb.finish_with_message(format!("✓ {url}"));
        return Ok(());
    }
}

/// 读取已有 `.part` 全量喂给 hasher，得到前缀状态
async fn hash_prefix(path: &Path, algo: HashAlgo) -> Result<Hasher> {
    let mut hasher = Hasher::new(algo);
    let mut file = tokio::fs::File::open(path)
        .await
        .with_context(|| format!("打开片段失败: {}", path.display()))?;
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher)
}

fn part_path(dest: &Path) -> PathBuf {
    let mut name = dest.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(".part");
    dest.with_file_name(name)
}

async fn file_len(path: &Path) -> u64 {
    tokio::fs::metadata(path).await.map(|m| m.len()).unwrap_or(0)
}

fn parse_content_range_total(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    let value = headers
        .get(reqwest::header::CONTENT_RANGE)?
        .to_str()
        .ok()?;
    value.rsplit('/').next()?.trim().parse().ok()
}

fn new_progress_bar(total: Option<u64>, url: &str, start: u64) -> ProgressBar {
    match total {
        Some(len) => {
            let pb = ProgressBar::new(len);
            pb.set_style(
                ProgressStyle::with_template(
                    "{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] \
                     {bytes}/{total_bytes} ({bytes_per_sec}, {eta})",
                )
                .unwrap()
                .progress_chars("=> "),
            );
            pb.set_position(start);
            pb.set_message(url.to_string());
            pb
        }
        None => {
            let pb = ProgressBar::new_spinner();
            pb.set_style(
                ProgressStyle::with_template(
                    "{spinner:.green} [{elapsed_precise}] {bytes} ({bytes_per_sec})",
                )
                .unwrap(),
            );
            pb.set_message(url.to_string());
            pb
        }
    }
}