use anyhow::{bail, Context, Result};
use futures_util::StreamExt;
use indicatif::ProgressBar;
use sha2::Digest as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Semaphore;

#[derive(Clone, Copy, Debug)]
pub enum HashAlgo {
    Sha1,
    Sha256,
}

#[derive(Clone, Debug)]
pub struct ExpectedHash {
    pub algo: HashAlgo,
    pub hex: String,
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

#[derive(Clone)]
pub struct Downloader {
    client: reqwest::Client,
    sem: Arc<Semaphore>,
    progress: Option<Arc<ProgressBar>>,
    quiet: bool,
}

impl Downloader {
    pub fn new(concurrency: usize, quiet: bool) -> Self {
        let client = reqwest::Client::builder()
            .user_agent("mc-downloader/0.1")
            .build()
            .expect("构建 HTTP 客户端失败");
        Self {
            client,
            sem: Arc::new(Semaphore::new(concurrency)),
            progress: None,
            quiet,
        }
    }

    pub fn with_progress(mut self, pb: Arc<ProgressBar>) -> Self {
        self.progress = Some(pb);
        self
    }

    pub async fn download(
        &self,
        url: &str,
        dest: &Path,
        expected: Option<ExpectedHash>,
    ) -> Result<()> {
        let _permit = self.sem.acquire().await.context("获取并发许可失败")?;

        if dest.exists() {
            if let Some(ref exp) = expected {
                if verify_file_hash(dest, exp.algo, &exp.hex).await.unwrap_or(false) {
                    self.inc_progress();
                    return Ok(());
                }
                tokio::fs::remove_file(dest).await.ok();
            } else {
                self.inc_progress();
                return Ok(());
            }
        }

        if let Some(parent) = dest.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .with_context(|| format!("创建目录失败: {}", parent.display()))?;
        }

        let result = self.do_download(url, dest, expected.as_ref()).await;
        if result.is_ok() {
            self.inc_progress();
        }
        result
    }

    async fn do_download(
        &self,
        url: &str,
        dest: &Path,
        expected: Option<&ExpectedHash>,
    ) -> Result<()> {
        let part = part_path(dest);
        let mut announced = false;

        loop {
            let existing = file_len(&part).await;
            let mut hasher = match expected {
                Some(e) if existing > 0 => Some(hash_prefix(&part, e.algo).await?),
                Some(e) => Some(Hasher::new(e.algo)),
                None => None,
            };

            let mut req = self.client.get(url);
            if existing > 0 {
                if !announced && !self.quiet {
                    eprintln!("续传 {} ({} 字节)", dest.display(), existing);
                    announced = true;
                }
                req = req.header(reqwest::header::RANGE, format!("bytes={}-", existing));
            }

            let resp = req.send().await.with_context(|| format!("请求失败: {url}"))?;
            let status = resp.status();

            if status == reqwest::StatusCode::RANGE_NOT_SATISFIABLE {
                let total = parse_content_range_total(resp.headers());
                if existing > 0 && total == Some(existing) {
                    if let (Some(exp), Some(h)) = (expected, hasher) {
                        let actual = h.finalize_hex();
                        if !actual.eq_ignore_ascii_case(&exp.hex) {
                            tokio::fs::remove_file(&part).await.ok();
                            bail!("哈希校验失败: 期望 {}, 实际 {}", exp.hex, actual);
                        }
                    }
                    tokio::fs::rename(&part, dest).await?;
                    return Ok(());
                }
                tokio::fs::remove_file(&part).await.ok();
                announced = false;
                continue;
            }

            let resp = resp.error_for_status().with_context(|| format!("服务器错误: {url}"))?;
            let resume = status == reqwest::StatusCode::PARTIAL_CONTENT;
            let (total, start) = if resume {
                (resp.content_length().map(|l| existing + l), existing)
            } else {
                (resp.content_length(), 0)
            };

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

            let pb = if self.progress.is_none() {
                Some(new_progress_bar(total, url, start))
            } else {
                None
            };

            let mut stream = resp.bytes_stream();
            let mut written = start;
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.with_context(|| format!("下载中断: {url}"))?;
                file.write_all(&chunk).await?;
                if let Some(h) = hasher.as_mut() {
                    h.update(&chunk);
                }
                written += chunk.len() as u64;
                if let Some(pb) = &pb {
                    pb.set_position(written);
                }
            }
            file.flush().await?;
            drop(file);

            if let Some(expected_total) = total {
                if written != expected_total {
                    bail!(
                        "大小不匹配: 期望 {} 实际 {} (片段保留在 {})",
                        expected_total,
                        written,
                        part.display()
                    );
                }
            }

            if let (Some(exp), Some(h)) = (expected, hasher.take()) {
                let actual = h.finalize_hex();
                if !actual.eq_ignore_ascii_case(&exp.hex) {
                    tokio::fs::remove_file(&part).await.ok();
                    bail!("哈希校验失败: 期望 {}, 实际 {}", exp.hex, actual);
                }
            }

            tokio::fs::rename(&part, dest).await?;
            if let Some(pb) = pb {
                pb.finish_and_clear();
            }
            return Ok(());
        }
    }

    fn inc_progress(&self) {
        if let Some(pb) = &self.progress {
            pb.inc(1);
        }
    }
}

pub async fn verify_file_hash(path: &Path, algo: HashAlgo, expected_hex: &str) -> Result<bool> {
    let mut hasher = Hasher::new(algo);
    let mut file = tokio::fs::File::open(path).await?;
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize_hex().eq_ignore_ascii_case(expected_hex))
}

async fn hash_prefix(path: &Path, algo: HashAlgo) -> Result<Hasher> {
    let mut hasher = Hasher::new(algo);
    let mut file = tokio::fs::File::open(path).await?;
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
    let value = headers.get(reqwest::header::CONTENT_RANGE)?.to_str().ok()?;
    value.rsplit('/').next()?.trim().parse().ok()
}

fn new_progress_bar(total: Option<u64>, url: &str, start: u64) -> ProgressBar {
    use indicatif::ProgressStyle;
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