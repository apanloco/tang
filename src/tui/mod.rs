use std::io;
use std::path::PathBuf;
use std::time::Duration;

use crossbeam_channel::Sender;
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
    MouseButton, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Terminal;

use view::filter_list::{FilterListItem, FilterListState};
use view::list::{ListItem, ListSpan, ListState};
use view::scroll_view::ScrollLine;
use view::text_input::TextInputState;
use view::{FilterList, List, ScrollView, TabBar, TextInput, centered_rect};

use crate::audio;
use crate::plugin;
use crate::plugin::chain::GraphCommand;
use crate::plugin::PluginInfo;

const TAB_NAMES: &[&str] = &["(1) Session", "(2) Piano", "(3) Scope", "(4) Help"];
const TAB_SEP: &str = " │ ";

// ---------------------------------------------------------------------------
// Plugin slot — main-thread mirror of what the audio thread has
// ---------------------------------------------------------------------------

struct PluginSlot {
    name: String,
    format: String,
    #[allow(dead_code)]
    id: String,
    #[allow(dead_code)]
    is_instrument: bool,
    params: Vec<ParamSlot>,
    modulators: Vec<ModulatorSlot>,
}

enum ParamKind {
    Float,
    Enum(Vec<String>),
    Separator,
}

struct ParamSlot {
    name: String,
    index: u32,
    min: f32,
    max: f32,
    default: f32,
    value: f32,
    kind: ParamKind,
}

// ---------------------------------------------------------------------------
// Keyboard/Split tree model
// ---------------------------------------------------------------------------

struct KeyboardNode {
    name: String,
    splits: Vec<SplitNode>,
}

/// Main-thread mirror of pattern state for a split.
struct PatternState {
    bpm: f32,
    length_beats: f32,
    looping: bool,
    base_note: Option<u8>,
    events: Vec<(u64, u8, u8, u8)>, // (frame, status, note, velocity)
    enabled: bool,
    recording: bool,
}

struct SplitNode {
    range: Option<(u8, u8)>,
    transpose: i8,
    instrument: Option<PluginSlot>,
    effects: Vec<PluginSlot>,
    pattern: Option<PatternState>,
}

enum ModSourceSlot {
    Lfo {
        waveform: crate::plugin::chain::LfoWaveform,
        rate: f32,
    },
    Envelope {
        attack: f32,
        decay: f32,
        sustain: f32,
        release: f32,
    },
}

struct ModulatorSlot {
    source: ModSourceSlot,
    targets: Vec<ModTargetSlot>,
}

struct ModTargetSlot {
    param_name: String,
    kind: crate::plugin::chain::ModTargetKind,
    depth: f32,
    #[allow(dead_code)]
    param_min: f32,
    #[allow(dead_code)]
    param_max: f32,
}

/// Addresses a specific node in the keyboard tree.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum TreeAddress {
    Keyboard(usize),
    Split { kb: usize, split: usize },
    Instrument { kb: usize, split: usize },
    Effect { kb: usize, split: usize, index: usize },
    /// The pattern node for a split.
    Pattern { kb: usize, split: usize },
    /// A modulator attached to a plugin.
    /// parent_slot: 0 = instrument, 1..N = effects.
    /// index: index within that plugin's modulator list.
    Modulator { kb: usize, split: usize, parent_slot: usize, index: usize },
}

impl TreeAddress {
    /// Get the (kb, split) indices for this address, if it's a split or plugin node.
    fn kb_split(&self) -> Option<(usize, usize)> {
        match *self {
            TreeAddress::Keyboard(_) => None,
            TreeAddress::Split { kb, split } => Some((kb, split)),
            TreeAddress::Instrument { kb, split } => Some((kb, split)),
            TreeAddress::Effect { kb, split, .. } => Some((kb, split)),
            TreeAddress::Pattern { kb, split } => Some((kb, split)),
            TreeAddress::Modulator { kb, split, .. } => Some((kb, split)),
        }
    }

    /// Get the audio thread slot index (0 = instrument, 1..N = effects).
    fn slot(&self) -> usize {
        match *self {
            TreeAddress::Keyboard(_) => 0,
            TreeAddress::Split { .. } => 0,
            TreeAddress::Instrument { .. } => 0,
            TreeAddress::Effect { index, .. } => index + 1,
            TreeAddress::Pattern { .. } => 0,
            TreeAddress::Modulator { parent_slot, .. } => parent_slot,
        }
    }
}

struct TreeEntry {
    label: String,
    address: TreeAddress,
    #[allow(dead_code)]
    color: Color,
    #[allow(dead_code)]
    indent: usize,
}

// ---------------------------------------------------------------------------
// Action bar
// ---------------------------------------------------------------------------

/// Build the action bar items for the current tree selection.
fn actions_for(addr: Option<&TreeAddress>) -> Vec<(&'static str, &'static str)> {
    match addr {
        Some(TreeAddress::Keyboard(_)) => vec![
            ("a", "add split"),
        ],
        Some(TreeAddress::Split { .. }) => vec![
            ("a", "add instrument"),
            ("r", "record"),
            ("d", "delete"),
        ],
        Some(TreeAddress::Instrument { .. }) => vec![
            ("a", "add effect"),
            ("m", "modulate"),
            ("d", "delete"),
            ("p", "presets"),
        ],
        Some(TreeAddress::Effect { .. }) => vec![
            ("a", "add effect"),
            ("m", "modulate"),
            ("d", "delete"),
            ("p", "presets"),
        ],
        Some(TreeAddress::Pattern { .. }) => vec![
            ("r", "record"),
            ("d", "clear"),
        ],
        Some(TreeAddress::Modulator { .. }) => vec![
            ("t", "add target"),
            ("d", "delete"),
        ],
        None => vec![],
    }
}

// ---------------------------------------------------------------------------
// Popup state types
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
    param_min: f32,
    param_max: f32,
}

/// One entry in the target selector popup.
struct TargetEntry {
    label: String,
    kind: crate::plugin::chain::ModTargetKind,
    param_min: f32,
    param_max: f32,
    base_value: f32,
}

struct TargetSelectorState {
    filter: FilterListState,
    items: Vec<FilterListItem>,
    entries: Vec<TargetEntry>,
    kb: usize,
    split: usize,
    parent_slot: usize,
    mod_index: usize,
}

struct RangeEditState {
    input: TextInputState,
    kb: usize,
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
    keyboards: Vec<KeyboardNode>,
    tree_entries: Vec<TreeEntry>,
    chain_state: ListState,
    param_state: ListState,
    focus_params: bool,
    help_lines: Vec<String>,
    help_offset: usize,
    scrollbar_dragging: bool,
    param_dragging: bool,
    param_scrollbar_dragging: bool,
    editing: Option<EditState>,
    range_edit: Option<RangeEditState>,
    selector: Option<SelectorState>,
    target_selector: Option<TargetSelectorState>,
    catalog: Vec<PluginInfo>,
    areas: Areas,
    quit: bool,
    session_path: Option<PathBuf>,
    dirty: bool,
    // Parameter filter (search bar in param pane).
    param_filter_input: TextInputState,
    param_filtering: bool,
    param_filtered: Vec<usize>,
    // Connections to the audio engine.
    cmd_tx: Sender<GraphCommand>,
    #[allow(dead_code)]
    midi_tx: Sender<audio::MidiEvent>,
    runtime: plugin::Runtime,
    sample_rate: f32,
    max_block_size: usize,
    // Pattern state.
    global_bpm: f32,
    bpm_editing: Option<EditState>,
    pattern_rx: crossbeam_channel::Receiver<crate::plugin::chain::PatternNotification>,
}

impl State {
    fn rebuild_tree(&mut self) {
        self.tree_entries = build_tree_entries(&self.keyboards);
        self.chain_state.set_len(self.tree_entries.len());
        self.sync_param_state();
    }

    fn sync_param_state(&mut self) {
        // Clear filter when selected node changes.
        self.param_filter_input = TextInputState::new("");
        self.param_filtering = false;

        let sel = self.chain_state.selected;
        if sel < self.tree_entries.len() {
            let addr = &self.tree_entries[sel].address;
            let param_len = match *addr {
                TreeAddress::Pattern { kb, split } => {
                    self.keyboards.get(kb)
                        .and_then(|k| k.splits.get(split))
                        .and_then(|s| s.pattern.as_ref())
                        .map_or(0, |p| {
                            let mut n = 3; // Length + Enabled + Loop
                            if !p.events.is_empty() { n += 1; } // Notes info
                            n
                        })
                }
                TreeAddress::Modulator { kb, split, parent_slot, index } => {
                    let plugin = if parent_slot == 0 {
                        self.keyboards.get(kb).and_then(|k| k.splits.get(split)).and_then(|s| s.instrument.as_ref())
                    } else {
                        self.keyboards.get(kb).and_then(|k| k.splits.get(split)).and_then(|s| s.effects.get(parent_slot - 1))
                    };
                    plugin.and_then(|p| p.modulators.get(index))
                        .map_or(0, |m| {
                            let fixed = match &m.source {
                                ModSourceSlot::Lfo { .. } => 3,      // Type + Waveform + Rate
                                ModSourceSlot::Envelope { .. } => 5,  // Type + A + D + S + R
                            };
                            // +1 for the "Targets" separator row
                            fixed + 1 + m.targets.len()
                        })
                }
                TreeAddress::Split { .. } => 1, // Transpose
                _ => self.plugin_at(addr).map_or(0, |p| p.params.len()),
            };
            self.param_state.set_len(param_len);
        }
        self.recompute_param_filter();
    }

    /// Recompute the filtered parameter indices based on the current filter text.
    /// Only applies to Instrument/Effect nodes (not modulators).
    fn recompute_param_filter(&mut self) {
        let sel = self.chain_state.selected;
        let is_plugin = sel < self.tree_entries.len()
            && matches!(
                self.tree_entries[sel].address,
                TreeAddress::Instrument { .. } | TreeAddress::Effect { .. }
            );
        if !is_plugin {
            self.param_filtered.clear();
            return;
        }
        let addr = &self.tree_entries[sel].address;
        let params = match self.plugin_at(addr) {
            Some(p) => &p.params,
            None => {
                self.param_filtered.clear();
                return;
            }
        };
        let filter = self.param_filter_input.value.to_lowercase();
        if filter.is_empty() {
            self.param_filtered = (0..params.len()).collect();
        } else {
            self.param_filtered = params
                .iter()
                .enumerate()
                .filter(|(_, p)| p.name.to_lowercase().contains(&filter))
                .map(|(i, _)| i)
                .collect();
        }
        self.param_state.set_len(self.param_filtered.len());
    }

    /// Map the current param_state.selected (index into filtered list) to the
    /// real param index. Returns None if no valid mapping.
    fn real_param_index(&self) -> Option<usize> {
        let sel = self.chain_state.selected;
        if sel >= self.tree_entries.len() {
            return None;
        }
        let is_plugin = matches!(
            self.tree_entries[sel].address,
            TreeAddress::Instrument { .. } | TreeAddress::Effect { .. }
        );
        if is_plugin && !self.param_filtered.is_empty() {
            self.param_filtered.get(self.param_state.selected).copied()
        } else {
            Some(self.param_state.selected)
        }
    }

    /// Returns (min, max) for the currently selected parameter, handling both
    /// plugin params (via filter mapping) and modulator pseudo-params.
    fn selected_param_range(&self) -> Option<(f32, f32)> {
        let sel = self.chain_state.selected;
        if sel >= self.tree_entries.len() {
            return None;
        }
        let addr = self.tree_entries[sel].address;
        match addr {
            TreeAddress::Modulator { kb, split, parent_slot, index } => {
                let plugin = if parent_slot == 0 {
                    self.keyboards.get(kb).and_then(|k| k.splits.get(split)).and_then(|s| s.instrument.as_ref())
                } else {
                    self.keyboards.get(kb).and_then(|k| k.splits.get(split)).and_then(|s| s.effects.get(parent_slot - 1))
                };
                let m = plugin?.modulators.get(index)?;
                let pa = self.param_state.selected;
                if pa == 0 {
                    return None; // Type enum
                }
                match &m.source {
                    ModSourceSlot::Lfo { .. } => match pa {
                        1 => None, // Waveform enum
                        2 => Some((0.01, 50.0)),
                        3 => None, // Separator
                        _ => m.targets.get(pa - 4).map(|_| (0.0f32, 1.0f32)),
                    },
                    ModSourceSlot::Envelope { .. } => match pa {
                        1 => Some((0.001, 10.0)),
                        2 => Some((0.001, 10.0)),
                        3 => Some((0.0, 1.0)),
                        4 => Some((0.001, 10.0)),
                        5 => None, // Separator
                        _ => m.targets.get(pa - 6).map(|_| (0.0f32, 1.0f32)),
                    },
                }
            }
            _ => {
                let pa = self.real_param_index()?;
                let param = self.plugin_at(&addr)?.params.get(pa)?;
                Some((param.min, param.max))
            }
        }
    }

    /// Returns true if the currently selected parameter is an enum (e.g. Type, Waveform).
    fn selected_param_is_enum(&self) -> bool {
        let sel = self.chain_state.selected;
        if sel >= self.tree_entries.len() {
            return false;
        }
        let addr = self.tree_entries[sel].address;
        match addr {
            TreeAddress::Modulator { kb, split, parent_slot, index } => {
                let plugin = if parent_slot == 0 {
                    self.keyboards.get(kb).and_then(|k| k.splits.get(split)).and_then(|s| s.instrument.as_ref())
                } else {
                    self.keyboards.get(kb).and_then(|k| k.splits.get(split)).and_then(|s| s.effects.get(parent_slot - 1))
                };
                let Some(m) = plugin.and_then(|p| p.modulators.get(index)) else { return false };
                let pa = self.param_state.selected;
                match &m.source {
                    ModSourceSlot::Lfo { .. } => pa == 0 || pa == 1, // Type, Waveform
                    ModSourceSlot::Envelope { .. } => pa == 0,       // Type
                }
            }
            _ => {
                if let Some(pa) = self.real_param_index() {
                    self.plugin_at(&addr)
                        .and_then(|p| p.params.get(pa))
                        .is_some_and(|p| matches!(p.kind, ParamKind::Enum(_)))
                } else {
                    false
                }
            }
        }
    }

