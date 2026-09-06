use std::{
    collections::BinaryHeap,
    fs::{self, OpenOptions},
    io,
    path::{Component, Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc::{self, Receiver, SyncSender},
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
const INDEX_QUEUE_BATCHES: usize = 16;
const MAX_INDEX_ENTRIES: usize = 500_000;
const MAX_SEARCH_RESULTS: usize = 500;

// Prune generated/dependency trees only during recursive indexing. Direct
// directory browsing (including starting an index inside one) remains available.
const SKIPPED_INDEX_DIRS: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    "node_modules",
    "target",
    "vendor",
    ".venv",
    "venv",
    "__pycache__",
    ".next",
    ".nuxt",
    ".cache",
];

fn skip_index_directory(name: &std::ffi::OsStr) -> bool {
    name.to_str()
        .is_some_and(|name| SKIPPED_INDEX_DIRS.contains(&name))
}

#[derive(Debug, Clone)]
pub struct Entry {
    pub display_name: Box<str>,
    relative_path: Box<Path>,
    pub is_dir: bool,
}

impl Entry {
    fn file_name(&self) -> &std::ffi::OsStr {
        self.relative_path
            .file_name()
            .unwrap_or(self.relative_path.as_os_str())
    }

    pub(crate) fn absolute_path(&self, cwd: &Path) -> PathBuf {
        cwd.join(&self.relative_path)
    }
}

#[derive(Debug, Eq, PartialEq)]
struct ScoredMatch {
    index: usize,
    rank: u8,
    fuzzy_score: u32,
    path_lower: Box<str>,
}

impl Ord for ScoredMatch {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Worse matches compare greater, leaving the worst retained match at
        // the top of the max-heap for cheap replacement.
        self.rank
            .cmp(&other.rank)
            .then_with(|| other.fuzzy_score.cmp(&self.fuzzy_score))
            .then_with(|| self.path_lower.len().cmp(&other.path_lower.len()))
            .then_with(|| self.path_lower.cmp(&other.path_lower))
            .then_with(|| self.index.cmp(&other.index))
    }
}

impl PartialOrd for ScoredMatch {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExitAction {
    Edit {
        path: PathBuf,
        working_directory: PathBuf,
    },
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
    tx: SyncSender<ScanMessage>,
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
    pub show_hidden: bool,
    pub indexing: bool,
    pub index_truncated: bool,
    pub total_matches: usize,
    launch_directory: PathBuf,
    viewport_height: usize,
    search_entries: Vec<Entry>,
    scan_generation: u64,
    scan_tx: SyncSender<ScanMessage>,
    scan_rx: Receiver<ScanMessage>,
    scan_cancel: Option<Arc<AtomicBool>>,
}

impl App {
    pub fn new(cwd: PathBuf, preview_enabled: bool) -> io::Result<Self> {
        let (scan_tx, scan_rx) = mpsc::sync_channel(INDEX_QUEUE_BATCHES);
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
            show_hidden: true,
            indexing: false,
            index_truncated: false,
            total_matches: 0,
            launch_directory: PathBuf::new(),
            viewport_height: 1,
            search_entries: Vec::new(),
            scan_generation: 0,
            scan_tx,
            scan_rx,
            scan_cancel: None,
        };
        app.switch_dir(cwd)?;
        app.launch_directory = app.cwd.clone();
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

    pub fn selected_path(&self) -> Option<PathBuf> {
        self.selected_entry()
            .map(|entry| entry.absolute_path(&self.cwd))
    }

