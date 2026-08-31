use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};
use unicode_width::UnicodeWidthStr;

use crate::{app::App, preview::Preview};

const DIRECTORY_COLOR: Color = Color::Cyan;

pub fn draw(frame: &mut Frame<'_>, app: &mut App, preview: &Preview) {
    let area = frame.area();
    let show_preview = app.preview_enabled && area.width >= 70;
    let columns = if show_preview {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
            .split(area)
    } else {
        Layout::default()
            .constraints([Constraint::Percentage(100)])
            .split(area)
    };
    let left = columns[0];
    app.set_viewport_height(left.height.saturating_sub(3) as usize);

    draw_left(frame, left, app);
    if show_preview {
        draw_preview(frame, columns[1], preview);
    }
}

fn draw_preview(frame: &mut Frame<'_>, area: Rect, preview: &Preview) {
    for (offset, line) in preview.lines.iter().take(area.height as usize).enumerate() {
        let row = Rect::new(area.x, area.y + offset as u16, area.width, 1);
        frame.render_widget(line, row);
    }
}

fn draw_left(frame: &mut Frame<'_>, area: Rect, app: &App) {
    if area.is_empty() {
        return;
    }
    let inner = Rect::new(
        area.x.saturating_add(1),
        area.y.saturating_add(1),
        area.width.saturating_sub(2),
        area.height.saturating_sub(1),
    );
    let top = Rect::new(inner.x, inner.y, inner.width, 1);
    let list = Rect::new(
        inner.x,
        inner.y.saturating_add(1),
        inner.width,
        inner.height.saturating_sub(2),
    );
    let status = Rect::new(inner.x, area.bottom().saturating_sub(1), inner.width, 1);

    if let Some(prompt) = &app.prompt {
        let mut spans = vec![Span::styled(
            prompt.label.clone(),
            Style::default().fg(Color::Black).bg(Color::White),
        )];
        if let Some((directory, file)) = prompt.input.rsplit_once('/') {
            spans.push(Span::styled(
                format!("{directory}/"),
                Style::default().fg(Color::Blue),
            ));
            spans.push(Span::raw(file.to_owned()));
        } else {
            spans.push(Span::raw(prompt.input.clone()));
        }
        frame.render_widget(Paragraph::new(Line::from(spans)), top);
        let cursor_x = top
            .x
            .saturating_add(UnicodeWidthStr::width(prompt.label.as_str()) as u16)
            .saturating_add(UnicodeWidthStr::width(prompt.input.as_str()) as u16)
            .min(top.right().saturating_sub(1));
        frame.set_cursor_position((cursor_x, top.y));
    } else {
        frame.render_widget(Paragraph::new(header_line(app)), top);
    }

    let mut drew_entry = false;
    for (offset, entry) in app
        .visible_entries()
        .skip(app.top_index)
        .take(list.height as usize)
        .enumerate()
    {
        drew_entry = true;
        let index = app.top_index + offset;
        let style = if index == app.selected && entry.is_dir {
            Style::default().fg(Color::White).bg(Color::DarkGray)
        } else if index == app.selected {
            Style::default().fg(Color::Black).bg(Color::White)
        } else if entry.is_dir {
            Style::default().fg(DIRECTORY_COLOR)
        } else {
            Style::default()
        };
        let row = Rect::new(list.x, list.y + offset as u16, list.width, 1);
        frame.render_widget(Span::styled(entry.display_name.as_str(), style), row);
        if entry.is_dir {
            let slash_x = row
                .x
                .saturating_add(UnicodeWidthStr::width(entry.display_name.as_str()) as u16);
            if slash_x < row.right() {
                frame.render_widget(Span::styled("/", style), Rect::new(slash_x, row.y, 1, 1));
            }
        }
    }

    if !drew_entry {
        frame.render_widget(
            Span::styled("*nothing here*", Style::default().fg(Color::DarkGray)),
            list,
        );
    }

    if let Some(message) = &app.status {
        frame.render_widget(
            Paragraph::new(Line::styled(
                message.as_str(),
                Style::default().fg(Color::Red),
            )),
            status,
        );
    }
}

fn header_line<'a>(app: &'a App) -> Line<'a> {
    let mut cwd = app.cwd.display().to_string();
    if !cwd.ends_with(std::path::MAIN_SEPARATOR) {
        cwd.push(std::path::MAIN_SEPARATOR);
    }
    let mut spans = vec![Span::raw(cwd), Span::raw(app.query.as_str())];
    if let Some(entry) = app.selected_entry() {
        let remainder = if app.query.is_empty() {
            Some(entry.display_name.as_str())
        } else if entry
            .display_name
            .to_lowercase()
            .starts_with(&app.query.to_lowercase())
        {
            entry
                .display_name
                .char_indices()
                .nth(app.query.chars().count())
                .map_or(Some(""), |(index, _)| entry.display_name.get(index..))
        } else {
            None
        };
        if let Some(remainder) = remainder {
            spans.push(Span::styled(
                remainder,
                Style::default().fg(Color::DarkGray),
            ));
        }
    }
    let count = if app.visible.is_empty() {
        "[0/0]".to_owned()
    } else if !app.query.is_empty() && app.total_matches > app.visible.len() {
        format!(
            "[{}/{} of {}]",
            app.selected + 1,
            app.visible.len(),
            app.total_matches
        )
    } else {
        format!("[{}/{}]", app.selected + 1, app.visible.len())
    };
    let index_state = if app.indexing {
        " indexing…"
    } else if app.index_truncated {
        " index limit reached"
    } else {
        ""
    };
    let hidden_state = if app.show_hidden {
        " hidden:on"
    } else {
        " hidden:off"
    };
    spans.push(Span::styled(
        format!("  {count}{index_state}{hidden_state}"),
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::DIM),
    ));
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use ratatui::{Terminal, backend::TestBackend};
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn draws_borderless_file_list() {
        let temp = TempDir::new().unwrap();
        fs::write(temp.path().join("hello.rs"), "fn main() {}\n").unwrap();
        fs::create_dir(temp.path().join("src")).unwrap();
        let mut app = App::new(temp.path().to_path_buf(), false).unwrap();
        let preview = Preview::default();
        let mut terminal = Terminal::new(TestBackend::new(80, 12)).unwrap();
        terminal
            .draw(|frame| draw(frame, &mut app, &preview))
            .unwrap();
        let rendered =
            terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .fold(String::new(), |mut text, cell| {
                    text.push_str(cell.symbol());
                    text
                });
        assert!(rendered.contains("hello.rs"));
        assert!(rendered.contains("src/"));
    }
}