    /// Get a reference to the PluginSlot at the given tree address.
    fn plugin_at(&self, addr: &TreeAddress) -> Option<&PluginSlot> {
        match *addr {
            TreeAddress::Keyboard(_) | TreeAddress::Split { .. } | TreeAddress::Pattern { .. } | TreeAddress::Modulator { .. } => None,
            TreeAddress::Instrument { kb, split } => {
                self.keyboards.get(kb)?.splits.get(split)?.instrument.as_ref()
            }
            TreeAddress::Effect { kb, split, index } => {
                self.keyboards.get(kb)?.splits.get(split)?.effects.get(index)
            }
        }
    }

    /// Get a mutable reference to the PluginSlot at the given tree address.
    fn plugin_at_mut(&mut self, addr: &TreeAddress) -> Option<&mut PluginSlot> {
        match *addr {
            TreeAddress::Keyboard(_) | TreeAddress::Split { .. } | TreeAddress::Pattern { .. } | TreeAddress::Modulator { .. } => None,
            TreeAddress::Instrument { kb, split } => {
                self.keyboards.get_mut(kb)?.splits.get_mut(split)?.instrument.as_mut()
            }
            TreeAddress::Effect { kb, split, index } => {
                self.keyboards.get_mut(kb)?.splits.get_mut(split)?.effects.get_mut(index)
            }
        }
    }

    fn selected_address(&self) -> Option<&TreeAddress> {
        self.tree_entries.get(self.chain_state.selected).map(|e| &e.address)
    }

