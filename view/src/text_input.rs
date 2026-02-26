use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::Widget;

/// State for a single-line text input.
#[derive(Clone)]
pub struct TextInputState {
    pub value: String,
    pub cursor: usize,
}

impl TextInputState {
    pub fn new(initial: &str) -> Self {
        Self {
            value: initial.to_string(),
            cursor: initial.len(),
        }
    }

    pub fn insert(&mut self, ch: char) {
        self.value.insert(self.cursor, ch);
        self.cursor += ch.len_utf8();
    }

    pub fn backspace(&mut self) {
        if self.cursor > 0 {
            let prev = self.value[..self.cursor]
                .char_indices()
                .next_back()
                .map(|(i, _)| i)
                .unwrap_or(0);
            self.value.remove(prev);
            self.cursor = prev;
        }
    }

    pub fn delete(&mut self) {
        if self.cursor < self.value.len() {
            self.value.remove(self.cursor);
        }
    }

    pub fn move_left(&mut self) {
        if self.cursor > 0 {
            self.cursor = self.value[..self.cursor]
                .char_indices()
                .next_back()
                .map(|(i, _)| i)
                .unwrap_or(0);
        }
    }

    pub fn move_right(&mut self) {
        if self.cursor < self.value.len() {
            self.cursor += self.value[self.cursor..]
                .chars()
                .next()
                .map_or(0, |c| c.len_utf8());
        }
    }

    pub fn home(&mut self) {
        self.cursor = 0;
    }

    pub fn end(&mut self) {
        self.cursor = self.value.len();
    }
}

/// Single-line text input widget.
///
/// Renders the text with a visible cursor (reverse-video block).
pub struct TextInput<'a> {
    state: &'a TextInputState,
    style: Style,
    cursor_style: Style,
}

impl<'a> TextInput<'a> {
    pub fn new(state: &'a TextInputState) -> Self {
        Self {
            state,
            style: Style::default(),
            cursor_style: Style::default()
                .fg(Color::Black)
                .bg(Color::White)
                .add_modifier(Modifier::BOLD),
        }
    }

    pub fn style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    pub fn cursor_style(mut self, style: Style) -> Self {
        self.cursor_style = style;
        self
    }
}

impl Widget for TextInput<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.height == 0 || area.width == 0 {
            return;
        }

        let y = area.y;
        let mut x = area.x;

        for (i, ch) in self.state.value.char_indices() {
            if x >= area.right() {
                break;
            }
            let style = if i == self.state.cursor {
                self.cursor_style
            } else {
                self.style
            };
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.set_char(ch);
                cell.set_style(style);
            }
            x += 1;
        }

        // Cursor at end of text: show block on empty space.
        if self.state.cursor >= self.state.value.len()
            && x < area.right()
            && let Some(cell) = buf.cell_mut((x, y))
        {
            cell.set_char(' ');
            cell.set_style(self.cursor_style);
        }
    }
}
