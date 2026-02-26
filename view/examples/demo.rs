use std::io;
use std::time::Duration;

use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
    MouseButton, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Terminal;

use view::filter_list::{FilterListItem, FilterListState};
use view::list::{ListItem, ListState};
use view::scroll_view::ScrollLine;
use view::text_input::TextInputState;
use view::{FilterList, List, ScrollView, TabBar, TextInput, centered_rect};

const TAB_NAMES: &[&str] = &["(1) Session", "(2) Piano", "(3) Scope", "(4) Help"];
const TAB_SEP: &str = " │ ";

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

struct PluginData {
    name: String,
    format: String,
    is_instrument: bool,
    params: Vec<(String, f32)>,
}

/// An entry in the plugin catalog (simulates enumerate output).
struct CatalogEntry {
    name: &'static str,
    format: &'static str,
    is_instrument: bool,
    params: usize,
    presets: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SelectorMode {
    Instrument,
    Effect,
}

struct SelectorState {
    mode: SelectorMode,
    filter: FilterListState,
    items: Vec<FilterListItem>,
}

struct EditState {
    input: TextInputState,
    param_name: String,
}

#[derive(Default, Clone)]
struct Areas {
    tab: Rect,
    content: Rect,
    action_bar: Rect,
    chain_inner: Rect,
    param_inner: Rect,
}

// ---------------------------------------------------------------------------
// App state
// ---------------------------------------------------------------------------

struct State {
    active_tab: usize,
    show_clip: bool,
    plugins: Vec<PluginData>,
    chain_labels: Vec<String>,
    chain_state: ListState,
    param_state: ListState,
    focus_params: bool,
    help_lines: Vec<String>,
    help_offset: usize,
    scrollbar_dragging: bool,
    param_dragging: bool,
    editing: Option<EditState>,
    selector: Option<SelectorState>,
    catalog: Vec<CatalogEntry>,
    areas: Areas,
    quit: bool,
}

impl State {
    fn new() -> Self {
        let plugins = demo_plugins();
        let chain_labels = build_chain_labels(&plugins);
        let param_len = plugins[0].params.len();
        let catalog = demo_catalog();

        let mut help_lines: Vec<String> = vec![
            "Tang — Terminal Audio Plugin Host".into(),
            "".into(),
            "Global keybindings:".into(),
            "  1 2 3 4    Switch to tab by number".into(),
            "  Tab        Next tab".into(),
            "  Shift+Tab  Previous tab".into(),
            "  Ctrl+Q     Quit".into(),
            "".into(),
            "Session tab (chain focus):".into(),
            "  Up/Down    Navigate chain".into(),
            "  Enter      Focus parameter list".into(),
            "  i          Replace instrument".into(),
            "  a          Add effect after selected".into(),
            "  d          Delete selected effect".into(),
            "".into(),
            "Session tab (param focus):".into(),
            "  Up/Down    Navigate parameters".into(),
            "  Left/Right Adjust value (±0.05)".into(),
            "  Shift+←/→  Fine adjust (±0.01)".into(),
            "  Ctrl+←/→   Coarse adjust (±0.10)".into(),
            "  Enter      Type a value".into(),
            "  Esc        Back to chain".into(),
            "".into(),
            "Plugin selector popup:".into(),
            "  Type       Filter by name/format".into(),
            "  Up/Down    Navigate results".into(),
            "  Enter      Select plugin".into(),
            "  Esc        Cancel".into(),
            "".into(),
            "Mouse:".into(),
            "  Click      Select tabs, chain items, parameters".into(),
            "  Drag       Drag parameter bars to set value".into(),
            "  Scroll     Scroll lists and help text".into(),
            "  Scrollbar  Click/drag to jump".into(),
            "".into(),
            "Press 'c' to toggle the CLIP indicator.".into(),
            "".into(),
            "---".into(),
        ];
        for i in 0..30 {
            help_lines.push(format!("  (scroll line {})", i + 1));
        }

        Self {
            active_tab: 0,
            show_clip: false,
            chain_labels,
            chain_state: ListState::new(plugins.len()),
            param_state: ListState::new(param_len),
            plugins,
            focus_params: false,
            help_lines,
            help_offset: 0,
            scrollbar_dragging: false,
            param_dragging: false,
            editing: None,
            selector: None,
            catalog,
            areas: Areas::default(),
            quit: false,
        }
    }

