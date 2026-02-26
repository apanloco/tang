use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::Widget;

/// A horizontal tab bar rendered as a single row.
///
/// Each tab shows its label. The active tab is highlighted.
/// An optional right-aligned status string (e.g. "CLIP") can be displayed.
pub struct TabBar<'a> {
    tabs: &'a [&'a str],
    active: usize,
    status: Option<(&'a str, Style)>,
    style: Style,
    active_style: Style,
    separator: &'a str,
}

impl<'a> TabBar<'a> {
    pub fn new(tabs: &'a [&'a str], active: usize) -> Self {
        Self {
            tabs,
            active,
            status: None,
            style: Style::default().fg(Color::DarkGray),
            active_style: Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
            separator: " │ ",
        }
    }

    /// Set a right-aligned status indicator (e.g. "CLIP" in red).
    pub fn status(mut self, text: &'a str, style: Style) -> Self {
        self.status = Some((text, style));
        self
    }

    pub fn style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    pub fn active_style(mut self, style: Style) -> Self {
        self.active_style = style;
        self
    }

    pub fn separator(mut self, sep: &'a str) -> Self {
        self.separator = sep;
        self
    }

    /// Hit-test: given a click at (x, y), return which tab index was clicked.
    /// `area` is the Rect the tab bar was rendered into.
    pub fn tab_at(x: u16, y: u16, area: Rect, tabs: &[&str], separator: &str) -> Option<usize> {
        if y != area.y || x < area.x || x >= area.right() {
            return None;
        }
        let rel_x = (x - area.x) as usize;
        let sep_len = separator.chars().count();
        let mut pos = 0;
        for (i, &tab) in tabs.iter().enumerate() {
            if i > 0 {
                pos += sep_len;
            }
            let tab_len = tab.chars().count();
            if rel_x >= pos && rel_x < pos + tab_len {
                return Some(i);
            }
            pos += tab_len;
        }
        None
    }
}

impl Widget for TabBar<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.height == 0 || area.width == 0 {
            return;
        }

        let mut x = area.x;
        let y = area.y;

        for (i, &label) in self.tabs.iter().enumerate() {
            if i > 0 {
                // Draw separator.
                for ch in self.separator.chars() {
                    if x >= area.right() {
                        break;
                    }
                    if let Some(cell) = buf.cell_mut((x, y)) {
                        cell.set_char(ch);
                        cell.set_style(self.style);
                    }
                    x += 1;
                }
            }

            let style = if i == self.active {
                self.active_style
            } else {
                self.style
            };

            for ch in label.chars() {
                if x >= area.right() {
                    break;
                }
                if let Some(cell) = buf.cell_mut((x, y)) {
                    cell.set_char(ch);
                    cell.set_style(style);
                }
                x += 1;
            }
        }

        // Right-aligned status.
        if let Some((text, style)) = self.status {
            let text_len = text.len() as u16;
            if text_len < area.width {
                let sx = area.right() - text_len;
                for (i, ch) in text.chars().enumerate() {
                    if let Some(cell) = buf.cell_mut((sx + i as u16, y)) {
                        cell.set_char(ch);
                        cell.set_style(style);
                    }
                }
            }
        }
    }
}
