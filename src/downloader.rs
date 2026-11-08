use anyhow::{bail, Context, Result};
use futures_util::StreamExt;
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

enum ProbeResult {
    Segmented(u64),
    Single,
    Unknown,
}

#[derive(Clone)]
pub struct Downloader {
    client: reqwest::Client,
    sem: Arc<Semaphore>,
    quiet: bool,
    chunk_size: u64,
    seg_threshold: u64,
}

impl Downloader {
    pub fn new(concurrency: usize, quiet: bool) -> Self {
        let client = reqwest::Client::builder()
            .user_agent("mc-downloader/0.1")
            .build()
            .expect("构建 HTTP 客户端失败");
        Self {
            client,
            sem: Arc::new(Semaphore::new(concurrency.max(1))),
            quiet,
            chunk_size: 4 * 1024 * 1024,
            seg_threshold: 8 * 1024 * 1024,
        }
    }

    pub fn with_chunking(mut self, chunk_size: u64, seg_threshold: u64) -> Self {
        self.chunk_size = chunk_size.max(64 * 1024);
        self.seg_threshold = seg_threshold.max(self.chunk_size * 2);
        self
    }

    pub async fn download(
        &self,
        url: &str,
        dest: &Path,
        expected: Option<ExpectedHash>,
    ) -> Result<()> {
        if dest.exists() {
            if let Some(ref exp) = expected {
                if verify_file_hash(dest, exp.algo, &exp.hex).await.unwrap_or(false) {
                    return Ok(());
                }
                tokio::fs::remove_file(dest).await.ok();
            } else {
                return Ok(());
            }
        }

        let segmented_total = match self.probe(url).await {
            Ok(ProbeResult::Segmented(total)) if total >= self.seg_threshold => Some(total),
            _ => None,
        };

        if let Some(total) = segmented_total {
            self.do_download_segmented(url, dest, expected, total).await
        } else {
            let _permit = self.sem.acquire().await.context("获取并发许可失败")?;
            self.do_download_single(url, dest, expected).await
        }
    }

    async fn probe(&self, url: &str) -> Result<ProbeResult> {
        let resp = self
            .client
            .get(url)
            .header(reqwest::header::RANGE, "bytes=0-0")
            .send()
            .await
            .with_context(|| format!("探测失败: {url}"))?;
        let status = resp.status();
        let total = parse_content_range_total(resp.headers());
        drop(resp);

        if status == reqwest::StatusCode::PARTIAL_CONTENT {
            if let Some(total) = total {
                return Ok(ProbeResult::Segmented(total));
            }
        } else if status.is_success() {
            return Ok(ProbeResult::Single);
        }
        Ok(ProbeResult::Unknown)
    }

    async fn do_download_segmented(
        &self,
        url: &str,
        dest: &Path,
        expected: Option<ExpectedHash>,
        total: u64,
    ) -> Result<()> {
        let chunk_size = self.chunk_size;
        let num_chunks = (total + chunk_size - 1) / chunk_size;

        let mut set = tokio::task::JoinSet::new();
        for i in 0..num_chunks {
            let client = self.client.clone();
            let sem = self.sem.clone();
            let url = url.to_string();
            let dest = dest.to_path_buf();
            set.spawn(async move {
                let _permit = sem
                    .acquire()
                    .await
                    .map_err(|e| anyhow::anyhow!("获取许可失败: {e}"))?;
                download_chunk(&client, &url, &dest, i, chunk_size, total).await
            });
        }

        let mut first_err: Option<anyhow::Error> = None;
        while let Some(res) = set.join_next().await {
            match res {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    if first_err.is_none() {
                        first_err = Some(e);
                    }
                }
                Err(join_err) => {
                    if first_err.is_none() {
                        first_err = Some(anyhow::anyhow!("块任务失败: {join_err}"));
                    }
                }
            }
        }

        if let Some(e) = first_err {
            return Err(e);
        }

