use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::Widget;

use crate::list::ListState;
use crate::text_input::TextInputState;

/// A filterable list: text input at the top, matching items below.
///
/// The caller provides all items and a filter function. The widget
/// renders only items whose text matches the current filter input.
/// Selection is maintained on the filtered view.
pub struct FilterList<'a> {
    state: &'a FilterListState,
    items: &'a [FilterListItem],
    style: Style,
    selected_style: Style,
    input_style: Style,
    match_style: Style,
    columns: &'a [(&'a str, u16)],
}

/// A row in the filter list.
pub struct FilterListItem {
    /// Column values for this row.
    pub cells: Vec<String>,
    /// The original index in the unfiltered list (for the caller to identify the item).
    pub index: usize,
}

/// State for a FilterList.
pub struct FilterListState {
    pub input: TextInputState,
    pub list: ListState,
    /// Indices into the items slice that match the current filter.
    pub filtered: Vec<usize>,
}

impl Default for FilterListState {
    fn default() -> Self {
        Self::new()
    }
}

impl FilterListState {
    pub fn new() -> Self {
        Self {
            input: TextInputState::new(""),
            list: ListState::new(0),
            filtered: Vec::new(),
        }
    }

    /// Recompute the filtered indices based on the current input.
    /// Call this after any input change.
    pub fn apply_filter(&mut self, items: &[FilterListItem]) {
        let query = self.input.value.to_lowercase();
        self.filtered = if query.is_empty() {
            (0..items.len()).collect()
        } else {
            items
                .iter()
                .enumerate()
                .filter(|(_, item)| {
                    item.cells
                        .iter()
                        .any(|cell| cell.to_lowercase().contains(&query))
                })
                .map(|(i, _)| i)
                .collect()
        };
        self.list.set_len(self.filtered.len());
    }

    /// The currently selected item index in the original (unfiltered) list,
    /// or None if the filtered list is empty.
    pub fn selected_item<'a>(&self, items: &'a [FilterListItem]) -> Option<&'a FilterListItem> {
        let filtered_idx = self.list.selected;
        self.filtered
            .get(filtered_idx)
            .and_then(|&i| items.get(i))
    }
}

impl<'a> FilterList<'a> {
    /// Create a new FilterList.
    /// `columns` defines header labels and widths for each column.
    pub fn new(
        state: &'a FilterListState,
        items: &'a [FilterListItem],
        columns: &'a [(&'a str, u16)],
    ) -> Self {
        Self {
            state,
            items,
            style: Style::default(),
            selected_style: Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
            input_style: Style::default().fg(Color::White),
            match_style: Style::default().fg(Color::Yellow),
            columns,
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
}

impl Widget for FilterList<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.height == 0 || area.width == 0 {
            return;
        }

        let y_input = area.y;
        let y_header = area.y + 1;
        let y_list_start = area.y + 2;
        let list_height = area.height.saturating_sub(2) as usize;

        // Row 0: filter input with prompt.
        let prompt = "/ ";
        let prompt_style = Style::default().fg(Color::DarkGray);
        let mut x = area.x;
        for ch in prompt.chars() {
            if x >= area.right() {
                break;
            }
            if let Some(cell) = buf.cell_mut((x, y_input)) {
                cell.set_char(ch);
                cell.set_style(prompt_style);
            }
            x += 1;
        }
        // Render text input inline.
        let input_area = Rect::new(x, y_input, area.right().saturating_sub(x), 1);
        let input_widget = crate::TextInput::new(&self.state.input).style(self.input_style);
        input_widget.render(input_area, buf);

        // Row 1: column headers.
        let header_style = Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::UNDERLINED);
        x = area.x;
        for &(name, width) in self.columns {
            if x >= area.right() {
                break;
            }
            for (i, ch) in name.chars().enumerate() {
                let cx = x + i as u16;
                if cx >= area.right() {
                    break;
                }
                if let Some(cell) = buf.cell_mut((cx, y_header)) {
                    cell.set_char(ch);
                    cell.set_style(header_style);
                }
            }
            x += width;
        }

        // Rows 2+: filtered items.
        let offset = self.state.list.offset;
        let selected = self.state.list.selected;
        let query = self.state.input.value.to_lowercase();

        for row in 0..list_height {
            let filtered_idx = offset + row;
            if filtered_idx >= self.state.filtered.len() {
                break;
            }
            let item_idx = self.state.filtered[filtered_idx];
            let item = &self.items[item_idx];
            let y = y_list_start + row as u16;
            let is_selected = filtered_idx == selected;
            let base_style = if is_selected {
                self.selected_style
            } else {
                self.style
            };

            x = area.x;
            for (col_i, &(_, width)) in self.columns.iter().enumerate() {
                if x >= area.right() {
                    break;
                }
                let cell_text = item.cells.get(col_i).map(|s| s.as_str()).unwrap_or("");

                // Highlight matching substring.
                let lower = cell_text.to_lowercase();
                let match_start = if !query.is_empty() {
                    lower.find(&query)
                } else {
                    None
                };

                for (i, ch) in cell_text.chars().enumerate() {
                    let cx = x + i as u16;
                    if cx >= area.right() || cx >= x + width {
                        break;
                    }
                    let style = if !is_selected
                        && match_start.is_some_and(|s| i >= s && i < s + query.len())
                    {
                        self.match_style
                    } else {
                        base_style
                    };
                    if let Some(cell) = buf.cell_mut((cx, y)) {
                        cell.set_char(ch);
                        cell.set_style(style);
                    }
                }
                x += width;
            }
        }
    }
}
