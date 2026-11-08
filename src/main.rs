mod app;
mod assets;
mod downloader;
mod install;
mod manifest;
mod mirror;
mod ui;
mod version;

use anyhow::{Context, Result};
use app::{App, Msg};
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use manifest::VersionManifest;
use mirror::MirrorConfig;
use ratatui::{backend::CrosstermBackend, Terminal};
use std::{
    io::stdout,
    path::PathBuf,
    sync::mpsc,
    time::Duration,
};

fn main() -> Result<()> {
    let rt = tokio::runtime::Runtime::new()?;

    enable_raw_mode()?;
    let mut out = stdout();
    execute!(out, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(out);
    let mut terminal = Terminal::new(backend)?;

    let (tx, rx) = mpsc::unbounded_channel::<Msg>();
    let mut app = App::new(tx.clone(), rx);

    // 初始加载 manifest
    spawn_load_manifest(&rt, tx.clone(), app.mirror_enabled);

    let res = run_loop(&mut terminal, &mut app, &rt);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    res
}

fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    app: &mut App,
    rt: &tokio::runtime::Runtime,
) -> Result<()> {
    loop {
        terminal.draw(|f| ui::render(f, app))?;

        // 阻塞式读键盘，带 120ms 超时
        if event::poll(Duration::from_millis(120))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    handle_key(app, key.code, rt);
                }
            }
        }

        // 非阻塞消费 UI 消息
        while let Ok(msg) = app.rx.try_recv() {
            app.handle(msg);
        }

        if app.should_quit {
            return Ok(());
        }
    }
}

fn handle_key(app: &mut App, code: KeyCode, rt: &tokio::runtime::Runtime) {
    match code {
        KeyCode::Char('q') | KeyCode::Esc => app.should_quit = true,
        KeyCode::Char('j') | KeyCode::Down => app.prev(), // 列表反向（最新在上）
        KeyCode::Char('k') | KeyCode::Up => app.next(),
        KeyCode::Char('r') => {
            app.phase = app::Phase::Loading;
            app.push_log("刷新清单...");
            spawn_load_manifest(rt, app.tx.clone(), app.mirror_enabled);
        }
        KeyCode::Char('+') | KeyCode::Char('=') => {
            app.concurrency = (app.concurrency + 2).min(64);
            app.push_log(format!("并发 → {}", app.concurrency));
        }
        KeyCode::Char('-') => {
            app.concurrency = app.concurrency.saturating_sub(2).max(1);
            app.push_log(format!("并发 → {}", app.concurrency));
        }
        KeyCode::Enter => {
            if app.phase != app::Phase::Ready {
                app.push_log("当前状态不可安装");
                return;
            }
            let Some(item) = app.selected().cloned() else {
                return;
            };
            app.phase = app::Phase::Downloading;
            app.progress_current = 0;
            app.progress_total = 0;
            app.push_log(format!("开始安装 {}", item.id));

            let mirror = if app.mirror_enabled {
                MirrorConfig::default()
            } else {
                MirrorConfig::official()
            };
            let base = PathBuf::from(&app.base_dir);
            let id = item.id.clone();
            let tx = app.tx.clone();
            let conc = app.concurrency;
            let cs = app.chunk_size;
            let st = app.seg_threshold;

            rt.spawn(async move {
                let res = install::install(&id, &base, &mirror, conc, cs, st, Some(tx.clone()))
                    .await
                    .map_err(|e| format!("{e:#}"));
                let _ = tx.send(Msg::DownloadDone(res));
            });
        }
        _ => {}
    }
}

fn spawn_load_manifest(
    rt: &tokio::runtime::Runtime,
    tx: mpsc::UnboundedSender<Msg>,
    mirror_enabled: bool,
) {
    rt.spawn(async move {
        let mirror = if mirror_enabled {
            MirrorConfig::default()
        } else {
            MirrorConfig::official()
        };
        match load_manifest(&mirror).await {
            Ok(news) => {
                let _ = tx.send(Msg::ManifestLoaded(news));
            }
            Err(e) => {
                let _ = tx.send(Msg::Log(format!("加载失败: {e:#}")));
            }
        }
    });
}

async fn load_manifest(mirror: &MirrorConfig) -> Result<Vec<app::NewsItem>> {
    let client = reqwest::Client::new();
    let resp = client
        .get(&mirror.version_manifest)
        .send()
        .await
        .context("请求 manifest 失败")?
        .error_for_status()?;
    let mut json: serde_json::Value = resp.json().await?;
    mirror.rewrite_version_manifest(&mut json)?;
    let bytes = serde_json::to_vec(&json)?;
    let manifest = VersionManifest::from_slice(&bytes)?;
    Ok(app::news_from_entries(manifest.versions))
}