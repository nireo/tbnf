use std::{
    ffi::OsString,
    fs::{self, OpenOptions},
    io,
    path::{Component, Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread,
};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ignore::{WalkBuilder, WalkState};
use nucleo_matcher::{
    Config, Matcher, Utf32Str,
    pattern::{AtomKind, CaseMatching, Normalization, Pattern},
};

const INDEX_BATCH_SIZE: usize = 256;
const MAX_INDEX_ENTRIES: usize = 500_000;

#[derive(Debug, Clone)]
pub struct Entry {
    pub name: OsString,
    pub display_name: String,
    pub path: PathBuf,
    pub is_dir: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExitAction {
    Edit(PathBuf),
    ChangeDirectory(PathBuf),
}

#[derive(Debug, Clone)]
pub enum PromptKind {
    Create,
    Delete { target: PathBuf },
}

#[derive(Debug, Clone)]
pub struct Prompt {
    pub label: String,
    pub input: String,
    pub kind: PromptKind,
}

#[derive(Debug)]
pub enum AppCommand {
    None,
    RefreshPreview,
    Open(PathBuf),
    Exit(Option<ExitAction>),
}

#[derive(Debug)]
enum ScanMessage {
    Batch {
        generation: u64,
        entries: Vec<Entry>,
    },
    Done {
        generation: u64,
        truncated: bool,
    },
}

struct BatchSender {
    generation: u64,
    entries: Vec<Entry>,
    tx: Sender<ScanMessage>,
}

impl BatchSender {
    fn push(&mut self, entry: Entry) {
        self.entries.push(entry);
        if self.entries.len() >= INDEX_BATCH_SIZE {
            self.flush();
        }
    }

    fn flush(&mut self) {
        if self.entries.is_empty() {
            return;
        }
        let entries = std::mem::take(&mut self.entries);
        let _ = self.tx.send(ScanMessage::Batch {
            generation: self.generation,
            entries,
        });
    }
}

impl Drop for BatchSender {
    fn drop(&mut self) {
        self.flush();
    }
}

#[derive(Debug)]
pub struct App {
    pub cwd: PathBuf,
    pub entries: Vec<Entry>,
    pub visible: Vec<usize>,
    pub query: String,
    pub selected: usize,
    pub top_index: usize,
    pub prompt: Option<Prompt>,
    pub status: Option<String>,
    pub preview_enabled: bool,
    pub indexing: bool,
    pub index_truncated: bool,
    viewport_height: usize,
    search_entries: Vec<Entry>,
    scan_generation: u64,
    scan_tx: Sender<ScanMessage>,
    scan_rx: Receiver<ScanMessage>,
}

impl App {
    pub fn new(cwd: PathBuf, preview_enabled: bool) -> io::Result<Self> {
        let (scan_tx, scan_rx) = mpsc::channel();
        let mut app = Self {
            cwd: PathBuf::new(),
            entries: Vec::new(),
            visible: Vec::new(),
            query: String::new(),
            selected: 0,
            top_index: 0,
            prompt: None,
            status: None,
            preview_enabled,
            indexing: false,
            index_truncated: false,
            viewport_height: 1,
            search_entries: Vec::new(),
            scan_generation: 0,
            scan_tx,
            scan_rx,
        };
        app.switch_dir(cwd)?;
        Ok(app)
    }

    pub fn set_viewport_height(&mut self, height: usize) {
        self.viewport_height = height.max(1);
        self.ensure_visible();
    }

    pub fn selected_entry(&self) -> Option<&Entry> {
        self.visible
            .get(self.selected)
            .and_then(|index| self.active_entries().get(*index))
    }

    pub fn visible_entries(&self) -> impl Iterator<Item = &Entry> {
        self.visible
            .iter()
            .filter_map(|index| self.active_entries().get(*index))
    }

    fn active_entries(&self) -> &[Entry] {
        if self.query.is_empty() {
            &self.entries
        } else {
            &self.search_entries
        }
    }

    pub fn switch_dir(&mut self, path: PathBuf) -> io::Result<()> {
        let cwd = path.canonicalize()?;
        if !cwd.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::NotADirectory,
                "not a directory",
            ));
        }

        let mut entries = Vec::new();
        for result in fs::read_dir(&cwd)? {
            let dir_entry = result?;
            let path = dir_entry.path();
            let metadata = fs::symlink_metadata(&path)?;
            let is_symlink = metadata.file_type().is_symlink();
            let is_dir = if is_symlink {
                fs::metadata(&path).is_ok_and(|target| target.is_dir())
            } else {
                metadata.is_dir()
            };
            let name = dir_entry.file_name();
            entries.push(Entry {
                display_name: name.to_string_lossy().into_owned(),
                name,
                path,
                is_dir,
            });
        }
        entries.sort_by(|a, b| a.name.cmp(&b.name));

        self.cwd = cwd;
        self.entries = entries;
        self.query.clear();
        self.selected = 0;
        self.top_index = 0;
        self.status = None;
        self.search_entries = self.entries.clone();
        self.rebuild_visible();
        self.start_recursive_index();
        Ok(())
    }

    pub fn poll_index(&mut self) -> bool {
        let mut changed = false;
        while let Ok(message) = self.scan_rx.try_recv() {
            match message {
                ScanMessage::Batch {
                    generation,
                    entries,
                } if generation == self.scan_generation => {
                    self.search_entries.extend(entries);
                    changed = true;
                }
                ScanMessage::Done {
                    generation,
                    truncated,
                } if generation == self.scan_generation => {
                    self.indexing = false;
                    self.index_truncated = truncated;
                    changed = true;
                }
                _ => {}
            }
        }
        if changed && !self.query.is_empty() {
            self.rebuild_visible_preserving_selection();
        }
        changed
    }

    fn start_recursive_index(&mut self) {
        self.scan_generation = self.scan_generation.wrapping_add(1);
        self.index_truncated = false;
        if self.cwd.parent().is_none() {
            self.indexing = false;
            return;
        }

        self.indexing = true;
        let generation = self.scan_generation;
        let root = self.cwd.clone();
        let tx = self.scan_tx.clone();
        thread::spawn(move || {
            let count = Arc::new(AtomicUsize::new(0));
            let mut builder = WalkBuilder::new(&root);
            builder
                .min_depth(Some(2))
                .follow_links(false)
                .threads(available_scan_threads());
            builder.build_parallel().run(|| {
                let root = root.clone();
                let count = Arc::clone(&count);
                let mut batch = BatchSender {
                    generation,
                    entries: Vec::with_capacity(INDEX_BATCH_SIZE),
                    tx: tx.clone(),
                };
                Box::new(move |result| {
                    if count.load(Ordering::Relaxed) >= MAX_INDEX_ENTRIES {
                        return WalkState::Quit;
                    }
                    let Ok(walk_entry) = result else {
                        return WalkState::Continue;
                    };
                    let path = walk_entry.path().to_path_buf();
                    let Some(relative) = path.strip_prefix(&root).ok() else {
                        return WalkState::Continue;
                    };
                    let Some(name) = path.file_name().map(ToOwned::to_owned) else {
                        return WalkState::Continue;
                    };
                    let file_type = walk_entry.file_type();
                    let is_symlink = file_type.is_some_and(|kind| kind.is_symlink());
                    let is_dir = file_type.is_some_and(|kind| kind.is_dir())
                        || (is_symlink && fs::metadata(&path).is_ok_and(|target| target.is_dir()));

                    if count.fetch_add(1, Ordering::Relaxed) >= MAX_INDEX_ENTRIES {
                        return WalkState::Quit;
                    }
                    batch.push(Entry {
                        name,
                        display_name: relative.to_string_lossy().into_owned(),
                        path,
                        is_dir,
                    });
                    WalkState::Continue
                })
            });
            let truncated = count.load(Ordering::Relaxed) >= MAX_INDEX_ENTRIES;
            let _ = tx.send(ScanMessage::Done {
                generation,
                truncated,
            });
        });
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> AppCommand {
        if self.prompt.is_some() {
            return self.handle_prompt_key(key);
        }

        match key.code {
            KeyCode::Esc => AppCommand::Exit(None),
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                AppCommand::Exit(None)
            }
            KeyCode::Down => {
                self.move_cursor(1);
                AppCommand::RefreshPreview
            }
            KeyCode::Char('j') | KeyCode::Char('n')
                if key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                self.move_cursor(1);
                AppCommand::RefreshPreview
            }
            KeyCode::Up => {
                self.move_cursor(-1);
                AppCommand::RefreshPreview
            }
            KeyCode::Char('k') | KeyCode::Char('p')
                if key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                self.move_cursor(-1);
                AppCommand::RefreshPreview
            }
            KeyCode::Tab => self.select(),
            KeyCode::Char('l') | KeyCode::Char('f')
                if key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                self.select()
            }
            KeyCode::Enter => self.selected_entry().map_or_else(
                || AppCommand::Exit(Some(ExitAction::ChangeDirectory(self.cwd.clone()))),
                |entry| {
                    let action = if entry.is_dir {
                        ExitAction::ChangeDirectory(entry.path.clone())
                    } else {
                        ExitAction::Edit(entry.path.clone())
                    };
                    AppCommand::Exit(Some(action))
                },
            ),
            KeyCode::Backspace => {
                self.backspace(false);
                AppCommand::RefreshPreview
            }
            KeyCode::Char('b') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.backspace(false);
                AppCommand::RefreshPreview
            }
            KeyCode::Char('w') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.backspace(true);
                AppCommand::RefreshPreview
            }
            KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.toggle_home();
                AppCommand::RefreshPreview
            }
            KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.prompt = Some(Prompt {
                    label: "create: ".to_owned(),
                    input: String::new(),
                    kind: PromptKind::Create,
                });
                AppCommand::None
            }
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(entry) = self.selected_entry() {
                    self.prompt = Some(Prompt {
                        label: format!("delete {}? (y/n): ", entry.display_name),
                        input: String::new(),
                        kind: PromptKind::Delete {
                            target: entry.path.clone(),
                        },
                    });
                }
                AppCommand::None
            }
            KeyCode::Char('o') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.selected_entry().map_or(AppCommand::None, |entry| {
                    AppCommand::Open(entry.path.clone())
                })
            }
            KeyCode::Char(character)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                if self.query.chars().count() < 100 {
                    self.query.push(character);
                    self.rebuild_visible();
                }
                AppCommand::RefreshPreview
            }
            _ => AppCommand::None,
        }
    }

    fn handle_prompt_key(&mut self, key: KeyEvent) -> AppCommand {
        match key.code {
            KeyCode::Esc => {
                self.prompt = None;
                AppCommand::None
            }
            KeyCode::Backspace => {
                if let Some(prompt) = &mut self.prompt {
                    prompt.input.pop();
                }
                AppCommand::None
            }
            KeyCode::Enter => {
                self.submit_prompt();
                AppCommand::RefreshPreview
            }
            KeyCode::Char(character)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                if let Some(prompt) = &mut self.prompt {
                    prompt.input.push(character);
                }
                AppCommand::None
            }
            _ => AppCommand::None,
        }
    }

    fn submit_prompt(&mut self) {
        let Some(prompt) = self.prompt.take() else {
            return;
        };
        let result = match prompt.kind {
            PromptKind::Create => self.create(&prompt.input),
            PromptKind::Delete { target } => {
                if prompt.input.eq_ignore_ascii_case("y") {
                    self.delete(&target)
                } else {
                    Ok(())
                }
            }
        };
        if let Err(error) = result {
            self.status = Some(error.to_string());
        }
    }

    fn create(&mut self, input: &str) -> io::Result<()> {
        if input.is_empty() {
            return Ok(());
        }
        let relative = Path::new(input);
        if relative.is_absolute()
            || relative
                .components()
                .any(|part| !matches!(part, Component::Normal(_)))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "name must be a relative path without '..'",
            ));
        }

        let target = self.cwd.join(relative);
        let make_dir = input.ends_with('/') || input.ends_with(std::path::MAIN_SEPARATOR);
        let next_dir = if make_dir {
            fs::create_dir_all(&target)?;
            target
        } else {
            let parent = target.parent().unwrap_or(&self.cwd);
            fs::create_dir_all(parent)?;
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&target)?;
            parent.to_path_buf()
        };
        self.switch_dir(next_dir)
    }

    fn delete(&mut self, target: &Path) -> io::Result<()> {
        if target == self.cwd || !target.starts_with(&self.cwd) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "invalid delete target",
            ));
        }
        let metadata = fs::symlink_metadata(target)?;
        if metadata.is_dir() && !metadata.file_type().is_symlink() {
            fs::remove_dir_all(target)?;
        } else {
            fs::remove_file(target)?;
        }
        let cwd = self.cwd.clone();
        self.switch_dir(cwd)
    }

    fn select(&mut self) -> AppCommand {
        let Some(entry) = self.selected_entry() else {
            return AppCommand::None;
        };
        if entry.is_dir {
            let path = entry.path.clone();
            if let Err(error) = self.switch_dir(path) {
                self.status = Some(error.to_string());
            }
            AppCommand::RefreshPreview
        } else {
            AppCommand::Exit(Some(ExitAction::Edit(entry.path.clone())))
        }
    }

    fn toggle_home(&mut self) {
        let home = dirs::home_dir();
        let target = match home {
            Some(home) if self.cwd != home => home,
            _ => PathBuf::from(std::path::MAIN_SEPARATOR.to_string()),
        };
        if let Err(error) = self.switch_dir(target) {
            self.status = Some(error.to_string());
        }
    }

    fn backspace(&mut self, full_query: bool) {
        if self.query.is_empty() {
            if let Some(parent) = self.cwd.parent().map(Path::to_path_buf)
                && parent != self.cwd
                && let Err(error) = self.switch_dir(parent)
            {
                self.status = Some(error.to_string());
            }
            return;
        }
        if full_query {
            self.query.clear();
        } else {
            self.query.pop();
        }
        self.rebuild_visible();
    }

    fn move_cursor(&mut self, amount: isize) {
        if self.visible.is_empty() {
            return;
        }
        let len = self.visible.len() as isize;
        self.selected = (self.selected as isize + amount).rem_euclid(len) as usize;
        self.ensure_visible();
    }

    fn ensure_visible(&mut self) {
        if self.selected < self.top_index {
            self.top_index = self.selected;
        } else if self.selected >= self.top_index + self.viewport_height {
            self.top_index = self.selected + 1 - self.viewport_height;
        }
    }

    fn rebuild_visible(&mut self) {
        if self.query.is_empty() {
            self.visible = (0..self.entries.len()).collect();
        } else {
            let query_lower = self.query.to_lowercase();
            let pattern = Pattern::new(
                &self.query,
                CaseMatching::Ignore,
                Normalization::Smart,
                AtomKind::Fuzzy,
            );
            let mut matcher = Matcher::new(Config::DEFAULT);
            let mut scored = Vec::new();
            for (index, entry) in self.search_entries.iter().enumerate() {
                let mut buffer = Vec::new();
                let score = pattern.score(
                    Utf32Str::new(&entry.display_name, &mut buffer),
                    &mut matcher,
                );
                if let Some(fuzzy_score) = score {
                    let path_lower = entry.display_name.to_lowercase();
                    let name_lower = entry.name.to_string_lossy().to_lowercase();
                    let rank = if path_lower == query_lower || name_lower == query_lower {
                        0
                    } else if name_lower.starts_with(&query_lower) {
                        1
                    } else if name_lower.contains(&query_lower) {
                        2
                    } else if path_lower.starts_with(&query_lower)
                        || path_lower.contains(&query_lower)
                    {
                        3
                    } else {
                        4
                    };
                    scored.push((index, rank, fuzzy_score, path_lower));
                }
            }
            scored.sort_by(|a, b| {
                a.1.cmp(&b.1)
                    .then_with(|| b.2.cmp(&a.2))
                    .then_with(|| a.3.len().cmp(&b.3.len()))
                    .then_with(|| a.3.cmp(&b.3))
            });
            self.visible = scored.into_iter().map(|item| item.0).collect();
        }
        self.selected = 0;
        self.top_index = 0;
    }

    fn rebuild_visible_preserving_selection(&mut self) {
        let selected_path = self.selected_entry().map(|entry| entry.path.clone());
        self.rebuild_visible();
        if let Some(selected_path) = selected_path
            && let Some(position) = self.visible.iter().position(|index| {
                self.search_entries
                    .get(*index)
                    .is_some_and(|entry| entry.path == selected_path)
            })
        {
            self.selected = position;
            self.ensure_visible();
        }
    }
}

