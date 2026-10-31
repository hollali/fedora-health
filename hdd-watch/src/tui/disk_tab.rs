use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Cell, Paragraph, Row, Table, Wrap};
use ratatui::Frame;

use super::app::App;
use super::themes::{ACCENT, MUTED};

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let chunks = Layout::vertical([
        Constraint::Length(4),
        Constraint::Min(5),
        Constraint::Length(8),
    ])
    .split(area);
    let disk = &app.disk;
    let summary = vec![
        Line::from(format!(
            "filesystem: {}   mounts: {}",
            if disk.filesystem.is_empty() {
                "unknown"
            } else {
                &disk.filesystem
            },
            if disk.mounts.is_empty() {
                "unknown"
            } else {
                &disk.mounts
            }
        )),
        Line::from(format!(
            "root usage: {}",
            if disk.usage.is_empty() {
                "unknown"
            } else {
                &disk.usage
            }
        )),
        Line::from(if app.disk_loading {
            "loading SMART data...".to_string()
        } else {
            "SMART needs root for attribute details; press r to reload.".to_string()
        }),
    ];
    frame.render_widget(
        Paragraph::new(summary)
            .block(Block::bordered().title(" Disk "))
            .style(Style::default().fg(ACCENT)),
        chunks[0],
    );

    let header = Row::new(vec!["device", "model", "health", "reallocated", "pending"])
        .style(Style::default().fg(ACCENT).add_modifier(Modifier::BOLD));
    let rows: Vec<Row> = disk
        .smart
        .iter()
        .map(|smart| {
            let health_color = if smart.health.contains("PASSED") {
                ratatui::style::Color::Green
            } else if smart.health.contains("unknown") {
                ratatui::style::Color::Yellow
            } else {
                ratatui::style::Color::Red
            };
            Row::new(vec![
                Cell::from(smart.device.clone()),
                Cell::from(smart.model.clone()),
                Cell::from(smart.health.clone()).style(Style::default().fg(health_color)),
                Cell::from(
                    smart
                        .reallocated
                        .map(|value| value.to_string())
                        .unwrap_or_else(|| "-".to_string()),
                ),
                Cell::from(
                    smart
                        .pending
                        .map(|value| value.to_string())
                        .unwrap_or_else(|| "-".to_string()),
                ),
            ])
        })
        .collect();
    let table = Table::new(
        rows,
        [
            Constraint::Length(10),
            Constraint::Min(14),
            Constraint::Length(34),
            Constraint::Length(12),
            Constraint::Length(9),
        ],
    )
    .header(header)
    .block(Block::bordered().title(" SMART health "));
    frame.render_widget(table, chunks[1]);

    let mut lines: Vec<Line> = disk
        .schedulers
        .iter()
        .map(|(name, scheduler)| Line::from(format!("{name}: {scheduler}")))
        .collect();
    if lines.is_empty() {
        lines.push(Line::from("No scheduler information available."));
    }
    lines.push(Line::from(""));
    let advice = crate::doctor::disk_advice(&app.doctor);
    if advice.is_empty() {
        lines.push(Line::from(
            "No disk-related warnings from the current doctor run.",
        ));
    } else {
        for item in advice.iter().take(4) {
            lines.push(Line::from(format!("- {item}")));
        }
    }
    let footnote = Paragraph::new(lines)
        .block(Block::bordered().title(" Schedulers and advice "))
        .wrap(Wrap { trim: true })
        .style(Style::default().fg(MUTED));
    frame.render_widget(footnote, chunks[2]);
}