    pub fn selected_relative_path(&self) -> Option<&Path> {
        self.selected_entry()
            .map(|entry| entry.relative_path.as_ref())
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
                display_name: name.to_string_lossy().into_owned().into_boxed_str(),
                relative_path: PathBuf::from(name).into_boxed_path(),
                is_dir,
            });
        }
        entries.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));

        self.cwd = cwd;
        self.entries = entries;
        self.query.clear();
        self.selected = 0;
        self.top_index = 0;
        self.status = None;
        self.reset_search_entries();
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
                    self.scan_cancel = None;
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
        self.cancel_active_scan();
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
        let show_hidden = self.show_hidden;
        let cancel = Arc::new(AtomicBool::new(false));
        self.scan_cancel = Some(Arc::clone(&cancel));
        thread::spawn(move || {
            let count = Arc::new(AtomicUsize::new(0));
            // Finish and flush visible results before spending the remaining
            // index budget on hidden paths. Parallel traversal alone has no
            // ordering guarantee.
            for hidden_pass in [false, true] {
                if (hidden_pass && !show_hidden)
                    || cancel.load(Ordering::Acquire)
                    || count.load(Ordering::Relaxed) >= MAX_INDEX_ENTRIES
                {
                    break;
                }
                let mut builder = WalkBuilder::new(&root);
                builder
                    .min_depth(Some(2))
                    .hidden(!hidden_pass)
                    .filter_entry(|entry| {
                        entry.depth() == 0
                            || !entry.file_type().is_some_and(|kind| kind.is_dir())
                            || !skip_index_directory(entry.file_name())
                    })
                    .follow_links(false)
                    .threads(available_scan_threads());
                builder.build_parallel().run(|| {
                    let root = root.clone();
                    let count = Arc::clone(&count);
                    let cancel = Arc::clone(&cancel);
                    let mut batch = BatchSender {
                        generation,
                        entries: Vec::with_capacity(INDEX_BATCH_SIZE),
                        tx: tx.clone(),
                    };
                    Box::new(move |result| {
                        if cancel.load(Ordering::Acquire) {
                            return WalkState::Quit;
                        }
                        if count.load(Ordering::Relaxed) >= MAX_INDEX_ENTRIES {
                            return WalkState::Quit;
                        }
                        let Ok(walk_entry) = result else {
                            return WalkState::Continue;
                        };
                        let path = walk_entry.path();
                        let Some(relative) = path.strip_prefix(&root).ok() else {
                            return WalkState::Continue;
                        };
                        // The hidden pass traverses visible ancestors too, but
                        // must not emit or count their entries a second time.
                        if hidden_pass
                            && !relative
                                .components()
                                .any(|part| is_hidden(part.as_os_str()))
                        {
                            return WalkState::Continue;
                        }
                        if path.file_name().is_none() {
                            return WalkState::Continue;
                        }
                        let file_type = walk_entry.file_type();
                        let is_symlink = file_type.is_some_and(|kind| kind.is_symlink());
                        let is_dir = file_type.is_some_and(|kind| kind.is_dir())
                            || (is_symlink
                                && fs::metadata(path).is_ok_and(|target| target.is_dir()));

                        if count.fetch_add(1, Ordering::Relaxed) >= MAX_INDEX_ENTRIES {
                            return WalkState::Quit;
                        }
                        batch.push(Entry {
                            display_name: relative.to_string_lossy().into_owned().into_boxed_str(),
                            relative_path: relative.to_path_buf().into_boxed_path(),
                            is_dir,
                        });
                        WalkState::Continue
                    })
                });
            }
            let truncated = count.load(Ordering::Relaxed) >= MAX_INDEX_ENTRIES;
            if !cancel.load(Ordering::Acquire) {
                let _ = tx.send(ScanMessage::Done {
                    generation,
                    truncated,
                });
            }
        });
    }

    fn cancel_active_scan(&mut self) {
        if let Some(cancel) = self.scan_cancel.take() {
            cancel.store(true, Ordering::Release);
        }
        self.indexing = false;
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
            KeyCode::Enter => {
                let Some(entry) = self.selected_entry() else {
                    return AppCommand::Exit(Some(ExitAction::ChangeDirectory(self.cwd.clone())));
                };
                let is_dir = entry.is_dir;
                let path = entry.absolute_path(&self.cwd);
                let action = if is_dir {
                    ExitAction::ChangeDirectory(path)
                } else {
                    self.edit_action(path)
                };
                AppCommand::Exit(Some(action))
            }
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
            KeyCode::Char('h') if key.modifiers.contains(KeyModifiers::ALT) => {
                self.toggle_hidden();
                AppCommand::RefreshPreview
            }
            KeyCode::F(2) => {
                self.toggle_hidden();
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
                            target: entry.absolute_path(&self.cwd),
                        },
                    });
                }
                AppCommand::None
            }
            KeyCode::Char('o') if key.modifiers.contains(KeyModifiers::CONTROL) => self
                .selected_path()
                .map_or(AppCommand::None, AppCommand::Open),
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
        let is_dir = entry.is_dir;
        let path = entry.absolute_path(&self.cwd);
        if is_dir {
            if let Err(error) = self.switch_dir(path) {
                self.status = Some(error.to_string());
            }
            AppCommand::RefreshPreview
        } else {
            AppCommand::Exit(Some(self.edit_action(path)))
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

    fn toggle_hidden(&mut self) {
        self.show_hidden = !self.show_hidden;
        self.reset_search_entries();
        self.rebuild_visible();
        self.start_recursive_index();
    }

    fn reset_search_entries(&mut self) {
        self.search_entries = self
            .entries
            .iter()
            .filter(|entry| self.show_hidden || !is_hidden(entry.file_name()))
            .cloned()
            .collect();
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
            self.visible = self
                .entries
                .iter()
                .enumerate()
                .filter_map(|(index, entry)| {
                    (self.show_hidden || !is_hidden(entry.file_name())).then_some(index)
                })
                .collect();
            self.total_matches = self.visible.len();
        } else {
            let query_lower = self.query.to_lowercase();
            let pattern = Pattern::new(
                &self.query,
                CaseMatching::Ignore,
                Normalization::Smart,
                AtomKind::Fuzzy,
            );
            let mut matcher = Matcher::new(Config::DEFAULT);
            let mut scored: BinaryHeap<ScoredMatch> =
                BinaryHeap::with_capacity(MAX_SEARCH_RESULTS + 1);
            let mut total_matches = 0;
            let mut utf32_buffer = Vec::new();
            let mut lowercase_buffer = Vec::new();
            for (index, entry) in self.search_entries.iter().enumerate() {
                utf32_buffer.clear();
                let score = pattern.score(
                    Utf32Str::new(&entry.display_name, &mut utf32_buffer),
                    &mut matcher,
                );
                if let Some(fuzzy_score) = score {
                    total_matches += 1;
                    let rank = match_rank(entry, &self.query, &query_lower, &mut lowercase_buffer);
                    let core_order = scored.peek().map(|worst| {
                        rank.cmp(&worst.rank)
                            .then_with(|| worst.fuzzy_score.cmp(&fuzzy_score))
                    });
                    if scored.len() < MAX_SEARCH_RESULTS
                        || core_order.is_some_and(std::cmp::Ordering::is_lt)
                    {
                        if scored.len() == MAX_SEARCH_RESULTS {
                            scored.pop();
                        }
                        scored.push(ScoredMatch {
                            index,
                            rank,
                            fuzzy_score,
                            path_lower: entry.display_name.to_lowercase().into_boxed_str(),
                        });
                    } else if core_order.is_some_and(std::cmp::Ordering::is_eq) {
                        let should_replace = scored.peek().is_some_and(|worst| {
                            if self.query.is_ascii() && entry.display_name.is_ascii() {
                                lowercase_buffer
                                    .len()
                                    .cmp(&worst.path_lower.len())
                                    .then_with(|| {
                                        lowercase_buffer.as_slice().cmp(worst.path_lower.as_bytes())
                                    })
                                    .then_with(|| index.cmp(&worst.index))
                                    .is_lt()
                            } else {
                                let path_lower = entry.display_name.to_lowercase();
                                path_lower
                                    .len()
                                    .cmp(&worst.path_lower.len())
                                    .then_with(|| path_lower.as_str().cmp(&worst.path_lower))
                                    .then_with(|| index.cmp(&worst.index))
                                    .is_lt()
                            }
                        });
                        if should_replace {
                            scored.pop();
                            scored.push(ScoredMatch {
                                index,
                                rank,
                                fuzzy_score,
                                path_lower: entry.display_name.to_lowercase().into_boxed_str(),
                            });
                        }
                    }
                }
            }
            let mut scored = scored.into_vec();
            scored.sort();
            self.visible = scored.into_iter().map(|item| item.index).collect();
            self.total_matches = total_matches;
        }
        self.selected = 0;
        self.top_index = 0;
    }

    fn rebuild_visible_preserving_selection(&mut self) {
        let selected_path = self
            .selected_entry()
            .map(|entry| entry.relative_path.clone());
        self.rebuild_visible();
        if let Some(selected_path) = selected_path
            && let Some(position) = self.visible.iter().position(|index| {
                self.search_entries
                    .get(*index)
                    .is_some_and(|entry| entry.relative_path == selected_path)
            })
        {
            self.selected = position;
            self.ensure_visible();
        }
    }

    fn edit_action(&self, path: PathBuf) -> ExitAction {
        let working_directory =
            project_root_for(&path).unwrap_or_else(|| self.launch_directory.clone());
        ExitAction::Edit {
            path,
            working_directory,
        }
    }
}

