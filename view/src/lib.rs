pub mod tab_bar;
pub mod scroll_view;
pub mod list;
pub mod text_input;
pub mod filter_list;

pub use tab_bar::TabBar;
pub use scroll_view::ScrollView;
pub use list::List;
pub use text_input::TextInput;
pub use filter_list::FilterList;

use ratatui::layout::Rect;

/// Compute a centered rectangle within `area`.
pub fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let w = width.min(area.width);
    let h = height.min(area.height);
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    Rect::new(x, y, w, h)
}
