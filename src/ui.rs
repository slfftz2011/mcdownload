use crate::app::{App, Phase};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Gauge, List, ListItem, Paragraph, Wrap},
    Frame,
};

pub fn render(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // 顶部标题
            Constraint::Min(10),   // 主体
            Constraint::Length(2), // 底部状态栏
        ])
        .split(area);

    render_header(f, app, outer[0]);
    render_body(f, app, outer[1]);
    render_footer(f, app, outer[2]);
}

fn render_header(f: &mut Frame, app: &App, area: Rect) {
    let mirror = if app.mirror_enabled { "BMCLAPI" } else { "官方源" };
    let phase = match app.phase {
        Phase::Loading => "加载中",
        Phase::Ready => "就绪",
        Phase::Downloading => "下载中",
        Phase::Done => "完成",
    };
    let line = Line::from(vec![
        Span::styled(
            " MC Downloader ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  PCL 风格主页  "),
        Span::styled(
            format!("[{mirror}] [{phase}]"),
            Style::default().fg(Color::Yellow),
        ),
    ]);
    let p = Paragraph::new(line)
        .block(Block::default().borders(Borders::ALL).title(" 主页 "));
    f.render_widget(p, area);
}

fn render_body(f: &mut Frame, app: &mut App, area: Rect) {
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(58), Constraint::Percentage(42)])
        .split(area);

    // 左：新闻列表 + 日志
    let left = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(8), Constraint::Length(8)])
        .split(cols[0]);

    render_news(f, app, left[0]);
    render_logs(f, app, left[1]);

    // 右：详情 + 进度
    render_detail(f, app, cols[1]);
}

fn render_news(f: &mut Frame, app: &mut App, area: Rect) {
    let items: Vec<ListItem> = app
        .news
        .iter()
        .map(|n| {
            let title = Line::from(vec![
                Span::styled("▸ ", Style::default().fg(Color::Cyan)),
                Span::styled(
                    n.title.clone(),
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                ),
            ]);
            let summary = Line::from(Span::styled(
                format!("  {}", n.summary),
                Style::default().fg(Color::Gray),
            ));
            ListItem::new(vec![title, summary])
        })
        .collect();

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" 新闻 / 版本动态 "),
        )
        .highlight_style(
            Style::default()
                .bg(Color::Rgb(40, 60, 90))
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");

    f.render_stateful_widget(list, area, &mut app.list_state);
}

fn render_logs(f: &mut Frame, app: &App, area: Rect) {
    let lines: Vec<Line> = app
        .logs
        .iter()
        .rev()
        .take((area.height as usize).saturating_sub(2))
        .rev()
        .map(|s| Line::from(Span::styled(s.clone(), Style::default().fg(Color::Gray))))
        .collect();
    let p = Paragraph::new(lines)
        .block(Block::default().borders(Borders::ALL).title(" 日志 "))
        .wrap(Wrap { trim: true });
    f.render_widget(p, area);
}

fn render_detail(f: &mut Frame, app: &App, area: Rect) {
    let detail_h = 10;
    let parts = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(detail_h), Constraint::Min(4)])
        .split(area);

    // 版本详情
    let (id, kind, time) = match app.selected() {
        Some(n) => (n.id.clone(), n.kind.clone(), n.time.clone()),
        None => ("-".into(), "-".into(), "-".into()),
    };
    let mut lines = vec![
        Line::from(vec![
            Span::styled("版本: ", Style::default().fg(Color::Gray)),
            Span::styled(id, Style::default().add_modifier(Modifier::BOLD)),
        ]),
        Line::from(vec![
            Span::styled("类型: ", Style::default().fg(Color::Gray)),
            Span::styled(kind, Style::default().fg(Color::Yellow)),
        ]),
        Line::from(vec![
            Span::styled("时间: ", Style::default().fg(Color::Gray)),
            Span::raw(time),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled("目录: ", Style::default().fg(Color::Gray)),
            Span::raw(app.base_dir.clone()),
        ]),
        Line::from(vec![
            Span::styled("并发: ", Style::default().fg(Color::Gray)),
            Span::raw(app.concurrency.to_string()),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "[Enter] 安装此版本",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        )),
    ];
    lines.push(Line::from(""));
    let p = Paragraph::new(lines)
        .block(Block::default().borders(Borders::ALL).title(" 版本详情 "))
        .wrap(Wrap { trim: true });
    f.render_widget(p, parts[0]);

    // 进度
    let ratio = if app.progress_total == 0 {
        0.0
    } else {
        app.progress_current as f64 / app.progress_total as f64
    };
    let label = if app.progress_total == 0 {
        "等待中".to_string()
    } else {
        format!(
            "{}/{} {}",
            app.progress_current, app.progress_total, app.progress_label
        )
    };
    let gauge = Gauge::default()
        .block(Block::default().borders(Borders::ALL).title(" 下载进度 "))
        .gauge_style(
            Style::default()
                .fg(Color::Cyan)
                .bg(Color::Black)
                .add_modifier(Modifier::BOLD),
        )
        .ratio(ratio.clamp(0.0, 1.0))
        .label(label);
    f.render_widget(gauge, parts[1]);
}

fn render_footer(f: &mut Frame, app: &App, area: Rect) {
    let hint = match app.phase {
        Phase::Loading => "正在加载版本清单...",
        Phase::Downloading => "下载中，请稍候...",
        _ => "q 退出 · j/k 选择 · Enter 安装 · r 刷新 · +/- 并发",
    };
    let p = Paragraph::new(Line::from(Span::styled(
        format!(" {hint}"),
        Style::default().fg(Color::Black).bg(Color::Gray),
    )));
    f.render_widget(p, area);
}