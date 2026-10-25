use anyhow::{Context, Result};
use futures_util::StreamExt;
use indicatif::{ProgressBar, ProgressStyle};
use std::path::Path;
use tokio::io::AsyncWriteExt;

pub async fn download(url: &str, dest: &Path) -> Result<()> {
    let client = reqwest::Client::builder()
        .user_agent("mc-downloader/0.1")
        .build()
        .context("创建 HTTP 客户端失败")?;

    let resp = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("请求失败: {url}"))?
        .error_for_status()
        .with_context(|| format!("服务器返回错误状态: {url}"))?;

    let total = resp.content_length();
    let mut file = tokio::fs::File::create(dest)
        .await
        .with_context(|| format!("创建文件失败: {}", dest.display()))?;

    let pb = new_progress_bar(total, url);
    let mut stream = resp.bytes_stream();
    let mut written: u64 = 0;

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.with_context(|| format!("下载中断: {url}"))?;
        file.write_all(&chunk)
            .await
            .with_context(|| format!("写入失败: {}", dest.display()))?;
        written += chunk.len() as u64;
        pb.set_position(written);
    }

    file.flush().await?;
    drop(file);

    if let Some(expected) = total {
        if written != expected {
            anyhow::bail!("文件大小不匹配: 期望 {expected} 字节，实际 {written} 字节");
        }
    }

    pb.finish_with_message(format!("✓ {url}"));
    Ok(())
}

fn new_progress_bar(total: Option<u64>, url: &str) -> ProgressBar {
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