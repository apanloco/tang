use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::Widget;

/// A navigable list with a selected item and vertical scrolling.
///
/// Used for parameter lists, plugin selectors, preset lists, etc.
/// Renders visible items within the area, auto-scrolling to keep the
/// selected item visible.
pub struct List<'a> {
    items: &'a [ListItem<'a>],
    selected: usize,
    offset: usize,
    style: Style,
    selected_style: Style,
    /// Prefix shown before the selected item.
    cursor: &'a str,
    /// Prefix width reserved for cursor alignment.
    cursor_width: u16,
    /// Show a scrollbar on the right edge when content overflows.
    scrollbar: bool,
    scrollbar_style: Style,
    scrollbar_track_style: Style,
}

/// A single item in the list.
pub struct ListItem<'a> {
    pub spans: Vec<ListSpan<'a>>,
}

/// A styled span within a list item.
pub struct ListSpan<'a> {
    pub text: &'a str,
    pub style: Style,
}

impl<'a> ListItem<'a> {
    pub fn raw(text: &'a str) -> Self {
        Self {
            spans: vec![ListSpan {
                text,
                style: Style::default(),
            }],
        }
    }

    pub fn styled(text: &'a str, style: Style) -> Self {
        Self {
            spans: vec![ListSpan { text, style }],
        }
    }

    pub fn spans(spans: Vec<ListSpan<'a>>) -> Self {
        Self { spans }
    }
}

impl<'a> ListSpan<'a> {
    pub fn new(text: &'a str, style: Style) -> Self {
        Self { text, style }
    }
}

/// Manages selection and scroll offset for a list.
#[derive(Default, Clone)]
pub struct ListState {
    pub selected: usize,
    pub offset: usize,
    pub len: usize,
}

impl ListState {
    pub fn new(len: usize) -> Self {
        Self {
            selected: 0,
            offset: 0,
            len,
        }
    }

    /// Move selection down, wrapping at the end.
    pub fn down(&mut self) {
        if self.len > 0 {
            self.selected = (self.selected + 1) % self.len;
        }
    }

    /// Move selection up, wrapping at the start.
    pub fn up(&mut self) {
        if self.len > 0 {
            self.selected = (self.selected + self.len - 1) % self.len;
        }
    }

    /// Move selection down without wrapping. Returns true if moved.
    pub fn down_nowrap(&mut self) -> bool {
        if self.len > 0 && self.selected < self.len - 1 {
            self.selected += 1;
            true
        } else {
            false
        }
    }

    /// Move selection up without wrapping. Returns true if moved.
    pub fn up_nowrap(&mut self) -> bool {
        if self.selected > 0 {
            self.selected -= 1;
            true
        } else {
            false
        }
    }

    /// Move selection down by `n` items without wrapping.
    pub fn page_down(&mut self, n: usize) {
        if self.len > 0 {
            self.selected = (self.selected + n).min(self.len - 1);
        }
    }

    /// Move selection up by `n` items without wrapping.
    pub fn page_up(&mut self, n: usize) {
        self.selected = self.selected.saturating_sub(n);
    }

    /// Set the total number of items (resets selection if out of bounds).
    pub fn set_len(&mut self, len: usize) {
        self.len = len;
        if self.selected >= len {
            self.selected = len.saturating_sub(1);
        }
    }

    /// Ensure the selected item is visible given a viewport height.
    pub fn ensure_visible(&mut self, visible_height: usize) {
        if visible_height == 0 {
            return;
        }
        if self.selected < self.offset {
            self.offset = self.selected;
        } else if self.selected >= self.offset + visible_height {
            self.offset = self.selected - visible_height + 1;
        }
    }

    /// Handle a mouse click at the given (x, y) position within the rendered area.
    /// Returns true if the click hit an item.
    pub fn click_at(&mut self, y: u16, area: Rect) -> bool {
        if y < area.y || y >= area.bottom() {
            return false;
        }
        let row = (y - area.y) as usize;
        let idx = self.offset + row;
        if idx < self.len {
            self.selected = idx;
            true
        } else {
            false
        }
    }

