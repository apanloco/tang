use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::widgets::Widget;

/// Scrollable content area.
///
/// Renders lines of styled text with vertical scrolling. The scroll offset
/// determines which line appears at the top of the visible area.
pub struct ScrollView<'a> {
    lines: &'a [ScrollLine<'a>],
    offset: usize,
    /// Show a scrollbar on the right edge.
    scrollbar: bool,
    scrollbar_style: Style,
    scrollbar_track_style: Style,
}

/// A single line of content for the scroll view.
pub struct ScrollLine<'a> {
    pub spans: Vec<ScrollSpan<'a>>,
}

/// A styled span within a line.
pub struct ScrollSpan<'a> {
    pub text: &'a str,
    pub style: Style,
}

impl<'a> ScrollLine<'a> {
    pub fn raw(text: &'a str) -> Self {
        Self {
            spans: vec![ScrollSpan {
                text,
                style: Style::default(),
            }],
        }
    }

    pub fn styled(text: &'a str, style: Style) -> Self {
        Self {
            spans: vec![ScrollSpan { text, style }],
        }
    }

    pub fn spans(spans: Vec<ScrollSpan<'a>>) -> Self {
        Self { spans }
    }
}

impl<'a> ScrollSpan<'a> {
    pub fn new(text: &'a str, style: Style) -> Self {
        Self { text, style }
    }

    pub fn raw(text: &'a str) -> Self {
        Self {
            text,
            style: Style::default(),
        }
    }
}

impl<'a> ScrollView<'a> {
    pub fn new(lines: &'a [ScrollLine<'a>], offset: usize) -> Self {
        Self {
            lines,
            offset,
            scrollbar: true,
            scrollbar_style: Style::default().fg(Color::White),
            scrollbar_track_style: Style::default().fg(Color::DarkGray),
        }
    }

    pub fn scrollbar(mut self, show: bool) -> Self {
        self.scrollbar = show;
        self
    }

    pub fn scrollbar_style(mut self, style: Style) -> Self {
        self.scrollbar_style = style;
        self
    }

    /// Total number of lines.
    pub fn line_count(lines: &[ScrollLine<'_>]) -> usize {
        lines.len()
    }

    /// Clamp an offset so the view doesn't scroll past the last line.
    pub fn clamp_offset(offset: usize, line_count: usize, visible_height: usize) -> usize {
        if line_count <= visible_height {
            0
        } else {
            offset.min(line_count - visible_height)
        }
    }

    /// Map a click on the scrollbar track to a content offset.
    /// `click_y` is the absolute terminal row, `area` is the rendered area,
    /// `total_lines` is the total number of content lines.
    pub fn offset_from_scrollbar(click_y: u16, area: Rect, total_lines: usize) -> usize {
        let visible = area.height as usize;
        if total_lines <= visible {
            return 0;
        }
        let rel_y = click_y.saturating_sub(area.y) as usize;
        let max_offset = total_lines - visible;
        if visible <= 1 {
            return 0;
        }
        (rel_y * max_offset / (visible - 1)).min(max_offset)
    }

    /// Returns true if the given (x, y) is on the scrollbar column.
    pub fn is_scrollbar_hit(x: u16, area: Rect, total_lines: usize) -> bool {
        total_lines > area.height as usize && x == area.right().saturating_sub(1)
    }
}

impl Widget for ScrollView<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.height == 0 || area.width == 0 {
            return;
        }

        let content_width = if self.scrollbar && self.lines.len() > area.height as usize {
            area.width.saturating_sub(1)
        } else {
            area.width
        };

        let visible = area.height as usize;

        for row in 0..visible {
            let line_idx = self.offset + row;
            let y = area.y + row as u16;

            if line_idx < self.lines.len() {
                let line = &self.lines[line_idx];
                let mut x = area.x;
                for span in &line.spans {
                    for ch in span.text.chars() {
                        if x >= area.x + content_width {
                            break;
                        }
                        if let Some(cell) = buf.cell_mut((x, y)) {
                            cell.set_char(ch);
                            cell.set_style(span.style);
                        }
                        x += 1;
                    }
                }
            }
        }

        // Scrollbar.
        if self.scrollbar && self.lines.len() > visible {
            let sb_x = area.right() - 1;
            let total = self.lines.len();

            // Thumb position and size.
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
