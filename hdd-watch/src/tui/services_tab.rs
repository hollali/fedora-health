use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, Cell, Paragraph, Row, Table, TableState, Wrap};
use ratatui::Frame;

use super::app::App;
use super::themes::{ACCENT, MUTED};

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let chunks = Layout::vertical([Constraint::Length(2), Constraint::Min(6)]).split(area);
    let enabled = app
        .services
        .iter()
        .filter(|service| service.state == "enabled")
        .count();
    let disabled = app
        .services
        .iter()
        .filter(|service| service.state == "disabled")
        .count();
    let masked = app
        .services
        .iter()
        .filter(|service| service.state == "masked")
        .count();
    let header = format!(
        "Services: {} listed, {} enabled, {} disabled, {} masked{}",
        app.services.len(),
        enabled,
        disabled,
        masked,
        if app.services_loading {
            "  (refreshing...)"
        } else {
            ""
        }
    );
    frame.render_widget(
        Paragraph::new(header).style(Style::default().fg(ACCENT)),
        chunks[0],
    );

    let columns = Layout::horizontal([Constraint::Percentage(64), Constraint::Percentage(36)])
        .split(chunks[1]);
    render_table(frame, columns[0], app);

    let detail = Paragraph::new(app.service_detail())
        .block(
            Block::bordered()
                .title(" Service detail ")
                .title_bottom(" e enable  d disable  m mask  u unmask  r reload "),
        )
        .wrap(Wrap { trim: false });
    frame.render_widget(detail, columns[1]);
}

fn render_table(frame: &mut Frame, area: Rect, app: &App) {
    let header = Row::new(vec!["unit", "boot", "state", "active"])
        .style(Style::default().fg(ACCENT).add_modifier(Modifier::BOLD));
    let rows: Vec<Row> = app
        .services
        .iter()
        .map(|service| {
            let boot_color = if service.boot_secs >= 60 {
                Color::Red
            } else if service.boot_secs >= 20 {
                Color::Yellow
            } else {
                MUTED
            };
            Row::new(vec![
                Cell::from(service.unit.clone()),
                Cell::from(if service.boot_time.is_empty() {
                    "-".to_string()
                } else {
                    service.boot_time.clone()
                })
                .style(Style::default().fg(boot_color)),
                Cell::from(service.state.clone())
                    .style(Style::default().fg(state_color(&service.state))),
                Cell::from(if service.active.is_empty() {
                    "unknown".to_string()
                } else {
                    service.active.clone()
                })
                .style(Style::default().fg(active_color(&service.active))),
            ])
        })
        .collect();
    let title = if app.services_loading {
        " systemd services (refreshing) "
    } else {
        " systemd services by boot time "
    };
    let table = Table::new(
        rows,
        [
            Constraint::Min(24),
            Constraint::Length(13),
            Constraint::Length(10),
            Constraint::Length(10),
        ],
    )
    .header(header)
    .block(Block::bordered().title(title))
    .row_highlight_style(
        Style::default()
            .bg(Color::DarkGray)
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    )
    .highlight_symbol("> ");
    let mut state = TableState::default();
    if !app.services.is_empty() {
        state = state.with_selected(Some(app.service_selected.min(app.services.len() - 1)));
    }
    frame.render_stateful_widget(table, area, &mut state);
}

fn state_color(state: &str) -> Color {
    match state {
        "enabled" | "enabled-runtime" => Color::Green,
        "disabled" => MUTED,
        "masked" | "masked-runtime" => Color::Red,
        "static" | "generated" | "indirect" => Color::Yellow,
        _ => MUTED,
    }
}

fn active_color(active: &str) -> Color {
    match active {
        "active" => Color::Green,
        "failed" => Color::Red,
        "activating" | "deactivating" | "reloading" => Color::Yellow,
        _ => MUTED,
    }
}
