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
            syntaxes: two_face::syntax::extra_newlines(),
            theme,
        }
    }

    pub fn load_path(&self, path: &Path, is_dir: bool, max_lines: usize) -> Preview {
        if max_lines == 0 {
            return Preview::default();
        }
        let lines = if is_dir {
            directory_lines(path, max_lines)
        } else {
            self.file_lines(path, max_lines)
        }
        .unwrap_or_else(|error| vec![message_line(format!("*{error}*"))]);
        Preview { lines }
    }

    fn file_lines(&self, path: &Path, max_lines: usize) -> io::Result<Vec<Line<'static>>> {
        let metadata = fs::metadata(path)?;
        if metadata.len() > MAX_FILE_SIZE {
            return Ok(vec![message_line("*file too large*")]);
        }
        if metadata.len() == 0 {
            return Ok(vec![message_line("*file empty*")]);
        }

        let read_limit = metadata.len().min(MAX_PREVIEW_BYTES) as usize;
        let mut bytes = Vec::with_capacity(read_limit);
        File::open(path)?
            .take(read_limit as u64)
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
        let mut lines = Vec::with_capacity(max_lines);
        for source_line in LinesWithEndings::from(content).take(max_lines) {
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

fn directory_lines(path: &Path, max_lines: usize) -> io::Result<Vec<Line<'static>>> {
    let mut entries = fs::read_dir(path)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(fs::DirEntry::file_name);
    Ok(entries
        .into_iter()
        .take(max_lines)
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
    use std::fs;

    use tempfile::TempDir;

    use super::{Previewer, looks_like_text};

    #[test]
    fn extended_syntaxes_include_common_languages() {
        let previewer = Previewer::new();

        for (extension, expected_name) in [
            ("ts", "TypeScript"),
            ("tsx", "TypeScriptReact"),
            ("kt", "Kotlin"),
            ("ex", "Elixir"),
            ("tf", "Terraform"),
            ("zig", "Zig"),
        ] {
            let syntax = previewer
                .syntaxes
                .find_syntax_by_extension(extension)
                .unwrap_or_else(|| panic!("missing syntax for .{extension}"));
            assert_eq!(syntax.name, expected_name);
        }
    }

    #[test]
    fn preview_highlights_only_visible_lines() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("many.rs");
        fs::write(&path, "let value = 1;\n".repeat(20)).unwrap();
        let previewer = Previewer::new();

        assert_eq!(previewer.load_path(&path, false, 3).lines.len(), 3);
        assert!(previewer.load_path(&path, false, 0).lines.is_empty());
    }

    #[test]
    fn text_detection_rejects_nulls_and_accepts_utf8() {
        assert!(looks_like_text("hello\nworld".as_bytes()));
        assert!(!looks_like_text(b"hello\0world"));
    }
}
