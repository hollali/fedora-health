use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Cell, Gauge, Paragraph, Row, Sparkline, Table, Wrap};
use ratatui::Frame;

use super::app::{App, SortMode};
use super::themes::{load_color, ram_color, severity_color, swap_color, ACCENT, MUTED};

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let chunks = Layout::vertical([Constraint::Length(5), Constraint::Min(6)]).split(area);
    let gauges = Layout::horizontal([
        Constraint::Percentage(20),
        Constraint::Percentage(20),
        Constraint::Percentage(20),
        Constraint::Percentage(20),
        Constraint::Percentage(20),
    ])
    .split(chunks[0]);

    let latest = app.live.back();
    let (io_psi, mem_psi, iowait, ram_mb, swap_mb) = latest
        .map(|sample| {
            (
                sample.io_psi,
                sample.mem_psi,
                sample.iowait_pct,
                sample.mem_avail_mb,
                sample.swap_used_mb,
            )
        })
        .unwrap_or((0.0, 0.0, 0.0, 0, 0));

    render_gauge(
        frame,
        gauges[0],
        "I/O pressure",
        io_psi / 100.0,
        &format!("{io_psi:.1}%"),
        load_color(io_psi),
    );
    render_gauge(
        frame,
        gauges[1],
        "Mem pressure",
        mem_psi / 100.0,
        &format!("{mem_psi:.1}%"),
        severity_color(crate::report::mem_psi_severity(mem_psi)),
    );
    render_gauge(
        frame,
        gauges[2],
        "iowait",
        iowait / 100.0,
        &format!("{iowait:.1}%"),
        severity_color(crate::report::iowait_severity(iowait)),
    );
    render_gauge(
        frame,
        gauges[3],
        "RAM avail",
        (ram_mb as f64 / 16_384.0).clamp(0.0, 1.0),
        &format!("{ram_mb} MB"),
        ram_color(ram_mb),
    );
    render_gauge(
        frame,
        gauges[4],
        "Swap used",
        (swap_mb as f64 / 8_192.0).clamp(0.0, 1.0),
        &format!("{swap_mb} MB"),
        swap_color(swap_mb),
    );

    let lower = Layout::horizontal([Constraint::Percentage(64), Constraint::Percentage(36)])
        .split(chunks[1]);
    render_processes(frame, lower[0], app);
    render_side(frame, lower[1], app, io_psi);
}

fn render_gauge(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    ratio: f64,
    label: &str,
    color: ratatui::style::Color,
) {
    let gauge = Gauge::default()
        .block(Block::bordered().title(format!(" {title} ")))
        .gauge_style(Style::default().fg(color))
        .ratio(ratio.clamp(0.0, 1.0))
        .label(label.to_string());
    frame.render_widget(gauge, area);
}

fn render_processes(frame: &mut Frame, area: Rect, app: &App) {
    let header = Row::new(vec!["process", "read/s", "write/s", "total/s"])
        .style(Style::default().fg(ACCENT).add_modifier(Modifier::BOLD));
    let mut rates = app.live_rates.clone();
    match app.sort {
        SortMode::Total => rates.sort_by(|a, b| b.total_kbps().total_cmp(&a.total_kbps())),
        SortMode::Read => rates.sort_by(|a, b| b.read_kbps.total_cmp(&a.read_kbps)),
        SortMode::Write => rates.sort_by(|a, b| b.write_kbps.total_cmp(&a.write_kbps)),
    }
    let rows: Vec<Row> = rates
        .iter()
        .take(area.height.saturating_sub(3) as usize)
        .map(|rate| {
            Row::new(vec![
                Cell::from(rate.name.clone()),
                Cell::from(format_mbps(rate.read_kbps)),
                Cell::from(format_mbps(rate.write_kbps)),
                Cell::from(format_mbps(rate.total_kbps())),
            ])
            .style(Style::default().fg(if rate.total_kbps() > 1000.0 {
                ratatui::style::Color::Red
            } else {
                ratatui::style::Color::Reset
            }))
        })
        .collect();
    let title = if app.paused {
        " Top processes (PAUSED) "
    } else {
        " Top processes "
    };
    let table = Table::new(
        rows,
        [
            Constraint::Min(12),
            Constraint::Length(10),
            Constraint::Length(10),
            Constraint::Length(10),
        ],
    )
    .header(header)
    .block(
        Block::bordered()
            .title(title)
            .title_bottom(format!(" sort: {} (s) ", app.sort.label())),
    );
    frame.render_widget(table, area);
}

fn render_side(frame: &mut Frame, area: Rect, app: &App, io_psi: f64) {
    let chunks = Layout::vertical([Constraint::Percentage(45), Constraint::Min(4)]).split(area);
    let data: Vec<u64> = app
        .live
        .iter()
        .rev()
        .take(120)
        .rev()
        .map(|sample| (sample.io_psi * 10.0).max(0.0) as u64)
        .collect();
    let sparkline = Sparkline::default()
        .block(Block::bordered().title(" I/O PSI trend (2 min) "))
        .data(&data)
        .style(Style::default().fg(load_color(io_psi)));
    frame.render_widget(sparkline, chunks[0]);

    let mut lines = vec![Line::from(format!("samples in window: {}", app.live.len()))];
    if let Some(name) = app.top_streak_name() {
        lines.push(Line::from(format!("longest top-3 streak: {name}")));
    }
    match app.live_rates.first() {
        Some(rate) => lines.push(Line::from(format!(
            "busiest right now: {} ({})",
            rate.name,
            format_mbps(rate.total_kbps())
        ))),
        None => lines.push(Line::from("no per-process samples yet")),
    }
    let live_samples: Vec<crate::logfmt::LogSample> = app.live.iter().cloned().collect();
    if let Some(hint) = crate::report::swap_thrash_hint(&live_samples) {
        lines.push(Line::from(hint));
    }
    if !crate::logfmt::is_root() {
        lines.push(Line::from("run with sudo to see other users' I/O"));
    }
    let paragraph = Paragraph::new(lines)
        .block(Block::bordered().title(" Summary "))
        .wrap(Wrap { trim: true })
        .style(Style::default().fg(MUTED));
    frame.render_widget(paragraph, chunks[1]);
}

fn format_mbps(kbps: f64) -> String {
    if kbps >= 1024.0 {
        format!("{:.1} MB/s", kbps / 1024.0)
    } else {
        format!("{kbps:.0} KB/s")
    }
}
