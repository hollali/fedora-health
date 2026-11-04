mod app;
mod demo;
mod disk_tab;
mod doctor_tab;
mod history_tab;
mod launch_tab;
mod live_tab;
mod services_tab;
mod themes;

use std::io::{self, stdout};
use std::sync::mpsc;
use std::time::Duration;

use crossterm::event::{self, Event, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use app::App;

pub fn run(demo: bool, interval: u64) -> io::Result<()> {
    enable_raw_mode()?;
    let mut output = stdout();
    execute!(output, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(output);
    let mut terminal = Terminal::new(backend)?;

    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
        previous_hook(info);
    }));

    let result = event_loop(&mut terminal, demo, interval);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
}

fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    demo: bool,
    interval: u64,
) -> io::Result<()> {
    let (tx, rx) = mpsc::channel();
    let mut app = App::new(demo, interval);
    app.start(tx.clone());

    while !app.should_quit {
        while let Ok(event) = rx.try_recv() {
            app.on_event(event);
        }
        terminal.draw(|frame| app.draw(frame))?;
        if event::poll(Duration::from_millis(200))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    app.on_key(key, &tx);
                }
            }
        }
    }
    Ok(())
}