impl Drop for App {
    fn drop(&mut self) {
        self.cancel_active_scan();
    }
}

fn available_scan_threads() -> usize {
    thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(2)
        .clamp(1, 4)
}

fn project_root_for(path: &Path) -> Option<PathBuf> {
    let directory = if path.is_dir() { path } else { path.parent()? };
    directory
        .ancestors()
        .find(|ancestor| ancestor.join(".git").exists())
        .map(Path::to_path_buf)
}

fn is_hidden(name: &std::ffi::OsStr) -> bool {
    name.to_string_lossy().starts_with('.')
}

fn match_rank(entry: &Entry, query: &str, query_lower: &str, lowercase_buffer: &mut Vec<u8>) -> u8 {
    let path = entry.display_name.as_ref();
    if query.is_ascii() && path.is_ascii() {
        lowercase_buffer.clear();
        lowercase_buffer.extend_from_slice(path.as_bytes());
        lowercase_buffer.make_ascii_lowercase();
        let path_lower = lowercase_buffer.as_slice();
        let name_lower = path_lower
            .iter()
            .rposition(|byte| *byte == std::path::MAIN_SEPARATOR as u8)
            .map_or(path_lower, |position| &path_lower[position + 1..]);
        let query_lower = query_lower.as_bytes();
        if path_lower == query_lower || name_lower == query_lower {
            0
        } else if name_lower.starts_with(query_lower) {
            1
        } else if contains_bytes(name_lower, query_lower) {
            2
        } else if path_lower.starts_with(query_lower) || contains_bytes(path_lower, query_lower) {
            3
        } else {
            4
        }
    } else {
        let name = entry.file_name().to_string_lossy();
        let path_lower = path.to_lowercase();
        let name_lower = name.to_lowercase();
        if path_lower == query_lower || name_lower == query_lower {
            0
        } else if name_lower.starts_with(query_lower) {
            1
        } else if name_lower.contains(query_lower) {
            2
        } else if path_lower.starts_with(query_lower) || path_lower.contains(query_lower) {
            3
        } else {
            4
        }
    }
}

