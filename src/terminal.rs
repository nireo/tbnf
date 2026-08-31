use std::{
    io::{self, Stderr},
    time::Duration,
};

use crossterm::{
    event::{self, Event},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend};

use crate::{
    app::{App, AppCommand, ExitAction},
    preview::Previewer,
    ui,
};

pub struct TerminalSession {
    terminal: Terminal<CrosstermBackend<Stderr>>,
}

impl TerminalSession {
    pub fn new() -> io::Result<Self> {
        enable_raw_mode()?;
        let mut stderr = io::stderr();
        if let Err(error) = execute!(stderr, EnterAlternateScreen) {
            let _ = disable_raw_mode();
            return Err(error);
        }
        match Terminal::new(CrosstermBackend::new(stderr)) {
            Ok(terminal) => Ok(Self { terminal }),
            Err(error) => {
                let _ = disable_raw_mode();
                let _ = execute!(io::stderr(), LeaveAlternateScreen);
                Err(error)
            }
        }
    }

    pub fn run(
        &mut self,
        app: &mut App,
        previewer: &mut Previewer,
    ) -> io::Result<Option<ExitAction>> {
        let mut preview = previewer.load(app.selected_entry());
        let mut dirty = true;
        loop {
            if app.poll_index() {
                if !app.query.is_empty() {
                    preview = previewer.load(app.selected_entry());
                }
                dirty = true;
            }
            if dirty {
                self.terminal.draw(|frame| ui::draw(frame, app, &preview))?;
                dirty = false;
            }
            if !event::poll(Duration::from_millis(30))? {
                continue;
            }
            match event::read()? {
                Event::Key(key) if key.is_press() => {
                    dirty = true;
                    match app.handle_key(key) {
                        AppCommand::None => {}
                        AppCommand::RefreshPreview => {
                            preview = previewer.load(app.selected_entry());
                        }
                        AppCommand::Open(path) => {
                            if let Err(error) = open::that_detached(path) {
                                app.status = Some(format!("could not open file: {error}"));
                            }
                        }
                        AppCommand::Exit(action) => return Ok(action),
                    }
                }
                Event::Resize(_, _) => {
                    preview = previewer.load(app.selected_entry());
                    dirty = true;
                }
                _ => {}
            }
        }
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(self.terminal.backend_mut(), LeaveAlternateScreen);
        let _ = self.terminal.show_cursor();
    }
}