    /// Returns true if the given x coordinate is on the scrollbar column
    /// and the list has more items than visible rows.
    pub fn is_scrollbar_hit(&self, x: u16, area: Rect) -> bool {
        let visible = area.height as usize;
        self.len > visible && x == area.right().saturating_sub(1)
    }

    /// Map a click/drag y position on the scrollbar to a selection index.
    pub fn select_from_scrollbar(&mut self, y: u16, area: Rect) {
        let visible = area.height as usize;
        if self.len <= visible {
            return;
        }
        let rel_y = y.saturating_sub(area.y) as usize;
        let idx = if visible <= 1 {
            0
        } else {
            (rel_y * (self.len - 1)) / (visible - 1)
        };
        self.selected = idx.min(self.len.saturating_sub(1));
    }
}

impl<'a> List<'a> {
    pub fn new(items: &'a [ListItem<'a>], state: &ListState) -> Self {
        Self {
            items,
            selected: state.selected,
            offset: state.offset,
            style: Style::default(),
            selected_style: Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
            cursor: "▸ ",
            cursor_width: 2,
            scrollbar: true,
            scrollbar_style: Style::default().fg(Color::White),
            scrollbar_track_style: Style::default().fg(Color::DarkGray),
        }
    }

    pub fn style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    pub fn selected_style(mut self, style: Style) -> Self {
        self.selected_style = style;
        self
    }

    pub fn cursor(mut self, cursor: &'a str, width: u16) -> Self {
        self.cursor = cursor;
        self.cursor_width = width;
        self
    }

    pub fn scrollbar(mut self, show: bool) -> Self {
        self.scrollbar = show;
        self
    }
}

impl Widget for List<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.height == 0 || area.width == 0 {
            return;
        }

        let visible = area.height as usize;
        let has_scrollbar =
            self.scrollbar && self.items.len() > visible;
        let content_right = if has_scrollbar {
            area.right().saturating_sub(1)
        } else {
            area.right()
        };

        for row in 0..visible {
            let item_idx = self.offset + row;
            let y = area.y + row as u16;

            if item_idx >= self.items.len() {
                break;
            }

            let is_selected = item_idx == self.selected;
            let base_style = if is_selected {
                self.selected_style
            } else {
                self.style
            };

            let mut x = area.x;

            // Cursor prefix.
            if is_selected {
                for ch in self.cursor.chars() {
                    if x >= content_right {
                        break;
                    }
                    if let Some(cell) = buf.cell_mut((x, y)) {
                        cell.set_char(ch);
                        cell.set_style(base_style);
                    }
                    x += 1;
                }
            } else {
                x += self.cursor_width;
            }

            // Item spans.
            let item = &self.items[item_idx];
            for span in &item.spans {
                let style = if is_selected {
                    // Merge: selected style takes priority for fg/modifiers,
                    // but span style can provide bg or other attrs.
                    base_style.patch(span.style)
                } else {
                    self.style.patch(span.style)
                };
                for ch in span.text.chars() {
                    if x >= content_right {
                        break;
                    }
                    if let Some(cell) = buf.cell_mut((x, y)) {
                        cell.set_char(ch);
                        cell.set_style(style);
                    }
                    x += 1;
                }
            }
        }

        // Scrollbar.
        if has_scrollbar {
            let sb_x = area.right() - 1;
            let total = self.items.len();
            let thumb_size = ((visible * visible) / total).max(1);
            let max_offset = total - visible;
            let thumb_start = if max_offset > 0 {
                (self.offset * (visible - thumb_size)) / max_offset
            } else {
                0
            };

            for row in 0..visible {
                let y = area.y + row as u16;
                let in_thumb = row >= thumb_start && row < thumb_start + thumb_size;
                let (ch, style) = if in_thumb {
                    ('┃', self.scrollbar_style)
                } else {
                    ('│', self.scrollbar_track_style)
                };
                if let Some(cell) = buf.cell_mut((sb_x, y)) {
                    cell.set_char(ch);
                    cell.set_style(style);
                }
            }
        }
    }
}