fn contains_bytes(value: &[u8], needle: &[u8]) -> bool {
    memchr::memmem::find(value, needle).is_some()
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
            .map(|entry| entry.display_name.as_ref())
            .collect();
        assert_eq!(names, ["foo", "food", "xfoo", "far-out-object"]);
    }

    #[test]
    fn ascii_search_ranking_remains_case_insensitive() {
        let (_temp, mut app) = app_with_files(&["FOO", "FooBar", "xFoo", "far-out-object"]);
        for character in "foo".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE));
        }
        let names: Vec<_> = app
            .visible_entries()
            .map(|entry| entry.display_name.as_ref())
            .collect();
        assert_eq!(names, ["FOO", "FooBar", "xFoo", "far-out-object"]);
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
        assert_eq!(
            app.selected_entry().unwrap().display_name.as_ref(),
            "src/main.rs"
        );
    }

    fn finish_index(app: &mut App) {
        for _ in 0..400 {
            app.poll_index();
            if !app.indexing {
                return;
            }
            thread::sleep(Duration::from_millis(5));
        }
        panic!("recursive index did not finish");
    }

    #[test]
    fn visible_paths_are_indexed_before_hidden_paths_without_duplicates() {
        let temp = TempDir::new().unwrap();
        for name in ["src/visible.rs", ".config/settings", "src/.hidden/file"] {
            let path = temp.path().join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, "test").unwrap();
        }
        let mut app = App::new(temp.path().to_path_buf(), true).unwrap();
        finish_index(&mut app);
        let paths: Vec<_> = app
            .search_entries
            .iter()
            .map(|entry| entry.display_name.as_ref())
            .collect();
        let visible = paths
            .iter()
            .position(|path| *path == "src/visible.rs")
            .unwrap();
        for hidden in [".config/settings", "src/.hidden/file"] {
            assert!(visible < paths.iter().position(|path| *path == hidden).unwrap());
        }
        let unique: std::collections::HashSet<_> = paths.iter().collect();
        assert_eq!(unique.len(), paths.len());

        app.toggle_hidden();
        finish_index(&mut app);
        assert!(app.search_entries.iter().all(|entry| {
            !entry
                .relative_path
                .components()
                .any(|part| is_hidden(part.as_os_str()))
        }));
    }

    #[test]
    fn generated_directories_are_pruned_but_can_be_opened_directly() {
        let temp = TempDir::new().unwrap();
        for name in SKIPPED_INDEX_DIRS {
            let dir = temp.path().join("project").join(name).join("nested");
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("generated.txt"), "test").unwrap();
        }
        let mut app = App::new(temp.path().to_path_buf(), true).unwrap();
        finish_index(&mut app);
        assert!(
            !app.search_entries
                .iter()
                .any(|entry| entry.display_name.contains("generated.txt"))
        );

        app.switch_dir(temp.path().join("project")).unwrap();
        assert!(
            app.entries
                .iter()
                .any(|entry| entry.display_name.as_ref() == "target")
        );
        app.switch_dir(temp.path().join("project/target")).unwrap();
        finish_index(&mut app);
        assert!(
            app.search_entries
                .iter()
                .any(|entry| entry.display_name.as_ref() == "nested/generated.txt")
        );
    }

    #[test]
    fn search_retains_only_the_top_results_and_counts_every_match() {
        let (_temp, mut app) = app_with_files(&[]);
        app.search_entries = (0..MAX_SEARCH_RESULTS + 50)
            .map(|index| {
                let display_name = format!("file-{index:04}.rs");
                Entry {
                    relative_path: PathBuf::from(&display_name).into_boxed_path(),
                    display_name: display_name.into_boxed_str(),
                    is_dir: false,
                }
            })
            .collect();
        app.query = "file".to_owned();
        app.rebuild_visible();

        assert_eq!(app.visible.len(), MAX_SEARCH_RESULTS);
        assert_eq!(app.total_matches, MAX_SEARCH_RESULTS + 50);
        assert_eq!(
            app.selected_entry().unwrap().display_name.as_ref(),
            "file-0000.rs"
        );
    }

    #[test]
    fn replacement_scan_cancels_the_obsolete_scan() {
        let (_temp, mut app) = app_with_files(&[]);
        let obsolete_scan = Arc::clone(app.scan_cancel.as_ref().unwrap());

        app.start_recursive_index();

        assert!(obsolete_scan.load(Ordering::Acquire));
    }

    #[test]
    fn hidden_files_can_be_toggled() {
        let (_temp, mut app) = app_with_files(&["visible", ".hidden"]);
        assert_eq!(app.visible.len(), 2);

        app.handle_key(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::ALT));
        let names: Vec<_> = app
            .visible_entries()
            .map(|entry| entry.display_name.as_ref())
            .collect();
        assert_eq!(names, ["visible"]);

        app.handle_key(KeyEvent::new(KeyCode::F(2), KeyModifiers::NONE));
        assert_eq!(app.visible.len(), 2);
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
        let AppCommand::Exit(Some(ExitAction::Edit { path, .. })) =
            app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE))
        else {
            panic!("expected edit action");
        };
        assert!(path.ends_with("file.rs"));
    }

    #[test]
    fn enter_on_a_file_exits_with_an_edit_action() {
        let (_temp, mut app) = app_with_files(&["file.rs"]);
        let AppCommand::Exit(Some(ExitAction::Edit { path, .. })) =
            app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        else {
            panic!("expected edit action");
        };
        assert!(path.ends_with("file.rs"));
    }

    #[test]
    fn editor_uses_the_nearest_git_project_root() {
        let temp = TempDir::new().unwrap();
        fs::create_dir(temp.path().join(".git")).unwrap();
        fs::create_dir(temp.path().join("src")).unwrap();
        fs::write(temp.path().join("src/main.rs"), "fn main() {}\n").unwrap();
        let mut app = App::new(temp.path().join("src"), true).unwrap();

        let AppCommand::Exit(Some(ExitAction::Edit {
            path,
            working_directory,
        })) = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        else {
            panic!("expected edit action");
        };

        assert!(path.ends_with("src/main.rs"));
        assert_eq!(working_directory, temp.path().canonicalize().unwrap());
    }
}