        merge_chunks(dest, num_chunks, expected).await
    }

    async fn do_download_single(
        &self,
        url: &str,
        dest: &Path,
        expected: Option<ExpectedHash>,
    ) -> Result<()> {
        let part = part_path(dest);
        let mut announced = false;

        loop {
            let existing = file_len(&part).await;
            let mut hasher = match &expected {
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
                    if let (Some(exp), Some(h)) = (&expected, hasher) {
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

            let resp = resp
                .error_for_status()
                .with_context(|| format!("服务器错误: {url}"))?;
            let resume = status == reqwest::StatusCode::PARTIAL_CONTENT;
            let total = if resume {
                resp.content_length().map(|l| existing + l)
            } else {
                resp.content_length()
            };
            let start = if resume { existing } else { 0 };

            if !resume {
                if let Some(e) = &expected {
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

            let mut stream = resp.bytes_stream();
            let mut written = start;
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.with_context(|| format!("下载中断: {url}"))?;
                file.write_all(&chunk).await?;
                if let Some(h) = hasher.as_mut() {
                    h.update(&chunk);
                }
                written += chunk.len() as u64;
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

            if let (Some(exp), Some(h)) = (&expected, hasher.take()) {
                let actual = h.finalize_hex();
                if !actual.eq_ignore_ascii_case(&exp.hex) {
                    tokio::fs::remove_file(&part).await.ok();
                    bail!("哈希校验失败: 期望 {}, 实际 {}", exp.hex, actual);
                }
            }

            tokio::fs::rename(&part, dest).await?;
            return Ok(());
        }
    }
}

async fn download_chunk(
    client: &reqwest::Client,
    url: &str,
    dest: &Path,
    index: u64,
    chunk_size: u64,
    total: u64,
) -> Result<()> {
    let start = index * chunk_size;
    let end_exclusive = (start + chunk_size).min(total);
    let chunk_len = end_exclusive - start;
    let chunk_path = chunk_path(dest, index);

    let existing = file_len(&chunk_path).await.min(chunk_len);
    if existing == chunk_len {
        return Ok(());
    }

    let range_start = start + existing;
    let range_end = end_exclusive - 1;

    let resp = client
        .get(url)
        .header(
            reqwest::header::RANGE,
            format!("bytes={range_start}-{range_end}"),
        )
        .send()
        .await
        .with_context(|| format!("块 {index} 请求失败"))?;

    if resp.status() != reqwest::StatusCode::PARTIAL_CONTENT {
        bail!(
            "块 {index} 未返回 206（实际 {}），服务器可能中途不支持 Range",
            resp.status()
        );
    }

    if let Some(parent) = chunk_path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    let mut file = if existing > 0 {
        tokio::fs::OpenOptions::new()
            .append(true)
            .open(&chunk_path)
            .await?
    } else {
        tokio::fs::File::create(&chunk_path).await?
    };

    let mut stream = resp.bytes_stream();
    let mut written = existing;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.with_context(|| format!("块 {index} 下载中断"))?;
        file.write_all(&chunk).await?;
        written += chunk.len() as u64;
    }
    file.flush().await?;

    if written != chunk_len {
        bail!("块 {index} 大小不匹配: 期望 {chunk_len}, 实际 {written}");
    }
    Ok(())
}

async fn merge_chunks(
    dest: &Path,
    num_chunks: u64,
    expected: Option<ExpectedHash>,
) -> Result<()> {
    let final_part = part_path(dest);
    let mut out = tokio::fs::File::create(&final_part)
        .await
        .with_context(|| format!("创建合并文件失败: {}", final_part.display()))?;
    let mut hasher = expected.as_ref().map(|e| Hasher::new(e.algo));
    let mut buf = vec![0u8; 64 * 1024];

    for i in 0..num_chunks {
        let cp = chunk_path(dest, i);
        let mut f = tokio::fs::File::open(&cp)
            .await
            .with_context(|| format!("打开块失败: {}", cp.display()))?;
        loop {
            let n = f.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            out.write_all(&buf[..n]).await?;
            if let Some(h) = hasher.as_mut() {
                h.update(&buf[..n]);
            }
        }
    }
    out.flush().await?;
    drop(out);

    if let (Some(exp), Some(h)) = (&expected, hasher) {
        let actual = h.finalize_hex();
        if !actual.eq_ignore_ascii_case(&exp.hex) {
            tokio::fs::remove_file(&final_part).await.ok();
            for i in 0..num_chunks {
                tokio::fs::remove_file(chunk_path(dest, i)).await.ok();
            }
            bail!("哈希校验失败: 期望 {}, 实际 {}", exp.hex, actual);
        }
    }

    tokio::fs::rename(&final_part, dest).await?;

    for i in 0..num_chunks {
        tokio::fs::remove_file(chunk_path(dest, i)).await.ok();
    }
    tokio::fs::remove_file(part_path(dest)).await.ok();
    Ok(())
}

fn chunk_path(dest: &Path, index: u64) -> PathBuf {
    let mut name = dest.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(format!(".part.{index}"));
    dest.with_file_name(name)
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