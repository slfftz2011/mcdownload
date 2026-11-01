use anyhow::{Context, Result};
use futures_util::StreamExt;
use indicatif::{ProgressBar, ProgressStyle};
use std::path::{Path, PathBuf};
use tokio::io::AsyncWriteExt;

pub async fn download(url: &str, dest: &Path) -> Result<()> {
    let client = reqwest::Client::builder()
        .user_agent("mc-downloader/0.1")
        .build()
        .context("创建 HTTP 客户端失败")?;

    let part = part_path(dest);
    let mut announced = false;

    loop {
        let existing = file_len(&part).await;

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

        // 416：本地 .part 已覆盖整个文件，或比远端还大
        if status == reqwest::StatusCode::RANGE_NOT_SATISFIABLE {
            let total = parse_content_range_total(resp.headers());
            if existing > 0 && total == Some(existing) {
                tokio::fs::rename(&part, dest)
                    .await
                    .with_context(|| format!("重命名失败: {}", part.display()))?;
                println!("本地片段已完整，直接使用: {}", dest.display());
                return Ok(());
            }
            // 远端文件变小了 / .part 过大，删掉重来
            tokio::fs::remove_file(&part).await.ok();
            announced = false;
            continue;
        }

        let resp = resp
            .error_for_status()
            .with_context(|| format!("服务器返回错误状态: {url}"))?;

        // 只有服务器真的回了 206 才认为可以追加
        let resume = status == reqwest::StatusCode::PARTIAL_CONTENT;

        let (total, start) = if resume {
            (resp.content_length().map(|len| existing + len), existing)
        } else {
            (resp.content_length(), 0)
        };

        let mut file = if resume && start > 0 {
            tokio::fs::OpenOptions::new()
                .append(true)
                .open(&part)
                .await
                .with_context(|| format!("打开片段失败: {}", part.display()))?
        } else {
            // 首次下载，或服务器不支持 Range → 截断重写
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
            written += chunk.len() as u64;
            pb.set_position(written);
        }

        file.flush().await?;
        drop(file);

        if let Some(expected) = total {
            if written != expected {
                anyhow::bail!(
                    "文件大小不匹配: 期望 {expected} 字节，实际 {written} 字节 (片段保留在 {})",
                    part.display()
                );
            }
        }

        tokio::fs::rename(&part, dest)
            .await
            .with_context(|| format!("重命名失败: {}", part.display()))?;
        pb.finish_with_message(format!("✓ {url}"));
        return Ok(());
    }
}

/// `foo.jar` → `foo.jar.part`
fn part_path(dest: &Path) -> PathBuf {
    let mut name = dest
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    name.push(".part");
    dest.with_file_name(name)
}

async fn file_len(path: &Path) -> u64 {
    tokio::fs::metadata(path)
        .await
        .map(|m| m.len())
        .unwrap_or(0)
}

/// 从 `Content-Range: bytes 0-99/1234` 或 `bytes */1234` 中取出总长度
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