use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Cell, Paragraph, Row, Table, TableState, Wrap};
use ratatui::Frame;

use super::app::App;
use super::themes::{ACCENT, MUTED};

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let chunks = Layout::vertical([Constraint::Percentage(58), Constraint::Min(6)]).split(area);
    let columns = Layout::horizontal([Constraint::Percentage(64), Constraint::Percentage(36)])
        .split(chunks[0]);
    render_apps(frame, columns[0], app);
    render_records(frame, columns[1], app);
    render_bottom(frame, chunks[1], app);
}

fn render_apps(frame: &mut Frame, area: Rect, app: &App) {
    let indices = app.filtered_app_indices();
    let header = Row::new(vec!["app", "id", "exec"])
        .style(Style::default().fg(ACCENT).add_modifier(Modifier::BOLD));
    let rows: Vec<Row> = indices
        .iter()
        .filter_map(|index| app.apps.get(*index))
        .map(|entry| {
            Row::new(vec![
                Cell::from(entry.name.clone()),
                Cell::from(entry.id.clone()),
                Cell::from(entry.exec.clone()),
            ])
        })
        .collect();
    let title = if app.filter_editing {
        format!(" Apps — filter> {}_ ", app.app_filter)
    } else if app.app_filter.is_empty() {
        " Apps ".to_string()
    } else {
        format!(" Apps — filter: {} ", app.app_filter)
    };
    let table = Table::new(
        rows,
        [
            Constraint::Length(24),
            Constraint::Min(18),
            Constraint::Length(24),
        ],
    )
    .header(header)
    .row_highlight_style(
        Style::default()
            .bg(ratatui::style::Color::DarkGray)
            .fg(ratatui::style::Color::White)
            .add_modifier(Modifier::BOLD),
    )
    .highlight_symbol("> ")
    .block(
        Block::bordered()
            .title(title)
            .title_bottom(" / filter  enter warm  c cold  p prewarm "),
    );
    let selected = if indices.is_empty() {
        None
    } else {
        Some(app.app_selected.min(indices.len() - 1))
    };
    let mut state = TableState::default().with_selected(selected);
    frame.render_stateful_widget(table, area, &mut state);
}

fn render_records(frame: &mut Frame, area: Rect, app: &App) {
    let mut lines: Vec<Line> = Vec::new();
    if let Some(index) = app.selected_app_index() {
        let records = app.app_records(index);
        if records.is_empty() {
            lines.push(Line::from("No benchmark history for this app."));
        } else {
            for record in records.iter().rev().take(7) {
                let age = app.now.saturating_sub(record.ts);
                lines.push(Line::from(Span::styled(
                    format!(
                        "{:>6}  {:<4} {:>7.1}s  cpu {:>6.1}s",
                        crate::report::format_duration(age),
                        record.kind,
                        record.settled_ms as f64 / 1000.0,
                        record.cpu_ms as f64 / 1000.0
                    ),
                    Style::default().fg(ACCENT),
                )));
                lines.push(Line::from(format!(
                    "        read {:.0} MB  majflt {}  D {:.0}%  blkio {:.0}%",
                    record.read_bytes as f64 / 1_048_576.0,
                    record.majflt,
                    record.d_pct,
                    record.blkio_pct
                )));
                lines.push(Line::from(vec![ratatui::text::Span::styled(
                    format!("        verdict: {}", record.verdict),
                    Style::default().fg(MUTED),
                )]));
            }
        }
    } else {
        lines.push(Line::from("No app selected."));
    }
    let paragraph = Paragraph::new(lines)
        .block(Block::bordered().title(" Benchmark history "))
        .wrap(Wrap { trim: true });
    frame.render_widget(paragraph, area);
}

fn render_bottom(frame: &mut Frame, area: Rect, app: &App) {
    let columns =
        Layout::horizontal([Constraint::Percentage(55), Constraint::Percentage(45)]).split(area);
    let mut left: Vec<Line> = Vec::new();
    if app.launch_running {
        if let Some(progress) = &app.launch_progress {
            left.push(Line::from(Span::styled(
                progress.phase.clone(),
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            )));
            left.push(Line::from(format!(
                "elapsed {:.1}s  cpu {:.1}s  read {:.0} MB  majflt {}  procs {}",
                progress.elapsed_ms as f64 / 1000.0,
                progress.cpu_ms as f64 / 1000.0,
                progress.read_bytes as f64 / 1_048_576.0,
                progress.majflt,
                progress.procs
            )));
        } else {
            left.push(Line::from("starting benchmark..."));
        }
    } else {
        left.push(Line::from(
            "Select an app, then: enter = warm start, c = cold start (root), p = add to prewarm.conf.",
        ));
        left.push(Line::from(
            "The app is started and closed automatically once it settles.",
        ));
    }
    if let Some(index) = app.selected_app_index() {
        if let Some(entry) = app.apps.get(index) {
            left.push(Line::from(format!(
                "command: {}",
                crate::apps::strip_exec_field_codes(&entry.exec)
            )));
            let records = app.app_records(index);
            if let Some(latest) = records.last() {
                left.push(Line::from(format!(
                    "latest verdict: {} — {}",
                    latest.verdict,
                    verdict_note(&latest.verdict)
                )));
            }
        }
    }
    let left_paragraph = Paragraph::new(left)
        .block(Block::bordered().title(" Selected app "))
        .wrap(Wrap { trim: true });
    frame.render_widget(left_paragraph, columns[0]);

    let mut right: Vec<Line> = Vec::new();
    if let Some(index) = app.selected_app_index() {
        let findings = app.matching_findings(index);
        if findings.is_empty() {
            right.push(Line::from("No related startup findings."));
        } else {
            right.push(Line::from("Related doctor findings:"));
            for finding in findings.iter().take(6) {
                right.push(Line::from(format!(
                    "- {} [{}]{}",
                    finding.title,
                    finding.id,
                    if finding.fix_available {
                        " (fixable)"
                    } else {
                        ""
                    }
                )));
            }
        }
    }
    right.push(Line::from(
        "Prewarm warms only the selected app and refuses if it would consume >25% of MemAvailable.",
    ));
    let right_paragraph = Paragraph::new(right)
        .block(Block::bordered().title(" Related checks "))
        .wrap(Wrap { trim: true })
        .style(Style::default().fg(MUTED));
    frame.render_widget(right_paragraph, columns[1]);
}

fn verdict_note(verdict: &str) -> &'static str {
    match verdict {
        "disk-bound" => "prewarm and reduce indexers; the disk is the bottleneck",
        "cpu-bound" => "CPU dominates; prewarming will not help much",
        "waiting" => "waiting on DNS, D-Bus or the network rather than the disk",
        _ => "no verdict recorded",
    }
}
