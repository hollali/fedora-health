use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::symbols::Marker;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Axis, Block, Chart, Dataset, GraphType, Paragraph, Wrap};
use ratatui::Frame;

use super::app::App;
use super::themes::{ACCENT, ACCENT_ALT, MUTED};

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let chunks = Layout::vertical([Constraint::Percentage(58), Constraint::Min(5)]).split(area);
    let samples = app.visible_history();
    let total = app.history.len();
    let shown = samples.len();
    let start = total.saturating_sub(app.history_offset + shown);
    let title = format!(
        " I/O pressure and iowait — {} of {} samples (zoom x{}){} ",
        shown,
        total,
        app.history_zoom,
        if start > 0 {
            format!(", {start} older hidden")
        } else {
            String::new()
        }
    );
    if samples.is_empty() {
        let paragraph = Paragraph::new(
            "No samples recorded yet.\n\nStart recording with:\n  sudo hdd-watch watch 10\n\nor run the systemd service (see README).",
        )
        .block(Block::bordered().title(" History "))
        .wrap(Wrap { trim: true });
        frame.render_widget(paragraph, chunks[0]);
        render_bottom(frame, chunks[1], app, samples);
        return;
    }

    let io: Vec<(f64, f64)> = samples
        .iter()
        .enumerate()
        .map(|(index, sample)| (index as f64, sample.io_psi))
        .collect();
    let iowait: Vec<(f64, f64)> = samples
        .iter()
        .enumerate()
        .map(|(index, sample)| (index as f64, sample.iowait_pct))
        .collect();
    let mem: Vec<(f64, f64)> = samples
        .iter()
        .enumerate()
        .map(|(index, sample)| (index as f64, sample.mem_psi))
        .collect();
    let max_x = (samples.len().saturating_sub(1)).max(1) as f64;
    let first_ts = samples.first().map(|sample| sample.ts).unwrap_or(app.now);
    let last_ts = samples.last().map(|sample| sample.ts).unwrap_or(app.now);
    let x_labels = vec![
        ratatui::text::Span::styled(
            format!("{}m ago", app.now.saturating_sub(first_ts) / 60),
            Style::default().fg(MUTED),
        ),
        ratatui::text::Span::styled(
            format!("{}m ago", app.now.saturating_sub(last_ts) / 60),
            Style::default().fg(MUTED),
        ),
    ];
    let datasets = vec![
        Dataset::default()
            .name("io_psi")
            .marker(Marker::Braille)
            .graph_type(GraphType::Line)
            .style(Style::default().fg(ACCENT))
            .data(&io),
        Dataset::default()
            .name("iowait")
            .marker(Marker::Braille)
            .graph_type(GraphType::Line)
            .style(Style::default().fg(ACCENT_ALT))
            .data(&iowait),
        Dataset::default()
            .name("mem_psi")
            .marker(Marker::Dot)
            .graph_type(GraphType::Line)
            .style(Style::default().fg(MUTED))
            .data(&mem),
    ];
    let chart = Chart::new(datasets)
        .block(Block::bordered().title(title))
        .x_axis(
            Axis::default()
                .style(Style::default().fg(MUTED))
                .bounds([0.0, max_x])
                .labels(x_labels),
        )
        .y_axis(
            Axis::default()
                .style(Style::default().fg(MUTED))
                .bounds([0.0, 100.0])
                .labels(vec![
                    ratatui::text::Span::raw("0"),
                    ratatui::text::Span::raw("50"),
                    ratatui::text::Span::raw("100"),
                ]),
        );
    frame.render_widget(chart, chunks[0]);
    render_bottom(frame, chunks[1], app, samples);
}

fn render_bottom(frame: &mut Frame, area: Rect, app: &App, samples: &[crate::logfmt::LogSample]) {
    let columns =
        Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).split(area);

    let mut trend_lines: Vec<Line> = Vec::new();
    match crate::report::calculate_trend(samples) {
        Some(trend) => {
            trend_lines.push(Line::from(Span::styled(
                format!(
                    "io_psi {:.1}% -> {:.1}%   iowait {:.1}% -> {:.1}%",
                    trend.first_io_psi, trend.last_io_psi, trend.first_iowait, trend.last_iowait
                ),
                Style::default().fg(ACCENT),
            )));
            trend_lines.push(Line::from(format!(
                "free RAM {:.0} MB -> {:.0} MB ({:+.1}%)",
                trend.first_free_ram_mb, trend.last_free_ram_mb, trend.free_ram_change_pct
            )));
            trend_lines.push(Line::from(if trend.degrading {
                "conclusion: DEGRADING over time"
            } else {
                "conclusion: stable (no clear degradation)"
            }));
        }
        None => trend_lines.push(Line::from(
            "Need at least 6 samples in the window for a trend.",
        )),
    }
    if let Some(hint) = crate::report::swap_thrash_hint(samples) {
        trend_lines.push(Line::from(hint));
    }
    let trend = Paragraph::new(trend_lines)
        .block(Block::bordered().title(" Stats "))
        .wrap(Wrap { trim: true });
    frame.render_widget(trend, columns[0]);

    let mut worst_lines: Vec<Line> = Vec::new();
    for moment in crate::report::worst_moments(samples, app.now, 6) {
        worst_lines.push(Line::from(Span::styled(
            format!(
                "{:>4}m ago  io {:>5.1}%  wait {:>5.1}%  mem {:>5.1}%",
                moment.minutes_ago, moment.io_psi, moment.iowait_pct, moment.mem_psi
            ),
            Style::default().fg(ACCENT_ALT).add_modifier(Modifier::BOLD),
        )));
        if let Some((name, kbps)) = &moment.top_process {
            worst_lines.push(Line::from(format!(
                "          {} ({} KB/s)",
                name, *kbps as u64
            )));
        }
    }
    if worst_lines.is_empty() {
        worst_lines.push(Line::from("No samples in this window."));
    }
    let worst = Paragraph::new(worst_lines)
        .block(Block::bordered().title(" Worst moments "))
        .wrap(Wrap { trim: true });
    frame.render_widget(worst, columns[1]);
}