fn available_scan_threads() -> usize {
    thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(2)
        .clamp(1, 4)
}

#[cfg(test)]
mod tests {
    use std::{fs, thread, time::Duration};

    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use tempfile::TempDir;

    use super::*;

    fn app_with_files(names: &[&str]) -> (TempDir, App) {
        let temp = TempDir::new().unwrap();
        for name in names {
            fs::write(temp.path().join(name), "test").unwrap();
        }
        let app = App::new(temp.path().to_path_buf(), true).unwrap();
        (temp, app)
    }

    #[test]
    fn search_prioritizes_exact_prefix_and_substring() {
        let (_temp, mut app) = app_with_files(&["xfoo", "food", "foo", "far-out-object"]);
        for character in "foo".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE));
        }
        let names: Vec<_> = app
            .visible_entries()
            .map(|entry| entry.display_name.as_str())
            .collect();
        assert_eq!(names, ["foo", "food", "xfoo", "far-out-object"]);
    }

    #[test]
    fn unicode_backspace_removes_a_character() {
        let (_temp, mut app) = app_with_files(&["café"]);
        app.query = "café".to_owned();
        app.rebuild_visible();
        app.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(app.query, "caf");
    }

    #[test]
    fn no_match_is_distinct_from_an_empty_query() {
        let (_temp, mut app) = app_with_files(&["one"]);
        app.query = "zzz".to_owned();
        app.rebuild_visible();
        assert!(app.visible.is_empty());
        app.backspace(true);
        assert_eq!(app.visible.len(), 1);
    }

    #[test]
    fn create_does_not_truncate_an_existing_file() {
        let (temp, mut app) = app_with_files(&["existing"]);
        let error = app.create("existing").unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(
            fs::read_to_string(temp.path().join("existing")).unwrap(),
            "test"
        );
    }

    #[test]
    fn create_rejects_parent_traversal() {
        let (_temp, mut app) = app_with_files(&[]);
        assert_eq!(
            app.create("../outside").unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
    }

    #[test]
    fn recursive_index_finds_a_nested_file() {
        let temp = TempDir::new().unwrap();
        fs::create_dir(temp.path().join("src")).unwrap();
        fs::write(temp.path().join("src/main.rs"), "fn main() {}\n").unwrap();
        let mut app = App::new(temp.path().to_path_buf(), true).unwrap();
        for _ in 0..200 {
            app.poll_index();
            if !app.indexing {
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
        assert!(!app.indexing, "recursive index did not finish");
        for character in "main.rs".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE));
        }
        assert_eq!(app.selected_entry().unwrap().display_name, "src/main.rs");
    }

    #[test]
    fn delete_prompt_targets_the_filtered_selection() {
        let (temp, mut app) = app_with_files(&["keep", "remove-me"]);
        for character in "remove".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE));
        }
        app.handle_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL));
        app.handle_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(temp.path().join("keep").exists());
        assert!(!temp.path().join("remove-me").exists());
    }

    #[test]
    fn selecting_a_file_exits_with_an_edit_action() {
        let (_temp, mut app) = app_with_files(&["file.rs"]);
        let AppCommand::Exit(Some(ExitAction::Edit(path))) =
            app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE))
        else {
            panic!("expected edit action");
        };
        assert!(path.ends_with("file.rs"));
    }

    #[test]
    fn enter_on_a_file_exits_with_an_edit_action() {
        let (_temp, mut app) = app_with_files(&["file.rs"]);
        let AppCommand::Exit(Some(ExitAction::Edit(path))) =
            app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        else {
            panic!("expected edit action");
        };
        assert!(path.ends_with("file.rs"));
    }
}