    fn rebuild_chain_labels(&mut self) {
        self.chain_labels = build_chain_labels(&self.plugins);
        self.chain_state.set_len(self.plugins.len());
        self.sync_param_state();
    }

    fn sync_param_state(&mut self) {
        let pi = self.chain_state.selected;
        if pi < self.plugins.len() {
            self.param_state.set_len(self.plugins[pi].params.len());
        }
    }

    fn open_selector(&mut self, mode: SelectorMode) {
        let items: Vec<FilterListItem> = self
            .catalog
            .iter()
            .enumerate()
            .filter(|(_, e)| match mode {
                SelectorMode::Instrument => e.is_instrument,
                SelectorMode::Effect => !e.is_instrument,
            })
            .map(|(i, e)| FilterListItem {
                cells: vec![
                    e.name.into(),
                    e.format.into(),
                    e.params.to_string(),
                    e.presets.to_string(),
                ],
                index: i,
            })
            .collect();

        let mut filter = FilterListState::new();
        filter.apply_filter(&items);

        self.selector = Some(SelectorState {
            mode,
            filter,
            items,
        });
    }

    fn confirm_selector(&mut self) {
        let sel = match self.selector.take() {
            Some(s) => s,
            None => return,
        };

        let chosen = match sel.filter.selected_item(&sel.items) {
            Some(item) => item.index,
            None => return,
        };
        let entry = &self.catalog[chosen];

        let new_plugin = PluginData {
            name: entry.name.into(),
            format: entry.format.into(),
            is_instrument: entry.is_instrument,
            params: make_fake_params(entry.params),
        };

        match sel.mode {
            SelectorMode::Instrument => {
                // Replace the first instrument (always index 0).
                if !self.plugins.is_empty() && self.plugins[0].is_instrument {
                    self.plugins[0] = new_plugin;
                } else {
                    self.plugins.insert(0, new_plugin);
                }
                self.chain_state.selected = 0;
            }
            SelectorMode::Effect => {
                // Insert after the currently selected chain item.
                let insert_at = (self.chain_state.selected + 1).min(self.plugins.len());
                self.plugins.insert(insert_at, new_plugin);
                self.chain_state.selected = insert_at;
            }
        }

        self.rebuild_chain_labels();
    }
}

// ---------------------------------------------------------------------------
// Main + event loop
// ---------------------------------------------------------------------------

fn main() -> io::Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run(&mut terminal);

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        DisableMouseCapture,
        LeaveAlternateScreen
    )?;
    result
}

