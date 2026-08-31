use std::{
    fs::{self, File},
    io::{self, Read},
    path::Path,
};

use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use syntect::{
    easy::HighlightLines,
    highlighting::{FontStyle, Theme, ThemeSet},
    parsing::SyntaxSet,
    util::LinesWithEndings,
};

use crate::app::Entry;

const MAX_FILE_SIZE: u64 = 50_000;
const MAX_PREVIEW_BYTES: u64 = 10_000;

#[derive(Debug, Default)]
pub struct Preview {
    pub lines: Vec<Line<'static>>,
}

pub struct Previewer {
    syntaxes: SyntaxSet,
    theme: Theme,
}

impl Previewer {
    pub fn new() -> Self {
        let themes = ThemeSet::load_defaults();
        let theme = themes
            .themes
            .get("base16-ocean.dark")
            .or_else(|| themes.themes.values().next())
            .cloned()
            .unwrap_or_default();
        Self {
            syntaxes: SyntaxSet::load_defaults_newlines(),
            theme,
        }
    }

    pub fn load(&self, entry: Option<&Entry>) -> Preview {
        let Some(entry) = entry else {
            return Preview::default();
        };
        let lines = if entry.is_dir {
            directory_lines(&entry.path)
        } else {
            self.file_lines(&entry.path)
        }
        .unwrap_or_else(|error| vec![message_line(format!("*{error}*"))]);
        Preview { lines }
    }

    fn file_lines(&self, path: &Path) -> io::Result<Vec<Line<'static>>> {
        let metadata = fs::metadata(path)?;
        if metadata.len() > MAX_FILE_SIZE {
            return Ok(vec![message_line("*file too large*")]);
        }
        if metadata.len() == 0 {
            return Ok(vec![message_line("*file empty*")]);
        }

        let mut bytes = Vec::new();
        File::open(path)?
            .take(MAX_PREVIEW_BYTES)
            .read_to_end(&mut bytes)?;
        if !looks_like_text(&bytes) {
            let mime =
                infer::get(&bytes).map_or("application/octet-stream", |kind| kind.mime_type());
            return Ok(vec![message_line(mime)]);
        }
        let content = match std::str::from_utf8(&bytes) {
            Ok(content) => content,
            Err(_) => return Ok(vec![message_line("application/octet-stream")]),
        };

        let syntax = self
            .syntaxes
            .find_syntax_for_file(path)
            .ok()
            .flatten()
            .unwrap_or_else(|| self.syntaxes.find_syntax_plain_text());
        let mut highlighter = HighlightLines::new(syntax, &self.theme);
        let mut lines = Vec::new();
        for source_line in LinesWithEndings::from(content) {
            let highlighted = highlighter
                .highlight_line(source_line, &self.syntaxes)
                .unwrap_or_else(|_| Vec::new());
            let spans = highlighted
                .into_iter()
                .map(|(style, text)| {
                    let color = style.foreground;
                    let mut terminal_style =
                        Style::default().fg(Color::Rgb(color.r, color.g, color.b));
                    if style.font_style.contains(FontStyle::BOLD) {
                        terminal_style = terminal_style.add_modifier(Modifier::BOLD);
                    }
                    if style.font_style.contains(FontStyle::ITALIC) {
                        terminal_style = terminal_style.add_modifier(Modifier::ITALIC);
                    }
                    if style.font_style.contains(FontStyle::UNDERLINE) {
                        terminal_style = terminal_style.add_modifier(Modifier::UNDERLINED);
                    }
                    Span::styled(
                        text.trim_end_matches(['\r', '\n']).to_owned(),
                        terminal_style,
                    )
                })
                .collect::<Vec<_>>();
            lines.push(Line::from(spans));
        }
        Ok(lines)
    }
}

fn directory_lines(path: &Path) -> io::Result<Vec<Line<'static>>> {
    let mut entries = fs::read_dir(path)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(fs::DirEntry::file_name);
    Ok(entries
        .into_iter()
        .take(1_000)
        .map(|entry| {
            let mut name = entry.file_name().to_string_lossy().into_owned();
            let is_dir = entry.file_type().is_ok_and(|kind| kind.is_dir())
                || fs::metadata(entry.path()).is_ok_and(|metadata| metadata.is_dir());
            if is_dir {
                name.push('/');
                Line::styled(name, Style::default().fg(Color::DarkGray))
            } else {
                Line::raw(name)
            }
        })
        .collect())
}

fn looks_like_text(bytes: &[u8]) -> bool {
    if bytes.contains(&0) {
        return false;
    }
    let controls = bytes
        .iter()
        .filter(|byte| **byte < 0x20 && !matches!(**byte, b'\n' | b'\r' | b'\t' | 0x0c))
        .count();
    controls.saturating_mul(20) <= bytes.len().max(1)
}

fn message_line(message: impl Into<String>) -> Line<'static> {
    Line::styled(message.into(), Style::default().fg(Color::DarkGray))
}

#[cfg(test)]
mod tests {
    use super::looks_like_text;

    #[test]
    fn text_detection_rejects_nulls_and_accepts_utf8() {
        assert!(looks_like_text("hello\nworld".as_bytes()));
        assert!(!looks_like_text(b"hello\0world"));
    }
}
