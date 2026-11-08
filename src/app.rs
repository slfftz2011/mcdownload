use crate::manifest::VersionEntry;
use ratatui::widgets::ListState;
use tokio::sync::mpsc;

#[derive(Debug, Clone)]
pub struct NewsItem {
    pub id: String,
    pub kind: String,
    pub time: String,
    pub url: String,
    pub title: String,
    pub summary: String,
}

pub fn news_from_entries(mut entries: Vec<VersionEntry>) -> Vec<NewsItem> {
    // 最新在前
    entries.sort_by(|a, b| b.release_time.cmp(&a.release_time));
    entries
        .into_iter()
        .map(|e| {
            let time = e.release_time.replace('T', " ").replace('Z', "");
            let time = time.split('.').next().unwrap_or(&time).to_string();
            NewsItem {
                title: format!("{} · {}", e.id, e.kind),
                summary: format!("发布于 {} · Enter 安装此版本", time),
                id: e.id,
                kind: e.kind,
                time,
                url: e.url,
            }
        })
        .collect()
}

#[derive(Debug)]
pub enum Msg {
    ManifestLoaded(Vec<NewsItem>),
    FileProgress { current: usize, total: usize, label: String },
    Log(String),
    DownloadDone(Result<(), String>),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Phase {
    Loading,
    Ready,
    Downloading,
    Done,
}

pub struct App {
    pub should_quit: bool,
    pub phase: Phase,
    pub news: Vec<NewsItem>,
    pub list_state: ListState,
    pub logs: Vec<String>,
    pub progress_current: usize,
    pub progress_total: usize,
    pub progress_label: String,
    pub concurrency: usize,
    pub chunk_size: u64,
    pub seg_threshold: u64,
    pub base_dir: String,
    pub mirror_enabled: bool,
    pub tx: mpsc::UnboundedSender<Msg>,
    pub rx: mpsc::UnboundedReceiver<Msg>,
}

impl App {
    pub fn new(tx: mpsc::UnboundedSender<Msg>, rx: mpsc::UnboundedReceiver<Msg>) -> Self {
        let mut list_state = ListState::default();
        list_state.select(Some(0));
        Self {
            should_quit: false,
            phase: Phase::Loading,
            news: Vec::new(),
            list_state,
            logs: vec!["启动中...".into()],
            progress_current: 0,
            progress_total: 0,
            progress_label: String::new(),
            concurrency: 16,
            chunk_size: 4 * 1024 * 1024,
            seg_threshold: 8 * 1024 * 1024,
            base_dir: "minecraft".into(),
            mirror_enabled: true,
            tx,
            rx,
        }
    }

    pub fn push_log(&mut self, msg: impl Into<String>) {
        self.logs.push(msg.into());
        if self.logs.len() > 200 {
            let drain = self.logs.len() - 200;
            self.logs.drain(0..drain);
        }
    }

    pub fn selected(&self) -> Option<&NewsItem> {
        self.list_state
            .selected()
            .and_then(|i| self.news.get(i))
    }

    pub fn next(&mut self) {
        if self.news.is_empty() {
            return;
        }
        let i = match self.list_state.selected() {
            Some(i) => (i + 1) % self.news.len(),
            None => 0,
        };
        self.list_state.select(Some(i));
    }

    pub fn prev(&mut self) {
        if self.news.is_empty() {
            return;
        }
        let i = match self.list_state.selected() {
            Some(i) if i == 0 => self.news.len() - 1,
            Some(i) => i - 1,
            None => 0,
        };
        self.list_state.select(Some(i));
    }

    pub fn handle(&mut self, msg: Msg) {
        match msg {
            Msg::ManifestLoaded(news) => {
                let n = news.len();
                self.news = news;
                self.list_state.select(Some(0));
                self.phase = Phase::Ready;
                self.push_log(format!("已加载 {n} 个版本"));
            }
            Msg::FileProgress {
                current,
                total,
                label,
            } => {
                self.progress_current = current;
                self.progress_total = total;
                self.progress_label = label;
            }
            Msg::Log(s) => self.push_log(s),
            Msg::DownloadDone(res) => {
                match res {
                    Ok(()) => {
                        self.phase = Phase::Done;
                        self.progress_current = self.progress_total;
                        self.push_log("✓ 安装完成");
                    }
                    Err(e) => {
                        self.phase = Phase::Ready;
                        self.push_log(format!("✗ 安装失败: {e}"));
                    }
                }
            }
        }
    }
}