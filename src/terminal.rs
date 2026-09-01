use std::{
    io::{self, Stderr},
    sync::mpsc::{self, Receiver, Sender},
    thread,
    time::Duration,
};

use crossterm::{
    event::{self, Event},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend};

use crate::{
    app::{App, AppCommand, Entry, ExitAction},
    preview::{Preview, Previewer},
    ui,
};

struct PreviewRequest {
    generation: u64,
    path: std::path::PathBuf,
    is_dir: bool,
    max_lines: usize,
}

struct PreviewResult {
    generation: u64,
    preview: Preview,
}

struct PreviewLoader {
    request_tx: Sender<PreviewRequest>,
    result_rx: Receiver<PreviewResult>,
    generation: u64,
}

impl PreviewLoader {
    fn new(previewer: Previewer) -> Self {
        let (request_tx, request_rx) = mpsc::channel::<PreviewRequest>();
        let (result_tx, result_rx) = mpsc::channel();
        thread::spawn(move || {
            while let Ok(mut request) = request_rx.recv() {
                while let Ok(newer_request) = request_rx.try_recv() {
                    request = newer_request;
                }
                let preview = previewer.load_path(&request.path, request.is_dir, request.max_lines);
                if result_tx
                    .send(PreviewResult {
                        generation: request.generation,
                        preview,
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        Self {
            request_tx,
            result_rx,
            generation: 0,
        }
    }

    fn request(
        &mut self,
        entry: Option<&Entry>,
        cwd: &std::path::Path,
        max_lines: usize,
    ) -> (u64, bool) {
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        let pending = entry.is_some_and(|entry| {
            max_lines > 0
                && self
                    .request_tx
                    .send(PreviewRequest {
                        generation,
                        path: entry.absolute_path(cwd),
                        is_dir: entry.is_dir,
                        max_lines,
                    })
                    .is_ok()
        });
        (generation, pending)
    }

    fn poll(&self, generation: u64) -> Option<Preview> {
        let mut current = None;
        while let Ok(result) = self.result_rx.try_recv() {
            if result.generation == generation {
                current = Some(result.preview);
            }
        }
        current
    }
}

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

    pub fn run(&mut self, app: &mut App, previewer: Previewer) -> io::Result<Option<ExitAction>> {
        let mut loader = PreviewLoader::new(previewer);
        let (mut preview_generation, mut preview_pending) =
            self.request_preview(app, &mut loader)?;
        let mut preview = Preview::default();
        let mut dirty = true;
        loop {
            if let Some(loaded) = loader.poll(preview_generation) {
                preview = loaded;
                preview_pending = false;
                dirty = true;
            }

            let selected_before_poll = app.selected_relative_path().map(ToOwned::to_owned);
            if app.poll_index() {
                let selected_after_poll = app.selected_relative_path();
                if !app.query.is_empty() && selected_before_poll.as_deref() != selected_after_poll {
                    (preview_generation, preview_pending) =
                        self.request_preview(app, &mut loader)?;
                }
                dirty = true;
            }
            if dirty {
                self.terminal.draw(|frame| ui::draw(frame, app, &preview))?;
                dirty = false;
            }
            let poll_timeout = if preview_pending {
                Duration::from_millis(2)
            } else {
                Duration::from_millis(30)
            };
            if !event::poll(poll_timeout)? {
                continue;
            }
            match event::read()? {
                Event::Key(key) if key.is_press() => {
                    dirty = true;
                    match app.handle_key(key) {
                        AppCommand::None => {}
                        AppCommand::RefreshPreview => {
                            (preview_generation, preview_pending) =
                                self.request_preview(app, &mut loader)?;
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
                    (preview_generation, preview_pending) =
                        self.request_preview(app, &mut loader)?;
                    dirty = true;
                }
                _ => {}
            }
        }
    }

    fn request_preview(&self, app: &App, loader: &mut PreviewLoader) -> io::Result<(u64, bool)> {
        let area = self.terminal.size()?;
        let max_lines = if app.preview_enabled && area.width >= 70 {
            area.height as usize
        } else {
            0
        };
        Ok(loader.request(app.selected_entry(), &app.cwd, max_lines))
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(self.terminal.backend_mut(), LeaveAlternateScreen);
        let _ = self.terminal.show_cursor();
    }
}
