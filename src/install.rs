use crate::app::Msg;
use crate::assets::AssetIndex;
use crate::downloader::{Downloader, ExpectedHash, HashAlgo};
use crate::manifest::VersionManifest;
use crate::mirror::MirrorConfig;
use crate::version::{collect_libraries, VersionJson};
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use tokio::sync::Mutex;

pub async fn install(
    version_id: &str,
    base_dir: &Path,
    mirror: &MirrorConfig,
    concurrency: usize,
    chunk_size: u64,
    seg_threshold: u64,
    tx: Option<mpsc::UnboundedSender<Msg>>,
) -> Result<()> {
    let send = |msg: Msg| {
        if let Some(t) = &tx {
            let _ = t.send(msg);
        }
    };

    send(Msg::Log(format!("目标目录: {}", base_dir.display())));
    send(Msg::Log(format!("版本: {version_id}")));

    // 1. manifest
    send(Msg::Log("获取版本清单...".into()));
    let manifest_url = mirror.version_manifest.clone();
    let manifest_bytes = http_get(&manifest_url).await?;
    let mut manifest_json: serde_json::Value =
        serde_json::from_slice(&manifest_bytes).context("解析 manifest JSON 失败")?;
    mirror.rewrite_version_manifest(&mut manifest_json)?;
    let manifest = VersionManifest::from_slice(&serde_json::to_vec(&manifest_json)?)?;

    let entry = manifest.resolve(version_id)?.clone();
    send(Msg::Log(format!(
        "解析到版本: {} ({})",
        entry.id, entry.kind
    )));

    // 2. version.json
    let version_bytes = http_get(&entry.url).await?;
    let version = VersionJson::from_slice(&version_bytes)?;

    let version_dir = base_dir.join("versions").join(&version.id);
    tokio::fs::create_dir_all(&version_dir).await?;
    tokio::fs::write(
        version_dir.join(format!("{}.json", version.id)),
        &version_bytes,
    )
    .await?;

    // 3. 收集任务
    let mut tasks: Vec<Task> = Vec::new();

    // 3a. client.jar
    if let Some(dl) = &version.downloads {
        if let Some(client) = &dl.client {
            tasks.push(Task {
                url: mirror.rewrite(&client.url),
                dest: version_dir.join(format!("{}.jar", version.id)),
                sha1: Some(client.sha1.clone()),
                label: format!("{}.jar", version.id),
            });
        }
    }

    // 3b. libraries
    let libs = collect_libraries(&version.libraries, &mirror.maven);
    send(Msg::Log(format!("libraries 待下载: {} 项", libs.len())));
    for lib in libs {
        tasks.push(Task {
            url: mirror.rewrite(&lib.url),
            dest: base_dir.join("libraries").join(&lib.rel_path),
            sha1: if lib.sha1.is_empty() {
                None
            } else {
                Some(lib.sha1)
            },
            label: lib.label,
        });
    }

    // 3c. asset index + objects
    if let Some(idx_ref) = &version.asset_index {
        let idx_url = mirror.rewrite(&idx_ref.url);
        let idx_dest = base_dir
            .join("assets")
            .join("indexes")
            .join(format!("{}.json", idx_ref.id));
        tokio::fs::create_dir_all(idx_dest.parent().unwrap()).await?;
        let idx_bytes = http_get(&idx_url).await?;
        tokio::fs::write(&idx_dest, &idx_bytes).await?;

        let index = AssetIndex::from_slice(&idx_bytes)?;
        let unique = index.unique_objects();
        send(Msg::Log(format!("assets 待下载: {} 个对象", unique.len())));
        for obj in unique {
            let h2 = &obj.hash[..2];
            let dest = base_dir
                .join("assets")
                .join("objects")
                .join(h2)
                .join(&obj.hash);
            let url = format!("https://resources.download.minecraft.net/{h2}/{}", obj.hash);
            tasks.push(Task {
                url: mirror.rewrite(&url),
                dest,
                sha1: Some(obj.hash.clone()),
                label: format!("assets {}", &obj.hash[..12]),
            });
        }
    }

    // 4. 并发下载
    let total = tasks.len();
    send(Msg::Log(format!(
        "开始下载，共 {} 个文件，全局并发 {}，分块阈值 {} MB，块大小 {} MB",
        total,
        concurrency,
        seg_threshold / (1024 * 1024),
        chunk_size / (1024 * 1024)
    )));
    send(Msg::FileProgress {
        current: 0,
        total,
        label: "准备中".into(),
    });

    let dl = Downloader::new(concurrency, true).with_chunking(chunk_size, seg_threshold);

    let errors = Arc::new(Mutex::new(Vec::<(String, String)>::new()));
    let done = Arc::new(AtomicUsize::new(0));

    let mut handles = Vec::with_capacity(total);
    for task in tasks {
        let dl = dl.clone();
        let tx = tx.clone();
        let errors = errors.clone();
        let done = done.clone();
        handles.push(tokio::spawn(async move {
            let expected = task.sha1.as_ref().map(|h| ExpectedHash {
                algo: HashAlgo::Sha1,
                hex: h.clone(),
            });
            let label = task.label.clone();
            if let Err(e) = dl.download(&task.url, &task.dest, expected).await {
                errors.lock().await.push((task.label, format!("{e:#}")));
            }
            let cur = done.fetch_add(1, Ordering::Relaxed) + 1;
            if let Some(t) = &tx {
                let _ = t.send(Msg::FileProgress {
                    current: cur,
                    total,
                    label,
                });
            }
        }));
    }
    for h in handles {
        let _ = h.await;
    }

    // 5. 汇总
    let errors = errors.lock().await;
    if errors.is_empty() {
        send(Msg::Log("✓ 安装完成".into()));
        Ok(())
    } else {
        send(Msg::Log(format!("✗ {} 个文件下载失败:", errors.len())));
        for (label, err) in errors.iter().take(20) {
            send(Msg::Log(format!("  - {label}: {err}")));
        }
        if errors.len() > 20 {
            send(Msg::Log(format!("  ...（省略 {} 条）", errors.len() - 20)));
        }
        Err(anyhow::anyhow!("{} 个文件下载失败", errors.len()))
    }
}

struct Task {
    url: String,
    dest: PathBuf,
    sha1: Option<String>,
    label: String,
}

async fn http_get(url: &str) -> Result<Vec<u8>> {
    let resp = reqwest::Client::new()
        .get(url)
        .send()
        .await
        .with_context(|| format!("请求失败: {url}"))?
        .error_for_status()
        .with_context(|| format!("服务器错误: {url}"))?;
    Ok(resp.bytes().await?.to_vec())
}