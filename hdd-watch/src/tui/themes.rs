use ratatui::style::Color;

use crate::report::Severity;

pub const ACCENT: Color = Color::Cyan;
pub const ACCENT_ALT: Color = Color::Magenta;
pub const MUTED: Color = Color::DarkGray;

pub fn severity_color(severity: Severity) -> Color {
    match severity {
        Severity::Ok => Color::Green,
        Severity::Warn => Color::Yellow,
        Severity::Bad => Color::Red,
    }
}

pub fn severity_icon(severity: Severity) -> &'static str {
    match severity {
        Severity::Ok => "OK",
        Severity::Warn => "!",
        Severity::Bad => "X",
    }
}

pub fn load_color(value: f64) -> Color {
    let severity = crate::report::io_psi_severity(value);
    severity_color(severity)
}

pub fn ram_color(mb: u64) -> Color {
    severity_color(crate::report::free_ram_severity(mb))
}

pub fn swap_color(mb: u64) -> Color {
    severity_color(crate::report::swap_severity(mb))
}