    fn open_selector(&mut self, mode: SelectorMode) {
        log::info!("open_selector: mode={:?}", mode);
        let items: Vec<FilterListItem> = self
            .catalog
            .iter()
            .enumerate()
            .filter(|(_, e)| match mode {
                SelectorMode::Instrument => e.is_instrument,
                SelectorMode::Effect => !e.is_instrument,
            })
            .map(|(i, e)| {
                let fmt = format_from_id(&e.id);
                FilterListItem {
                    cells: vec![
                        e.name.clone(),
                        fmt,
                        e.param_count.to_string(),
                        e.preset_count.to_string(),
                    ],
                    index: i,
                }
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
            None => {
                log::warn!("confirm_selector: no item selected");
                return;
            }
        };
        let entry = &self.catalog[chosen];

        // Determine which keyboard/split to operate on from current selection.
        let (kb, split) = self.selected_kb_split().unwrap_or((0, 0));

        // Load the real plugin.
        let source = &entry.id;
        log::info!("Loading plugin '{}' (id={}) into kb={} split={}", entry.name, source, kb, split);
        let loaded = match plugin::load(source, self.sample_rate, self.max_block_size, &self.runtime) {
            Ok(p) => p,
            Err(e) => {
                log::error!("Failed to load plugin '{}': {e}", entry.name);
                return;
            }
        };

        let params: Vec<ParamSlot> = loaded
            .parameters()
            .into_iter()
            .filter(|p| !p.name.starts_with("(locked)"))
            .map(|p| ParamSlot {
                name: p.name,
                index: p.index,
                min: p.min,
                max: p.max,
                default: p.default,
                value: p.default,
                kind: ParamKind::Float,
            })
            .collect();

        let slot = PluginSlot {
            name: loaded.name().to_string(),
            format: format_from_id(source),
            id: source.to_string(),
            is_instrument: loaded.is_instrument(),
            params,
            modulators: vec![],
        };

        match sel.mode {
            SelectorMode::Instrument => {
                let inst_buf = (0..loaded.audio_output_count())
                    .map(|_| Vec::new())
                    .collect();
                let _ = self.cmd_tx.send(GraphCommand::SwapInstrument {
                    kb,
                    split,
                    instrument: loaded,
                    inst_buf,
                    remapper: None,
                });
                if let Some(sp) = self.keyboards.get_mut(kb).and_then(|k| k.splits.get_mut(split)) {
                    sp.instrument = Some(slot);
                }
            }
            SelectorMode::Effect => {
                if let Some(sp) = self.keyboards.get_mut(kb).and_then(|k| k.splits.get_mut(split)) {
                    let insert_at = sp.effects.len();
                    let _ = self.cmd_tx.send(GraphCommand::InsertEffect {
                        kb,
                        split,
                        index: insert_at,
                        effect: loaded,
                        mix: 1.0,
                    });
                    sp.effects.push(slot);
                }
            }
        }

        self.dirty = true;
        self.rebuild_tree();
    }

    /// Get the (kb, split) for the currently selected tree entry.
    fn selected_kb_split(&self) -> Option<(usize, usize)> {
        let addr = self.selected_address()?;
        match *addr {
            TreeAddress::Keyboard(kb) => {
                // Select the first split of this keyboard, if any.
                if self.keyboards.get(kb).is_some_and(|k| !k.splits.is_empty()) {
                    Some((kb, 0))
                } else {
                    None
                }
            }
            TreeAddress::Split { kb, split }
            | TreeAddress::Instrument { kb, split }
            | TreeAddress::Effect { kb, split, .. }
            | TreeAddress::Pattern { kb, split }
            | TreeAddress::Modulator { kb, split, .. } => Some((kb, split)),
        }
    }

    fn open_target_selector(&mut self, kb: usize, split: usize, parent_slot: usize, mod_index: usize) {
        let sp = match self.keyboards.get(kb).and_then(|k| k.splits.get(split)) {
            Some(sp) => sp,
            None => return,
        };

        // Get the parent plugin's params.
        let plugin = if parent_slot == 0 {
            sp.instrument.as_ref()
        } else {
            sp.effects.get(parent_slot - 1)
        };
        let plugin = match plugin {
            Some(p) => p,
            None => return,
        };

        let mut entries = Vec::new();
        let mut items = Vec::new();

        // Plugin parameters.
        for p in &plugin.params {
            let idx = entries.len();
            entries.push(TargetEntry {
                label: p.name.clone(),
                kind: crate::plugin::chain::ModTargetKind::PluginParam { param_index: p.index },
                param_min: p.min,
                param_max: p.max,
                base_value: p.default,
            });
            items.push(FilterListItem {
                cells: vec![p.name.clone()],
                index: idx,
            });
        }

        // Sibling modulator parameters (cross-mod).
        for (sib_idx, sib) in plugin.modulators.iter().enumerate() {
            if sib_idx == mod_index {
                continue; // Skip self.
            }
            let prefix = format!("Mod {} ", sib_idx);
            match &sib.source {
                ModSourceSlot::Lfo { rate, .. } => {
                    let idx = entries.len();
                    entries.push(TargetEntry {
                        label: format!("{prefix}rate"),
                        kind: crate::plugin::chain::ModTargetKind::ModulatorRate { mod_index: sib_idx },
                        param_min: 0.01,
                        param_max: 50.0,
                        base_value: *rate,
                    });
                    items.push(FilterListItem { cells: vec![format!("{prefix}rate")], index: idx });
                }
                ModSourceSlot::Envelope { attack, decay, sustain, release } => {
                    for (field_name, kind, min, max, base) in [
                        ("attack", crate::plugin::chain::ModTargetKind::ModulatorAttack { mod_index: sib_idx }, 0.001f32, 10.0f32, *attack),
                        ("decay", crate::plugin::chain::ModTargetKind::ModulatorDecay { mod_index: sib_idx }, 0.001, 10.0, *decay),
                        ("sustain", crate::plugin::chain::ModTargetKind::ModulatorSustain { mod_index: sib_idx }, 0.0, 1.0, *sustain),
                        ("release", crate::plugin::chain::ModTargetKind::ModulatorRelease { mod_index: sib_idx }, 0.001, 10.0, *release),
                    ] {
                        let idx = entries.len();
                        let label = format!("{prefix}{field_name}");
                        entries.push(TargetEntry {
                            label: label.clone(),
                            kind,
                            param_min: min,
                            param_max: max,
                            base_value: base,
                        });
                        items.push(FilterListItem { cells: vec![label], index: idx });
                    }
                }
            }
            // Sibling modulator's target depths.
            for (tgt_idx, tgt) in sib.targets.iter().enumerate() {
                let idx = entries.len();
                let label = format!("{prefix}{} depth", tgt.param_name);
                entries.push(TargetEntry {
                    label: label.clone(),
                    kind: crate::plugin::chain::ModTargetKind::ModulatorDepth { mod_index: sib_idx, target_index: tgt_idx },
                    param_min: 0.0,
                    param_max: 1.0,
                    base_value: tgt.depth,
                });
                items.push(FilterListItem { cells: vec![label], index: idx });
            }
        }

        let mut filter = FilterListState::new();
        filter.apply_filter(&items);

        self.target_selector = Some(TargetSelectorState {
            filter,
            items,
            entries,
            kb,
            split,
            parent_slot,
            mod_index,
        });
    }

    fn confirm_target_selector(&mut self) {
        let ts = match self.target_selector.take() {
            Some(s) => s,
            None => return,
        };
        let chosen = match ts.filter.selected_item(&ts.items) {
            Some(item) => item.index,
            None => return,
        };
        let entry = &ts.entries[chosen];

        let target = crate::plugin::chain::ModTarget {
            kind: entry.kind.clone(),
            depth: 0.5,
            base_value: entry.base_value,
            param_min: entry.param_min,
            param_max: entry.param_max,
        };
        let _ = self.cmd_tx.send(GraphCommand::AddModTarget {
            kb: ts.kb,
            split: ts.split,
            parent_slot: ts.parent_slot,
            mod_index: ts.mod_index,
            target,
        });

        let plugin = if ts.parent_slot == 0 {
            self.keyboards.get_mut(ts.kb)
                .and_then(|k| k.splits.get_mut(ts.split))
                .and_then(|s| s.instrument.as_mut())
        } else {
            self.keyboards.get_mut(ts.kb)
                .and_then(|k| k.splits.get_mut(ts.split))
                .and_then(|s| s.effects.get_mut(ts.parent_slot - 1))
        };
        if let Some(m) = plugin
            .and_then(|p| p.modulators.get_mut(ts.mod_index))
        {
            m.targets.push(ModTargetSlot {
                param_name: entry.label.clone(),
                kind: entry.kind.clone(),
                depth: 0.5,
                param_min: entry.param_min,
                param_max: entry.param_max,
            });
        }
        self.dirty = true;
        self.rebuild_tree();
    }

    fn adjust_param(&mut self, delta: f32) {
        let sel = self.chain_state.selected;
        if sel >= self.tree_entries.len() {
            return;
        }
        let addr = self.tree_entries[sel].address;
        let (kb, split) = match addr.kb_split() {
            Some(ks) => ks,
            None => return,
        };

        // Handle split params (transpose).
        if let TreeAddress::Split { .. } = addr {
            self.adjust_split_param(kb, split, delta);
            return;
        }

        // Handle pattern params separately.
        if let TreeAddress::Pattern { .. } = addr {
            let pa = self.param_state.selected;
            self.adjust_pattern_param(kb, split, pa, delta);
            return;
        }

        // Handle modulator params separately.
        if let TreeAddress::Modulator { parent_slot, index, .. } = addr {
            let pa = self.param_state.selected;
            self.adjust_modulator_param(kb, split, parent_slot, index, pa, delta);
            return;
        }

        let pa = match self.real_param_index() {
            Some(i) => i,
            None => return,
        };
        let slot = addr.slot();
        if let Some(param) = self.plugin_at_mut(&addr).and_then(|p| p.params.get_mut(pa)) {
            param.value = (param.value + delta).clamp(param.min, param.max);
            let new_value = param.value;
            let idx = param.index;
            let _ = self.cmd_tx.send(GraphCommand::SetParameter {
                kb,
                split,
                slot,
                param_index: idx,
                value: new_value,
            });
            self.dirty = true;
        }
    }

    fn adjust_split_param(&mut self, kb: usize, split: usize, delta: f32) {
        let sp = match self.keyboards.get_mut(kb).and_then(|k| k.splits.get_mut(split)) {
            Some(s) => s,
            None => return,
        };
        // Ctrl modifier gives large delta (range*0.10 ≈ 9.6) → octave jump (±12).
        // Normal/Shift gives smaller delta → single semitone (±1).
        let step: i16 = if delta.abs() >= 5.0 {
            if delta > 0.0 { 12 } else { -12 }
        } else if delta > 0.0 {
            1
        } else {
            -1
        };
        sp.transpose = (sp.transpose as i16 + step).clamp(-48, 48) as i8;
        let _ = self.cmd_tx.send(GraphCommand::SetTranspose {
            kb, split, semitones: sp.transpose,
        });
        self.dirty = true;
        self.rebuild_tree();
    }

    fn adjust_pattern_param(&mut self, kb: usize, split: usize, pa: usize, delta: f32) {
        let pat = match self.keyboards.get_mut(kb)
            .and_then(|k| k.splits.get_mut(split))
            .and_then(|s| s.pattern.as_mut())
        {
            Some(p) => p,
            None => return,
        };
        match pa {
            0 => {
                // Length (beats)
                pat.length_beats = (pat.length_beats + delta).clamp(1.0, 32.0);
                let _ = self.cmd_tx.send(GraphCommand::SetPatternLength {
                    kb, split, beats: pat.length_beats,
                });
                self.dirty = true;
                self.rebuild_tree();
            }
            1 => {
                // Enabled (enum toggle)
                pat.enabled = !pat.enabled;
                let _ = self.cmd_tx.send(GraphCommand::SetPatternEnabled {
                    kb, split, enabled: pat.enabled,
                });
                self.dirty = true;
                self.rebuild_tree();
            }
            2 => {
                // Loop (enum toggle)
                pat.looping = !pat.looping;
                let _ = self.cmd_tx.send(GraphCommand::SetPatternLooping {
                    kb, split, looping: pat.looping,
                });
                self.dirty = true;
            }
            _ => {} // Notes row is informational
        }
    }

    fn adjust_modulator_param(&mut self, kb: usize, split: usize, parent_slot: usize, mod_index: usize, pa: usize, delta: f32) {
        let plugin = if parent_slot == 0 {
            self.keyboards.get_mut(kb).and_then(|k| k.splits.get_mut(split)).and_then(|s| s.instrument.as_mut())
        } else {
            self.keyboards.get_mut(kb).and_then(|k| k.splits.get_mut(split)).and_then(|s| s.effects.get_mut(parent_slot - 1))
        };
        let m = match plugin.and_then(|p| p.modulators.get_mut(mod_index)) {
            Some(m) => m,
            None => return,
        };
        if pa == 0 {
            // Type (enum) — switch between LFO and Envelope.
            let new_source = match &m.source {
                ModSourceSlot::Lfo { .. } => ModSourceSlot::Envelope {
                    attack: 0.01, decay: 0.3, sustain: 0.7, release: 0.5,
                },
                ModSourceSlot::Envelope { .. } => ModSourceSlot::Lfo {
                    waveform: crate::plugin::chain::LfoWaveform::Sine,
                    rate: 1.0,
                },
            };
            let graph_source = mod_source_slot_to_graph(&new_source);
            m.source = new_source;
            let _ = self.cmd_tx.send(GraphCommand::SetModulatorSource {
                kb, split, parent_slot, mod_index,
                source: graph_source,
            });
            self.param_state.selected = 0;
            self.rebuild_tree();
        } else {
            match &mut m.source {
                ModSourceSlot::Lfo { waveform, rate } => {
                    if pa == 1 {
                        // Waveform (enum).
                        *waveform = if delta > 0.0 { waveform.next() } else { waveform.prev() };
                        let _ = self.cmd_tx.send(GraphCommand::SetModulatorWaveform {
                            kb, split, parent_slot, mod_index,
                            waveform: *waveform,
                        });
                        self.rebuild_tree();
                    } else if pa == 2 {
                        // Rate.
                        *rate = (*rate + delta).clamp(0.01, 50.0);
                        let _ = self.cmd_tx.send(GraphCommand::SetModulatorRate {
                            kb, split, parent_slot, mod_index,
                            rate: *rate,
                        });
                        self.rebuild_tree();
                    } else if pa == 3 {
                        // Separator row — no-op.
                    } else if let Some(t) = m.targets.get_mut(pa - 4) {
                        t.depth = (t.depth + delta).clamp(0.0, 1.0);
                        let _ = self.cmd_tx.send(GraphCommand::SetModTargetDepth {
                            kb, split, parent_slot, mod_index,
                            target_index: pa - 4,
                            depth: t.depth,
                        });
                    }
                }
                ModSourceSlot::Envelope { attack, decay, sustain, release } => {
                    match pa {
                        1 => {
                            *attack = (*attack + delta).clamp(0.001, 10.0);
                        }
                        2 => {
                            *decay = (*decay + delta).clamp(0.001, 10.0);
                        }
                        3 => {
                            *sustain = (*sustain + delta).clamp(0.0, 1.0);
                        }
                        4 => {
                            *release = (*release + delta).clamp(0.001, 10.0);
                        }
                        5 => {
                            // Separator row — no-op.
                        }
                        _ => {
                            let target_idx = pa - 6;
                            if let Some(t) = m.targets.get_mut(target_idx) {
                                t.depth = (t.depth + delta).clamp(0.0, 1.0);
                                let _ = self.cmd_tx.send(GraphCommand::SetModTargetDepth {
                                    kb, split, parent_slot, mod_index,
                                    target_index: target_idx,
                                    depth: t.depth,
                                });
                            }
                        }
                    }
                    if (1..=4).contains(&pa) {
                        let _ = self.cmd_tx.send(GraphCommand::SetModulatorEnvelopeParam {
                            kb, split, parent_slot, mod_index,
                            attack: *attack, decay: *decay, sustain: *sustain, release: *release,
                        });
                    }
                }
            }
        }
        self.dirty = true;
    }

    fn set_param_value(&mut self, value: f32) {
        let sel = self.chain_state.selected;
        if sel >= self.tree_entries.len() {
            return;
        }
        let addr = self.tree_entries[sel].address;
        let (kb, split) = match addr.kb_split() {
            Some(ks) => ks,
            None => return,
        };

        // Handle split params (transpose).
        if let TreeAddress::Split { .. } = addr {
            let clamped = (value as i8).clamp(-48, 48);
            if let Some(sp) = self.keyboards.get_mut(kb).and_then(|k| k.splits.get_mut(split)) {
                sp.transpose = clamped;
                let _ = self.cmd_tx.send(GraphCommand::SetTranspose {
                    kb, split, semitones: clamped,
                });
                self.dirty = true;
                self.rebuild_tree();
            }
            return;
        }

        // Handle pattern params.
        if let TreeAddress::Pattern { .. } = addr {
            let pa = self.param_state.selected;
            if pa == 0 {
                // Length (beats) — set directly.
                let clamped = value.clamp(1.0, 32.0);
                if let Some(pat) = self.keyboards.get_mut(kb)
                    .and_then(|k| k.splits.get_mut(split))
                    .and_then(|s| s.pattern.as_mut())
                {
                    pat.length_beats = clamped;
                    let _ = self.cmd_tx.send(GraphCommand::SetPatternLength {
                        kb, split, beats: clamped,
                    });
                    self.dirty = true;
                    self.rebuild_tree();
                }
            }
            return;
        }

        // Handle modulator params.
        if let TreeAddress::Modulator { parent_slot, index, .. } = addr {
            let pa = self.param_state.selected;
            self.set_modulator_param_value(kb, split, parent_slot, index, pa, value);
            return;
        }

        let pa = match self.real_param_index() {
            Some(i) => i,
            None => return,
        };
        let slot = addr.slot();
        if let Some(param) = self.plugin_at_mut(&addr).and_then(|p| p.params.get_mut(pa)) {
            param.value = value.clamp(param.min, param.max);
            let new_value = param.value;
            let idx = param.index;
            let _ = self.cmd_tx.send(GraphCommand::SetParameter {
                kb,
                split,
                slot,
                param_index: idx,
                value: new_value,
            });
            self.dirty = true;
        }
    }

    fn set_modulator_param_value(&mut self, kb: usize, split: usize, parent_slot: usize, mod_index: usize, pa: usize, value: f32) {
        let plugin = if parent_slot == 0 {
            self.keyboards.get_mut(kb).and_then(|k| k.splits.get_mut(split)).and_then(|s| s.instrument.as_mut())
        } else {
            self.keyboards.get_mut(kb).and_then(|k| k.splits.get_mut(split)).and_then(|s| s.effects.get_mut(parent_slot - 1))
        };
        let m = match plugin.and_then(|p| p.modulators.get_mut(mod_index)) {
            Some(m) => m,
            None => return,
        };
        if pa == 0 {
            // Type enum — not settable via numeric value entry, skip.
            return;
        }
        match &mut m.source {
            ModSourceSlot::Lfo { waveform: _, rate } => {
                if pa == 1 {
                    // Waveform enum — not settable via numeric value entry.
                    return;
                } else if pa == 2 {
                    *rate = value.clamp(0.01, 50.0);
                    let _ = self.cmd_tx.send(GraphCommand::SetModulatorRate {
                        kb, split, parent_slot, mod_index, rate: *rate,
                    });
                    self.rebuild_tree();
                } else if pa == 3 {
                    // Separator — not settable.
                    return;
                } else if let Some(t) = m.targets.get_mut(pa - 4) {
                    t.depth = value.clamp(0.0, 1.0);
                    let _ = self.cmd_tx.send(GraphCommand::SetModTargetDepth {
                        kb, split, parent_slot, mod_index,
                        target_index: pa - 4,
                        depth: t.depth,
                    });
                }
            }
            ModSourceSlot::Envelope { attack, decay, sustain, release } => {
                match pa {
                    1 => *attack = value.clamp(0.001, 10.0),
                    2 => *decay = value.clamp(0.001, 10.0),
                    3 => *sustain = value.clamp(0.0, 1.0),
                    4 => *release = value.clamp(0.001, 10.0),
                    5 => return, // Separator — not settable.
                    _ => {
                        let target_idx = pa - 6;
                        if let Some(t) = m.targets.get_mut(target_idx) {
                            t.depth = value.clamp(0.0, 1.0);
                            let _ = self.cmd_tx.send(GraphCommand::SetModTargetDepth {
                                kb, split, parent_slot, mod_index,
                                target_index: target_idx,
                                depth: t.depth,
                            });
                        }
                        self.dirty = true;
                        return;
                    }
                }
                let _ = self.cmd_tx.send(GraphCommand::SetModulatorEnvelopeParam {
                    kb, split, parent_slot, mod_index,
                    attack: *attack, decay: *decay, sustain: *sustain, release: *release,
                });
            }
        }
        self.dirty = true;
    }

    fn save_session(&mut self) {
        let path = match &self.session_path {
            Some(p) => p.clone(),
            None => {
                log::warn!("No session path — cannot save");
                return;
            }
        };

        // Ensure parent directory exists (e.g. ~/.config/tang/sessions/).
        if let Some(parent) = path.parent() {
            if !parent.exists() {
                if let Err(e) = std::fs::create_dir_all(parent) {
                    log::error!("Failed to create directory {}: {e}", parent.display());
                    return;
                }
            }
        }

        let mods_to_save = |mods: &[ModulatorSlot]| -> Vec<crate::session::SaveModulator> {
            mods.iter()
                .map(|m| {
                    let source = match &m.source {
                        ModSourceSlot::Lfo { waveform, rate } => {
                            crate::session::SaveModSource::Lfo {
                                waveform: waveform.name().to_string(),
                                rate: *rate,
                            }
                        }
                        ModSourceSlot::Envelope { attack, decay, sustain, release } => {
                            crate::session::SaveModSource::Envelope {
                                attack: *attack,
                                decay: *decay,
                                sustain: *sustain,
                                release: *release,
                            }
                        }
                    };
                    crate::session::SaveModulator {
                        source,
                        targets: m
                            .targets
                            .iter()
                            .map(|t| crate::session::SaveModTarget {
                                kind: t.kind.clone(),
                                label: t.param_name.clone(),
                                depth: t.depth,
                            })
                            .collect(),
                    }
                })
                .collect()
        };
        let save_keyboards: Vec<crate::session::SaveKeyboard> = self
            .keyboards
            .iter()
            .map(|kb| crate::session::SaveKeyboard {
                name: kb.name.clone(),
                splits: kb
                    .splits
                    .iter()
                    .map(|sp| crate::session::SaveSplit {
                        range: sp.range,
                        transpose: sp.transpose,
                        instrument: sp.instrument.as_ref().map(|inst| {
                            crate::session::SaveInstrument {
                                plugin: inst.id.clone(),
                                volume: 1.0, // TODO: track volume in PluginSlot
                                params: inst
                                    .params
                                    .iter()
                                    .filter(|p| (p.value - p.default).abs() > f32::EPSILON)
                                    .map(|p| (p.name.clone(), p.value))
                                    .collect(),
                                modulators: mods_to_save(&inst.modulators),
                            }
                        }),
                        effects: sp
                            .effects
                            .iter()
                            .map(|fx| crate::session::SaveEffect {
                                plugin: fx.id.clone(),
                                mix: 1.0, // TODO: track mix in PluginSlot
                                params: fx
                                    .params
                                    .iter()
                                    .filter(|p| (p.value - p.default).abs() > f32::EPSILON)
                                    .map(|p| (p.name.clone(), p.value))
                                    .collect(),
                                modulators: mods_to_save(&fx.modulators),
                            })
                            .collect(),
                        pattern: sp.pattern.as_ref().map(|p| crate::session::SavePattern {
                            bpm: p.bpm,
                            length_beats: p.length_beats,
                            looping: p.looping,
                            base_note: p.base_note,
                            events: p.events.clone(),
                            enabled: p.enabled,
                        }),
                    })
                    .collect(),
            })
            .collect();

        match crate::session::save(&path, &save_keyboards) {
            Ok(()) => {
                self.dirty = false;
                log::info!("Session saved to {}", path.display());
            }
            Err(e) => {
                log::error!("Failed to save session: {e}");
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Information about a loaded keyboard for the TUI.
pub struct LoadedKeyboard {
    pub name: String,
    pub splits: Vec<LoadedSplit>,
}

pub struct LoadedSplit {
    pub range: Option<(u8, u8)>,
    pub transpose: i8,
    pub instrument: Option<LoadedPlugin>,
    pub effects: Vec<LoadedPlugin>,
    pub pattern: Option<LoadedPattern>,
}

/// Pattern data loaded from session config, passed to the TUI.
pub struct LoadedPattern {
    pub bpm: f32,
    pub length_beats: f32,
    pub looping: bool,
    pub base_note: Option<u8>,
    pub events: Vec<(u64, u8, u8, u8)>, // (frame, status, note, velocity)
    pub enabled: bool,
}

pub enum LoadedModSource {
    Lfo {
        waveform: crate::plugin::chain::LfoWaveform,
        rate: f32,
    },
    Envelope {
        attack: f32,
        decay: f32,
        sustain: f32,
        release: f32,
    },
}

pub struct LoadedModulator {
    pub source: LoadedModSource,
    pub targets: Vec<LoadedModTarget>,
}

pub struct LoadedModTarget {
    pub param_name: String,
    pub param_index: u32,
    pub depth: f32,
    pub param_min: f32,
    pub param_max: f32,
}

/// Information about a loaded plugin slot, passed from play() to the TUI.
pub struct LoadedPlugin {
    pub name: String,
    pub id: String,
    pub is_instrument: bool,
    pub params: Vec<plugin::ParameterInfo>,
    pub param_values: Vec<f32>,
    pub modulators: Vec<LoadedModulator>,
}

#[allow(clippy::too_many_arguments)]
pub fn run(
    loaded_keyboards: Vec<LoadedKeyboard>,
    cmd_tx: Sender<GraphCommand>,
    midi_tx: Sender<audio::MidiEvent>,
    runtime: plugin::Runtime,
    sample_rate: f32,
    max_block_size: usize,
    session_path: Option<PathBuf>,
    pattern_rx: crossbeam_channel::Receiver<crate::plugin::chain::PatternNotification>,
) -> anyhow::Result<()> {
    // Build catalog from enumerate.
    let catalog = build_catalog();

    // Convert loaded keyboards into KeyboardNodes.
    let keyboards: Vec<KeyboardNode> = loaded_keyboards
        .into_iter()
        .map(|lk| {
            let splits = lk.splits.into_iter().map(|ls| {
                let instrument = ls.instrument.map(to_plugin_slot);
                let effects = ls.effects.into_iter().map(to_plugin_slot).collect();
                let pattern = ls.pattern.map(|p| PatternState {
                    bpm: p.bpm,
                    length_beats: p.length_beats,
                    looping: p.looping,
                    base_note: p.base_note,
                    events: p.events,
                    enabled: p.enabled,
                    recording: false,
                });
                SplitNode {
                    range: ls.range,
                    transpose: ls.transpose,
                    instrument,
                    effects,
                    pattern,
                }
            }).collect();
            KeyboardNode {
                name: lk.name,
                splits,
            }
        })
        .collect();

    let tree_entries = build_tree_entries(&keyboards);
    let param_len = if let Some(first) = tree_entries.first() {
        match first.address {
            TreeAddress::Keyboard(kb) => {
                keyboards.get(kb)
                    .and_then(|k| k.splits.first())
                    .and_then(|s| s.instrument.as_ref())
                    .map_or(0, |p| p.params.len())
            }
            _ => 0,
        }
    } else {
        0
    };

    let help_lines = build_help_lines();

    // Determine initial BPM from loaded patterns (if any).
    let initial_bpm = keyboards.iter()
        .flat_map(|kb| &kb.splits)
        .filter_map(|sp| sp.pattern.as_ref())
        .map(|p| p.bpm)
        .next()
        .unwrap_or(120.0);

    // Send transpose and pattern data to audio graph for loaded splits.
    for (kb_idx, kb) in keyboards.iter().enumerate() {
        for (sp_idx, sp) in kb.splits.iter().enumerate() {
            if sp.transpose != 0 {
                let _ = cmd_tx.send(GraphCommand::SetTranspose {
                    kb: kb_idx,
                    split: sp_idx,
                    semitones: sp.transpose,
                });
            }
            if let Some(ref p) = sp.pattern {
                let pattern_events: Vec<crate::plugin::chain::PatternEvent> = p.events.iter().map(|&(frame, status, note, vel)| {
                    crate::plugin::chain::PatternEvent {
                        frame,
                        status,
                        note,
                        velocity: vel,
                    }
                }).collect();
                let beats_per_sec = p.bpm / 60.0;
                let length_samples = (p.length_beats / beats_per_sec * sample_rate) as u64;
                let _ = cmd_tx.send(GraphCommand::SetPattern {
                    kb: kb_idx,
                    split: sp_idx,
                    pattern: crate::plugin::chain::Pattern {
                        events: pattern_events,
                        length_samples,
                    },
                    base_note: p.base_note,
                });
                let _ = cmd_tx.send(GraphCommand::SetPatternEnabled {
                    kb: kb_idx,
                    split: sp_idx,
                    enabled: p.enabled,
                });
                if !p.looping {
                    let _ = cmd_tx.send(GraphCommand::SetPatternLooping {
                        kb: kb_idx,
                        split: sp_idx,
                        looping: false,
                    });
                }
            }
        }
    }

    let mut s = State {
        active_tab: 0,
        chain_state: ListState::new(tree_entries.len()),
        param_state: ListState::new(param_len),
        tree_entries,
        keyboards,
        focus_params: false,
        help_lines,
        help_offset: 0,
        scrollbar_dragging: false,
        param_dragging: false,
        param_scrollbar_dragging: false,
        editing: None,
        range_edit: None,
        selector: None,
        target_selector: None,
        catalog,
        areas: Areas::default(),
        quit: false,
        session_path,
        dirty: false,
        param_filter_input: TextInputState::new(""),
        param_filtering: false,
        param_filtered: (0..param_len).collect(),
        cmd_tx,
        midi_tx,
        runtime,
        sample_rate,
        max_block_size,
        global_bpm: initial_bpm,
        bpm_editing: None,
        pattern_rx,
    };

    // Set up terminal.
    crossterm::terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // When stderr is redirected (e.g. `tang 2> debug.log`), keep logging enabled
    // so plugin load errors are visible. When stderr is a terminal, suppress
    // logging to avoid corrupting the alternate screen.
    let prev_log_level = log::max_level();
    if std::io::IsTerminal::is_terminal(&std::io::stderr()) {
        log::set_max_level(log::LevelFilter::Off);
    }

    let result = event_loop(&mut terminal, &mut s);

    log::set_max_level(prev_log_level);

    execute!(
        terminal.backend_mut(),
        DisableMouseCapture,
        LeaveAlternateScreen
    )?;
    crossterm::terminal::disable_raw_mode()?;

    result.map_err(Into::into)
}

fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    s: &mut State,
) -> io::Result<()> {
    loop {
        // Drain pattern recording completion notifications.
        while let Ok(notif) = s.pattern_rx.try_recv() {
            if let Some(sp) = s.keyboards.get_mut(notif.kb).and_then(|k| k.splits.get_mut(notif.split)) {
                sp.pattern = Some(PatternState {
                    bpm: s.global_bpm,
                    length_beats: notif.length_beats,
                    looping: notif.looping,
                    base_note: notif.base_note,
                    events: notif.events,
                    enabled: notif.enabled,
                    recording: false,
                });
                s.rebuild_tree();
            }
        }

        render(terminal, s)?;
        if s.quit {
            break;
        }

        // Poll with timeout so we wake up to drain pattern notifications
        // even when there's no user input.
        if !event::poll(Duration::from_millis(100))? {
            continue;
        }
        let ev = event::read()?;
        process_event(s, ev);
        while event::poll(Duration::ZERO)? {
            process_event(s, event::read()?);
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Event processing
// ---------------------------------------------------------------------------

fn process_event(s: &mut State, ev: Event) {
    match ev {
        Event::Key(key) if key.kind == KeyEventKind::Press => {
            if s.selector.is_some() {
                handle_selector_key(s, key.code);
            } else if s.target_selector.is_some() {
                handle_target_selector_key(s, key.code);
            } else if s.bpm_editing.is_some() {
                handle_bpm_edit_key(s, key.code);
            } else if s.editing.is_some() {
                handle_edit_key(s, key.code);
            } else if s.range_edit.is_some() {
                handle_range_edit_key(s, key.code);
            } else if s.param_filtering {
                handle_param_filter_key(s, key.code);
            } else {
                handle_key(s, key.code, key.modifiers);
            }
        }
        Event::Mouse(mouse) => {
            if s.selector.is_some() || s.target_selector.is_some() || s.editing.is_some() || s.range_edit.is_some() || s.bpm_editing.is_some() {
                if let MouseEventKind::Down(MouseButton::Left) = mouse.kind {
                    s.selector = None;
                    s.target_selector = None;
                    s.editing = None;
                    s.range_edit = None;
                    s.bpm_editing = None;
                }
                return;
            }
            handle_mouse(s, mouse.kind, mouse.column, mouse.row);
        }
        _ => {}
    }
}

fn handle_selector_key(s: &mut State, code: KeyCode) {
    let sel = s.selector.as_mut().unwrap();
    match code {
        KeyCode::Esc => s.selector = None,
        KeyCode::Enter => s.confirm_selector(),
        KeyCode::Up => {
            sel.filter.list.up();
            sel.filter.list.ensure_visible(20);
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

fn handle_target_selector_key(s: &mut State, code: KeyCode) {
    let ts = s.target_selector.as_mut().unwrap();
    match code {
        KeyCode::Esc => s.target_selector = None,
        KeyCode::Enter => s.confirm_target_selector(),
        KeyCode::Up => {
            ts.filter.list.up();
            ts.filter.list.ensure_visible(20);
        }
        KeyCode::Down => {
            ts.filter.list.down();
            ts.filter.list.ensure_visible(20);
        }
        KeyCode::Backspace => {
            ts.filter.input.backspace();
            ts.filter.apply_filter(&ts.items);
        }
        KeyCode::Char(ch) => {
            ts.filter.input.insert(ch);
            ts.filter.apply_filter(&ts.items);
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
                s.set_param_value(val);
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

fn handle_range_edit_key(s: &mut State, code: KeyCode) {
    let re = s.range_edit.as_mut().unwrap();
    match code {
        KeyCode::Esc => s.range_edit = None,
        KeyCode::Enter => {
            let input = re.input.value.trim().to_string();
            let kb = re.kb;
            let range = if input.is_empty() {
                None
            } else {
                match crate::session::parse_range(&input) {
                    Ok(r) => Some(r),
                    Err(_) => return, // keep popup open on parse error
                }
            };
            let _ = s.cmd_tx.send(GraphCommand::AddSplit { kb, range });
            s.keyboards[kb].splits.push(SplitNode {
                range,
                transpose: 0,
                instrument: None,
                effects: vec![],
                pattern: None,
            });
            s.dirty = true;
            s.rebuild_tree();
            s.range_edit = None;
        }
        KeyCode::Backspace => re.input.backspace(),
        KeyCode::Delete => re.input.delete(),
        KeyCode::Left => re.input.move_left(),
        KeyCode::Right => re.input.move_right(),
        KeyCode::Home => re.input.home(),
        KeyCode::End => re.input.end(),
        KeyCode::Char(ch) => re.input.insert(ch),
        _ => {}
    }
}

fn handle_bpm_edit_key(s: &mut State, code: KeyCode) {
    let edit = s.bpm_editing.as_mut().unwrap();
    match code {
        KeyCode::Esc => s.bpm_editing = None,
        KeyCode::Enter => {
            if let Ok(val) = edit.input.value.trim().parse::<f32>() {
                let bpm = val.clamp(edit.param_min, edit.param_max);
                s.global_bpm = bpm;
                let _ = s.cmd_tx.send(GraphCommand::SetGlobalBpm { bpm });
                // Update all pattern states
                for kb in &mut s.keyboards {
                    for sp in &mut kb.splits {
                        if let Some(ref mut p) = sp.pattern {
                            p.bpm = bpm;
                        }
                    }
                }
            }
            s.bpm_editing = None;
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

fn handle_param_filter_key(s: &mut State, code: KeyCode) {
    match code {
        KeyCode::Esc => {
            // Cancel filter, clear text.
            s.param_filtering = false;
            s.param_filter_input = TextInputState::new("");
            s.recompute_param_filter();
        }
        KeyCode::Enter => {
            // Accept filter, keep text active, stop typing.
            s.param_filtering = false;
        }
        KeyCode::Backspace => {
            s.param_filter_input.backspace();
            s.recompute_param_filter();
        }
        KeyCode::Delete => {
            s.param_filter_input.delete();
            s.recompute_param_filter();
        }
        KeyCode::Left => s.param_filter_input.move_left(),
        KeyCode::Right => s.param_filter_input.move_right(),
        KeyCode::Home => s.param_filter_input.home(),
        KeyCode::End => s.param_filter_input.end(),
        KeyCode::Up => s.param_state.up(),
        KeyCode::Down => s.param_state.down(),
        KeyCode::PageUp => s.param_state.page_up(20),
        KeyCode::PageDown => s.param_state.page_down(20),
        KeyCode::Char(ch) => {
            s.param_filter_input.insert(ch);
            s.recompute_param_filter();
        }
        _ => {}
    }
}

fn handle_key(s: &mut State, code: KeyCode, modifiers: KeyModifiers) {
    match code {
        KeyCode::Char('q') | KeyCode::Char('c')
            if modifiers.contains(KeyModifiers::CONTROL) =>
        {
            s.quit = true;
        }
        KeyCode::Char('s') if modifiers.contains(KeyModifiers::CONTROL) => {
            s.save_session();
        }
        KeyCode::Char('1') => s.active_tab = 0,
        KeyCode::Char('2') => s.active_tab = 1,
        KeyCode::Char('3') => s.active_tab = 2,
        KeyCode::Char('4') => s.active_tab = 3,
        KeyCode::Tab => s.active_tab = (s.active_tab + 1) % TAB_NAMES.len(),
        KeyCode::BackTab => s.active_tab = (s.active_tab + TAB_NAMES.len() - 1) % TAB_NAMES.len(),

        // Session: contextual add (chain focus only).
        // Keyboard → add split, Split → add instrument, Instrument/Effect → add effect.
        KeyCode::Char('a') if s.active_tab == 0 && !s.focus_params => {
            match s.selected_address().copied() {
                Some(TreeAddress::Keyboard(kb)) => {
                    s.range_edit = Some(RangeEditState {
                        input: TextInputState::new(""),
                        kb,
                    });
                }
                Some(TreeAddress::Split { .. }) => {
                    s.open_selector(SelectorMode::Instrument);
                }
                Some(TreeAddress::Instrument { .. } | TreeAddress::Effect { .. }) => {
                    s.open_selector(SelectorMode::Effect);
                }
                Some(TreeAddress::Pattern { .. }) => {}
                Some(TreeAddress::Modulator { .. }) => {}
                None => {}
            }
        }

        // 'm' — add LFO modulator to the selected plugin (instrument or effect).
        KeyCode::Char('m') if s.active_tab == 0 && !s.focus_params => {
            if let Some(addr) = s.selected_address().copied() {
                let parent_slot = match addr {
                    TreeAddress::Instrument { .. } => Some(0usize),
                    TreeAddress::Effect { index, .. } => Some(index + 1),
                    _ => None,
                };
                if let (Some(parent_slot), Some((kb, split))) = (parent_slot, addr.kb_split()) {
                    let plugin = if parent_slot == 0 {
                        s.keyboards.get_mut(kb).and_then(|k| k.splits.get_mut(split)).and_then(|sp| sp.instrument.as_mut())
                    } else {
                        s.keyboards.get_mut(kb).and_then(|k| k.splits.get_mut(split)).and_then(|sp| sp.effects.get_mut(parent_slot - 1))
                    };
                    if let Some(plugin) = plugin {
                        let mod_index = plugin.modulators.len();
                        let _ = s.cmd_tx.send(GraphCommand::InsertModulator {
                            kb,
                            split,
                            parent_slot,
                            index: mod_index,
                            source: crate::plugin::chain::ModSource::Lfo {
                                waveform: crate::plugin::chain::LfoWaveform::Sine,
                                rate: 1.0,
                                phase: 0.0,
                            },
                        });
                        plugin.modulators.push(ModulatorSlot {
                            source: ModSourceSlot::Lfo {
                                waveform: crate::plugin::chain::LfoWaveform::Sine,
                                rate: 1.0,
                            },
                            targets: vec![],
                        });
                        s.dirty = true;
                        s.rebuild_tree();
                    }
                }
            }
        }

        // 't' — add modulation target (when modulator selected).
        KeyCode::Char('t') if s.active_tab == 0 && !s.focus_params => {
            if let Some(TreeAddress::Modulator { kb, split, parent_slot, index }) = s.selected_address().copied() {
                s.open_target_selector(kb, split, parent_slot, index);
            }
        }

        // 'r' — toggle pattern recording (on Pattern or Split node).
        KeyCode::Char('r') if s.active_tab == 0 && !s.focus_params => {
            let target = match s.selected_address().copied() {
                Some(TreeAddress::Pattern { kb, split }) => Some((kb, split)),
                Some(TreeAddress::Split { kb, split }) => Some((kb, split)),
                _ => None,
            };
            if let Some((kb, split)) = target {
                let sp = &mut s.keyboards[kb].splits[split];
                let currently_recording = sp.pattern.as_ref().is_some_and(|p| p.recording);
                if currently_recording {
                    // Stop recording
                    let _ = s.cmd_tx.send(GraphCommand::SetPatternRecording { kb, split, recording: false });
                    if let Some(ref mut p) = sp.pattern {
                        p.recording = false;
                    }
                } else {
                    // Start recording: create pattern state if needed
                    if sp.pattern.is_none() {
                        sp.pattern = Some(PatternState {
                            bpm: s.global_bpm,
                            length_beats: 4.0,
                            looping: true,
                            base_note: None,
                            events: vec![],
                            enabled: false,
                            recording: false,
                        });
                    }
                    // Send BPM and length first
                    let _ = s.cmd_tx.send(GraphCommand::SetGlobalBpm { bpm: s.global_bpm });
                    let _ = s.cmd_tx.send(GraphCommand::SetPatternLength {
                        kb, split,
                        beats: sp.pattern.as_ref().unwrap().length_beats,
                    });
                    let _ = s.cmd_tx.send(GraphCommand::SetPatternRecording { kb, split, recording: true });
                    sp.pattern.as_mut().unwrap().recording = true;
                }
                s.dirty = true;
                s.rebuild_tree();
            }
        }

        // 'b' — edit BPM.
        KeyCode::Char('b') if s.active_tab == 0 && !s.focus_params => {
            s.bpm_editing = Some(EditState {
                input: TextInputState::new(&format!("{:.0}", s.global_bpm)),
                param_name: "BPM".to_string(),
                param_min: 20.0,
                param_max: 300.0,
            });
        }

        KeyCode::Char('d') if s.active_tab == 0 && !s.focus_params => {
            let sel = s.chain_state.selected;
            if sel < s.tree_entries.len() {
                let addr = s.tree_entries[sel].address;
                match addr {
                    TreeAddress::Effect { kb, split, index } => {
                        let _ = s.cmd_tx.send(GraphCommand::RemoveEffect { kb, split, index });
                        if let Some(sp) = s.keyboards.get_mut(kb).and_then(|k| k.splits.get_mut(split)) {
                            if index < sp.effects.len() {
                                sp.effects.remove(index);
                            }
                        }
                        s.dirty = true;
                        s.rebuild_tree();
                    }
                    TreeAddress::Instrument { kb, split } => {
                        if let Some(k) = s.keyboards.get_mut(kb) {
                            if k.splits[split].instrument.is_some() {
                                let _ = s.cmd_tx.send(GraphCommand::RemoveInstrument { kb, split });
                                k.splits[split].instrument = None;
                                s.dirty = true;
                                s.rebuild_tree();
                            }
                        }
                    }
                    TreeAddress::Split { kb, split } => {
                        // Remove the entire split (but keep at least one per keyboard).
                        if let Some(k) = s.keyboards.get_mut(kb) {
                            if k.splits.len() > 1 {
                                let _ = s.cmd_tx.send(GraphCommand::RemoveSplit { kb, split });
                                k.splits.remove(split);
                                s.dirty = true;
                                s.rebuild_tree();
                            }
                        }
                    }
                    TreeAddress::Pattern { kb, split } => {
                        let _ = s.cmd_tx.send(GraphCommand::ClearPattern { kb, split });
                        s.keyboards[kb].splits[split].pattern = None;
                        s.dirty = true;
                        s.rebuild_tree();
                    }
                    TreeAddress::Modulator { kb, split, parent_slot, index } => {
                        let _ = s.cmd_tx.send(GraphCommand::RemoveModulator { kb, split, parent_slot, index });
                        let plugin = if parent_slot == 0 {
                            s.keyboards.get_mut(kb).and_then(|k| k.splits.get_mut(split)).and_then(|sp| sp.instrument.as_mut())
                        } else {
                            s.keyboards.get_mut(kb).and_then(|k| k.splits.get_mut(split)).and_then(|sp| sp.effects.get_mut(parent_slot - 1))
                        };
                        if let Some(p) = plugin {
                            if index < p.modulators.len() {
                                p.modulators.remove(index);
                                // Clean up cross-mod targets in siblings.
                                fixup_tui_cross_mod_after_remove(&mut p.modulators, index);
                            }
                        }
                        s.dirty = true;
                        s.rebuild_tree();
                    }
                    TreeAddress::Keyboard(_) => {}
                }
            }
        }

        // Enter: focus params or open value editor.
        KeyCode::Enter if s.active_tab == 0 => {
            if s.focus_params {
                let sel = s.chain_state.selected;
                let pa = s.param_state.selected;
                if sel < s.tree_entries.len() {
                    let addr = s.tree_entries[sel].address;
                    match addr {
                        TreeAddress::Modulator { kb, split, parent_slot, index } => {
                            // pa 0 = Type (enum, skip). pa 1+ depends on source type.
                            if pa == 0 {
                                // Type is an enum — no edit popup, use Left/Right.
                            } else {
                                let plugin = if parent_slot == 0 {
                                    s.keyboards.get(kb).and_then(|k| k.splits.get(split)).and_then(|sp| sp.instrument.as_ref())
                                } else {
                                    s.keyboards.get(kb).and_then(|k| k.splits.get(split)).and_then(|sp| sp.effects.get(parent_slot - 1))
                                };
                                if let Some(m) = plugin.and_then(|p| p.modulators.get(index)) {
                                    match &m.source {
                                        ModSourceSlot::Lfo { waveform: _, rate } => {
                                            if pa == 1 {
                                                // Waveform enum — skip.
                                            } else if pa == 2 {
                                                s.editing = Some(EditState {
                                                    input: TextInputState::new(&format!("{:.2}", rate)),
                                                    param_name: "Rate (Hz)".to_string(),
                                                    param_min: 0.01,
                                                    param_max: 50.0,
                                                });
                                            } else if pa == 3 {
                                                // Separator — skip.
                                            } else if let Some(t) = m.targets.get(pa - 4) {
                                                s.editing = Some(EditState {
                                                    input: TextInputState::new(&format!("{:.2}", t.depth)),
                                                    param_name: format!("{} depth", t.param_name),
                                                    param_min: 0.0,
                                                    param_max: 1.0,
                                                });
                                            }
                                        }
                                        ModSourceSlot::Envelope { attack, decay, sustain, release } => {
                                            let edit = match pa {
                                                1 => Some((*attack, "Attack (s)".to_string(), 0.001f32, 10.0f32)),
                                                2 => Some((*decay, "Decay (s)".to_string(), 0.001, 10.0)),
                                                3 => Some((*sustain, "Sustain".to_string(), 0.0, 1.0)),
                                                4 => Some((*release, "Release (s)".to_string(), 0.001, 10.0)),
                                                5 => None, // Separator — skip.
                                                _ => m.targets.get(pa - 6).map(|t| {
                                                    (t.depth, format!("{} depth", t.param_name), 0.0f32, 1.0f32)
                                                }),
                                            };
                                            if let Some((val, pname, min, max)) = edit {
                                                s.editing = Some(EditState {
                                                    input: TextInputState::new(&format!("{:.3}", val)),
                                                    param_name: pname,
                                                    param_min: min,
                                                    param_max: max,
                                                });
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        TreeAddress::Pattern { kb, split } => {
                            let pat = s.keyboards.get(kb)
                                .and_then(|k| k.splits.get(split))
                                .and_then(|sp| sp.pattern.as_ref());
                            if let Some(p) = pat {
                                match pa {
                                    0 => {
                                        s.editing = Some(EditState {
                                            input: TextInputState::new(&format!("{:.0}", p.length_beats)),
                                            param_name: "Length (beats)".to_string(),
                                            param_min: 1.0,
                                            param_max: 32.0,
                                        });
                                    }
                                    1 => {} // Enabled is enum — use Left/Right
                                    2 => {} // Loop is enum — use Left/Right
                                    _ => {} // Notes is info
                                }
                            }
                        }
                        _ => {
                            let real_pa = s.real_param_index().unwrap_or(pa);
                            if let Some(param) = s.plugin_at(&addr).and_then(|p| p.params.get(real_pa)) {
                                s.editing = Some(EditState {
                                    input: TextInputState::new(&format!("{:.2}", param.value)),
                                    param_name: param.name.clone(),
                                    param_min: param.min,
                                    param_max: param.max,
                                });
                            }
                        }
                    }
                }
            } else {
                // Only enter param focus if selection is a plugin or modulator (not keyboard header)
                let sel = s.chain_state.selected;
                if sel < s.tree_entries.len() {
                    match s.tree_entries[sel].address {
                        TreeAddress::Keyboard(_) => {}
                        _ => s.focus_params = true,
                    }
                }
            }
        }
        KeyCode::Esc if s.active_tab == 0 => {
            if s.param_filtering {
                // Cancel filter input, clear filter text.
                s.param_filtering = false;
                s.param_filter_input = TextInputState::new("");
                s.recompute_param_filter();
            } else if s.focus_params && !s.param_filter_input.value.is_empty() {
                // Clear active filter first.
                s.param_filter_input = TextInputState::new("");
                s.recompute_param_filter();
            } else {
                s.focus_params = false;
            }
        }

        // '/' — activate parameter filter (only for plugin nodes, not modulators).
        KeyCode::Char('/') if s.active_tab == 0 && s.focus_params && !s.param_filtering => {
            let sel = s.chain_state.selected;
            if sel < s.tree_entries.len() {
                let is_plugin = matches!(
                    s.tree_entries[sel].address,
                    TreeAddress::Instrument { .. } | TreeAddress::Effect { .. }
                );
                if is_plugin {
                    s.param_filtering = true;
                }
            }
        }

        // Parameter adjustment.
        KeyCode::Left if s.active_tab == 0 && s.focus_params && !s.param_filtering => {
            let step = param_step(s, modifiers);
            s.adjust_param(-step);
        }
        KeyCode::Right if s.active_tab == 0 && s.focus_params && !s.param_filtering => {
            let step = param_step(s, modifiers);
            s.adjust_param(step);
        }

        // Reorder effects / move instruments between splits.
        KeyCode::Up
            if s.active_tab == 0
                && !s.focus_params
                && modifiers.contains(KeyModifiers::SHIFT) =>
        {
            let sel = s.chain_state.selected;
            if sel < s.tree_entries.len() {
                match s.tree_entries[sel].address {
                    TreeAddress::Effect { kb, split, index } => {
                        if index > 0 {
                            let _ = s.cmd_tx.send(GraphCommand::ReorderEffect {
                                kb,
                                split,
                                from: index,
                                to: index - 1,
                            });
                            if let Some(sp) = s.keyboards.get_mut(kb).and_then(|k| k.splits.get_mut(split)) {
                                if index < sp.effects.len() {
                                    sp.effects.swap(index, index - 1);
                                }
                            }
                            s.dirty = true;
                            s.rebuild_tree();
                            if s.chain_state.selected > 0 {
                                s.chain_state.selected -= 1;
                            }
                        }
                    }
                    TreeAddress::Instrument { kb, split } if split > 0 => {
                        let _ = s.cmd_tx.send(GraphCommand::SwapInstruments {
                            kb,
                            split_a: split,
                            split_b: split - 1,
                        });
                        if let Some(k) = s.keyboards.get_mut(kb) {
                            if split < k.splits.len() {
                                let a_inst = k.splits[split].instrument.take();
                                let b_inst = k.splits[split - 1].instrument.take();
                                k.splits[split].instrument = b_inst;
                                k.splits[split - 1].instrument = a_inst;
                            }
                        }
                        s.dirty = true;
                        s.rebuild_tree();
                        // Move cursor to follow the instrument to its new split.
                        let new_addr = TreeAddress::Instrument { kb, split: split - 1 };
                        if let Some(pos) = s.tree_entries.iter().position(|e| e.address == new_addr) {
                            s.chain_state.selected = pos;
                        }
                        s.sync_param_state();
                    }
                    TreeAddress::Pattern { kb, split } if split > 0 => {
                        let _ = s.cmd_tx.send(GraphCommand::SwapPatterns {
                            kb,
                            split_a: split,
                            split_b: split - 1,
                        });
                        if let Some(k) = s.keyboards.get_mut(kb) {
                            if split < k.splits.len() {
                                let a_pat = k.splits[split].pattern.take();
                                let b_pat = k.splits[split - 1].pattern.take();
                                k.splits[split].pattern = b_pat;
                                k.splits[split - 1].pattern = a_pat;
                            }
                        }
                        s.dirty = true;
                        s.rebuild_tree();
                        let new_addr = TreeAddress::Pattern { kb, split: split - 1 };
                        if let Some(pos) = s.tree_entries.iter().position(|e| e.address == new_addr) {
                            s.chain_state.selected = pos;
                        }
                        s.sync_param_state();
                    }
                    _ => {}
                }
            }
        }
        KeyCode::Down
            if s.active_tab == 0
                && !s.focus_params
                && modifiers.contains(KeyModifiers::SHIFT) =>
        {
            let sel = s.chain_state.selected;
            if sel < s.tree_entries.len() {
                match s.tree_entries[sel].address {
                    TreeAddress::Effect { kb, split, index } => {
                        let effect_count = s.keyboards.get(kb)
                            .and_then(|k| k.splits.get(split))
                            .map_or(0, |sp| sp.effects.len());
                        if index + 1 < effect_count {
                            let _ = s.cmd_tx.send(GraphCommand::ReorderEffect {
                                kb,
                                split,
                                from: index,
                                to: index + 1,
                            });
                            if let Some(sp) = s.keyboards.get_mut(kb).and_then(|k| k.splits.get_mut(split)) {
                                sp.effects.swap(index, index + 1);
                            }
                            s.dirty = true;
                            s.rebuild_tree();
                            s.chain_state.selected += 1;
                        }
                    }
                    TreeAddress::Instrument { kb, split } => {
                        let split_count = s.keyboards.get(kb).map_or(0, |k| k.splits.len());
                        if split + 1 < split_count {
                            let _ = s.cmd_tx.send(GraphCommand::SwapInstruments {
                                kb,
                                split_a: split,
                                split_b: split + 1,
                            });
                            if let Some(k) = s.keyboards.get_mut(kb) {
                                let a_inst = k.splits[split].instrument.take();
                                let b_inst = k.splits[split + 1].instrument.take();
                                k.splits[split].instrument = b_inst;
                                k.splits[split + 1].instrument = a_inst;
                            }
                            s.dirty = true;
                            s.rebuild_tree();
                            let new_addr = TreeAddress::Instrument { kb, split: split + 1 };
                            if let Some(pos) = s.tree_entries.iter().position(|e| e.address == new_addr) {
                                s.chain_state.selected = pos;
                            }
                            s.sync_param_state();
                        }
                    }
                    TreeAddress::Pattern { kb, split } => {
                        let split_count = s.keyboards.get(kb).map_or(0, |k| k.splits.len());
                        if split + 1 < split_count {
                            let _ = s.cmd_tx.send(GraphCommand::SwapPatterns {
                                kb,
                                split_a: split,
                                split_b: split + 1,
                            });
                            if let Some(k) = s.keyboards.get_mut(kb) {
                                let a_pat = k.splits[split].pattern.take();
                                let b_pat = k.splits[split + 1].pattern.take();
                                k.splits[split].pattern = b_pat;
                                k.splits[split + 1].pattern = a_pat;
                            }
                            s.dirty = true;
                            s.rebuild_tree();
                            let new_addr = TreeAddress::Pattern { kb, split: split + 1 };
                            if let Some(pos) = s.tree_entries.iter().position(|e| e.address == new_addr) {
                                s.chain_state.selected = pos;
                            }
                            s.sync_param_state();
                        }
                    }
                    _ => {}
                }
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
        KeyCode::PageUp => match s.active_tab {
            0 if s.focus_params => s.param_state.page_up(20),
            0 => {
                s.chain_state.page_up(20);
                s.sync_param_state();
            }
            3 => s.help_offset = s.help_offset.saturating_sub(20),
            _ => {}
        },
        KeyCode::PageDown => match s.active_tab {
            0 if s.focus_params => s.param_state.page_down(20),
            0 => {
                s.chain_state.page_down(20);
                s.sync_param_state();
            }
            3 => s.help_offset += 20,
            _ => {}
        },
        _ => {}
    }
}

fn handle_mouse(s: &mut State, kind: MouseEventKind, x: u16, y: u16) {
    match kind {
        MouseEventKind::Down(MouseButton::Left) => {
            s.scrollbar_dragging = false;
            s.param_dragging = false;
            s.param_scrollbar_dragging = false;

            if let Some(tab) = TabBar::tab_at(x, y, s.areas.tab, TAB_NAMES, TAB_SEP) {
                s.active_tab = tab;
                return;
            }

            // Action bar.
            if s.active_tab == 0 {
                let sel = s.chain_state.selected;
                let addr = s.tree_entries.get(sel).map(|e| &e.address);
                let actions = actions_for(addr);
                if let Some(key) = action_bar_hit(x, y, s.areas.action_bar, &actions) {
                    handle_key(s, KeyCode::Char(key), KeyModifiers::NONE);
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
                        // Check scrollbar first.
                        if s.param_state.is_scrollbar_hit(x, s.areas.param_inner) {
                            s.param_state.select_from_scrollbar(y, s.areas.param_inner);
                            s.param_scrollbar_dragging = true;
                        } else {
                            s.param_state.click_at(y, s.areas.param_inner);
                            if s.selected_param_is_enum() {
                                // Enum param: click left half → prev, right half → next.
                                // Enum text starts after cursor(2) + name(25) = 27.
                                let enum_start = s.areas.param_inner.x + 27;
                                let enum_end = s.areas.param_inner.right();
                                if x >= enum_start && x < enum_end {
                                    let mid = enum_start + (enum_end - enum_start) / 2;
                                    if x < mid {
                                        s.adjust_param(-1.0);
                                    } else {
                                        s.adjust_param(1.0);
                                    }
                                }
                            } else if let Some(val) = bar_value_at(x, s.areas.param_inner) {
                                if let Some((min, max)) = s.selected_param_range() {
                                    let mapped = min + val * (max - min);
                                    s.set_param_value(mapped);
                                    s.param_dragging = true;
                                }
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
            } else if s.param_scrollbar_dragging && s.active_tab == 0 {
                s.param_state.select_from_scrollbar(y, s.areas.param_inner);
            } else if s.param_dragging && s.active_tab == 0 {
                if let Some(val) = bar_value_at(x, s.areas.param_inner) {
                    if let Some((min, max)) = s.selected_param_range() {
                        let mapped = min + val * (max - min);
                        s.set_param_value(mapped);
                    }
                }
            }
        }
        MouseEventKind::Up(MouseButton::Left) => {
            s.scrollbar_dragging = false;
            s.param_dragging = false;
            s.param_scrollbar_dragging = false;
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
    terminal.draw(|frame| {
        let area = frame.area();
        let [tab_area, content_area, action_area] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Fill(1),
            Constraint::Length(1),
        ])
        .areas(area);

        s.areas.tab = tab_area;
        s.areas.content = content_area;
        s.areas.action_bar = action_area;

        let session_label = if s.dirty { "(1) Session *" } else { "(1) Session" };
        let tab_names: &[&str] = &[session_label, TAB_NAMES[1], TAB_NAMES[2], TAB_NAMES[3]];
        frame.render_widget(TabBar::new(tab_names, s.active_tab), tab_area);

        // BPM display on the right side of the tab bar.
        let bpm_text = format!("{:.0} BPM", s.global_bpm);
        let bpm_width = bpm_text.len() as u16;
        if tab_area.width > bpm_width + 2 {
            let bpm_area = Rect {
                x: tab_area.right() - bpm_width - 1,
                y: tab_area.y,
                width: bpm_width + 1,
                height: 1,
            };
            frame.render_widget(
                Paragraph::new(bpm_text).style(Style::default().fg(Color::DarkGray)),
                bpm_area,
            );
        }

        match s.active_tab {
            0 => {
                // Pre-compute approximate inner heights and sync scroll offsets
                // so mouse click_at uses the correct offset.
                let inner_h = content_area.height.saturating_sub(2) as usize;
                s.chain_state.ensure_visible(inner_h);
                s.param_state.ensure_visible(inner_h);

                let (ci, pi) = render_session(
                    frame,
                    content_area,
                    &s.tree_entries,
                    &s.chain_state,
                    &s.keyboards,
                    &s.param_state,
                    s.focus_params,
                    &s.param_filter_input,
                    s.param_filtering,
                    &s.param_filtered,
                );
                s.areas.chain_inner = ci;
                s.areas.param_inner = pi;

                render_action_bar(frame, action_area, &s.tree_entries, &s.chain_state, s.focus_params);

                if let Some(edit) = &s.editing {
                    render_edit_popup(frame, area, edit);
                }
                if let Some(re) = &s.range_edit {
                    render_range_edit_popup(frame, area, re);
                }
                if let Some(sel) = &s.selector {
                    render_selector_popup(frame, area, sel);
                }
                if let Some(ts) = &s.target_selector {
                    render_target_selector_popup(frame, area, ts);
                }
                if let Some(edit) = &s.bpm_editing {
                    render_edit_popup(frame, area, edit);
                }
            }
            1 => {
                frame.render_widget(
                    Paragraph::new("Piano tab — keyboard input goes here")
                        .style(Style::default().fg(Color::DarkGray)),
                    content_area,
                );
            }
            2 => {
                frame.render_widget(
                    Paragraph::new("Oscilloscope — not yet implemented")
                        .style(Style::default().fg(Color::DarkGray)),
                    content_area,
                );
            }
            3 => render_help(frame, content_area, &s.help_lines, s.help_offset),
            _ => {}
        }
    })?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn render_session(
    frame: &mut ratatui::Frame,
    area: Rect,
    tree_entries: &[TreeEntry],
    chain_state: &ListState,
    keyboards: &[KeyboardNode],
    param_state: &ListState,
    focus_params: bool,
    param_filter_input: &TextInputState,
    param_filtering: bool,
    param_filtered: &[usize],
) -> (Rect, Rect) {
    let [left, right] =
        Layout::horizontal([Constraint::Percentage(42), Constraint::Fill(1)]).areas(area);

    // Chain pane.
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

    let items: Vec<ListItem> = tree_entries
        .iter()
        .map(|e| ListItem::raw(&e.label))
        .collect();
    let mut cs = chain_state.clone();
    cs.ensure_visible(left_inner.height as usize);
    frame.render_widget(
        List::new(&items, &cs)
            .cursor("", 0)
            .style(Style::default().fg(Color::DarkGray))
            .selected_style(Style::default().fg(Color::White)),
        left_inner,
    );

    // Param pane — find the selected plugin or modulator.
    let selected = chain_state.selected;
    let mut mod_params: Vec<ParamSlot> = Vec::new(); // temp storage for modulator pseudo-params
    let (plugin_name, plugin_params) = if selected < tree_entries.len() {
        let addr = &tree_entries[selected].address;
        match addr {
            TreeAddress::Keyboard(kb) => {
                let name = keyboards.get(*kb).map_or("Keyboard", |k| &k.name);
                (name.to_string(), &[] as &[ParamSlot])
            }
            TreeAddress::Modulator { kb, split, parent_slot, index } => {
                let m = keyboards.get(*kb)
                    .and_then(|k| k.splits.get(*split))
                    .and_then(|s| {
                        if *parent_slot == 0 {
                            s.instrument.as_ref()
                        } else {
                            s.effects.get(parent_slot - 1)
                        }
                    })
                    .and_then(|p| p.modulators.get(*index));
                match m {
                    Some(m) => {
                        use crate::plugin::chain::LfoWaveform;
                        // Type enum (index 0) — always present.
                        let type_names = vec!["LFO".to_string(), "Envelope".to_string()];
                        let (name, type_idx) = match &m.source {
                            ModSourceSlot::Lfo { waveform, rate } => {
                                let name = format!("LFO {:.1}Hz {}", rate, waveform.name());
                                mod_params.push(ParamSlot {
                                    name: "Type".to_string(),
                                    index: 0,
                                    min: 0.0,
                                    max: 1.0,
                                    default: 0.0,
                                    value: 0.0,
                                    kind: ParamKind::Enum(type_names),
                                });
                                mod_params.push(ParamSlot {
                                    name: "Waveform".to_string(),
                                    index: 1,
                                    min: 0.0,
                                    max: (LfoWaveform::ALL.len() - 1) as f32,
                                    default: 0.0,
                                    value: waveform.to_index() as f32,
                                    kind: ParamKind::Enum(
                                        LfoWaveform::ALL.iter().map(|w| w.name().to_string()).collect(),
                                    ),
                                });
                                mod_params.push(ParamSlot {
                                    name: "Rate (Hz)".to_string(),
                                    index: 2,
                                    min: 0.01,
                                    max: 50.0,
                                    default: 1.0,
                                    value: *rate,
                                    kind: ParamKind::Float,
                                });
                                (name, 0)
                            }
                            ModSourceSlot::Envelope { attack, decay, sustain, release } => {
                                let name = "ADSR".to_string();
                                mod_params.push(ParamSlot {
                                    name: "Type".to_string(),
                                    index: 0,
                                    min: 0.0,
                                    max: 1.0,
                                    default: 0.0,
                                    value: 1.0,
                                    kind: ParamKind::Enum(type_names),
                                });
                                mod_params.push(ParamSlot {
                                    name: "Attack (s)".to_string(),
                                    index: 1,
                                    min: 0.001,
                                    max: 10.0,
                                    default: 0.01,
                                    value: *attack,
                                    kind: ParamKind::Float,
                                });
                                mod_params.push(ParamSlot {
                                    name: "Decay (s)".to_string(),
                                    index: 2,
                                    min: 0.001,
                                    max: 10.0,
                                    default: 0.3,
                                    value: *decay,
                                    kind: ParamKind::Float,
                                });
                                mod_params.push(ParamSlot {
                                    name: "Sustain".to_string(),
                                    index: 3,
                                    min: 0.0,
                                    max: 1.0,
                                    default: 0.7,
                                    value: *sustain,
                                    kind: ParamKind::Float,
                                });
                                mod_params.push(ParamSlot {
                                    name: "Release (s)".to_string(),
                                    index: 4,
                                    min: 0.001,
                                    max: 10.0,
                                    default: 0.5,
                                    value: *release,
                                    kind: ParamKind::Float,
                                });
                                (name, 1)
                            }
                        };
                        let _ = type_idx;
                        // Separator before target depths.
                        let depth_offset = match &m.source {
                            ModSourceSlot::Lfo { .. } => 4,  // 3 source params + 1 separator
                            ModSourceSlot::Envelope { .. } => 6,  // 5 source params + 1 separator
                        };
                        mod_params.push(ParamSlot {
                            name: "Targets".to_string(),
                            index: 0,
                            min: 0.0,
                            max: 0.0,
                            default: 0.0,
                            value: 0.0,
                            kind: ParamKind::Separator,
                        });
                        for (i, t) in m.targets.iter().enumerate() {
                            mod_params.push(ParamSlot {
                                name: format!("{} depth", t.param_name),
                                index: (i + depth_offset) as u32,
                                min: 0.0,
                                max: 1.0,
                                default: 0.5,
                                value: t.depth,
                                kind: ParamKind::Float,
                            });
                        }
                        (name, mod_params.as_slice())
                    }
                    None => ("(none)".to_string(), &[] as &[ParamSlot]),
                }
            }
            TreeAddress::Pattern { kb, split } => {
                let pat = keyboards.get(*kb)
                    .and_then(|k| k.splits.get(*split))
                    .and_then(|s| s.pattern.as_ref());
                match pat {
                    Some(p) => {
                        mod_params.push(ParamSlot {
                            name: "Length (beats)".to_string(),
                            index: 0,
                            min: 1.0,
                            max: 32.0,
                            default: 4.0,
                            value: p.length_beats,
                            kind: ParamKind::Float,
                        });
                        mod_params.push(ParamSlot {
                            name: "Enabled".to_string(),
                            index: 1,
                            min: 0.0,
                            max: 1.0,
                            default: 1.0,
                            value: if p.enabled { 1.0 } else { 0.0 },
                            kind: ParamKind::Enum(vec!["Off".to_string(), "On".to_string()]),
                        });
                        mod_params.push(ParamSlot {
                            name: "Loop".to_string(),
                            index: 2,
                            min: 0.0,
                            max: 1.0,
                            default: 1.0,
                            value: if p.looping { 1.0 } else { 0.0 },
                            kind: ParamKind::Enum(vec!["Off".to_string(), "On".to_string()]),
                        });
                        if !p.events.is_empty() {
                            let notes = p.events.iter().filter(|e| e.1 == 0x90).count();
                            mod_params.push(ParamSlot {
                                name: "Notes".to_string(),
                                index: 3,
                                min: 0.0,
                                max: 0.0,
                                default: 0.0,
                                value: notes as f32,
                                kind: ParamKind::Separator,
                            });
                        }
                        ("Pattern".to_string(), mod_params.as_slice())
                    }
                    None => ("Pattern".to_string(), &[] as &[ParamSlot]),
                }
            }
            TreeAddress::Split { kb, split } => {
                let sp = keyboards.get(*kb).and_then(|k| k.splits.get(*split));
                let transpose = sp.map_or(0, |s| s.transpose);
                let name = match sp.and_then(|s| s.range) {
                    Some(r) => format_range(r),
                    None => "Full range".into(),
                };
                mod_params.push(ParamSlot {
                    name: "Transpose".to_string(),
                    index: 0,
                    min: -48.0,
                    max: 48.0,
                    default: 0.0,
                    value: transpose as f32,
                    kind: ParamKind::Float,
                });
                (name, mod_params.as_slice())
            }
            _ => {
                // Find the PluginSlot for this address.
                let slot = match addr {
                    TreeAddress::Instrument { kb, split } => {
                        keyboards.get(*kb)
                            .and_then(|k| k.splits.get(*split))
                            .and_then(|s| s.instrument.as_ref())
                    }
                    TreeAddress::Effect { kb, split, index } => {
                        keyboards.get(*kb)
                            .and_then(|k| k.splits.get(*split))
                            .and_then(|s| s.effects.get(*index))
                    }
                    _ => None,
                };
                match slot {
                    Some(p) => (p.name.clone(), p.params.as_slice()),
                    None => ("(none)".to_string(), &[] as &[ParamSlot]),
                }
            }
        }
    } else {
        ("(none)".to_string(), &[] as &[ParamSlot])
    };

    let right_style = if focus_params {
        Style::default().fg(Color::White)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let right_block = Block::default()
        .borders(Borders::ALL)
        .border_style(right_style)
        .title(format!(" {} ", plugin_name));
    let right_inner = right_block.inner(right);
    frame.render_widget(right_block, right);

    // Determine if the filter bar should be shown.
    let is_plugin_node = selected < tree_entries.len()
        && matches!(
            tree_entries[selected].address,
            TreeAddress::Instrument { .. } | TreeAddress::Effect { .. }
        );
    let show_filter = is_plugin_node
        && (param_filtering || !param_filter_input.value.is_empty());

    // Split right_inner into filter bar + list area when filter is active.
    let (filter_area, list_area) = if show_filter && right_inner.height > 1 {
        let [fa, la] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Fill(1),
        ]).areas(right_inner);
        (Some(fa), la)
    } else {
        (None, right_inner)
    };

    // Render filter bar.
    if let Some(fa) = filter_area {
        let prompt = "/ ";
        let pw = prompt.len() as u16;
        frame.render_widget(
            Paragraph::new(prompt).style(Style::default().fg(Color::Yellow)),
            Rect::new(fa.x, fa.y, pw, 1),
        );
        frame.render_widget(
            TextInput::new(param_filter_input),
            Rect::new(fa.x + pw, fa.y, fa.width.saturating_sub(pw), 1),
        );
    }

    // Build the display params — apply filter for plugin nodes.
    let display_params: Vec<&ParamSlot> = if is_plugin_node && !param_filtered.is_empty() {
        param_filtered.iter().filter_map(|&i| plugin_params.get(i)).collect()
    } else if is_plugin_node && param_filtered.is_empty() && !param_filter_input.value.is_empty() {
        // Filter active but no matches.
        vec![]
    } else {
        plugin_params.iter().collect()
    };

    let name_width = 24;
    let bar_width = list_area.width.saturating_sub(name_width as u16 + 12) as usize;

    #[derive(PartialEq)]
    enum ParamRow { Normal, Enum, Separator }
    // (name, col1, col2, col3, row_kind)
    let param_strings: Vec<(String, String, String, String, ParamRow)> = display_params
        .iter()
        .map(|p| {
            let name_str = format!("{:<width$} ", truncate(&p.name, name_width), width = name_width);
            match &p.kind {
                ParamKind::Separator => {
                    (name_str, "──────".to_string(), String::new(), String::new(), ParamRow::Separator)
                }
                ParamKind::Enum(options) => {
                    let idx = p.value.round() as usize;
                    let label = options.get(idx).map_or("?", |s| s.as_str());
                    (name_str, format!("◂ {} ▸", label), String::new(), String::new(), ParamRow::Enum)
                }
                ParamKind::Float => {
                    let normalized = if (p.max - p.min).abs() > f32::EPSILON {
                        (p.value - p.min) / (p.max - p.min)
                    } else {
                        0.0
                    };
                    let filled = (normalized * bar_width as f32).round() as usize;
                    let empty = bar_width.saturating_sub(filled);
                    (
                        name_str,
                        "▓".repeat(filled),
                        "░".repeat(empty),
                        format!(" {:>8.2}", p.value),
                        ParamRow::Normal,
                    )
                }
            }
        })
        .collect();
    let sep_style = Style::default().fg(Color::DarkGray);
    let param_items: Vec<ListItem> = param_strings
        .iter()
        .map(|(name, col1, col2, col3, row_kind)| match row_kind {
            ParamRow::Separator => ListItem::spans(vec![
                ListSpan::new(name, sep_style),
                ListSpan::new(col1, sep_style),
            ]),
            ParamRow::Enum => ListItem::spans(vec![
                ListSpan::new(name, Style::default()),
                ListSpan::new(col1, Style::default()),
            ]),
            ParamRow::Normal => ListItem::spans(vec![
                ListSpan::new(name, Style::default()),
                ListSpan::new(col1, Style::default()),
                ListSpan::new(col2, Style::default()),
                ListSpan::new(col3, Style::default()),
            ]),
        })
        .collect();

    let mut ps = param_state.clone();
    ps.ensure_visible(list_area.height as usize);
    let param_list = if focus_params {
        List::new(&param_items, &ps)
            .style(Style::default().fg(Color::DarkGray))
            .selected_style(Style::default().fg(Color::White))
    } else {
        List::new(&param_items, &ps)
            .style(Style::default().fg(Color::DarkGray))
            .selected_style(Style::default().fg(Color::DarkGray))
            .cursor("  ", 2)
    };
    frame.render_widget(param_list, list_area);

    (left_inner, right_inner)
}

fn render_action_bar(
    frame: &mut ratatui::Frame,
    area: Rect,
    tree_entries: &[TreeEntry],
    chain_state: &ListState,
    focus_params: bool,
) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let sel = chain_state.selected;
    let addr = tree_entries.get(sel).map(|e| &e.address);
    let actions = actions_for(addr);

    let key_style = Style::default().fg(Color::Black).bg(Color::DarkGray).add_modifier(Modifier::BOLD);
    let label_style = Style::default().fg(Color::DarkGray);
    let active_key_style = Style::default().fg(Color::Black).bg(Color::White).add_modifier(Modifier::BOLD);
    let active_label_style = Style::default().fg(Color::White);

    let y = area.y;
    let mut x = area.x;

    for &(key, desc) in &actions {
        let (ks, ls) = if focus_params {
            (key_style, label_style)
        } else {
            (active_key_style, active_label_style)
        };
        if x > area.x {
            x += 1;
        }
        for ch in format!(" {key} ").chars() {
            if x >= area.right() { break; }
            if let Some(c) = frame.buffer_mut().cell_mut((x, y)) { c.set_char(ch); c.set_style(ks); }
            x += 1;
        }
        for ch in format!(" {desc}").chars() {
            if x >= area.right() { break; }
            if let Some(c) = frame.buffer_mut().cell_mut((x, y)) { c.set_char(ch); c.set_style(ls); }
            x += 1;
        }
    }
}

fn render_edit_popup(frame: &mut ratatui::Frame, area: Rect, edit: &EditState) {
    let popup = centered_rect(34, 5, area);
    frame.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow))
        .title(format!(" {} ", edit.param_name));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    if inner.height >= 2 {
        let hint = format!("Range: {:.2} — {:.2}", edit.param_min, edit.param_max);
        frame.render_widget(
            Paragraph::new(hint).style(Style::default().fg(Color::DarkGray)),
            Rect::new(inner.x, inner.y, inner.width, 1),
        );
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

fn render_range_edit_popup(frame: &mut ratatui::Frame, area: Rect, re: &RangeEditState) {
    let popup = centered_rect(34, 5, area);
    frame.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow))
        .title(" Add Split ");
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    if inner.height >= 2 {
        frame.render_widget(
            Paragraph::new("Range (e.g. C0-B3), empty=all")
                .style(Style::default().fg(Color::DarkGray)),
            Rect::new(inner.x, inner.y, inner.width, 1),
        );
        frame.render_widget(
            TextInput::new(&re.input),
            Rect::new(inner.x, inner.y + 1, inner.width, 1),
        );
    }
}

fn render_selector_popup(frame: &mut ratatui::Frame, area: Rect, sel: &SelectorState) {
    let title = match sel.mode {
        SelectorMode::Instrument => " Select Instrument ",
        SelectorMode::Effect => " Select Effect ",
    };
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
    frame.render_widget(FilterList::new(&sel.filter, &sel.items, columns), inner);
}

fn render_target_selector_popup(frame: &mut ratatui::Frame, area: Rect, ts: &TargetSelectorState) {
    let w = (area.width * 60 / 100).max(36).min(area.width);
    let h = (area.height * 50 / 100).max(10).min(area.height);
    let popup = centered_rect(w, h, area);
    frame.render_widget(Clear, popup);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Magenta))
        .title(" Select Target Parameter ");
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let columns: &[(&str, u16)] = &[
        ("Plugin", inner.width.saturating_sub(20).min(20)),
        ("Parameter", inner.width.saturating_sub(20)),
    ];
    frame.render_widget(FilterList::new(&ts.filter, &ts.items, columns), inner);
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

/// After removing a modulator at `removed_index` from the TUI model,
/// clean up cross-mod targets in siblings: remove targets pointing at the
/// removed index, and decrement indices > removed_index.
fn fixup_tui_cross_mod_after_remove(modulators: &mut [ModulatorSlot], removed_index: usize) {
    use crate::plugin::chain::ModTargetKind;
    for m in modulators.iter_mut() {
        m.targets.retain(|t| {
            let idx = match &t.kind {
                ModTargetKind::PluginParam { .. } => None,
                ModTargetKind::ModulatorRate { mod_index }
                | ModTargetKind::ModulatorAttack { mod_index }
                | ModTargetKind::ModulatorDecay { mod_index }
                | ModTargetKind::ModulatorSustain { mod_index }
                | ModTargetKind::ModulatorRelease { mod_index }
                | ModTargetKind::ModulatorDepth { mod_index, .. } => Some(*mod_index),
            };
            idx != Some(removed_index)
        });
        for t in &mut m.targets {
            let idx = match &mut t.kind {
                ModTargetKind::PluginParam { .. } => continue,
                ModTargetKind::ModulatorRate { mod_index }
                | ModTargetKind::ModulatorAttack { mod_index }
                | ModTargetKind::ModulatorDecay { mod_index }
                | ModTargetKind::ModulatorSustain { mod_index }
                | ModTargetKind::ModulatorRelease { mod_index }
                | ModTargetKind::ModulatorDepth { mod_index, .. } => mod_index,
            };
            if *idx > removed_index {
                *idx -= 1;
            }
        }
    }
}

/// Convert a TUI ModSourceSlot to an audio-thread ModSource for GraphCommands.
fn mod_source_slot_to_graph(slot: &ModSourceSlot) -> crate::plugin::chain::ModSource {
    match slot {
        ModSourceSlot::Lfo { waveform, rate } => crate::plugin::chain::ModSource::Lfo {
            waveform: *waveform,
            rate: *rate,
            phase: 0.0,
        },
        ModSourceSlot::Envelope { attack, decay, sustain, release } => crate::plugin::chain::ModSource::Envelope {
            attack: *attack,
            decay: *decay,
            sustain: *sustain,
            release: *release,
            state: crate::plugin::chain::EnvState::Idle,
            level: 0.0,
            notes_held: 0,
        },
    }
}

fn to_plugin_slot(lp: LoadedPlugin) -> PluginSlot {
    let params = lp
        .params
        .into_iter()
        .zip(lp.param_values)
        .filter(|(p, _)| !p.name.starts_with("(locked)"))
        .map(|(p, v)| ParamSlot {
            name: p.name,
            index: p.index,
            min: p.min,
            max: p.max,
            default: p.default,
            value: v,
            kind: ParamKind::Float,
        })
        .collect();
    let modulators = lp.modulators.into_iter().map(|lm| {
        let source = match lm.source {
            LoadedModSource::Lfo { waveform, rate } => ModSourceSlot::Lfo { waveform, rate },
            LoadedModSource::Envelope { attack, decay, sustain, release } => {
                ModSourceSlot::Envelope { attack, decay, sustain, release }
            }
        };
        ModulatorSlot {
            source,
            targets: lm.targets.into_iter().map(|lt| {
                ModTargetSlot {
                    param_name: lt.param_name.clone(),
                    kind: crate::plugin::chain::ModTargetKind::PluginParam { param_index: lt.param_index },
                    depth: lt.depth,
                    param_min: lt.param_min,
                    param_max: lt.param_max,
                }
            }).collect(),
        }
    }).collect();
    PluginSlot {
        name: lp.name,
        format: format_from_id(&lp.id),
        id: lp.id,
        is_instrument: lp.is_instrument,
        params,
        modulators,
    }
}

fn param_step(s: &State, modifiers: KeyModifiers) -> f32 {
    let pa = s.real_param_index().unwrap_or(s.param_state.selected);
    let sel = s.chain_state.selected;
    let range = if sel < s.tree_entries.len() {
        let addr = &s.tree_entries[sel].address;
        s.plugin_at(addr)
            .and_then(|p| p.params.get(pa))
            .map(|p| p.max - p.min)
            .unwrap_or(1.0)
    } else {
        1.0
    };

    if modifiers.contains(KeyModifiers::CONTROL) {
        range * 0.10
    } else if modifiers.contains(KeyModifiers::SHIFT) {
        range * 0.01
    } else {
        range * 0.05
    }
}

fn bar_value_at(x: u16, param_inner: Rect) -> Option<f32> {
    // cursor(2) + name(24) + space(1) = 27
    let bar_start = param_inner.x + 27;
    let bar_width = param_inner.width.saturating_sub(24 + 12);
    if bar_width == 0 || x < bar_start || x >= bar_start + bar_width {
        return None;
    }
    Some(((x - bar_start) as f32 / (bar_width - 1).max(1) as f32).clamp(0.0, 1.0))
}

/// Format a note range as "C4-B5" style string.
fn format_range(range: (u8, u8)) -> String {
    format!("{}-{}", crate::note_name(range.0), crate::note_name(range.1))
}

fn build_tree_entries(keyboards: &[KeyboardNode]) -> Vec<TreeEntry> {
    let mut entries = Vec::new();

    // Helper: build modulator labels for a plugin's modulators.
    fn push_modulators(
        entries: &mut Vec<TreeEntry>,
        modulators: &[ModulatorSlot],
        parent_slot: usize,
        kb_idx: usize,
        sp_idx: usize,
        parent_cont: &str,
        is_last_parent: bool,
    ) {
        let cont = if is_last_parent {
            format!("{parent_cont}  ")
        } else {
            format!("{parent_cont}│ ")
        };
        for (mod_idx, m) in modulators.iter().enumerate() {
            let branch = if mod_idx == 0 { "╰" } else { " " };
            let source_label = match &m.source {
                ModSourceSlot::Lfo { waveform, rate } => format!("LFO {:.1}Hz {}", rate, waveform.name()),
                ModSourceSlot::Envelope { .. } => "ADSR".to_string(),
            };
            entries.push(TreeEntry {
                label: format!("{cont}{branch} ~ {source_label}"),
                address: TreeAddress::Modulator { kb: kb_idx, split: sp_idx, parent_slot, index: mod_idx },
                color: Color::Magenta,
                indent: 3,
            });
        }
    }

    for (kb_idx, kb) in keyboards.iter().enumerate() {
        // Keyboard header
        entries.push(TreeEntry {
            label: format!("⌨ {}", kb.name),
            address: TreeAddress::Keyboard(kb_idx),
            color: Color::Cyan,
            indent: 0,
        });

        for (sp_idx, sp) in kb.splits.iter().enumerate() {
            let is_last_split = sp_idx == kb.splits.len() - 1;
            let split_branch = if is_last_split { "╰" } else { "├" };
            let split_cont = if is_last_split { "  " } else { "│ " };

            // Split node
            let split_label = match sp.range {
                Some(r) => format_range(r),
                None => "Full range".into(),
            };
            let transpose_label = if sp.transpose != 0 {
                let sign = if sp.transpose > 0 { "+" } else { "" };
                format!("  {sign}{}", sp.transpose)
            } else {
                String::new()
            };
            entries.push(TreeEntry {
                label: format!("{split_branch} {split_label}{transpose_label}"),
                address: TreeAddress::Split { kb: kb_idx, split: sp_idx },
                color: Color::White,
                indent: 1,
            });

            // Count top-level children (pattern + instrument + effects, not modulators).
            let has_pattern = sp.pattern.as_ref().is_some_and(|p| p.recording || !p.events.is_empty());
            let has_inst = sp.instrument.is_some();
            let child_count = if has_pattern { 1 } else { 0 }
                + if has_inst { 1 } else { 0 }
                + sp.effects.len();
            let mut child_idx = 0;

            // Pattern node (only when recording or has data)
            if let Some(pat) = &sp.pattern {
                if pat.recording || !pat.events.is_empty() {
                    let is_last_child = child_idx == child_count - 1;
                    let child_branch = if is_last_child { "╰" } else { "├" };
                    let (icon, color, detail) = if pat.recording {
                        ("\u{23fa}", Color::Red, "recording...".to_string())
                    } else {
                        let n = pat.events.iter().filter(|e| e.1 == 0x90).count();
                        ("\u{25b6}", Color::Blue, format!("{:.0} beats, {n} notes", pat.length_beats))
                    };
                    entries.push(TreeEntry {
                        label: format!("{split_cont}{child_branch} {icon} Pattern  {detail}"),
                        address: TreeAddress::Pattern { kb: kb_idx, split: sp_idx },
                        color,
                        indent: 2,
                    });
                    child_idx += 1;
                }
            }

            // Instrument (only show if present)
            if let Some(inst) = &sp.instrument {
                let is_last_child = child_idx == child_count - 1;
                let child_branch = if is_last_child { "╰" } else { "├" };
                let inst_label = format!("{split_cont}{child_branch} \u{266a} {}  [{}]", inst.name, inst.format);
                entries.push(TreeEntry {
                    label: inst_label,
                    address: TreeAddress::Instrument { kb: kb_idx, split: sp_idx },
                    color: Color::Green,
                    indent: 2,
                });
                // Instrument modulators (sub-nodes)
                push_modulators(&mut entries, &inst.modulators, 0, kb_idx, sp_idx, split_cont, is_last_child);
                child_idx += 1;
            }

            // Effects
            for (fx_idx, fx) in sp.effects.iter().enumerate() {
                let is_last_child = child_idx == child_count - 1;
                let child_branch = if is_last_child { "╰" } else { "├" };
                entries.push(TreeEntry {
                    label: format!("{split_cont}{child_branch} fx {}  [{}]", fx.name, fx.format),
                    address: TreeAddress::Effect { kb: kb_idx, split: sp_idx, index: fx_idx },
                    color: Color::Yellow,
                    indent: 2,
                });
                // Effect modulators (sub-nodes)
                push_modulators(&mut entries, &fx.modulators, fx_idx + 1, kb_idx, sp_idx, split_cont, is_last_child);
                child_idx += 1;
            }
        }
    }

    entries
}

fn format_from_id(id: &str) -> String {
    if id.starts_with("builtin:") {
        "Built-in".into()
    } else if id.starts_with("lv2:") || id.starts_with("http://") || id.starts_with("urn:") {
        "LV2".into()
    } else if id.starts_with("clap:") || id.contains('.') && !id.contains('/') {
        "CLAP".into()
    } else if id.starts_with("vst3:") {
        "VST3".into()
    } else if id.ends_with(".lv2") {
        "LV2".into()
    } else if id.ends_with(".clap") {
        "CLAP".into()
    } else if id.ends_with(".vst3") {
        "VST3".into()
    } else {
        "?".into()
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max - 1])
    }
}

fn action_bar_hit(x: u16, y: u16, area: Rect, actions: &[(&str, &str)]) -> Option<char> {
    if y != area.y || x < area.x || x >= area.right() {
        return None;
    }
    let rel_x = (x - area.x) as usize;
    let mut pos = 0;
    for &(key, desc) in actions {
        if pos > 0 {
            pos += 1;
        }
        let total = key.len() + 2 + desc.len() + 1;
        if rel_x >= pos && rel_x < pos + total {
            return key.chars().next();
        }
        pos += total;
    }
    None
}

fn build_catalog() -> Vec<PluginInfo> {
    let mut catalog = Vec::new();

    catalog.extend(plugin::builtin::enumerate_plugins());

    #[cfg(feature = "lv2")]
    catalog.extend(plugin::lv2::enumerate_plugins());

    catalog.extend(plugin::clap::enumerate_plugins());

    #[cfg(feature = "vst3")]
    catalog.extend(plugin::vst3::enumerate_plugins());

    catalog.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    catalog
}

fn build_help_lines() -> Vec<String> {
    vec![
        "Tang — Terminal Audio Plugin Host".into(),
        "".into(),
        "Global keybindings:".into(),
        "  1 2 3 4    Switch to tab by number".into(),
        "  Tab        Next tab".into(),
        "  Shift+Tab  Previous tab".into(),
        "  Ctrl+S     Save session".into(),
        "  Ctrl+Q     Quit".into(),
        "".into(),
        "Session tab (chain focus):".into(),
        "  Up/Down    Navigate chain".into(),
        "  Shift+↑/↓  Move effect up/down".into(),
        "  Enter      Focus parameter list".into(),
        "  i          Replace instrument".into(),
        "  a          Add effect after selected".into(),
        "  d          Delete selected".into(),
        "  m          Add modulator".into(),
        "  r          Record/stop pattern".into(),
        "  Ctrl+R     Clear pattern".into(),
        "  b          Set BPM".into(),
        "  s          Add split to keyboard".into(),
        "".into(),
        "Modulator (chain focus):".into(),
        "  t          Add modulation target".into(),
        "  d          Delete modulator".into(),
        "".into(),
        "Session tab (param focus):".into(),
        "  Up/Down    Navigate parameters".into(),
        "  Left/Right Adjust value (5%)".into(),
        "  Shift+←/→  Fine adjust (1%)".into(),
        "  Ctrl+←/→   Coarse adjust (10%)".into(),
        "  Enter      Type a value".into(),
        "  /          Search parameters".into(),
        "  Esc        Clear filter / back to chain".into(),
        "".into(),
        "Plugin selector:".into(),
        "  Type       Filter by name/format".into(),
        "  Up/Down    Navigate results".into(),
        "  Enter      Confirm".into(),
        "  Esc        Cancel".into(),
        "".into(),
        "Mouse:".into(),
        "  Click      Select items, tabs, actions".into(),
        "  Drag       Adjust parameter bars".into(),
        "  Scroll     Navigate lists".into(),
    ]
}
