use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Frame;

use super::app::App;
use super::themes::{severity_color, severity_icon, ACCENT, MUTED};
use crate::report::Severity;

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let chunks = Layout::vertical([Constraint::Length(2), Constraint::Min(6)]).split(area);
    let bad = app
        .doctor
        .iter()
        .filter(|finding| finding.severity == Severity::Bad)
        .count();
    let warn = app
        .doctor
        .iter()
        .filter(|finding| finding.severity == Severity::Warn)
        .count();
    let ok = app
        .doctor
        .iter()
        .filter(|finding| finding.severity == Severity::Ok)
        .count();
    let header = format!(
        "Findings: {bad} bad, {warn} warning, {ok} ok{}",
        if app.doctor_loading {
            "  (refreshing...)"
        } else {
            ""
        }
    );
    frame.render_widget(
        Paragraph::new(header).style(Style::default().fg(ACCENT)),
        chunks[0],
    );

    let columns = Layout::horizontal([Constraint::Percentage(48), Constraint::Percentage(52)])
        .split(chunks[1]);
    render_list(frame, columns[0], app);

    let right =
        Layout::vertical([Constraint::Percentage(56), Constraint::Min(5)]).split(columns[1]);
    let detail = Paragraph::new(app.doctor_detail())
        .block(
            Block::bordered()
                .title(" Finding detail ")
                .title_bottom(" r re-check  f fix  a fix all "),
        )
        .wrap(Wrap { trim: false });
    frame.render_widget(detail, right[0]);

    let startup = if app.startup_text.is_empty() {
        "Login startup audit is loading..."
    } else {
        app.startup_text.as_str()
    };
    let audit = Paragraph::new(startup)
        .block(Block::bordered().title(" Login startup audit "))
        .wrap(Wrap { trim: false })
        .style(Style::default().fg(MUTED));
    frame.render_widget(audit, right[1]);
}

fn render_list(frame: &mut Frame, area: Rect, app: &App) {
    let items: Vec<ListItem> = app
        .doctor
        .iter()
        .map(|finding| {
            let color = severity_color(finding.severity);
            ListItem::new(vec![
                Line::from(vec![
                    ratatui::text::Span::styled(
                        format!("[{}] ", severity_icon(finding.severity)),
                        Style::default().fg(color).add_modifier(Modifier::BOLD),
                    ),
                    ratatui::text::Span::styled(finding.title.clone(), Style::default().fg(color)),
                ]),
                Line::from(vec![
                    ratatui::text::Span::styled(
                        format!("  {} ", finding.id),
                        Style::default().fg(MUTED),
                    ),
                    ratatui::text::Span::styled(
                        if finding.fix_available {
                            "fixable"
                        } else {
                            "advice"
                        },
                        Style::default().fg(if finding.fix_available { ACCENT } else { MUTED }),
                    ),
                ]),
            ])
        })
        .collect();
    let list = List::new(items)
        .block(Block::bordered().title(" fedora-doctor findings "))
        .highlight_style(
            Style::default()
                .bg(ratatui::style::Color::DarkGray)
                .fg(ratatui::style::Color::White)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("> ");
    let mut state = ListState::default();
    if !app.doctor.is_empty() {
        state.select(Some(app.doctor_selected.min(app.doctor.len() - 1)));
    }
    frame.render_stateful_widget(list, area, &mut state);
}