fn run(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> io::Result<()> {
    let mut s = State::new();

    loop {
        // --- Render ---
        render(terminal, &mut s)?;

        if s.quit {
            break;
        }

        // --- Event loop: block for first, drain rest ---
        let ev = event::read()?;
        process_event(&mut s, ev);
        while event::poll(Duration::ZERO)? {
            process_event(&mut s, event::read()?);
        }
    }

    Ok(())
}

fn process_event(s: &mut State, ev: Event) {
    match ev {
        Event::Key(key) if key.kind == KeyEventKind::Press => {
            // Priority: selector > editing > normal.
            if s.selector.is_some() {
                handle_selector_key(s, key.code);
            } else if s.editing.is_some() {
                handle_edit_key(s, key.code);
            } else {
                handle_key(s, key.code, key.modifiers);
            }
        }
        Event::Mouse(mouse) => {
            // Dismiss popups on click.
            if s.selector.is_some() || s.editing.is_some() {
                if let MouseEventKind::Down(MouseButton::Left) = mouse.kind {
                    s.selector = None;
                    s.editing = None;
                }
                return;
            }
            handle_mouse(s, mouse.kind, mouse.column, mouse.row);
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Key handlers
// ---------------------------------------------------------------------------

fn handle_selector_key(s: &mut State, code: KeyCode) {
    let sel = s.selector.as_mut().unwrap();
    match code {
        KeyCode::Esc => s.selector = None,
        KeyCode::Enter => s.confirm_selector(),
        KeyCode::Up => {
            sel.filter.list.up();
            sel.filter
                .list
                .ensure_visible(20); // approximate popup height
        }
        KeyCode::Down => {
            sel.filter.list.down();
            sel.filter.list.ensure_visible(20);
        }
        KeyCode::Backspace => {
            sel.filter.input.backspace();
            sel.filter.apply_filter(&sel.items);
        }
        KeyCode::Char(ch) => {
            sel.filter.input.insert(ch);
            sel.filter.apply_filter(&sel.items);
        }
        _ => {}
    }
}

fn handle_edit_key(s: &mut State, code: KeyCode) {
    let edit = s.editing.as_mut().unwrap();
    match code {
        KeyCode::Esc => s.editing = None,
        KeyCode::Enter => {
            if let Ok(val) = edit.input.value.parse::<f32>() {
                let pi = s.chain_state.selected;
                let pa = s.param_state.selected;
                if let Some(param) = s.plugins.get_mut(pi).and_then(|p| p.params.get_mut(pa)) {
                    param.1 = val.clamp(0.0, 1.0);
                }
            }
            s.editing = None;
        }
        KeyCode::Backspace => edit.input.backspace(),
        KeyCode::Delete => edit.input.delete(),
        KeyCode::Left => edit.input.move_left(),
        KeyCode::Right => edit.input.move_right(),
        KeyCode::Home => edit.input.home(),
        KeyCode::End => edit.input.end(),
        KeyCode::Char(ch) => edit.input.insert(ch),
        _ => {}
    }
}

fn handle_key(s: &mut State, code: KeyCode, modifiers: KeyModifiers) {
    match code {
        KeyCode::Char('q') => s.quit = true,
        KeyCode::Char('c') => s.show_clip = !s.show_clip,
        KeyCode::Char('1') => s.active_tab = 0,
        KeyCode::Char('2') => s.active_tab = 1,
        KeyCode::Char('3') => s.active_tab = 2,
        KeyCode::Char('4') => s.active_tab = 3,
        KeyCode::Tab => s.active_tab = (s.active_tab + 1) % TAB_NAMES.len(),
        KeyCode::BackTab => s.active_tab = (s.active_tab + TAB_NAMES.len() - 1) % TAB_NAMES.len(),

        // --- Session tab ---
        KeyCode::Char('i') if s.active_tab == 0 && !s.focus_params => {
            s.open_selector(SelectorMode::Instrument);
        }
        KeyCode::Char('a') if s.active_tab == 0 && !s.focus_params => {
            s.open_selector(SelectorMode::Effect);
        }
        KeyCode::Char('d') if s.active_tab == 0 && !s.focus_params => {
            // Delete selected effect (never delete the instrument).
            let sel = s.chain_state.selected;
            if sel < s.plugins.len() && !s.plugins[sel].is_instrument {
                s.plugins.remove(sel);
                s.rebuild_chain_labels();
            }
        }
        KeyCode::Enter if s.active_tab == 0 => {
            if s.focus_params {
                let pi = s.chain_state.selected;
                let pa = s.param_state.selected;
                if let Some((name, val)) =
                    s.plugins.get(pi).and_then(|p| p.params.get(pa))
                {
                    s.editing = Some(EditState {
                        input: TextInputState::new(&format!("{val:.2}")),
                        param_name: name.clone(),
                    });
                }
            } else {
                s.focus_params = true;
            }
        }
        KeyCode::Esc if s.active_tab == 0 => s.focus_params = false,

        // Parameter adjustment.
        KeyCode::Left if s.active_tab == 0 && s.focus_params => {
            let step = if modifiers.contains(KeyModifiers::CONTROL) {
                0.10
            } else if modifiers.contains(KeyModifiers::SHIFT) {
                0.01
            } else {
                0.05
            };
            adjust_param(&mut s.plugins, s.chain_state.selected, s.param_state.selected, -step);
        }
        KeyCode::Right if s.active_tab == 0 && s.focus_params => {
            let step = if modifiers.contains(KeyModifiers::CONTROL) {
                0.10
            } else if modifiers.contains(KeyModifiers::SHIFT) {
                0.01
            } else {
                0.05
            };
            adjust_param(&mut s.plugins, s.chain_state.selected, s.param_state.selected, step);
        }

        // Reorder: Shift+Up/Down moves the selected effect in the chain.
        KeyCode::Up if s.active_tab == 0 && !s.focus_params && modifiers.contains(KeyModifiers::SHIFT) => {
            let sel = s.chain_state.selected;
            // Can swap with the item above if both are effects (never move past the instrument).
            if sel > 0 && !s.plugins[sel].is_instrument && !s.plugins[sel - 1].is_instrument {
                s.plugins.swap(sel, sel - 1);
                s.chain_state.selected = sel - 1;
                s.rebuild_chain_labels();
            }
        }
        KeyCode::Down if s.active_tab == 0 && !s.focus_params && modifiers.contains(KeyModifiers::SHIFT) => {
            let sel = s.chain_state.selected;
            if sel + 1 < s.plugins.len() && !s.plugins[sel].is_instrument {
                s.plugins.swap(sel, sel + 1);
                s.chain_state.selected = sel + 1;
                s.rebuild_chain_labels();
            }
        }

        // Navigation.
        KeyCode::Up => match s.active_tab {
            0 if s.focus_params => s.param_state.up(),
            0 => {
                s.chain_state.up();
                s.sync_param_state();
            }
            3 => s.help_offset = s.help_offset.saturating_sub(1),
            _ => {}
        },
        KeyCode::Down => match s.active_tab {
            0 if s.focus_params => s.param_state.down(),
            0 => {
                s.chain_state.down();
                s.sync_param_state();
            }
            3 => s.help_offset += 1,
            _ => {}
        },
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Mouse handler
// ---------------------------------------------------------------------------

fn handle_mouse(s: &mut State, kind: MouseEventKind, x: u16, y: u16) {
    match kind {
        MouseEventKind::Down(MouseButton::Left) => {
            s.scrollbar_dragging = false;
            s.param_dragging = false;

            if let Some(tab) = TabBar::tab_at(x, y, s.areas.tab, TAB_NAMES, TAB_SEP) {
                s.active_tab = tab;
                return;
            }

            // Action bar click.
            if s.active_tab == 0 {
                let sel = s.chain_state.selected;
                let is_effect = sel < s.plugins.len() && !s.plugins[sel].is_instrument;
                if let Some(key) = action_bar_hit(x, y, s.areas.action_bar, is_effect) {
                    match key {
                        'i' => s.open_selector(SelectorMode::Instrument),
                        'a' => s.open_selector(SelectorMode::Effect),
                        'd' if is_effect => {
                            s.plugins.remove(sel);
                            s.rebuild_chain_labels();
                        }
                        _ => {}
                    }
                    return;
                }
            }

            match s.active_tab {
                0 => {
                    if s.areas.chain_inner.contains((x, y).into()) {
                        if s.chain_state.click_at(y, s.areas.chain_inner) {
                            s.focus_params = false;
                            s.sync_param_state();
                        }
                    } else if s.areas.param_inner.contains((x, y).into()) {
                        s.focus_params = true;
                        s.param_state.click_at(y, s.areas.param_inner);
                        if let Some(val) = bar_value_at(x, s.areas.param_inner) {
                            let pi = s.chain_state.selected;
                            let pa = s.param_state.selected;
                            if let Some(p) =
                                s.plugins.get_mut(pi).and_then(|p| p.params.get_mut(pa))
                            {
                                p.1 = val;
                                s.param_dragging = true;
                            }
                        }
                    }
                }
                3 => {
                    let total = s.help_lines.len();
                    if ScrollView::is_scrollbar_hit(x, s.areas.content, total) {
                        s.help_offset =
                            ScrollView::offset_from_scrollbar(y, s.areas.content, total);
                        s.scrollbar_dragging = true;
                    }
                }
                _ => {}
            }
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            if s.scrollbar_dragging && s.active_tab == 3 {
                let total = s.help_lines.len();
                s.help_offset = ScrollView::offset_from_scrollbar(y, s.areas.content, total);
            } else if s.param_dragging && s.active_tab == 0 {
                if let Some(val) = bar_value_at(x, s.areas.param_inner) {
                    let pi = s.chain_state.selected;
                    let pa = s.param_state.selected;
                    if let Some(p) = s.plugins.get_mut(pi).and_then(|p| p.params.get_mut(pa)) {
                        p.1 = val;
                    }
                }
            }
        }
        MouseEventKind::Up(MouseButton::Left) => {
            s.scrollbar_dragging = false;
            s.param_dragging = false;
        }
        MouseEventKind::ScrollUp => match s.active_tab {
            0 if s.focus_params => {
                for _ in 0..3 { s.param_state.up_nowrap(); }
            }
            0 => {
                for _ in 0..3 { s.chain_state.up_nowrap(); }
                s.sync_param_state();
            }
            3 => s.help_offset = s.help_offset.saturating_sub(3),
            _ => {}
        },
        MouseEventKind::ScrollDown => match s.active_tab {
            0 if s.focus_params => {
                for _ in 0..3 { s.param_state.down_nowrap(); }
            }
            0 => {
                for _ in 0..3 { s.chain_state.down_nowrap(); }
                s.sync_param_state();
            }
            3 => s.help_offset += 3,
            _ => {}
        },
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

fn render(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    s: &mut State,
) -> io::Result<()> {
    let selected_plugin = s.chain_state.selected;
    terminal.draw(|frame| {
        let area = frame.area();
        let [tab_area, content_area, action_area] =
            Layout::vertical([
                Constraint::Length(1),
                Constraint::Fill(1),
                Constraint::Length(1),
            ])
            .areas(area);

        s.areas.tab = tab_area;
        s.areas.content = content_area;
        s.areas.action_bar = action_area;

        // Tab bar.
        let mut tab_bar = TabBar::new(TAB_NAMES, s.active_tab);
        if s.show_clip {
            tab_bar = tab_bar.status(
                "CLIP",
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            );
        }
        frame.render_widget(tab_bar, tab_area);

        match s.active_tab {
            0 => {
                let plugin = &s.plugins[selected_plugin];
                let (ci, pi) = render_session(
                    frame,
                    content_area,
                    &s.chain_labels,
                    &s.chain_state,
                    plugin,
                    &s.param_state,
                    s.focus_params,
                );
                s.areas.chain_inner = ci;
                s.areas.param_inner = pi;

                // Action bar.
                render_action_bar(frame, action_area, &s.plugins, &s.chain_state, s.focus_params);

                if let Some(edit) = &s.editing {
                    render_edit_popup(frame, area, edit);
                }
                if let Some(sel) = &s.selector {
                    render_selector_popup(frame, area, sel);
                }
            }
            1 => {
                let p = Paragraph::new("Piano tab — keyboard input goes here")
                    .style(Style::default().fg(Color::DarkGray));
                frame.render_widget(p, content_area);
            }
            2 => {
                let p = Paragraph::new("Oscilloscope — not yet implemented")
                    .style(Style::default().fg(Color::DarkGray));
                frame.render_widget(p, content_area);
            }
            3 => render_help(frame, content_area, &s.help_lines, s.help_offset),
            _ => {}
        }
    })?;
    Ok(())
}

fn render_session(
    frame: &mut ratatui::Frame,
    area: Rect,
    chain_labels: &[String],
    chain_state: &ListState,
    plugin: &PluginData,
    param_state: &ListState,
    focus_params: bool,
) -> (Rect, Rect) {
    let [left, right] =
        Layout::horizontal([Constraint::Percentage(35), Constraint::Fill(1)]).areas(area);

    // Left pane: chain.
    let left_style = if !focus_params {
        Style::default().fg(Color::White)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let left_block = Block::default()
        .borders(Borders::ALL)
        .border_style(left_style)
        .title(" Chain ");
    let left_inner = left_block.inner(left);
    frame.render_widget(left_block, left);

    let items: Vec<ListItem> = chain_labels.iter().map(|s| ListItem::raw(s)).collect();
    let mut cs = chain_state.clone();
    cs.ensure_visible(left_inner.height as usize);
    let list = List::new(&items, &cs).cursor("", 0);
    frame.render_widget(list, left_inner);

    // Right pane: parameters.
    let right_style = if focus_params {
        Style::default().fg(Color::White)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let right_block = Block::default()
        .borders(Borders::ALL)
        .border_style(right_style)
        .title(format!(" {} ", plugin.name));
    let right_inner = right_block.inner(right);
    frame.render_widget(right_block, right);

    let bar_width = right_inner.width.saturating_sub(20) as usize;
    let param_items: Vec<ListItem> = plugin
        .params
        .iter()
        .map(|(name, val)| {
            let filled = (val * bar_width as f32) as usize;
            let empty = bar_width.saturating_sub(filled);
            let text = format!(
                "{:<12} {}{} {:>5.2}",
                name,
                "▓".repeat(filled),
                "░".repeat(empty),
                val,
            );
            ListItem::raw(Box::leak(text.into_boxed_str()))
        })
        .collect();

    let mut ps = param_state.clone();
    ps.ensure_visible(right_inner.height as usize);
    let param_list = if focus_params {
        List::new(&param_items, &ps)
    } else {
        List::new(&param_items, &ps)
            .selected_style(Style::default().fg(Color::DarkGray))
            .cursor("  ", 2)
    };
    frame.render_widget(param_list, right_inner);

    (left_inner, right_inner)
}

/// Action bar items: (key_label, description, always_visible).
/// Items with always_visible=false only show when an effect is selected.
const ACTIONS: &[(&str, &str, bool)] = &[
    ("i", "instrument", true),
    ("a", "add effect", true),
    ("d", "delete", false),
    ("p", "presets", true),
];

fn render_action_bar(
    frame: &mut ratatui::Frame,
    area: Rect,
    plugins: &[PluginData],
    chain_state: &ListState,
    focus_params: bool,
) {
    if area.height == 0 || area.width == 0 {
        return;
    }

    let sel = chain_state.selected;
    let is_effect = sel < plugins.len() && !plugins[sel].is_instrument;
    let key_style = Style::default()
        .fg(Color::Black)
        .bg(Color::DarkGray)
        .add_modifier(Modifier::BOLD);
    let label_style = Style::default().fg(Color::DarkGray);
    let active_key_style = Style::default()
        .fg(Color::Black)
        .bg(Color::White)
        .add_modifier(Modifier::BOLD);
    let active_label_style = Style::default().fg(Color::White);

    let y = area.y;
    let mut x = area.x;

    for &(key, desc, always) in ACTIONS {
        if !always && !is_effect {
            continue;
        }

        // Dim the actions when params pane is focused (keys go to params).
        let (ks, ls) = if focus_params {
            (key_style, label_style)
        } else {
            (active_key_style, active_label_style)
        };

        if x > area.x {
            // Separator space.
            if x < area.right() {
                x += 1;
            }
        }

        // Key badge: " k "
        let badge = format!(" {key} ");
        for ch in badge.chars() {
            if x >= area.right() {
                break;
            }
            frame.buffer_mut().cell_mut((x, y)).map(|cell| {
                cell.set_char(ch);
                cell.set_style(ks);
            });
            x += 1;
        }

        // Description.
        let desc_with_space = format!(" {desc}");
        for ch in desc_with_space.chars() {
            if x >= area.right() {
                break;
            }
            frame.buffer_mut().cell_mut((x, y)).map(|cell| {
                cell.set_char(ch);
                cell.set_style(ls);
            });
            x += 1;
        }
    }
}

/// Hit-test the action bar. Returns the key char if an action was clicked.
fn action_bar_hit(x: u16, y: u16, area: Rect, is_effect: bool) -> Option<char> {
    if y != area.y || x < area.x || x >= area.right() {
        return None;
    }

    let rel_x = (x - area.x) as usize;
    let mut pos = 0;

    for &(key, desc, always) in ACTIONS {
        if !always && !is_effect {
            continue;
        }

        if pos > 0 {
            pos += 1; // separator
        }

        let badge_len = key.len() + 2; // " k "
        let desc_len = desc.len() + 1; // " desc"
        let total = badge_len + desc_len;

        if rel_x >= pos && rel_x < pos + total {
            return key.chars().next();
        }
        pos += total;
    }
    None
}

fn render_edit_popup(frame: &mut ratatui::Frame, area: Rect, edit: &EditState) {
    let popup = centered_rect(30, 5, area);
    frame.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow))
        .title(format!(" {} ", edit.param_name));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    if inner.height >= 2 {
        let hint =
            Paragraph::new("Range: 0.00 — 1.00").style(Style::default().fg(Color::DarkGray));
        frame.render_widget(hint, Rect::new(inner.x, inner.y, inner.width, 1));

        let label = "Value: ";
        let lw = label.len() as u16;
        frame.render_widget(
            Paragraph::new(label).style(Style::default().fg(Color::White)),
            Rect::new(inner.x, inner.y + 1, lw, 1),
        );
        frame.render_widget(
            TextInput::new(&edit.input),
            Rect::new(inner.x + lw, inner.y + 1, inner.width.saturating_sub(lw), 1),
        );
    }
}

fn render_selector_popup(frame: &mut ratatui::Frame, area: Rect, sel: &SelectorState) {
    let title = match sel.mode {
        SelectorMode::Instrument => " Select Instrument ",
        SelectorMode::Effect => " Select Effect ",
    };

    // Size: 70% width, 60% height, at least 40×10.
    let w = (area.width * 70 / 100).max(40).min(area.width);
    let h = (area.height * 60 / 100).max(10).min(area.height);
    let popup = centered_rect(w, h, area);
    frame.render_widget(Clear, popup);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(title);
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let columns: &[(&str, u16)] = &[
        ("Name", inner.width.saturating_sub(22)),
        ("Format", 8),
        ("Params", 7),
        ("Presets", 7),
    ];

    let fl = FilterList::new(&sel.filter, &sel.items, columns);
    frame.render_widget(fl, inner);
}

fn render_help(frame: &mut ratatui::Frame, area: Rect, lines: &[String], offset: usize) {
    let scroll_lines: Vec<ScrollLine> = lines
        .iter()
        .map(|l| {
            if l.starts_with("  ") {
                ScrollLine::raw(l)
            } else if l.starts_with("---") {
                ScrollLine::styled(l, Style::default().fg(Color::DarkGray))
            } else {
                ScrollLine::styled(
                    l,
                    Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
                )
            }
        })
        .collect();
    let clamped = ScrollView::clamp_offset(offset, scroll_lines.len(), area.height as usize);
    frame.render_widget(ScrollView::new(&scroll_lines, clamped), area);
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn adjust_param(plugins: &mut [PluginData], pi: usize, pa: usize, delta: f32) {
    if let Some(p) = plugins.get_mut(pi).and_then(|p| p.params.get_mut(pa)) {
        p.1 = (p.1 + delta).clamp(0.0, 1.0);
    }
}

fn bar_value_at(x: u16, param_inner: Rect) -> Option<f32> {
    let bar_start = param_inner.x + 15;
    let bar_width = param_inner.width.saturating_sub(20) as u16;
    if bar_width == 0 || x < bar_start || x >= bar_start + bar_width {
        return None;
    }
    Some(((x - bar_start) as f32 / (bar_width - 1).max(1) as f32).clamp(0.0, 1.0))
}

fn build_chain_labels(plugins: &[PluginData]) -> Vec<String> {
    let effect_count = plugins.iter().filter(|p| !p.is_instrument).count();
    let mut effect_idx = 0;
    plugins
        .iter()
        .map(|p| {
            if p.is_instrument {
                format!("♪ {}  [{}]", p.name, p.format)
            } else {
                effect_idx += 1;
                let c = if effect_idx == effect_count { "└─" } else { "├─" };
                format!("{c} fx {}  [{}]", p.name, p.format)
            }
        })
        .collect()
}

fn make_fake_params(count: usize) -> Vec<(String, f32)> {
    let names = [
        "gain", "cutoff", "resonance", "attack", "decay", "sustain", "release",
        "mix", "depth", "rate", "feedback", "width", "tone", "drive", "level",
        "threshold", "ratio", "time", "size", "damping",
    ];
    (0..count)
        .map(|i| {
            let name = names[i % names.len()].to_string();
            let val = ((i as f32 * 0.17 + 0.3) % 1.0 * 100.0).round() / 100.0;
            (name, val)
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Demo data
// ---------------------------------------------------------------------------

fn demo_plugins() -> Vec<PluginData> {
    vec![
        PluginData {
            name: "Helm".into(),
            format: "LV2".into(),
            is_instrument: true,
            params: vec![
                ("cutoff".into(), 0.75),
                ("resonance".into(), 0.25),
                ("attack".into(), 0.05),
                ("decay".into(), 0.30),
                ("sustain".into(), 0.80),
                ("release".into(), 0.40),
                ("osc1 level".into(), 1.0),
                ("osc2 level".into(), 0.60),
                ("lfo rate".into(), 0.15),
                ("lfo depth".into(), 0.50),
            ],
        },
        PluginData {
            name: "ACE Reverb".into(),
            format: "LV2".into(),
            is_instrument: false,
            params: vec![
                ("room size".into(), 0.65),
                ("damping".into(), 0.40),
                ("dry".into(), 0.80),
                ("wet".into(), 0.35),
                ("width".into(), 1.0),
            ],
        },
        PluginData {
            name: "Dragonfly Hall".into(),
            format: "CLAP".into(),
            is_instrument: false,
            params: vec![
                ("size".into(), 0.50),
                ("width".into(), 0.80),
                ("predelay".into(), 0.10),
                ("decay".into(), 0.70),
                ("diffuse".into(), 0.60),
                ("spin".into(), 0.30),
                ("low cut".into(), 0.05),
                ("high cut".into(), 0.90),
            ],
        },
        PluginData {
            name: "TAL-Chorus".into(),
            format: "VST3".into(),
            is_instrument: false,
            params: vec![
                ("dry/wet".into(), 0.50),
                ("rate".into(), 0.35),
                ("depth".into(), 0.60),
            ],
        },
    ]
}

fn demo_catalog() -> Vec<CatalogEntry> {
    vec![
        // Instruments
        CatalogEntry { name: "Helm",               format: "LV2",  is_instrument: true,  params: 10, presets: 256 },
        CatalogEntry { name: "ZynAddSubFX",         format: "LV2",  is_instrument: true,  params: 24, presets: 128 },
        CatalogEntry { name: "Dexed",               format: "CLAP", is_instrument: true,  params: 18, presets: 512 },
        CatalogEntry { name: "Surge XT",            format: "CLAP", is_instrument: true,  params: 42, presets: 1024 },
        CatalogEntry { name: "Vital",               format: "CLAP", is_instrument: true,  params: 36, presets: 384 },
        CatalogEntry { name: "Pianoteq 8",          format: "VST3", is_instrument: true,  params: 28, presets: 96 },
        CatalogEntry { name: "OB-Xd",               format: "VST3", is_instrument: true,  params: 16, presets: 200 },
        CatalogEntry { name: "Sine Oscillator",     format: "Built-in", is_instrument: true, params: 0, presets: 0 },
        // Effects
        CatalogEntry { name: "ACE Reverb",          format: "LV2",  is_instrument: false, params: 5,  presets: 0 },
        CatalogEntry { name: "Calf Compressor",     format: "LV2",  is_instrument: false, params: 8,  presets: 0 },
        CatalogEntry { name: "Calf Equalizer",      format: "LV2",  is_instrument: false, params: 12, presets: 0 },
        CatalogEntry { name: "ZaMaximX2",           format: "LV2",  is_instrument: false, params: 6,  presets: 0 },
        CatalogEntry { name: "Dragonfly Hall",      format: "CLAP", is_instrument: false, params: 8,  presets: 12 },
        CatalogEntry { name: "Dragonfly Room",      format: "CLAP", is_instrument: false, params: 7,  presets: 10 },
        CatalogEntry { name: "ChowTape Model",      format: "CLAP", is_instrument: false, params: 14, presets: 20 },
        CatalogEntry { name: "TAL-Chorus",          format: "VST3", is_instrument: false, params: 3,  presets: 5 },
        CatalogEntry { name: "TAL-Reverb 4",        format: "VST3", is_instrument: false, params: 6,  presets: 8 },
        CatalogEntry { name: "OctaSine Distortion", format: "VST3", is_instrument: false, params: 4,  presets: 0 },
    ]
}
