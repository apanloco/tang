mod clip;
#[cfg(test)]
mod golden_tests;
mod piano_view;
pub mod splash;
mod terminal_input;
mod web;

use std::io;
use std::time::{Duration, Instant};

use crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
    KeyboardEnhancementFlags, MouseButton, MouseEventKind, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::backend::{Backend, CrosstermBackend};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Terminal;

use crossbeam_channel::Receiver;
use terminal_input::TerminalReader;
use view::DIM;
use web::{Web, WebStatus};
use view::filter_list::{FilterListItem, FilterListState};
use view::list::{ListItem, ListSpan, ListState};
use view::scroll_view::ScrollLine;
use view::text_input::TextInputState;
use view::{FilterList, List, ScrollView, TabBar, TextInput, centered_rect};

use crate::app::controller::{Controller, NewTarget};
use crate::cli::WebArgs;
use crate::app::intent::{self, Applied, Gain, Intent, ModulatorField};
use crate::app::model::{
    DEPTH_MAX, DEPTH_MIN, GroupNode, InstrumentNode, ModSourceSlot, ModTargetSlot, ModulatorRow, ModulatorSlot, NEW_CABLE_DEPTH, NodeId,
    ParamKind, ParamSlot, PatternState, PluginSlot, fixup_group_member_after_remove, format_from_id, lane_member_ordinal, mod_source_slot_to_graph, modulator_param_is_enum,
    modulator_param_len, modulator_param_range, modulator_row, modulator_row_to_cross_mod, remap_group_bus_after_reorder,
    shift_group_member_after_insert,
};
use crate::piano_filter::PianoMode;
use crate::plugin;
use crate::plugin::chain::{GraphCommand, ModulatorLocation};
use crate::scale::{ScaleSetting, NOTE_NAMES};
use crate::telemetry;

const TAB_NAMES: &[&str] = &["(1) Session", "(2) Piano", "(3) Scope", "(4) Help"];
const TAB_SEP: &str = " │ ";

/// Addresses a specific node in the instrument tree.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum TreeAddress {
    Instrument(usize),
    Effect { inst: usize, index: usize },
    Pattern(usize),
    /// A lane-scoped modulator. `index` is its position in the instrument's
    /// modulator list.
    Modulator { inst: usize, index: usize },
    /// A submix group header node.
    Group(usize),
    /// An effect on a group's bus chain.
    GroupEffect { group: usize, index: usize },
    /// A group-scoped modulator. `index` is its position in the group's
    /// modulator list.
    GroupModulator { group: usize, index: usize },
}

impl TreeAddress {
    /// Get the instrument index for this address (None for group nodes).
    fn inst(&self) -> Option<usize> {
        match *self {
            TreeAddress::Instrument(inst) => Some(inst),
            TreeAddress::Effect { inst, .. } => Some(inst),
            TreeAddress::Pattern(inst) => Some(inst),
            TreeAddress::Modulator { inst, .. } => Some(inst),
            TreeAddress::Group(_)
            | TreeAddress::GroupEffect { .. }
            | TreeAddress::GroupModulator { .. } => None,
        }
    }

    /// Get the audio thread slot index (0 = instrument, 1..N = effects).
    /// Modulators are lane-scoped (not tied to a slot), so they report 0.
    fn slot(&self) -> usize {
        match *self {
            TreeAddress::Instrument(_) => 0,
            TreeAddress::Effect { index, .. } => index + 1,
            TreeAddress::Pattern(_) => 0,
            TreeAddress::Modulator { .. } => 0,
            TreeAddress::Group(_) => 0,
            TreeAddress::GroupEffect { index, .. } => index + 1,
            TreeAddress::GroupModulator { .. } => 0,
        }
    }
}

struct TreeEntry {
    label: String,
    address: TreeAddress,
    /// The node of the session the entry is of (a lane's, for its pattern). An address is a
    /// position, and what is at it changes as nodes come and go; this does not.
    node: NodeId,
    #[allow(dead_code)]
    color: Color,
    #[allow(dead_code)]
    indent: usize,
}

/// What an entry of the tree is made of, whatever place it has: the kind of entry (a lane's pattern is an entry of
/// the lane's node, as its instrument is) and the id of the node.
type EntryKey = (std::mem::Discriminant<TreeAddress>, NodeId);

fn entry_key(entry: &TreeEntry) -> EntryKey {
    (std::mem::discriminant(&entry.address), entry.node)
}

// ---------------------------------------------------------------------------
// Action bar
// ---------------------------------------------------------------------------

/// Build the action bar items for the current tree selection.
fn actions_for(addr: Option<&TreeAddress>) -> Vec<(&'static str, &'static str)> {
    match addr {
        Some(TreeAddress::Instrument(_)) => vec![
            ("n", "new instr"),
            ("i", "instrument"),
            ("R", "range"),
            ("v", "volume"),
            ("a", "add effect"),
            ("m", "modulate"),
            ("g", "group"),
            ("d", "delete"),
            ("p", "presets"),
        ],
        Some(TreeAddress::Effect { .. }) => vec![
            ("n", "new instr"),
            ("a", "add effect"),
            ("m", "modulate"),
            ("x", "mix"),
            ("d", "delete"),
            ("p", "presets"),
        ],
        Some(TreeAddress::Pattern(_)) => vec![
            ("r", "record"),
            ("d", "clear"),
        ],
        Some(TreeAddress::Modulator { .. }) => vec![
            ("t", "add target"),
            ("d", "delete"),
        ],
        Some(TreeAddress::Group(_)) => vec![
            ("a", "add fx"),
            ("m", "modulate"),
            ("v", "volume"),
            ("R", "rename"),
            ("d", "delete group"),
        ],
        Some(TreeAddress::GroupEffect { .. }) => vec![
            ("a", "add fx"),
            ("m", "modulate"),
            ("x", "mix"),
            ("d", "delete"),
            ("p", "presets"),
        ],
        Some(TreeAddress::GroupModulator { .. }) => vec![
            ("t", "add target"),
            ("d", "delete"),
        ],
        None => vec![("n", "new instr")],
    }
}

/// Build the action bar items for the parameter pane (when it has focus). These
/// are the keys that act on the *selected parameter*, which differ from the
/// chain-focus actions in `actions_for`.
fn param_actions_for(addr: Option<&TreeAddress>) -> Vec<(&'static str, &'static str)> {
    match addr {
        // Plugin parameters: edit, modulate, MIDI-learn, filter the list.
        Some(TreeAddress::Instrument(_))
        | Some(TreeAddress::Effect { .. })
        | Some(TreeAddress::GroupEffect { .. }) => vec![
            ("Enter", "edit"),
            ("m", "modulate"),
            ("l", "learn"),
            ("/", "filter"),
        ],
        // Modulator pseudo-params (rate/ADSR/depth): edit, cross-modulate, or
        // MIDI-learn the row.
        Some(TreeAddress::Modulator { .. }) | Some(TreeAddress::GroupModulator { .. }) => {
            vec![("Enter", "edit"), ("m", "modulate"), ("l", "learn")]
        }
        // Group Volume/Pan and pattern settings: edit only.
        Some(TreeAddress::Group(_)) | Some(TreeAddress::Pattern(_)) => vec![("Enter", "edit")],
        None => vec![],
    }
}

/// The action bar items for the currently focused pane.
fn action_bar_items(addr: Option<&TreeAddress>, focus_params: bool) -> Vec<(&'static str, &'static str)> {
    if focus_params {
        param_actions_for(addr)
    } else {
        actions_for(addr)
    }
}

// ---------------------------------------------------------------------------
// Popup state types
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SelectorMode {
    Instrument,
    Effect,
    /// Add an effect to a group's bus chain.
    GroupEffect(usize),
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

/// Editing a host-side gain (effect mix or instrument/group volume) via the
/// value popup. These live outside the plugin parameter list.
struct GainEditState {
    node: NodeId,
    gain: Gain,
    edit: EditState,
}

impl GainEditState {
    /// A popup for `gain` of `node`, starting at its `current` value.
    fn new(node: NodeId, gain: Gain, current: f32, label: &str) -> Self {
        let (param_min, param_max) = gain.range();
        GainEditState {
            node,
            gain,
            edit: EditState {
                input: TextInputState::new(&format!("{current:.2}")),
                param_name: label.to_string(),
                param_min,
                param_max,
            },
        }
    }
}

/// One entry in the target selector popup.
struct TargetEntry {
    label: String,
    /// Chain slot for plugin-param entries (0 = instrument, 1..N = effect);
    /// 0 and unused for cross-mod entries.
    slot: usize,
    kind: crate::plugin::chain::ModTargetKind,
    param_min: f32,
    param_max: f32,
    base_value: f32,
}

struct TargetSelectorState {
    filter: FilterListState,
    items: Vec<FilterListItem>,
    entries: Vec<TargetEntry>,
    /// Which modulator rack this selector targets (lane or group).
    scope: ModulatorLocation,
    mod_index: usize,
}

/// A choice in the "modulate this parameter" popup (`m` on a parameter):
/// create a new lane modulator bound to the parameter, or attach the
/// parameter to one of the instrument's existing lane modulators.
#[derive(Clone, Copy)]
enum ModulateChoice {
    NewLfo,
    NewEnvelope,
    Existing(usize),
}

/// State for the "modulate <param>" popup.
struct ModulateState {
    /// Which modulator rack a created/attached modulator lives in.
    scope: ModulatorLocation,
    /// Chain slot the parameter lives on (0 = instrument, 1..N = effect);
    /// used for the model's display label only.
    slot: usize,
    /// The target kind to bind (PluginParam for lanes, GroupBus for groups).
    target_kind: crate::plugin::chain::ModTargetKind,
    param_name: String,
    param_min: f32,
    param_max: f32,
    /// Current value of the parameter, used as the modulation base.
    base_value: f32,
    choices: Vec<ModulateChoice>,
    filter: FilterListState,
    items: Vec<FilterListItem>,
}

/// A choice in the "assign to group" popup (`g` on an instrument).
#[derive(Clone, Copy)]
enum GroupAssignChoice {
    New,
    Ungroup,
    Existing(usize),
}

/// State for the "assign <instrument> to group" popup.
struct GroupAssignState {
    inst: usize,
    choices: Vec<GroupAssignChoice>,
    filter: FilterListState,
    items: Vec<FilterListItem>,
}

struct PresetSelectorState {
    filter: FilterListState,
    items: Vec<FilterListItem>,
    /// Parallel to `items`: the (name, id) pair for each row before filtering.
    presets: Vec<plugin::Preset>,
    inst: usize,
    /// 0 = instrument, 1..N = effects.
    slot: usize,
}

struct RangeEditState {
    /// The instrument whose key range is being edited.
    inst: usize,
    input: TextInputState,
}

/// State for the "rename group" popup.
struct RenameGroupState {
    /// The group whose display name is being edited.
    group: usize,
    input: TextInputState,
}

/// State for the "save as" filename popup.
struct SaveAsState {
    input: TextInputState,
    /// Error from the last failed save attempt, shown in the popup.
    error: Option<String>,
}

/// State for the scale picker popup (Piano tab).
struct ScaleSelectorState {
    filter: FilterListState,
    items: Vec<FilterListItem>,
    /// Parallel to items: the (root, scale_idx) pair for each row before filtering.
    entries: Vec<(u8, usize)>,
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
    /// The session and the engine handles; the view only holds what it shows.
    ctl: Controller,
    /// The CLIP light of the status bar, fed by the master meter.
    clip: clip::ClipLight,
    /// The web interface this TUI serves, if it does (`tang --web`).
    web: Option<WebStatus>,
    active_tab: usize,
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
    rename_group: Option<RenameGroupState>,
    selector: Option<SelectorState>,
    target_selector: Option<TargetSelectorState>,
    modulate: Option<ModulateState>,
    group_assign: Option<GroupAssignState>,
    preset_selector: Option<PresetSelectorState>,
    areas: Areas,
    quit: bool,
    // Parameter filter (search bar in param pane).
    param_filter_input: TextInputState,
    param_filtering: bool,
    param_filtered: Vec<usize>,
    // Pattern state.
    bpm_editing: Option<EditState>,
    gain_editing: Option<GainEditState>,
    save_as: Option<SaveAsState>,
    // Piano tab state.
    /// Center MIDI octave for the piano view (0..=8). Default 4.
    piano_view_octave: i8,
    scale_selector: Option<ScaleSelectorState>,
}

impl State {
    /// Build the TUI state around a controller holding a loaded session. Pure
    /// construction: no engine commands are sent and no threads are spawned, so
    /// tests can build a `State` around stub channels.
    fn new(ctl: Controller) -> State {
        let tree_entries = build_tree_entries(&ctl.model.instruments, &ctl.model.groups);
        let param_len = ctl.model.instruments.first()
            .and_then(|n| n.instrument.as_ref())
            .map_or(0, |p| p.params.len());

        let help_lines = build_help_lines(None);
        let clip = clip::ClipLight::new(ctl.telemetry.count(telemetry::MASTER.clips));

        State {
            ctl,
            clip,
            web: None,
            active_tab: 0,
            chain_state: ListState::new(tree_entries.len()),
            param_state: ListState::new(param_len),
            tree_entries,
            focus_params: false,
            help_lines,
            help_offset: 0,
            scrollbar_dragging: false,
            param_dragging: false,
            param_scrollbar_dragging: false,
            editing: None,
            range_edit: None,
            rename_group: None,
            selector: None,
            target_selector: None,
            modulate: None,
            group_assign: None,
            preset_selector: None,
            areas: Areas::default(),
            quit: false,
            param_filter_input: TextInputState::new(""),
            param_filtering: false,
            param_filtered: (0..param_len).collect(),
            bpm_editing: None,
            gain_editing: None,
            save_as: None,
            piano_view_octave: 4,
            scale_selector: None,
        }
    }

    /// The web interface is up: say so in the tab bar and on the Help tab. (After this
    /// only the number of browsers changes, in `self.web`.)
    fn set_web(&mut self, web: WebStatus) {
        self.help_lines = build_help_lines(Some(&web));
        self.web = Some(web);
    }

    /// Build the tree again from the model. What is typed into the parameter filter stays if the
    /// node the cursor is on is the same one (a value changed under it, by a browser, say), and goes
    /// if it is not.
    fn rebuild_tree(&mut self) {
        let was_on = self.node_under_cursor();
        self.tree_entries = build_tree_entries(&self.ctl.model.instruments, &self.ctl.model.groups);
        self.chain_state.set_len(self.tree_entries.len());
        // The cursor stays on the node it was on, wherever the tree has put it now: what came or went above it
        // (a browser added a modulator to an earlier lane, say) has moved its place, and what the person does
        // next (`d`, a value typed in) would be for whatever is at the old one.
        if let Some(on) = was_on {
            if let Some(position) = self.tree_entries.iter().position(|entry| entry_key(entry) == on) {
                self.chain_state.selected = position;
            }
        }
        if self.node_under_cursor() == was_on {
            self.sync_param_len();
        } else {
            self.sync_param_state();
        }
    }

    /// What the cursor is on. It does not change as the nodes around it come and go, as its address does.
    fn node_under_cursor(&self) -> Option<EntryKey> {
        self.tree_entries.get(self.chain_state.selected).map(entry_key)
    }

    /// Every entry of the tree, in the order they are shown.
    fn tree_places(&self) -> Vec<EntryKey> {
        self.tree_entries.iter().map(entry_key).collect()
    }

    /// Close what is open and holds a place in the tree (a lane's, a modulator's, a group's) that has moved, or is
    /// not there any more: it would act on what is at the place now. What is typed in as the value of a parameter
    /// goes to the node under the cursor, which follows its node (`rebuild_tree`), so it stays unless that node is
    /// gone; a host-side gain has the id of its node.
    fn close_popups_that_hold_places(&mut self, cursor_node_is_there: bool) {
        self.range_edit = None;
        self.rename_group = None;
        self.selector = None;
        self.target_selector = None;
        self.modulate = None;
        self.group_assign = None;
        self.preset_selector = None;
        if !cursor_node_is_there {
            self.editing = None;
        }
    }

    /// The cursor is on another node: what was typed into the parameter filter was for the other one.
    fn sync_param_state(&mut self) {
        self.param_filter_input = TextInputState::new("");
        self.param_filtering = false;
        self.sync_param_len();
    }

    /// The parameter pane has as many rows as the node the cursor is on has parameters.
    fn sync_param_len(&mut self) {
        let sel = self.chain_state.selected;
        if sel < self.tree_entries.len() {
            let addr = &self.tree_entries[sel].address;
            let param_len = match *addr {
                TreeAddress::Pattern(inst) => {
                    self.ctl.model.instruments.get(inst)
                        .and_then(|n| n.pattern.as_ref())
                        .map_or(0, |p| {
                            let mut n = 4; // Length + Enabled + Loop + Transpose
                            if !p.events.is_empty() { n += 1; } // Notes info
                            n
                        })
                }
                TreeAddress::Modulator { inst, index } => {
                    self.modulator(inst, index).map_or(0, modulator_param_len)
                }
                TreeAddress::GroupModulator { group, index } => {
                    self.group_modulator(group, index).map_or(0, modulator_param_len)
                }
                // A group's bus pane shows two host-side params: Volume + Pan.
                TreeAddress::Group(_) => 2,
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
                TreeAddress::Instrument(_) | TreeAddress::Effect { .. }
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
            TreeAddress::Instrument(_) | TreeAddress::Effect { .. }
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
            TreeAddress::Modulator { inst, index } => {
                let m = self.modulator(inst, index)?;
                modulator_param_range(m, self.param_state.selected)
            }
            TreeAddress::GroupModulator { group, index } => {
                let m = self.group_modulator(group, index)?;
                modulator_param_range(m, self.param_state.selected)
            }
            TreeAddress::Group(_) => match self.param_state.selected {
                0 => Some((0.0, 4.0)),  // volume
                1 => Some((-1.0, 1.0)), // pan
                _ => None,
            },
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
            TreeAddress::Modulator { inst, index } => {
                let Some(m) = self.modulator(inst, index) else { return false };
                modulator_param_is_enum(m, self.param_state.selected)
            }
            TreeAddress::GroupModulator { group, index } => {
                let Some(m) = self.group_modulator(group, index) else { return false };
                modulator_param_is_enum(m, self.param_state.selected)
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
            TreeAddress::Pattern(_)
            | TreeAddress::Modulator { .. }
            | TreeAddress::Group(_)
            | TreeAddress::GroupModulator { .. } => None,
            TreeAddress::Instrument(inst) => {
                self.ctl.model.instruments.get(inst)?.instrument.as_ref()
            }
            TreeAddress::Effect { inst, index } => {
                self.ctl.model.instruments.get(inst)?.effects.get(index)
            }
            TreeAddress::GroupEffect { group, index } => {
                self.ctl.model.groups.get(group)?.effects.get(index)
            }
        }
    }

    fn selected_address(&self) -> Option<&TreeAddress> {
        self.tree_entries.get(self.chain_state.selected).map(|e| &e.address)
    }

    /// A lane modulator by instrument + lane-global index.
    fn modulator(&self, inst: usize, mod_index: usize) -> Option<&ModulatorSlot> {
        self.ctl.model.instruments.get(inst)?.modulators.get(mod_index)
    }

    /// A group-scoped modulator by group + index.
    fn group_modulator(&self, group: usize, mod_index: usize) -> Option<&ModulatorSlot> {
        self.ctl.model.groups.get(group)?.modulators.get(mod_index)
    }

    fn selector_items(&self, mode: SelectorMode) -> Vec<FilterListItem> {
        self.ctl
            .catalog
            .plugins()
            .iter()
            .enumerate()
            .filter(|(_, e)| match mode {
                SelectorMode::Instrument => e.is_instrument,
                SelectorMode::Effect | SelectorMode::GroupEffect(_) => !e.is_instrument,
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
            .collect()
    }

    fn open_selector(&mut self, mode: SelectorMode) {
        log::info!("open_selector: mode={:?}", mode);
        let items = self.selector_items(mode);

        let mut filter = FilterListState::new();
        filter.apply_filter(&items);

        self.selector = Some(SelectorState {
            mode,
            filter,
            items,
        });
    }

    /// Rebuild the rows of an open selector popup from the catalog, keeping its
    /// filter text, so that rows appear as the background scan progresses.
    fn refresh_selector(&mut self) {
        if let Some(mode) = self.selector.as_ref().map(|sel| sel.mode) {
            let items = self.selector_items(mode);
            if let Some(sel) = self.selector.as_mut() {
                sel.items = items;
                sel.filter.apply_filter(&sel.items);
            }
        }
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
        let entry = &self.ctl.catalog.plugins()[chosen];

        // Determine which instrument to operate on from current selection.
        let inst = self.selected_inst().unwrap_or(0);

        // Load the real plugin, and put it where it goes. (An effect that cannot be added, because it will
        // not load or its outputs are not the chain's, leaves the session as it was: the controller says why.)
        let source = entry.id.clone();
        log::info!("Loading plugin '{}' (id={}) into inst={}", entry.name, source, inst);
        match sel.mode {
            SelectorMode::Instrument => {
                let (loaded, slot) = match self.ctl.load_plugin(&source) {
                    Some(x) => x,
                    None => return,
                };
                self.ctl.set_instrument(inst, loaded, slot);
            }
            SelectorMode::Effect => {
                if let Err(e) = self.ctl.add_effect(inst, &source) {
                    log::error!("Cannot add '{source}': {e}");
                    return;
                }
            }
            SelectorMode::GroupEffect(group) => {
                if let Err(e) = self.ctl.add_bus_effect(group, &source) {
                    log::error!("Cannot add '{source}' to the bus: {e}");
                    return;
                }
            }
        }

        self.ctl.dirty = true;
        self.rebuild_tree();
    }

    /// Get the instrument index for the currently selected tree entry.
    fn selected_inst(&self) -> Option<usize> {
        self.selected_address()?.inst()
    }

    fn open_target_selector(&mut self, inst: usize, mod_index: usize) {
        let inst_node = match self.ctl.model.instruments.get(inst) {
            Some(n) => n,
            None => return,
        };

        let mut entries = Vec::new();
        let mut items = Vec::new();

        // Plugin parameters across the whole chain (instrument + effects),
        // labelled by plugin and routed by slot.
        for t in self.ctl.model.param_targets(ModulatorLocation::Lane(inst)) {
            let idx = entries.len();
            // `label` is the bare param name (stored on the target and
            // serialized); the selector list shows plugin + param columns.
            items.push(FilterListItem { cells: vec![t.plugin_name, t.param_name.clone()], index: idx });
            entries.push(TargetEntry {
                label: t.param_name,
                slot: t.slot,
                kind: t.kind,
                param_min: t.param_min,
                param_max: t.param_max,
                base_value: t.base_value,
            });
        }

        // Sibling lane modulators (cross-mod).
        for (sib_idx, sib) in inst_node.modulators.iter().enumerate() {
            if sib_idx == mod_index {
                continue; // Skip self.
            }
            let prefix = format!("Mod {sib_idx} ");
            let push = |entries: &mut Vec<TargetEntry>, items: &mut Vec<FilterListItem>, label: String, kind, min, max, base| {
                let idx = entries.len();
                entries.push(TargetEntry { label: label.clone(), slot: 0, kind, param_min: min, param_max: max, base_value: base });
                items.push(FilterListItem { cells: vec!["Mod".into(), label], index: idx });
            };
            match &sib.source {
                ModSourceSlot::Lfo { rate, .. } => {
                    push(&mut entries, &mut items, format!("{prefix}rate"),
                        crate::plugin::chain::ModTargetKind::ModulatorRate { mod_index: sib_idx }, 0.01, 50.0, *rate);
                }
                ModSourceSlot::Envelope { attack, decay, sustain, release } => {
                    push(&mut entries, &mut items, format!("{prefix}attack"),
                        crate::plugin::chain::ModTargetKind::ModulatorAttack { mod_index: sib_idx }, 0.001, 10.0, *attack);
                    push(&mut entries, &mut items, format!("{prefix}decay"),
                        crate::plugin::chain::ModTargetKind::ModulatorDecay { mod_index: sib_idx }, 0.001, 10.0, *decay);
                    push(&mut entries, &mut items, format!("{prefix}sustain"),
                        crate::plugin::chain::ModTargetKind::ModulatorSustain { mod_index: sib_idx }, 0.0, 1.0, *sustain);
                    push(&mut entries, &mut items, format!("{prefix}release"),
                        crate::plugin::chain::ModTargetKind::ModulatorRelease { mod_index: sib_idx }, 0.001, 10.0, *release);
                }
                // MidiLearn has no editable source params — only its target
                // depths (added below) can be cross-modded.
                ModSourceSlot::MidiLearn { .. } => {}
            }
            for (tgt_idx, tgt) in sib.targets.iter().enumerate() {
                push(&mut entries, &mut items, format!("{prefix}{} depth", tgt.param_name),
                    crate::plugin::chain::ModTargetKind::ModulatorDepth { mod_index: sib_idx, target_index: tgt_idx }, DEPTH_MIN, DEPTH_MAX, tgt.depth);
            }
        }

        let mut filter = FilterListState::new();
        filter.apply_filter(&items);

        self.target_selector = Some(TargetSelectorState {
            filter,
            items,
            entries,
            scope: ModulatorLocation::Lane(inst),
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

        self.ctl.add_target(
            ts.scope,
            ts.mod_index,
            NewTarget {
                kind: entry.kind.clone(),
                slot: entry.slot,
                param_name: entry.label.clone(),
                depth: NEW_CABLE_DEPTH,
                base_value: entry.base_value,
                param_min: entry.param_min,
                param_max: entry.param_max,
            },
        );
        self.ctl.dirty = true;
        self.rebuild_tree();
    }

    /// Open the "modulate <param>" popup for the parameter currently selected
    /// in the param pane (a plugin parameter on the Instrument/Effect node).
    fn open_modulate(&mut self) {
        let sel = self.chain_state.selected;
        if sel >= self.tree_entries.len() {
            return;
        }
        let addr = self.tree_entries[sel].address;
        let (inst, slot) = match addr {
            TreeAddress::Instrument(inst) => (inst, 0),
            TreeAddress::Effect { inst, index } => (inst, index + 1),
            _ => return,
        };
        let pa = match self.real_param_index() {
            Some(i) => i,
            None => return,
        };
        let param = match self.plugin_at(&addr).and_then(|p| p.params.get(pa)) {
            Some(p) => p,
            None => return,
        };
        let (param_index, param_name, param_min, param_max, base_value) =
            (param.index, param.name.clone(), param.min, param.max, param.value);

        // Choices: new LFO, new envelope, then each existing lane modulator.
        let mut choices = vec![ModulateChoice::NewLfo, ModulateChoice::NewEnvelope];
        let mut items = vec![
            FilterListItem { cells: vec![format!("New LFO → {param_name}")], index: 0 },
            FilterListItem { cells: vec![format!("New envelope → {param_name}")], index: 1 },
        ];
        if let Some(node) = self.ctl.model.instruments.get(inst) {
            for (i, m) in node.modulators.iter().enumerate() {
                let label = m.source.label();
                let idx = choices.len();
                choices.push(ModulateChoice::Existing(i));
                items.push(FilterListItem { cells: vec![format!("attach → {label}")], index: idx });
            }
        }

        let mut filter = FilterListState::new();
        filter.apply_filter(&items);
        self.modulate = Some(ModulateState {
            scope: ModulatorLocation::Lane(inst),
            slot,
            target_kind: crate::plugin::chain::ModTargetKind::PluginParam {
                slot,
                param_index,
            },
            param_name,
            param_min,
            param_max,
            base_value,
            choices,
            filter,
            items,
        });
    }

    /// Open the "modulate" popup for a parameter of a *modulator* (its rate /
    /// ADSR stage / a target depth) — i.e. cross-modulation: a sibling modulator
    /// (new or existing, in the same lane or group rack) drives this field. The
    /// inverse of picking this modulator as a target via `t`. Enum / separator
    /// rows aren't modulatable, and the modulator can't modulate itself.
    fn open_modulate_cross_mod(&mut self) {
        let sel = self.chain_state.selected;
        if sel >= self.tree_entries.len() {
            return;
        }
        let (scope, mod_index) = match self.tree_entries[sel].address {
            TreeAddress::Modulator { inst, index } => (ModulatorLocation::Lane(inst), index),
            TreeAddress::GroupModulator { group, index } => (ModulatorLocation::Group(group), index),
            _ => return,
        };
        let pa = self.param_state.selected;
        // Resolve the cross-mod target kind (+ range / base / label) for this row.
        let resolved = {
            let m = match scope {
                ModulatorLocation::Lane(inst) => self.modulator(inst, mod_index),
                ModulatorLocation::Group(group) => self.group_modulator(group, mod_index),
            };
            m.and_then(|m| modulator_row_to_cross_mod(m, mod_index, pa))
        };
        let Some((kind, param_min, param_max, base_value, param_name)) = resolved else {
            return;
        };

        // Choices: new LFO / envelope, then each sibling modulator in the same
        // rack except this one (no self-modulation).
        let mut choices = vec![ModulateChoice::NewLfo, ModulateChoice::NewEnvelope];
        let mut items = vec![
            FilterListItem { cells: vec![format!("New LFO → {param_name}")], index: 0 },
            FilterListItem { cells: vec![format!("New envelope → {param_name}")], index: 1 },
        ];
        let rack_len = match scope {
            ModulatorLocation::Lane(inst) => self.ctl.model.instruments.get(inst).map_or(0, |n| n.modulators.len()),
            ModulatorLocation::Group(group) => self.ctl.model.groups.get(group).map_or(0, |g| g.modulators.len()),
        };
        for i in 0..rack_len {
            if i == mod_index {
                continue; // can't modulate itself
            }
            let label = match scope {
                ModulatorLocation::Lane(inst) => self.modulator(inst, i).map(|m| m.source.label()),
                ModulatorLocation::Group(group) => self.group_modulator(group, i).map(|m| m.source.label()),
            };
            if let Some(label) = label {
                let idx = choices.len();
                choices.push(ModulateChoice::Existing(i));
                items.push(FilterListItem { cells: vec![format!("attach → {label}")], index: idx });
            }
        }

        let mut filter = FilterListState::new();
        filter.apply_filter(&items);
        self.modulate = Some(ModulateState {
            scope,
            slot: 0, // unused for cross-mod target kinds
            target_kind: kind,
            param_name,
            param_min,
            param_max,
            base_value,
            choices,
            filter,
            items,
        });
    }

    fn confirm_modulate(&mut self) {
        let ms = match self.modulate.take() {
            Some(m) => m,
            None => return,
        };
        let choice = match ms.filter.selected_item(&ms.items) {
            Some(item) => ms.choices[item.index],
            None => return,
        };

        // Resolve the target modulator index, creating one in the right rack if
        // requested.
        let created = match choice {
            ModulateChoice::NewLfo => self.ctl.add_modulator(ms.scope, ModSourceSlot::new_lfo()),
            ModulateChoice::NewEnvelope => self.ctl.add_modulator(ms.scope, ModSourceSlot::new_envelope()),
            ModulateChoice::Existing(i) => Some(i),
        };
        let Some(mod_index) = created else { return };

        // Bind the parameter as a target of that modulator.
        self.ctl.add_target(
            ms.scope,
            mod_index,
            NewTarget {
                kind: ms.target_kind,
                slot: ms.slot,
                param_name: ms.param_name,
                depth: NEW_CABLE_DEPTH,
                base_value: ms.base_value,
                param_min: ms.param_min,
                param_max: ms.param_max,
            },
        );
        self.ctl.dirty = true;
        self.rebuild_tree();
    }

    /// Open the target selector for a group modulator. Candidate targets span
    /// the group's world: every member instrument's chain (`GroupMember`), the
    /// group's own bus effects (`GroupBus`), and sibling group modulators.
    fn open_group_target_selector(&mut self, group: usize, mod_index: usize) {
        use crate::plugin::chain::ModTargetKind;
        let group_node = match self.ctl.model.groups.get(group) {
            Some(g) => g,
            None => return,
        };
        let mut entries = Vec::new();
        let mut items = Vec::new();

        // Member instruments' params (instrument slot 0 + effects 1..N), labelled
        // by member ordinal and plugin.
        let params = self.ctl.model.param_targets(ModulatorLocation::Group(group));
        for t in &params {
            let ModTargetKind::GroupMember { member, .. } = t.kind else { continue };
            let idx = entries.len();
            entries.push(TargetEntry {
                label: t.param_name.clone(),
                slot: t.slot,
                kind: t.kind.clone(),
                param_min: t.param_min,
                param_max: t.param_max,
                base_value: t.base_value,
            });
            items.push(FilterListItem { cells: vec![format!("M{member} {}", t.plugin_name), t.param_name.clone()], index: idx });
        }
        let members: Vec<&InstrumentNode> =
            self.ctl.model.instruments.iter().filter(|n| n.group == Some(group)).collect();

        // Member instruments' modulators (cross-mod a member's LFO/envelope).
        for (ord, member) in members.iter().enumerate() {
            for (mk, mm) in member.modulators.iter().enumerate() {
                let prefix = format!("M{ord} Mod{mk}");
                let mut push_mm = |fname: &str,
                                   field: crate::plugin::chain::CrossModField,
                                   min: f32,
                                   max: f32,
                                   base: f32| {
                    let idx = entries.len();
                    entries.push(TargetEntry {
                        label: format!("{prefix} {fname}"),
                        slot: 0,
                        kind: ModTargetKind::GroupMemberMod { member: ord, mod_index: mk, field },
                        param_min: min,
                        param_max: max,
                        base_value: base,
                    });
                    items.push(FilterListItem {
                        cells: vec![prefix.clone(), fname.to_string()],
                        index: idx,
                    });
                };
                use crate::plugin::chain::CrossModField;
                match &mm.source {
                    ModSourceSlot::Lfo { rate, .. } => {
                        push_mm("rate", CrossModField::Rate, 0.01, 50.0, *rate);
                    }
                    ModSourceSlot::Envelope { attack, decay, sustain, release } => {
                        push_mm("attack", CrossModField::Attack, 0.001, 10.0, *attack);
                        push_mm("decay", CrossModField::Decay, 0.001, 10.0, *decay);
                        push_mm("sustain", CrossModField::Sustain, 0.0, 1.0, *sustain);
                        push_mm("release", CrossModField::Release, 0.001, 10.0, *release);
                    }
                    ModSourceSlot::MidiLearn { .. } => {} // only its target depths below
                }
                for (ti, t) in mm.targets.iter().enumerate() {
                    push_mm(&format!("{} depth", t.param_name), CrossModField::Depth(ti), DEPTH_MIN, DEPTH_MAX, t.depth);
                }
            }
        }

        // The group's own bus effects.
        for t in params.iter().filter(|t| matches!(t.kind, ModTargetKind::GroupBus { .. })) {
            let idx = entries.len();
            entries.push(TargetEntry {
                label: t.param_name.clone(),
                slot: t.slot,
                kind: t.kind.clone(),
                param_min: t.param_min,
                param_max: t.param_max,
                base_value: t.base_value,
            });
            items.push(FilterListItem { cells: vec![format!("Bus {}", t.plugin_name), t.param_name.clone()], index: idx });
        }

        // Sibling group modulators (cross-mod).
        for (sib_idx, sib) in group_node.modulators.iter().enumerate() {
            if sib_idx == mod_index {
                continue;
            }
            let prefix = format!("Mod {sib_idx} ");
            let push = |entries: &mut Vec<TargetEntry>, items: &mut Vec<FilterListItem>, label: String, kind, min, max, base| {
                let idx = entries.len();
                entries.push(TargetEntry { label: label.clone(), slot: 0, kind, param_min: min, param_max: max, base_value: base });
                items.push(FilterListItem { cells: vec!["Mod".into(), label], index: idx });
            };
            match &sib.source {
                ModSourceSlot::Lfo { rate, .. } => {
                    push(&mut entries, &mut items, format!("{prefix}rate"),
                        ModTargetKind::ModulatorRate { mod_index: sib_idx }, 0.01, 50.0, *rate);
                }
                ModSourceSlot::Envelope { attack, decay, sustain, release } => {
                    push(&mut entries, &mut items, format!("{prefix}attack"),
                        ModTargetKind::ModulatorAttack { mod_index: sib_idx }, 0.001, 10.0, *attack);
                    push(&mut entries, &mut items, format!("{prefix}decay"),
                        ModTargetKind::ModulatorDecay { mod_index: sib_idx }, 0.001, 10.0, *decay);
                    push(&mut entries, &mut items, format!("{prefix}sustain"),
                        ModTargetKind::ModulatorSustain { mod_index: sib_idx }, 0.0, 1.0, *sustain);
                    push(&mut entries, &mut items, format!("{prefix}release"),
                        ModTargetKind::ModulatorRelease { mod_index: sib_idx }, 0.001, 10.0, *release);
                }
                ModSourceSlot::MidiLearn { .. } => {} // only its target depths below
            }
            for (tgt_idx, tgt) in sib.targets.iter().enumerate() {
                push(&mut entries, &mut items, format!("{prefix}{} depth", tgt.param_name),
                    ModTargetKind::ModulatorDepth { mod_index: sib_idx, target_index: tgt_idx }, DEPTH_MIN, DEPTH_MAX, tgt.depth);
            }
        }

        let mut filter = FilterListState::new();
        filter.apply_filter(&items);
        self.target_selector = Some(TargetSelectorState {
            filter,
            items,
            entries,
            scope: ModulatorLocation::Group(group),
            mod_index,
        });
    }

    /// Open the "modulate <param>" popup for a group bus-effect parameter.
    /// Lets the user create a new group modulator or attach to an existing one.
    fn open_group_modulate(&mut self) {
        let sel = self.chain_state.selected;
        if sel >= self.tree_entries.len() {
            return;
        }
        let addr = self.tree_entries[sel].address;
        let (group, eff_idx) = match addr {
            TreeAddress::GroupEffect { group, index } => (group, index),
            _ => return,
        };
        let pa = match self.real_param_index() {
            Some(i) => i,
            None => return,
        };
        let param = match self.plugin_at(&addr).and_then(|p| p.params.get(pa)) {
            Some(p) => p,
            None => return,
        };
        let (param_index, param_name, param_min, param_max, base_value) =
            (param.index, param.name.clone(), param.min, param.max, param.value);

        let mut choices = vec![ModulateChoice::NewLfo, ModulateChoice::NewEnvelope];
        let mut items = vec![
            FilterListItem { cells: vec![format!("New LFO → {param_name}")], index: 0 },
            FilterListItem { cells: vec![format!("New envelope → {param_name}")], index: 1 },
        ];
        if let Some(node) = self.ctl.model.groups.get(group) {
            for (i, m) in node.modulators.iter().enumerate() {
                let label = m.source.label();
                let idx = choices.len();
                choices.push(ModulateChoice::Existing(i));
                items.push(FilterListItem { cells: vec![format!("attach → {label}")], index: idx });
            }
        }

        let mut filter = FilterListState::new();
        filter.apply_filter(&items);
        self.modulate = Some(ModulateState {
            scope: ModulatorLocation::Group(group),
            slot: 0,
            target_kind: crate::plugin::chain::ModTargetKind::GroupBus {
                effect_index: eff_idx,
                param_index,
            },
            param_name,
            param_min,
            param_max,
            base_value,
            choices,
            filter,
            items,
        });
    }

    /// MIDI-learn: create an armed `MidiLearn` modulator bound to the parameter
    /// currently selected in the param pane. Works for plugin params
    /// (instrument / effect / group-bus effect) and modulator fields (binding a
    /// control to an LFO's rate, an envelope stage, or a target depth). The
    /// audio thread captures the next CC / pitch-bend and notifies back.
    fn learn_selected_param(&mut self) {
        use crate::plugin::chain::ModTargetKind;
        let sel = self.chain_state.selected;
        if sel >= self.tree_entries.len() {
            return;
        }
        let addr = self.tree_entries[sel].address;

        // (scope, target kind, min, max, base value, label)
        let (scope, kind, min, max, base, label): (ModulatorLocation, ModTargetKind, f32, f32, f32, String) =
            match addr {
                TreeAddress::Instrument(inst) | TreeAddress::Effect { inst, .. } => {
                    let pa = match self.real_param_index() {
                        Some(i) => i,
                        None => return,
                    };
                    let Some(p) = self.plugin_at(&addr).and_then(|p| p.params.get(pa)) else {
                        return;
                    };
                    (
                        ModulatorLocation::Lane(inst),
                        ModTargetKind::PluginParam { slot: addr.slot(), param_index: p.index },
                        p.min, p.max, p.value, p.name.clone(),
                    )
                }
                TreeAddress::GroupEffect { group, index } => {
                    let pa = match self.real_param_index() {
                        Some(i) => i,
                        None => return,
                    };
                    let Some(p) = self.plugin_at(&addr).and_then(|p| p.params.get(pa)) else {
                        return;
                    };
                    (
                        ModulatorLocation::Group(group),
                        ModTargetKind::GroupBus { effect_index: index, param_index: p.index },
                        p.min, p.max, p.value, p.name.clone(),
                    )
                }
                TreeAddress::Modulator { inst, index } => {
                    let Some(m) = self.modulator(inst, index) else { return };
                    let Some((kind, min, max, base, label)) =
                        modulator_row_to_cross_mod(m, index, self.param_state.selected)
                    else {
                        return;
                    };
                    (ModulatorLocation::Lane(inst), kind, min, max, base, label)
                }
                TreeAddress::GroupModulator { group, index } => {
                    let Some(m) = self.group_modulator(group, index) else { return };
                    let Some((kind, min, max, base, label)) =
                        modulator_row_to_cross_mod(m, index, self.param_state.selected)
                    else {
                        return;
                    };
                    (ModulatorLocation::Group(group), kind, min, max, base, label)
                }
                _ => return,
            };

        // Create an armed MidiLearn modulator in the right rack.
        let Some(mod_index) = self.ctl.add_modulator(scope, ModSourceSlot::new_midi_learn()) else { return };

        // Bind the target (depth 1.0 = full-range absolute control).
        self.ctl.add_target(
            scope,
            mod_index,
            NewTarget {
                kind,
                slot: addr.slot(),
                param_name: label,
                depth: 1.0,
                base_value: base,
                param_min: min,
                param_max: max,
            },
        );
        self.ctl.dirty = true;
        self.rebuild_tree();
    }

    /// Open the "assign to group" popup for instrument `inst`.
    fn open_group_assign(&mut self, inst: usize) {
        let current = self.ctl.model.instruments.get(inst).and_then(|n| n.group);
        let mut choices = vec![GroupAssignChoice::New];
        let mut items = vec![FilterListItem { cells: vec!["New group".into()], index: 0 }];
        if current.is_some() {
            choices.push(GroupAssignChoice::Ungroup);
            items.push(FilterListItem { cells: vec!["Ungroup".into()], index: 1 });
        }
        for (g_idx, group) in self.ctl.model.groups.iter().enumerate() {
            let name = group.display_name(g_idx);
            let marker = if current == Some(g_idx) { " (current)" } else { "" };
            let idx = choices.len();
            choices.push(GroupAssignChoice::Existing(g_idx));
            items.push(FilterListItem { cells: vec![format!("{name}{marker}")], index: idx });
        }
        let mut filter = FilterListState::new();
        filter.apply_filter(&items);
        self.group_assign = Some(GroupAssignState { inst, choices, filter, items });
    }

    fn confirm_group_assign(&mut self) {
        let gs = match self.group_assign.take() {
            Some(g) => g,
            None => return,
        };
        let choice = match gs.filter.selected_item(&gs.items) {
            Some(item) => gs.choices[item.index],
            None => return,
        };
        let inst = gs.inst;
        let new_group = match choice {
            GroupAssignChoice::Ungroup => None,
            GroupAssignChoice::Existing(g) => Some(g),
            GroupAssignChoice::New => {
                Some(self.ctl.add_group())
            }
        };
        let _ = self.ctl.cmd_tx.send(GraphCommand::SetLaneGroup { inst, group: new_group });
        // Keep group-modulator member ordinals in sync with membership (mirrors
        // the audio thread's SetLaneGroup fixups).
        let old = lane_member_ordinal(&self.ctl.model.instruments, inst);
        if let Some(node) = self.ctl.model.instruments.get_mut(inst) {
            node.group = new_group;
        }
        let new = lane_member_ordinal(&self.ctl.model.instruments, inst);
        let old_group = old.map(|(g, _)| g);
        let new_group_idx = new.map(|(g, _)| g);
        if old_group != new_group_idx {
            if let Some((og, oo)) = old {
                if let Some(g) = self.ctl.model.groups.get_mut(og) {
                    fixup_group_member_after_remove(&mut g.modulators, oo);
                }
            }
            if let Some((ng, no)) = new {
                if let Some(g) = self.ctl.model.groups.get_mut(ng) {
                    shift_group_member_after_insert(&mut g.modulators, no);
                }
            }
        }
        self.ctl.dirty = true;
        self.rebuild_tree();
    }

    /// Open the preset selector for the selected plugin slot. inst+slot
    fn open_scale_selector(&mut self) {
        let entries = piano_view::scale_picker_entries();
        let items: Vec<FilterListItem> = entries
            .iter()
            .enumerate()
            .map(|(i, (name, _root, _idx))| FilterListItem {
                cells: vec![name.clone()],
                index: i,
            })
            .collect();
        let mut filter = FilterListState::new();
        filter.apply_filter(&items);
        // Pre-select the current scale if it's in the list.
        let current = self.ctl.piano_filter.scale();
        if let Some(i) = entries
            .iter()
            .position(|(_, r, s)| *r == current.root && *s == current.scale_idx)
        {
            filter.list.selected = i;
        }
        self.scale_selector = Some(ScaleSelectorState {
            filter,
            items,
            entries: entries.into_iter().map(|(_, r, s)| (r, s)).collect(),
        });
    }

    fn confirm_scale_selector(&mut self) {
        let ss = match self.scale_selector.take() {
            Some(s) => s,
            None => return,
        };
        let item = match ss.filter.selected_item(&ss.items) {
            Some(it) => it,
            None => return,
        };
        let (root, scale_idx) = ss.entries[item.index];
        let current = self.ctl.piano_filter.scale();
        if current.root != root || current.scale_idx != scale_idx {
            let new_scale = ScaleSetting { root, scale_idx };
            self.ctl.piano_filter.set_scale(new_scale);
            self.ctl.dirty = true;
            log::info!("Scale set to {}", new_scale.display());
        }
    }

    /// matches the addressing used by GraphCommand: slot=0 = instrument,
    /// slot=1..N = effects.
    fn open_preset_selector(&mut self, inst: usize, slot: usize) {
        let plugin = if slot == 0 {
            self.ctl.model.instruments.get(inst).and_then(|n| n.instrument.as_ref())
        } else {
            self.ctl.model.instruments.get(inst).and_then(|n| n.effects.get(slot - 1))
        };
        let plugin = match plugin {
            Some(p) => p,
            None => return,
        };
        if plugin.presets.is_empty() {
            log::info!("Plugin '{}' has no presets", plugin.name);
            return;
        }

        let presets = plugin.presets.clone();
        let items: Vec<FilterListItem> = presets
            .iter()
            .enumerate()
            .map(|(idx, p)| FilterListItem {
                cells: vec![p.name.clone()],
                index: idx,
            })
            .collect();
        let mut filter = FilterListState::new();
        filter.apply_filter(&items);
        // Pre-select the current preset if any.
        if let Some(ref current) = plugin.current_preset {
            if let Some(i) = presets.iter().position(|p| p.name == *current) {
                filter.list.selected = i;
            }
        }
        self.preset_selector = Some(PresetSelectorState {
            filter,
            items,
            presets,
            inst,
            slot,
        });
    }

    fn confirm_preset_selector(&mut self) {
        let Some(ps) = self.preset_selector.take() else { return };
        let Some(item) = ps.filter.selected_item(&ps.items) else { return };
        let preset = ps.presets[item.index].id.clone();
        let lane = self.ctl.model.instruments.get(ps.inst);
        let plugin = if ps.slot == 0 {
            lane.and_then(|n| n.instrument.as_ref())
        } else {
            lane.and_then(|n| n.effects.get(ps.slot - 1))
        };
        let Some(plugin) = plugin else { return };
        let intent = Intent::LoadPreset { node: plugin.node, preset };
        self.apply(intent);
    }

    /// Set a group's host-side bus param (0 = volume, 1 = pan) to what `f` makes
    /// of its value now; the controller clamps it to the param's range.
    fn set_group_bus_param(&mut self, group: usize, pa: usize, f: impl FnOnce(f32) -> f32) {
        let Some(g) = self.ctl.model.groups.get(group) else {
            return;
        };
        let (gain, current) = match pa {
            0 => (Gain::GroupVolume, g.volume),
            1 => (Gain::GroupPan, g.pan),
            _ => return,
        };
        let intent = Intent::SetGain { node: g.node, gain, value: f(current) };
        self.apply(intent);
    }

    fn adjust_param(&mut self, delta: f32) {
        let sel = self.chain_state.selected;
        if sel >= self.tree_entries.len() {
            return;
        }
        let addr = self.tree_entries[sel].address;

        // Group bus effect params take a dedicated command (no instrument).
        if let TreeAddress::GroupEffect { .. } = addr {
            let pa = match self.real_param_index() {
                Some(i) => i,
                None => return,
            };
            self.set_plugin_param(&addr, pa, |v| v + delta);
            return;
        }

        // The rows of a modulator, a lane's or a group's.
        if let TreeAddress::Modulator { .. } | TreeAddress::GroupModulator { .. } = addr {
            self.step_modulator_row(addr, self.param_state.selected, delta);
            return;
        }

        // A group's host-side bus params (Volume + Pan).
        if let TreeAddress::Group(group) = addr {
            let pa = self.param_state.selected;
            self.set_group_bus_param(group, pa, |v| v + delta);
            return;
        }

        let inst = match addr.inst() {
            Some(i) => i,
            None => return,
        };

        // Handle pattern params separately.
        if let TreeAddress::Pattern(_) = addr {
            let pa = self.param_state.selected;
            self.adjust_pattern_param(inst, pa, delta);
            return;
        }

        let pa = match self.real_param_index() {
            Some(i) => i,
            None => return,
        };
        self.set_plugin_param(&addr, pa, |v| v + delta);
    }

    /// Set the parameter in row `row` of the plugin at `addr` to what `f` makes of
    /// its value now; the controller clamps it (and rounds an enum to a label).
    fn set_plugin_param(&mut self, addr: &TreeAddress, row: usize, f: impl FnOnce(f32) -> f32) {
        let Some(plugin) = self.plugin_at(addr) else { return };
        let Some(param) = plugin.params.get(row) else { return };
        let intent = Intent::SetParam { node: plugin.node, param: param.index, value: f(param.value) };
        self.apply(intent);
    }

    /// Hand an intent to the controller. A refusal is logged and the view
    /// carries on as it was.
    fn apply(&mut self, intent: Intent) -> Option<Applied> {
        match self.ctl.apply(intent) {
            Ok(applied) => {
                self.react_to(&applied);
                Some(applied)
            }
            Err(e) => {
                log::warn!("{e}");
                None
            }
        }
    }

    /// Bring what the view derives from the model up to date with what `applied` did.
    fn react_to(&mut self, applied: &Applied) {
        // Nodes or cables came or went: the tree is built from the model again, and what is open that is for a
        // place in it is closed if the places have moved.
        if applied.restructured {
            let places = self.tree_places();
            let on = self.node_under_cursor();
            self.rebuild_tree();
            if self.tree_places() != places {
                let cursor_node_is_there = self.node_under_cursor() == on;
                self.close_popups_that_hold_places(cursor_node_is_there);
            }
        }
        for event in &applied.events {
            match event {
                // The group header shows the volume.
                intent::Event::GainSet { gain: Gain::GroupVolume, .. } => self.rebuild_tree(),
                // The tree shows a modulator's rate and waveform, and its cables' depths.
                intent::Event::ModulatorFieldSet { .. } | intent::Event::EdgeDepthSet { .. } => self.rebuild_tree(),
                intent::Event::ParamSet { .. }
                | intent::Event::GainSet { .. }
                | intent::Event::PresetLoaded { .. }
                // (The tab shows what the controller says is unsaved, which is not the case now.)
                | intent::Event::Saved { .. } => {}
            }
        }
    }

    /// The modulator at `addr`, if the address is one: a lane's or a group's.
    fn modulator_at(&self, addr: TreeAddress) -> Option<&ModulatorSlot> {
        match addr {
            TreeAddress::Modulator { inst, index } => self.modulator(inst, index),
            TreeAddress::GroupModulator { group, index } => self.group_modulator(group, index),
            _ => None,
        }
    }

    /// Left or Right on row `row` of the modulator at `addr`: a step of `delta` in the
    /// number or the depth the row is, one waveform along, or, on the Type row, the other
    /// kind of source. What a row is, its range and the commands the engine gets are the
    /// intents'; a row that is none of these does nothing.
    fn step_modulator_row(&mut self, addr: TreeAddress, row: usize, delta: f32) {
        let Some(m) = self.modulator_at(addr) else { return };
        let intent = match modulator_row(m, row) {
            Some(ModulatorRow::Type) => return self.switch_modulator_source(addr),
            Some(ModulatorRow::Waveform) => {
                let ModSourceSlot::Lfo { waveform, .. } = m.source else { return };
                let next = if delta > 0.0 { waveform.next() } else { waveform.prev() };
                Intent::SetModulatorField { node: m.node, field: ModulatorField::Waveform, value: next.to_index() as f32 }
            }
            _ => match m.row_intent(row, |value| value + delta) {
                Some(intent) => intent,
                None => return,
            },
        };
        self.apply(intent);
    }

    /// A number typed for row `row` of the modulator at `addr`.
    fn set_modulator_row(&mut self, addr: TreeAddress, row: usize, value: f32) {
        let Some(intent) = self.modulator_at(addr).and_then(|m| m.row_intent(row, |_| value)) else { return };
        self.apply(intent);
    }

    /// The Type row: an LFO becomes an envelope with the defaults of one, and an envelope
    /// an LFO. A change of what kind of node it is, which no intent makes yet (a browser
    /// is told by the next `resync`).
    fn switch_modulator_source(&mut self, addr: TreeAddress) {
        let (at, index) = match addr {
            TreeAddress::Modulator { inst, index } => (ModulatorLocation::Lane(inst), index),
            TreeAddress::GroupModulator { group, index } => (ModulatorLocation::Group(group), index),
            _ => return,
        };
        let Some(m) = self.ctl.model.modulators_mut(at).and_then(|rack| rack.get_mut(index)) else { return };
        let new_source = match m.source {
            ModSourceSlot::Lfo { .. } => ModSourceSlot::new_envelope(),
            _ => ModSourceSlot::new_lfo(),
        };
        let source = mod_source_slot_to_graph(&new_source);
        m.source = new_source;
        let command = match at {
            ModulatorLocation::Lane(inst) => GraphCommand::SetModulatorSource { inst, mod_index: index, source },
            ModulatorLocation::Group(group) => GraphCommand::SetGroupModulatorSource { group, mod_index: index, source },
        };
        let _ = self.ctl.cmd_tx.send(command);
        self.ctl.dirty = true;
        self.param_state.selected = 0;
        self.rebuild_tree();
    }

    fn adjust_pattern_param(&mut self, inst: usize, pa: usize, delta: f32) {
        let pat = match self.ctl.model.instruments.get_mut(inst)
            .and_then(|n| n.pattern.as_mut())
        {
            Some(p) => p,
            None => return,
        };
        match pa {
            0 => {
                // Length (beats)
                pat.length_beats = (pat.length_beats + delta).clamp(1.0, 32.0);
                let _ = self.ctl.cmd_tx.send(GraphCommand::SetPatternLength {
                    inst, beats: pat.length_beats,
                });
                self.ctl.dirty = true;
                self.rebuild_tree();
            }
            1 => {
                // Enabled (enum toggle)
                pat.enabled = !pat.enabled;
                let _ = self.ctl.cmd_tx.send(GraphCommand::SetPatternEnabled {
                    inst, enabled: pat.enabled,
                });
                self.ctl.dirty = true;
                self.rebuild_tree();
            }
            2 => {
                // Loop (enum toggle)
                pat.looping = !pat.looping;
                let _ = self.ctl.cmd_tx.send(GraphCommand::SetPatternLooping {
                    inst, looping: pat.looping,
                });
                self.ctl.dirty = true;
            }
            3 => {
                // Transpose (enum toggle: chromatic / in-key)
                pat.in_key = !pat.in_key;
                let _ = self.ctl.cmd_tx.send(GraphCommand::SetPatternInKey {
                    inst, in_key: pat.in_key,
                });
                self.ctl.dirty = true;
            }
            _ => {} // Notes row is informational
        }
    }

    fn set_param_value(&mut self, value: f32) {
        let sel = self.chain_state.selected;
        if sel >= self.tree_entries.len() {
            return;
        }
        let addr = self.tree_entries[sel].address;

        // Group bus effect params take a dedicated command (no instrument).
        if let TreeAddress::GroupEffect { .. } = addr {
            let pa = match self.real_param_index() {
                Some(i) => i,
                None => return,
            };
            self.set_plugin_param(&addr, pa, |_| value);
            return;
        }

        // The rows of a modulator, a lane's or a group's.
        if let TreeAddress::Modulator { .. } | TreeAddress::GroupModulator { .. } = addr {
            self.set_modulator_row(addr, self.param_state.selected, value);
            return;
        }

        // A group's host-side bus params (Volume + Pan) — set absolute.
        if let TreeAddress::Group(group) = addr {
            let pa = self.param_state.selected;
            self.set_group_bus_param(group, pa, |_| value);
            return;
        }

        let inst = match addr.inst() {
            Some(i) => i,
            None => return,
        };

        // Handle pattern params.
        if let TreeAddress::Pattern(_) = addr {
            let pa = self.param_state.selected;
            if pa == 0 {
                // Length (beats) — set directly.
                let clamped = value.clamp(1.0, 32.0);
                if let Some(pat) = self.ctl.model.instruments.get_mut(inst)
                    .and_then(|n| n.pattern.as_mut())
                {
                    pat.length_beats = clamped;
                    let _ = self.ctl.cmd_tx.send(GraphCommand::SetPatternLength {
                        inst, beats: clamped,
                    });
                    self.ctl.dirty = true;
                    self.rebuild_tree();
                }
            }
            return;
        }

        let pa = match self.real_param_index() {
            Some(i) => i,
            None => return,
        };
        self.set_plugin_param(&addr, pa, |_| value);
    }
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

pub fn run(mut ctl: Controller, web: Option<&WebArgs>) -> anyhow::Result<Controller> {
    // Suppress Rust logging for the whole TUI lifetime when stderr is the
    // terminal: the background catalog scan and plugin loads log verbosely and
    // would corrupt the alternate screen. Must be set BEFORE the scan spawns,
    // since that thread starts logging immediately. Logging to a redirected
    // stderr (`tang 2> debug.log`) is left enabled. Restored at teardown.
    let prev_log_level = log::max_level();
    let stderr_is_terminal = std::io::IsTerminal::is_terminal(&std::io::stderr());
    if stderr_is_terminal {
        log::set_max_level(log::LevelFilter::Off);
    }

    // Native plugin code (CLAP preset scanners, VST3 factories, ALSA/JACK, …)
    // writes straight to the stderr fd, bypassing `log`, which would corrupt
    // the alternate screen. Redirect stderr to /dev/null for the TUI lifetime
    // when it's a terminal; when it's redirected (`tang 2> debug.log`) leave it
    // so logs/plugin diagnostics still land in the file. Restored on drop.
    let _stderr_silencer = if stderr_is_terminal {
        crate::tty::StderrSilencer::new()
    } else {
        None
    };

    // Serve the session to browsers too, if asked to. A server that cannot start (the
    // port is taken) is an error before the terminal is taken over; the address is
    // printed on stdout, where a script can find it (the TUI shows it as well).
    let mut web = web.map(|args| Web::start(&mut ctl, args)).transpose()?;
    if let Some(web) = &web {
        let status = web.status();
        println!("Web interface: {}", status.url);
        for warning in status.warnings() {
            println!("{warning}");
        }
    }

    // Scan the plugin catalog on a background thread so the TUI can draw
    // immediately — a full scan instantiates every installed plugin and can
    // take seconds. Results arrive per format and are folded in by
    // `Controller::poll`. The catalog is only needed by the selector popup.
    ctl.catalog.scan();

    let mut s = State::new(ctl);
    if let Some(web) = &web {
        s.set_web(web.status());
    }

    // Probe keyboard enhancement support (must be done before entering raw mode).
    let kitty_supported = crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false);

    // Set up terminal.
    crossterm::terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    // Disambiguate modifier combos where supported, so Ctrl+Shift+S (save as)
    // is distinguishable from Ctrl+S. Pushed after EnterAlternateScreen —
    // kitty keeps separate flag stacks for the main and alternate screens.
    if kitty_supported {
        execute!(
            stdout,
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        )?;
    }
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // (Rust logging was already suppressed at the top of this function, before
    // the background scan spawned; it's restored below at teardown.)
    let result = TerminalReader::start().and_then(|reader| event_loop(&mut terminal, &mut s, reader.events(), web.as_mut()));

    log::set_max_level(prev_log_level);

    if kitty_supported {
        let _ = execute!(terminal.backend_mut(), PopKeyboardEnhancementFlags);
    }
    execute!(
        terminal.backend_mut(),
        DisableMouseCapture,
        LeaveAlternateScreen
    )?;
    crossterm::terminal::disable_raw_mode()?;

    // The controller goes back to the caller, which shuts it down once the audio
    // stream has stopped. (The web server, if there is one, stops when this function
    // returns, and the browsers are told: before the controller is shut down.)
    result.map(|()| s.ctl).map_err(Into::into)
}

/// What happens between two frames, with or without user input: the controller
/// does its housekeeping and folds in what the plugin scan and the audio thread
/// have reported, and the view catches up with what changed.
fn tick(s: &mut State) {
    tick_at(s, Instant::now());
}

/// `tick`, at the time `now`.
fn tick_at(s: &mut State, now: Instant) {
    let polled = s.ctl.poll();
    if polled.model_changed {
        s.rebuild_tree();
    }
    if polled.catalog_changed {
        s.refresh_selector();
    }
    s.clip.update(s.ctl.telemetry.count(telemetry::MASTER.clips), now);
}

/// How often the loop wakes by itself: to fold in what the audio thread and the
/// catalog scan have reported, and to redraw what moves (the meters, the CLIP
/// light, the held notes).
const TICK: Duration = Duration::from_millis(100);

/// Run the interface until the user quits: draw, then wait for `input` (the terminal's
/// events, from a `TerminalReader`), the requests of the browsers if the session is
/// served (`web`), or the tick; handle what came, and draw again. The events that are
/// waiting when the loop wakes are all handled before the next frame.
///
/// The browsers and the terminal act on the one controller from here, one thing at a
/// time. What a browser changes is reacted to like what the TUI itself changed, and
/// what the TUI changes other than by intents is found by `resync` when it has handled
/// its input and told to the browsers.
fn event_loop<B: Backend>(
    terminal: &mut Terminal<B>,
    s: &mut State,
    input: &Receiver<io::Result<Event>>,
    mut web: Option<&mut Web>,
) -> io::Result<()>
where
    B::Error: Send + Sync + 'static,
{
    let ticks = crossbeam_channel::tick(TICK);
    let mut web_requests = web.as_deref().map(Web::requests);
    loop {
        tick(s);
        if let Some(web) = web.as_deref_mut() {
            web.refresh(&s.ctl);
            s.web = Some(web.status());
        }

        render(terminal, s)?;
        if s.quit {
            break;
        }

        let requests = web_requests.clone().unwrap_or_else(crossbeam_channel::never);
        crossbeam_channel::select! {
            recv(input) -> event => {
                let Ok(event) = event else {
                    return Err(io::Error::other("the terminal reader has stopped"));
                };
                process_event(s, event?);
                while let Ok(event) = input.try_recv() {
                    process_event(s, event?);
                }
                web::resync(&mut s.ctl);
            }
            recv(requests) -> request => match (request, web.as_deref_mut()) {
                (Ok(request), Some(web)) => {
                    for applied in web.serve(&mut s.ctl, request) {
                        s.react_to(&applied);
                    }
                }
                // The server is gone: there is nothing more to wait for from it.
                _ => web_requests = None,
            },
            recv(ticks) -> _ => {}
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
            } else if s.modulate.is_some() {
                handle_modulate_key(s, key.code);
            } else if s.group_assign.is_some() {
                handle_group_assign_key(s, key.code);
            } else if s.preset_selector.is_some() {
                handle_preset_selector_key(s, key.code);
            } else if s.scale_selector.is_some() {
                handle_scale_selector_key(s, key.code);
            } else if s.bpm_editing.is_some() {
                handle_bpm_edit_key(s, key.code);
            } else if s.gain_editing.is_some() {
                handle_gain_edit_key(s, key.code);
            } else if s.save_as.is_some() {
                handle_save_as_key(s, key.code);
            } else if s.editing.is_some() {
                handle_edit_key(s, key.code);
            } else if s.range_edit.is_some() {
                handle_range_edit_key(s, key.code);
            } else if s.rename_group.is_some() {
                handle_rename_group_key(s, key.code);
            } else if s.param_filtering {
                handle_param_filter_key(s, key.code);
            } else {
                handle_key(s, key.code, key.modifiers);
            }
        }
        Event::Mouse(mouse) => {
            if s.selector.is_some()
                || s.target_selector.is_some()
                || s.modulate.is_some()
                || s.group_assign.is_some()
                || s.preset_selector.is_some()
                || s.scale_selector.is_some()
                || s.editing.is_some()
                || s.range_edit.is_some()
                || s.rename_group.is_some()
                || s.bpm_editing.is_some()
                || s.gain_editing.is_some()
                || s.save_as.is_some()
            {
                if let MouseEventKind::Down(MouseButton::Left) = mouse.kind {
                    s.selector = None;
                    s.target_selector = None;
                    s.modulate = None;
                    s.group_assign = None;
                    s.preset_selector = None;
                    s.scale_selector = None;
                    s.editing = None;
                    s.range_edit = None;
                    s.rename_group = None;
                    s.bpm_editing = None;
                    s.gain_editing = None;
                    s.save_as = None;
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

fn handle_modulate_key(s: &mut State, code: KeyCode) {
    let ms = s.modulate.as_mut().unwrap();
    match code {
        KeyCode::Esc => s.modulate = None,
        KeyCode::Enter => s.confirm_modulate(),
        KeyCode::Up => {
            ms.filter.list.up();
            ms.filter.list.ensure_visible(20);
        }
        KeyCode::Down => {
            ms.filter.list.down();
            ms.filter.list.ensure_visible(20);
        }
        _ => {}
    }
}

fn handle_group_assign_key(s: &mut State, code: KeyCode) {
    let gs = s.group_assign.as_mut().unwrap();
    match code {
        KeyCode::Esc => s.group_assign = None,
        KeyCode::Enter => s.confirm_group_assign(),
        KeyCode::Up => {
            gs.filter.list.up();
            gs.filter.list.ensure_visible(20);
        }
        KeyCode::Down => {
            gs.filter.list.down();
            gs.filter.list.ensure_visible(20);
        }
        _ => {}
    }
}

fn handle_scale_selector_key(s: &mut State, code: KeyCode) {
    let ss = s.scale_selector.as_mut().unwrap();
    match code {
        KeyCode::Esc => s.scale_selector = None,
        KeyCode::Enter => s.confirm_scale_selector(),
        KeyCode::Up => {
            ss.filter.list.up();
            ss.filter.list.ensure_visible(20);
        }
        KeyCode::Down => {
            ss.filter.list.down();
            ss.filter.list.ensure_visible(20);
        }
        KeyCode::Backspace => {
            ss.filter.input.backspace();
            ss.filter.apply_filter(&ss.items);
        }
        KeyCode::Char(ch) => {
            ss.filter.input.insert(ch);
            ss.filter.apply_filter(&ss.items);
        }
        _ => {}
    }
}

fn handle_preset_selector_key(s: &mut State, code: KeyCode) {
    let ps = s.preset_selector.as_mut().unwrap();
    match code {
        KeyCode::Esc => s.preset_selector = None,
        KeyCode::Enter => s.confirm_preset_selector(),
        KeyCode::Up => {
            ps.filter.list.up();
            ps.filter.list.ensure_visible(20);
        }
        KeyCode::Down => {
            ps.filter.list.down();
            ps.filter.list.ensure_visible(20);
        }
        KeyCode::Backspace => {
            ps.filter.input.backspace();
            ps.filter.apply_filter(&ps.items);
        }
        KeyCode::Char(ch) => {
            ps.filter.input.insert(ch);
            ps.filter.apply_filter(&ps.items);
        }
        _ => {}
    }
}

/// The number typed into a popup, if it is one. `str::parse` takes `nan` and `inf` for
/// numbers, and a NaN is not stopped by `clamp`: it would get into the model, and from
/// there to the audio thread and to every browser (JSON has no NaN).
fn parse_number(text: &str) -> Option<f32> {
    text.trim().parse::<f32>().ok().filter(|value| value.is_finite())
}

fn handle_edit_key(s: &mut State, code: KeyCode) {
    let edit = s.editing.as_mut().unwrap();
    match code {
        KeyCode::Esc => s.editing = None,
        KeyCode::Enter => {
            if let Some(val) = parse_number(&edit.input.value) {
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
            let inst = re.inst;
            let range = if input.is_empty() {
                None
            } else {
                match crate::session::parse_range(&input) {
                    Ok(r) => Some(r),
                    Err(_) => return, // keep popup open on parse error
                }
            };
            let _ = s
                .ctl
                .cmd_tx
                .send(GraphCommand::SetInstrumentRange { inst, range });
            if let Some(node) = s.ctl.model.instruments.get_mut(inst) {
                node.range = range;
            }
            s.ctl.dirty = true;
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

fn handle_rename_group_key(s: &mut State, code: KeyCode) {
    let rg = s.rename_group.as_mut().unwrap();
    match code {
        KeyCode::Esc => s.rename_group = None,
        KeyCode::Enter => {
            let name = rg.input.value.trim().to_string();
            let group = rg.group;
            // Empty clears the custom name (falls back to "Group N"). The name
            // is display-only, so no audio-thread command is needed.
            if let Some(g) = s.ctl.model.groups.get_mut(group) {
                g.name = if name.is_empty() { None } else { Some(name) };
            }
            s.ctl.dirty = true;
            s.rebuild_tree();
            s.rename_group = None;
        }
        KeyCode::Backspace => rg.input.backspace(),
        KeyCode::Delete => rg.input.delete(),
        KeyCode::Left => rg.input.move_left(),
        KeyCode::Right => rg.input.move_right(),
        KeyCode::Home => rg.input.home(),
        KeyCode::End => rg.input.end(),
        KeyCode::Char(ch) => rg.input.insert(ch),
        _ => {}
    }
}

fn handle_bpm_edit_key(s: &mut State, code: KeyCode) {
    let edit = s.bpm_editing.as_mut().unwrap();
    match code {
        KeyCode::Esc => s.bpm_editing = None,
        KeyCode::Enter => {
            if let Some(val) = parse_number(&edit.input.value) {
                let bpm = val.clamp(edit.param_min, edit.param_max);
                s.ctl.global_bpm = bpm;
                let _ = s.ctl.cmd_tx.send(GraphCommand::SetGlobalBpm { bpm });
                // Update all pattern states
                for inst_node in &mut s.ctl.model.instruments {
                    if let Some(ref mut p) = inst_node.pattern {
                        p.bpm = bpm;
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

fn handle_gain_edit_key(s: &mut State, code: KeyCode) {
    let ge = s.gain_editing.as_mut().unwrap();
    match code {
        KeyCode::Esc => s.gain_editing = None,
        KeyCode::Enter => {
            let (node, gain) = (ge.node, ge.gain);
            if let Some(value) = parse_number(&ge.edit.input.value) {
                s.apply(Intent::SetGain { node, gain, value });
            }
            s.gain_editing = None;
        }
        KeyCode::Backspace => ge.edit.input.backspace(),
        KeyCode::Delete => ge.edit.input.delete(),
        KeyCode::Left => ge.edit.input.move_left(),
        KeyCode::Right => ge.edit.input.move_right(),
        KeyCode::Home => ge.edit.input.home(),
        KeyCode::End => ge.edit.input.end(),
        KeyCode::Char(ch) => ge.edit.input.insert(ch),
        _ => {}
    }
}

fn handle_save_as_key(s: &mut State, code: KeyCode) {
    let sa = s.save_as.as_mut().unwrap();
    match code {
        KeyCode::Esc => s.save_as = None,
        KeyCode::Enter => {
            let name = sa.input.value.trim().to_string();
            if name.is_empty() {
                return; // keep popup open until a name is given
            }
            match s.ctl.save_as(&name) {
                // Subsequent Ctrl+S now targets the new file.
                Ok(_) => s.save_as = None,
                // Keep the popup (and the original session path) and show the
                // error so the user can fix the name or cancel.
                Err(e) => s.save_as.as_mut().unwrap().error = Some(e.to_string()),
            }
        }
        KeyCode::Backspace => {
            sa.error = None;
            sa.input.backspace();
        }
        KeyCode::Delete => {
            sa.error = None;
            sa.input.delete();
        }
        KeyCode::Left => sa.input.move_left(),
        KeyCode::Right => sa.input.move_right(),
        KeyCode::Home => sa.input.home(),
        KeyCode::End => sa.input.end(),
        KeyCode::Char(ch) => {
            sa.error = None;
            sa.input.insert(ch);
        }
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
        // Ctrl+Shift+S — save as (filename popup). Checked before plain Ctrl+S.
        KeyCode::Char('s' | 'S')
            if modifiers.contains(KeyModifiers::CONTROL)
                && modifiers.contains(KeyModifiers::SHIFT) =>
        {
            let current = s
                .ctl
                .session_path
                .as_ref()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            s.save_as = Some(SaveAsState {
                input: TextInputState::new(&current),
                error: None,
            });
        }
        KeyCode::Char('s') if modifiers.contains(KeyModifiers::CONTROL) => {
            if let Err(e) = s.ctl.save() {
                log::error!("Failed to save session: {e}");
            }
        }
        KeyCode::Char('1') => s.active_tab = 0,
        KeyCode::Char('2') => s.active_tab = 1,
        KeyCode::Char('3') => s.active_tab = 2,
        KeyCode::Char('4') => s.active_tab = 3,
        KeyCode::Tab => s.active_tab = (s.active_tab + 1) % TAB_NAMES.len(),
        KeyCode::BackTab => s.active_tab = (s.active_tab + TAB_NAMES.len() - 1) % TAB_NAMES.len(),

        // ---- Piano tab ----
        KeyCode::Char('k') if s.active_tab == 1 => {
            s.open_scale_selector();
        }
        KeyCode::Char('l') if s.active_tab == 1 => {
            let new_mode = match s.ctl.piano_filter.mode() {
                PianoMode::Highlight => PianoMode::Locked,
                PianoMode::Locked => PianoMode::Highlight,
            };
            s.ctl.piano_filter.set_mode(new_mode);
            s.ctl.dirty = true;
            log::info!("Piano mode → {}", new_mode.label());
        }
        KeyCode::Char('[') if s.active_tab == 1 && s.piano_view_octave > 0 => {
            s.piano_view_octave -= 1;
        }
        KeyCode::Char(']') if s.active_tab == 1 && s.piano_view_octave < 8 => {
            s.piano_view_octave += 1;
        }

        // 'i' — replace instrument on the selected instrument slot.
        KeyCode::Char('i') if s.active_tab == 0 && !s.focus_params => {
            if let Some(TreeAddress::Instrument(_)) = s.selected_address().copied() {
                s.open_selector(SelectorMode::Instrument);
            }
        }

        // 'n' — add a new instrument lane (full range) and immediately open the
        // plugin selector to fill it. The lane defaults to builtin:osc, so it
        // makes sound even if the selector is dismissed without a pick. Set its
        // key range afterwards with 'R'. This is how you build a keyboard split.
        KeyCode::Char('n') if s.active_tab == 0 && !s.focus_params => {
            let new_idx = s.ctl.add_lane();
            // Default the new lane to builtin:osc so it isn't silent if the
            // selector is cancelled.
            if let Some((loaded, slot)) = s.ctl.load_plugin("builtin:osc") {
                s.ctl.set_instrument(new_idx, loaded, slot);
            }
            s.ctl.dirty = true;
            s.rebuild_tree();
            // Move the cursor onto the new lane so the selector fills it.
            if let Some(pos) = s.tree_entries.iter().position(
                |e| matches!(e.address, TreeAddress::Instrument(i) if i == new_idx),
            ) {
                s.chain_state.selected = pos;
                s.sync_param_state();
            }
            s.open_selector(SelectorMode::Instrument);
        }

        // 'R' — set the key range of the selected instrument, or rename the
        // selected group (range doesn't apply to groups).
        KeyCode::Char('R') if s.active_tab == 0 && !s.focus_params => {
            if let Some(TreeAddress::Group(group)) = s.selected_address().copied() {
                let initial = s
                    .ctl
                    .model
                    .groups
                    .get(group)
                    .and_then(|g| g.name.clone())
                    .unwrap_or_default();
                let mut input = TextInputState::new(&initial);
                input.end();
                s.rename_group = Some(RenameGroupState { group, input });
            } else if let Some(inst) = s.selected_inst() {
                let initial = s
                    .ctl
                    .model
                    .instruments
                    .get(inst)
                    .and_then(|n| n.range)
                    .map(format_range)
                    .unwrap_or_default();
                let mut input = TextInputState::new(&initial);
                input.end();
                s.range_edit = Some(RangeEditState { inst, input });
            }
        }

        // Session: contextual add (chain focus only).
        // Instrument/Effect → add effect.
        KeyCode::Char('a') if s.active_tab == 0 && !s.focus_params => {
            match s.selected_address().copied() {
                Some(TreeAddress::Instrument(_) | TreeAddress::Effect { .. }) => {
                    s.open_selector(SelectorMode::Effect);
                }
                Some(TreeAddress::Group(g)) => {
                    s.open_selector(SelectorMode::GroupEffect(g));
                }
                Some(TreeAddress::GroupEffect { group, .. }) => {
                    s.open_selector(SelectorMode::GroupEffect(group));
                }
                Some(TreeAddress::Pattern(_)) => {}
                Some(TreeAddress::Modulator { .. }) => {}
                Some(TreeAddress::GroupModulator { .. }) => {}
                None => {}
            }
        }

        // 'm' (param focus) — modulate the selected parameter: open a popup to
        // bind it to a new or existing lane modulator.
        KeyCode::Char('m') if s.active_tab == 0 && s.focus_params => {
            match s.selected_address().copied() {
                Some(TreeAddress::Instrument(_) | TreeAddress::Effect { .. }) => {
                    s.open_modulate();
                }
                // A group bus-effect parameter → create/attach a group modulator.
                Some(TreeAddress::GroupEffect { .. }) => {
                    s.open_group_modulate();
                }
                // A modulator's own param (rate / ADSR / a target depth) →
                // cross-modulate it with a sibling modulator.
                Some(TreeAddress::Modulator { .. } | TreeAddress::GroupModulator { .. }) => {
                    s.open_modulate_cross_mod();
                }
                _ => {}
            }
        }

        // 'l' (param focus) — MIDI learn: bind the next CC / pitch-bend to the
        // selected parameter (plugin param or modulator field).
        KeyCode::Char('l') if s.active_tab == 0 && s.focus_params => {
            s.learn_selected_param();
        }

        // 'm' (chain focus) — add an empty LFO modulator to the instrument's
        // lane rack. (In param focus, `m` modulates the selected parameter —
        // handled above.)
        KeyCode::Char('m') if s.active_tab == 0 && !s.focus_params => {
            // On a group / group-bus node, add a group-scoped modulator; on an
            // instrument-side node, add a lane modulator.
            let group = match s.selected_address().copied() {
                Some(TreeAddress::Group(g))
                | Some(TreeAddress::GroupEffect { group: g, .. })
                | Some(TreeAddress::GroupModulator { group: g, .. }) => Some(g),
                _ => None,
            };
            let rack = match (group, s.selected_address().and_then(|a| a.inst())) {
                (Some(g), _) => Some(ModulatorLocation::Group(g)),
                (None, Some(inst)) => Some(ModulatorLocation::Lane(inst)),
                (None, None) => None,
            };
            if let Some(rack) = rack {
                if s.ctl.add_modulator(rack, ModSourceSlot::new_lfo()).is_some() {
                    s.ctl.dirty = true;
                    s.rebuild_tree();
                }
            }
        }

        // 'g' (chain focus) — assign the selected instrument to a group
        // (new, existing, or ungroup), via a popup.
        KeyCode::Char('g') if s.active_tab == 0 && !s.focus_params => {
            if let Some(TreeAddress::Instrument(inst)) = s.selected_address().copied() {
                s.open_group_assign(inst);
            }
        }

        // 'p' — open preset selector for the selected plugin slot.
        KeyCode::Char('p') if s.active_tab == 0 && !s.focus_params => {
            if let Some(addr) = s.selected_address().copied() {
                let target = match addr {
                    TreeAddress::Instrument(inst) => Some((inst, 0)),
                    TreeAddress::Effect { inst, index } => Some((inst, index + 1)),
                    _ => None,
                };
                if let Some((inst, slot)) = target {
                    s.open_preset_selector(inst, slot);
                }
            }
        }

        // 't' — add modulation target (when modulator selected).
        KeyCode::Char('t') if s.active_tab == 0 && !s.focus_params => {
            match s.selected_address().copied() {
                Some(TreeAddress::Modulator { inst, index }) => {
                    s.open_target_selector(inst, index);
                }
                Some(TreeAddress::GroupModulator { group, index }) => {
                    s.open_group_target_selector(group, index);
                }
                _ => {}
            }
        }

        // 'r' — toggle pattern recording (on Pattern or Instrument node).
        KeyCode::Char('r') if s.active_tab == 0 && !s.focus_params => {
            let target = match s.selected_address().copied() {
                Some(TreeAddress::Pattern(inst)) => Some(inst),
                Some(TreeAddress::Instrument(inst)) => Some(inst),
                _ => None,
            };
            if let Some(inst) = target {
                if let Some(inst_node) = s.ctl.model.instruments.get_mut(inst) {
                    let currently_recording = inst_node.pattern.as_ref().is_some_and(|p| p.recording);
                    if currently_recording {
                        // Stop recording
                        let _ = s.ctl.cmd_tx.send(GraphCommand::SetPatternRecording { inst, recording: false });
                        if let Some(ref mut p) = inst_node.pattern {
                            p.recording = false;
                        }
                    } else {
                        // Start recording: create pattern state if needed
                        if inst_node.pattern.is_none() {
                            inst_node.pattern = Some(PatternState {
                                bpm: s.ctl.global_bpm,
                                length_beats: 4.0,
                                looping: true,
                                base_note: None,
                                events: vec![],
                                enabled: false,
                                recording: false,
                                in_key: false,
                            });
                        }
                        // Send BPM and length first
                        let _ = s.ctl.cmd_tx.send(GraphCommand::SetGlobalBpm { bpm: s.ctl.global_bpm });
                        let _ = s.ctl.cmd_tx.send(GraphCommand::SetPatternLength {
                            inst,
                            beats: inst_node.pattern.as_ref().unwrap().length_beats,
                        });
                        let _ = s.ctl.cmd_tx.send(GraphCommand::SetPatternRecording { inst, recording: true });
                        inst_node.pattern.as_mut().unwrap().recording = true;
                    }
                    s.ctl.dirty = true;
                    s.rebuild_tree();
                }
            }
        }

        // 'b' — edit BPM.
        KeyCode::Char('b') if s.active_tab == 0 && !s.focus_params => {
            s.bpm_editing = Some(EditState {
                input: TextInputState::new(&format!("{:.0}", s.ctl.global_bpm)),
                param_name: "BPM".to_string(),
                param_min: 20.0,
                param_max: 300.0,
            });
        }

        // 'x' — edit the dry/wet mix of the selected effect (host-side blend).
        KeyCode::Char('x') if s.active_tab == 0 && !s.focus_params => {
            let effect = match s.selected_address().copied() {
                Some(TreeAddress::Effect { inst, index }) => {
                    s.ctl.model.instruments.get(inst).and_then(|n| n.effects.get(index))
                }
                Some(TreeAddress::GroupEffect { group, index }) => {
                    s.ctl.model.groups.get(group).and_then(|g| g.effects.get(index))
                }
                _ => None,
            };
            if let Some(fx) = effect {
                s.gain_editing = Some(GainEditState::new(fx.node, Gain::Mix, fx.mix, "mix (0=dry, 1=wet)"));
            }
        }

        // 'v' — edit the output volume of the selected instrument or group
        // (host-side gain, applied before effects). UI range 0–4.
        KeyCode::Char('v') if s.active_tab == 0 && !s.focus_params => {
            let target = match s.selected_address().copied() {
                Some(TreeAddress::Group(group)) => {
                    s.ctl.model.groups.get(group).map(|g| (g.node, Gain::GroupVolume, g.volume))
                }
                Some(addr) => addr
                    .inst()
                    .and_then(|inst| s.ctl.model.instruments.get(inst))
                    .map(|n| (n.node, Gain::Volume, n.volume)),
                None => None,
            };
            if let Some((node, gain, volume)) = target {
                s.gain_editing = Some(GainEditState::new(node, gain, volume, "volume (1.0 = unity)"));
            }
        }

        KeyCode::Char('d') if s.active_tab == 0 && !s.focus_params => {
            let sel = s.chain_state.selected;
            if sel < s.tree_entries.len() {
                let addr = s.tree_entries[sel].address;
                match addr {
                    TreeAddress::Effect { inst, index } => {
                        s.ctl.remove_effect(inst, index);
                        s.ctl.dirty = true;
                        s.rebuild_tree();
                    }
                    TreeAddress::Instrument(inst) => {
                        let has_plugin =
                            s.ctl.model.instruments.get(inst).is_some_and(|n| n.instrument.is_some());
                        if has_plugin {
                            s.ctl.clear_instrument(inst);
                            s.ctl.dirty = true;
                            s.rebuild_tree();
                        } else if s.ctl.remove_lane(inst) {
                            // (A session keeps a lane: the last one is not taken out.)
                            s.ctl.dirty = true;
                            s.rebuild_tree();
                        }
                    }
                    TreeAddress::Pattern(inst) => {
                        let _ = s.ctl.cmd_tx.send(GraphCommand::ClearPattern { inst });
                        if let Some(inst_node) = s.ctl.model.instruments.get_mut(inst) {
                            inst_node.pattern = None;
                        }
                        s.ctl.dirty = true;
                        s.rebuild_tree();
                    }
                    TreeAddress::Modulator { inst, index } => {
                        s.ctl.remove_modulator(ModulatorLocation::Lane(inst), index);
                        s.ctl.dirty = true;
                        s.rebuild_tree();
                    }
                    TreeAddress::GroupEffect { group, index } => {
                        s.ctl.remove_bus_effect(group, index);
                        s.ctl.dirty = true;
                        s.rebuild_tree();
                    }
                    TreeAddress::GroupModulator { group, index } => {
                        s.ctl.remove_modulator(ModulatorLocation::Group(group), index);
                        s.ctl.dirty = true;
                        s.rebuild_tree();
                    }
                    TreeAddress::Group(group) => {
                        // Delete the group: members become ungrouped, higher
                        // group indices shift down.
                        s.ctl.remove_group(group);
                        s.ctl.dirty = true;
                        s.rebuild_tree();
                    }
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
                        TreeAddress::Modulator { inst, index } => {
                            let edit = s.ctl.model.instruments.get(inst)
                                .and_then(|n| n.modulators.get(index))
                                .and_then(|m| modulator_edit_state(m, pa));
                            if edit.is_some() {
                                s.editing = edit;
                            }
                        }
                        TreeAddress::GroupModulator { group, index } => {
                            let edit = s.ctl.model.groups.get(group)
                                .and_then(|g| g.modulators.get(index))
                                .and_then(|m| modulator_edit_state(m, pa));
                            if edit.is_some() {
                                s.editing = edit;
                            }
                        }
                        TreeAddress::Pattern(inst) => {
                            let pat = s.ctl.model.instruments.get(inst)
                                .and_then(|n| n.pattern.as_ref());
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
                                    3 => {} // Transpose is enum — use Left/Right
                                    _ => {} // Notes is info
                                }
                            }
                        }
                        TreeAddress::Group(group) => {
                            // Group bus pane: 0 = volume, 1 = pan.
                            if let Some(g) = s.ctl.model.groups.get(group) {
                                let (val, name, min, max) = match pa {
                                    0 => (g.volume, "volume (1.0 = unity)", 0.0, 4.0),
                                    1 => (g.pan, "pan (-1 L … +1 R)", -1.0, 1.0),
                                    _ => return,
                                };
                                s.editing = Some(EditState {
                                    input: TextInputState::new(&format!("{val:.2}")),
                                    param_name: name.to_string(),
                                    param_min: min,
                                    param_max: max,
                                });
                            }
                        }
                        _ => {
                            let real_pa = s.real_param_index().unwrap_or(pa);
                            if let Some(param) = s.plugin_at(&addr).and_then(|p| p.params.get(real_pa)) {
                                if matches!(param.kind, ParamKind::Enum(_)) {
                                    // Enum — switch with Left/Right, no numeric entry.
                                } else {
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
                }
            } else {
                let sel = s.chain_state.selected;
                if sel < s.tree_entries.len() {
                    s.focus_params = true;
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
                    TreeAddress::Instrument(_) | TreeAddress::Effect { .. }
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

        // Reorder effects / reorder instruments.
        KeyCode::Up
            if s.active_tab == 0
                && !s.focus_params
                && modifiers.contains(KeyModifiers::SHIFT) =>
        {
            let sel = s.chain_state.selected;
            if sel < s.tree_entries.len() {
                match s.tree_entries[sel].address {
                    TreeAddress::Effect { inst, index } if index > 0 => {
                        let _ = s.ctl.cmd_tx.send(GraphCommand::ReorderEffect {
                            inst,
                            from: index,
                            to: index - 1,
                        });
                        s.ctl.model.move_effect(inst, index, index - 1);
                        s.ctl.dirty = true;
                        // (The cursor follows the effect it was on.)
                        s.rebuild_tree();
                    }
                    TreeAddress::Instrument(inst) if inst > 0 => {
                        s.ctl.swap_lanes(inst, inst - 1);
                        s.ctl.dirty = true;
                        s.rebuild_tree();
                        let new_addr = TreeAddress::Instrument(inst - 1);
                        if let Some(pos) = s.tree_entries.iter().position(|e| e.address == new_addr) {
                            s.chain_state.selected = pos;
                        }
                        s.sync_param_state();
                    }
                    TreeAddress::Pattern(inst) if inst > 0 => {
                        let _ = s.ctl.cmd_tx.send(GraphCommand::SwapPatterns {
                            inst_a: inst,
                            inst_b: inst - 1,
                        });
                        let a_pat = s.ctl.model.instruments[inst].pattern.take();
                        let b_pat = s.ctl.model.instruments[inst - 1].pattern.take();
                        s.ctl.model.instruments[inst].pattern = b_pat;
                        s.ctl.model.instruments[inst - 1].pattern = a_pat;
                        s.ctl.dirty = true;
                        s.rebuild_tree();
                        let new_addr = TreeAddress::Pattern(inst - 1);
                        if let Some(pos) = s.tree_entries.iter().position(|e| e.address == new_addr) {
                            s.chain_state.selected = pos;
                        }
                        s.sync_param_state();
                    }
                    TreeAddress::GroupEffect { group, index } if index > 0 => {
                        let _ = s.ctl.cmd_tx.send(GraphCommand::ReorderGroupEffect {
                            group,
                            from: index,
                            to: index - 1,
                        });
                        if let Some(g) = s.ctl.model.groups.get_mut(group) {
                            if index < g.effects.len() {
                                g.effects.swap(index, index - 1);
                                remap_group_bus_after_reorder(&mut g.modulators, index, index - 1);
                            }
                        }
                        s.ctl.dirty = true;
                        s.rebuild_tree();
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
                    TreeAddress::Effect { inst, index } => {
                        let effect_count = s.ctl.model.instruments.get(inst)
                            .map_or(0, |n| n.effects.len());
                        if index + 1 < effect_count {
                            let _ = s.ctl.cmd_tx.send(GraphCommand::ReorderEffect {
                                inst,
                                from: index,
                                to: index + 1,
                            });
                            s.ctl.model.move_effect(inst, index, index + 1);
                            s.ctl.dirty = true;
                            s.rebuild_tree();
                        }
                    }
                    TreeAddress::Instrument(inst) if inst + 1 < s.ctl.model.instruments.len() => {
                        s.ctl.swap_lanes(inst, inst + 1);
                        s.ctl.dirty = true;
                        s.rebuild_tree();
                        let new_addr = TreeAddress::Instrument(inst + 1);
                        if let Some(pos) = s.tree_entries.iter().position(|e| e.address == new_addr) {
                            s.chain_state.selected = pos;
                        }
                        s.sync_param_state();
                    }
                    TreeAddress::Pattern(inst) if inst + 1 < s.ctl.model.instruments.len() => {
                        let _ = s.ctl.cmd_tx.send(GraphCommand::SwapPatterns {
                            inst_a: inst,
                            inst_b: inst + 1,
                        });
                        let a_pat = s.ctl.model.instruments[inst].pattern.take();
                        let b_pat = s.ctl.model.instruments[inst + 1].pattern.take();
                        s.ctl.model.instruments[inst].pattern = b_pat;
                        s.ctl.model.instruments[inst + 1].pattern = a_pat;
                        s.ctl.dirty = true;
                        s.rebuild_tree();
                        let new_addr = TreeAddress::Pattern(inst + 1);
                        if let Some(pos) = s.tree_entries.iter().position(|e| e.address == new_addr) {
                            s.chain_state.selected = pos;
                        }
                        s.sync_param_state();
                    }
                    TreeAddress::GroupEffect { group, index } => {
                        let count = s.ctl.model.groups.get(group).map_or(0, |g| g.effects.len());
                        if index + 1 < count {
                            let _ = s.ctl.cmd_tx.send(GraphCommand::ReorderGroupEffect {
                                group,
                                from: index,
                                to: index + 1,
                            });
                            if let Some(g) = s.ctl.model.groups.get_mut(group) {
                                g.effects.swap(index, index + 1);
                                remap_group_bus_after_reorder(&mut g.modulators, index, index + 1);
                            }
                            s.ctl.dirty = true;
                            s.rebuild_tree();
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
                let actions = action_bar_items(addr, s.focus_params);
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

fn render<B: Backend>(terminal: &mut Terminal<B>, s: &mut State) -> io::Result<()>
where
    B::Error: Send + Sync + 'static,
{
    terminal.draw(|frame| draw(frame, s)).map_err(io::Error::other)?;
    Ok(())
}

/// Draw one frame. Kept free of the terminal backend so the golden-render
/// tests (`golden_tests.rs`) can drive it through ratatui's `TestBackend`.
fn draw(frame: &mut ratatui::Frame, s: &mut State) {
    let area = frame.area();
    // `--debug` carves a MIDI-monitor strip off the bottom (0 rows otherwise).
    let debug_h: u16 = if s.ctl.midi_monitor.is_enabled() { 9 } else { 0 };
    let [tab_area, content_area, action_area, debug_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(1),
        Constraint::Length(debug_h),
    ])
    .areas(area);

    s.areas.tab = tab_area;
    s.areas.content = content_area;
    s.areas.action_bar = action_area;

    let session_label = if s.ctl.dirty { "(1) Session *" } else { "(1) Session" };
    let tab_names: &[&str] = &[session_label, TAB_NAMES[1], TAB_NAMES[2], TAB_NAMES[3]];
    frame.render_widget(TabBar::new(tab_names, s.active_tab), tab_area);

    // The right side of the tab bar: the web interface when there is one, the CLIP
    // light while it is lit, then the BPM. The web label gives way first: it is
    // shortened, or left out, to keep clear of the tabs.
    let bpm_text = format!("{:.0} BPM", s.ctl.global_bpm);
    let clip_text = if s.clip.is_lit() { "CLIP  " } else { "" };
    let tabs_width = tab_names.iter().map(|name| name.chars().count()).sum::<usize>() + 3 * (tab_names.len() - 1);
    // (The tabs, a gap of two, the label, two blanks after it, the rest, and a blank at the edge.)
    let room = (tab_area.width as usize).saturating_sub(tabs_width + 2 + 2 + clip_text.len() + bpm_text.len() + 1);
    let web_text = s.web.as_ref().and_then(|web| web.label(room)).map(|label| format!("{label}  ")).unwrap_or_default();
    let status_width = (web_text.len() + clip_text.len() + bpm_text.len()) as u16;
    if tab_area.width > status_width + 2 {
        let status_area = Rect {
            x: tab_area.right() - status_width - 1,
            y: tab_area.y,
            width: status_width + 1,
            height: 1,
        };
        let clip_style = Style::default().fg(Color::Red).add_modifier(Modifier::BOLD);
        frame.render_widget(
            Paragraph::new(ratatui::text::Line::from(vec![
                ratatui::text::Span::raw(web_text),
                ratatui::text::Span::styled(clip_text, clip_style),
                ratatui::text::Span::raw(bpm_text),
            ]))
            .style(Style::default().fg(DIM)),
            status_area,
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
                &s.ctl.model.instruments,
                &s.ctl.model.groups,
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
            if let Some(rg) = &s.rename_group {
                render_rename_group_popup(frame, area, rg);
            }
            if let Some(sel) = &s.selector {
                render_selector_popup(frame, area, sel, s.ctl.catalog.is_scanning());
            }
            if let Some(ts) = &s.target_selector {
                render_target_selector_popup(frame, area, ts);
            }
            if let Some(ms) = &s.modulate {
                render_modulate_popup(frame, area, ms);
            }
            if let Some(gs) = &s.group_assign {
                render_group_assign_popup(frame, area, gs);
            }
            if let Some(ps) = &s.preset_selector {
                render_preset_selector_popup(frame, area, ps);
            }
            if let Some(edit) = &s.bpm_editing {
                render_edit_popup(frame, area, edit);
            }
            if let Some(ge) = &s.gain_editing {
                render_edit_popup(frame, area, &ge.edit);
            }
            if let Some(sa) = &s.save_as {
                render_save_as_popup(frame, area, sa);
            }
        }
        1 => {
            render_piano_tab(frame, content_area, s);
            if let Some(ss) = &s.scale_selector {
                render_scale_selector_popup(frame, area, ss);
            }
        }
        2 => {
            frame.render_widget(
                Paragraph::new("Oscilloscope — not yet implemented")
                    .style(Style::default().fg(DIM)),
                content_area,
            );
        }
        3 => render_help(frame, content_area, &s.help_lines, s.help_offset),
        _ => {}
    }

    if s.ctl.midi_monitor.is_enabled() {
        render_midi_monitor(frame, debug_area, &s.ctl.midi_monitor);
    }
}

/// Live incoming-MIDI tally shown at the bottom under `--debug`. Each row is one
/// message kind (note / bend / channel pressure / poly aftertouch / CC N) with
/// its latest raw bytes and a hit count — so you can see at a glance what a
/// controller actually emits (e.g. whether a hard press sends Channel Pressure).
fn render_midi_monitor(
    frame: &mut ratatui::Frame,
    area: Rect,
    monitor: &crate::midi_monitor::MidiMonitor,
) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(DIM))
        .title(" MIDI in  (--debug) ");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let rows = monitor.snapshot();
    let text = if rows.is_empty() {
        "waiting for MIDI…  play a note, then wiggle / press hard and watch what appears".to_string()
    } else {
        rows.join("\n")
    };
    frame.render_widget(Paragraph::new(text).style(Style::default().fg(Color::White)), inner);
}

#[allow(clippy::too_many_arguments)]
fn render_session(
    frame: &mut ratatui::Frame,
    area: Rect,
    tree_entries: &[TreeEntry],
    chain_state: &ListState,
    instruments: &[InstrumentNode],
    groups: &[GroupNode],
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
        Style::default().fg(DIM)
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
            .style(Style::default().fg(DIM))
            .selected_style(Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
        left_inner,
    );

    // Param pane — find the selected plugin or modulator.
    let selected = chain_state.selected;
    let mut mod_params: Vec<ParamSlot> = Vec::new(); // temp storage for modulator pseudo-params
    let (plugin_name, plugin_params) = if selected < tree_entries.len() {
        let addr = &tree_entries[selected].address;
        match addr {
            TreeAddress::Modulator { .. } | TreeAddress::GroupModulator { .. } => {
                let m = match addr {
                    TreeAddress::Modulator { inst, index } => {
                        instruments.get(*inst).and_then(|n| n.modulators.get(*index))
                    }
                    TreeAddress::GroupModulator { group, index } => {
                        groups.get(*group).and_then(|g| g.modulators.get(*index))
                    }
                    _ => None,
                };
                match m {
                    Some(m) => {
                        let (name, rows) = modulator_pane(m);
                        mod_params = rows;
                        (name, mod_params.as_slice())
                    }
                    None => ("(none)".to_string(), &[] as &[ParamSlot]),
                }
            }
            TreeAddress::Pattern(inst) => {
                let pat = instruments.get(*inst)
                    .and_then(|n| n.pattern.as_ref());
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
                            ..Default::default()
                        });
                        mod_params.push(ParamSlot {
                            name: "Enabled".to_string(),
                            index: 1,
                            min: 0.0,
                            max: 1.0,
                            default: 1.0,
                            value: if p.enabled { 1.0 } else { 0.0 },
                            kind: ParamKind::Enum(vec!["Off".to_string(), "On".to_string()]),
                            ..Default::default()
                        });
                        mod_params.push(ParamSlot {
                            name: "Loop".to_string(),
                            index: 2,
                            min: 0.0,
                            max: 1.0,
                            default: 1.0,
                            value: if p.looping { 1.0 } else { 0.0 },
                            kind: ParamKind::Enum(vec!["Off".to_string(), "On".to_string()]),
                            ..Default::default()
                        });
                        mod_params.push(ParamSlot {
                            name: "Transpose".to_string(),
                            index: 3,
                            min: 0.0,
                            max: 1.0,
                            default: 0.0,
                            value: if p.in_key { 1.0 } else { 0.0 },
                            kind: ParamKind::Enum(vec!["Chromatic".to_string(), "In Key".to_string()]),
                            ..Default::default()
                        });
                        if !p.events.is_empty() {
                            let notes = p.events.iter().filter(|e| e.1 == 0x90).count();
                            mod_params.push(ParamSlot {
                                name: "Notes".to_string(),
                                index: 4,
                                min: 0.0,
                                max: 0.0,
                                default: 0.0,
                                value: notes as f32,
                                kind: ParamKind::Separator,
                                ..Default::default()
                            });
                        }
                        ("Pattern".to_string(), mod_params.as_slice())
                    }
                    None => ("Pattern".to_string(), &[] as &[ParamSlot]),
                }
            }
            TreeAddress::Group(group) => {
                // The group's bus pane: host-side Volume + Pan (no plugin).
                match groups.get(*group) {
                    Some(g) => {
                        mod_params.push(ParamSlot {
                            name: "volume".to_string(),
                            index: 0,
                            min: 0.0,
                            max: 4.0,
                            default: 1.0,
                            value: g.volume,
                            kind: ParamKind::Float,
                            ..Default::default()
                        });
                        mod_params.push(ParamSlot {
                            name: "pan".to_string(),
                            index: 1,
                            min: -1.0,
                            max: 1.0,
                            default: 0.0,
                            value: g.pan,
                            kind: ParamKind::Float,
                            ..Default::default()
                        });
                        let name = g.display_name(*group);
                        (name, mod_params.as_slice())
                    }
                    None => ("(none)".to_string(), &[] as &[ParamSlot]),
                }
            }
            _ => {
                // Find the PluginSlot for this address.
                let slot = match addr {
                    TreeAddress::Instrument(inst) => {
                        instruments.get(*inst)
                            .and_then(|n| n.instrument.as_ref())
                    }
                    TreeAddress::Effect { inst, index } => {
                        instruments.get(*inst)
                            .and_then(|n| n.effects.get(*index))
                    }
                    TreeAddress::GroupEffect { group, index } => {
                        groups.get(*group).and_then(|g| g.effects.get(*index))
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
        Style::default().fg(DIM)
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
            TreeAddress::Instrument(_) | TreeAddress::Effect { .. }
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
    let sep_style = Style::default().fg(DIM);
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
            .style(Style::default().fg(DIM))
            .selected_style(Style::default().fg(Color::White).add_modifier(Modifier::BOLD))
    } else {
        List::new(&param_items, &ps)
            .style(Style::default().fg(DIM))
            .selected_style(Style::default().fg(Color::White))
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
    // Show the actions for whichever pane has focus (chain vs. parameters).
    let actions = action_bar_items(addr, focus_params);

    let key_style = Style::default().fg(Color::Black).bg(Color::White).add_modifier(Modifier::BOLD);
    let label_style = Style::default().fg(Color::White);

    let y = area.y;
    let mut x = area.x;

    for &(key, desc) in &actions {
        if x > area.x {
            x += 1;
        }
        for ch in format!(" {key} ").chars() {
            if x >= area.right() { break; }
            if let Some(c) = frame.buffer_mut().cell_mut((x, y)) { c.set_char(ch); c.set_style(key_style); }
            x += 1;
        }
        for ch in format!(" {desc}").chars() {
            if x >= area.right() { break; }
            if let Some(c) = frame.buffer_mut().cell_mut((x, y)) { c.set_char(ch); c.set_style(label_style); }
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
            Paragraph::new(hint).style(Style::default().fg(DIM)),
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
        .title(" Set Range ");
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    if inner.height >= 2 {
        frame.render_widget(
            Paragraph::new("C0-B3 / C4- / -B3 / empty=all")
                .style(Style::default().fg(DIM)),
            Rect::new(inner.x, inner.y, inner.width, 1),
        );
        frame.render_widget(
            TextInput::new(&re.input),
            Rect::new(inner.x, inner.y + 1, inner.width, 1),
        );
    }
}

fn render_rename_group_popup(frame: &mut ratatui::Frame, area: Rect, rg: &RenameGroupState) {
    let popup = centered_rect(40, 5, area);
    frame.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(" Rename Group ");
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    if inner.height >= 2 {
        frame.render_widget(
            Paragraph::new("Group name (empty = default)")
                .style(Style::default().fg(DIM)),
            Rect::new(inner.x, inner.y, inner.width, 1),
        );
        frame.render_widget(
            TextInput::new(&rg.input),
            Rect::new(inner.x, inner.y + 1, inner.width, 1),
        );
    }
}

fn render_save_as_popup(frame: &mut ratatui::Frame, area: Rect, sa: &SaveAsState) {
    let popup = centered_rect(44, 5, area);
    frame.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow))
        .title(" Save As ");
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    if inner.height >= 2 {
        // The hint line doubles as the error line after a failed save.
        let hint = match &sa.error {
            Some(e) => Paragraph::new(e.as_str()).style(Style::default().fg(Color::Red)),
            None => Paragraph::new("Filename (saved in session dir; .toml added)")
                .style(Style::default().fg(DIM)),
        };
        frame.render_widget(hint, Rect::new(inner.x, inner.y, inner.width, 1));
        frame.render_widget(
            TextInput::new(&sa.input),
            Rect::new(inner.x, inner.y + 1, inner.width, 1),
        );
    }
}

fn render_selector_popup(
    frame: &mut ratatui::Frame,
    area: Rect,
    sel: &SelectorState,
    scanning: bool,
) {
    let title = match (sel.mode, scanning) {
        (SelectorMode::Instrument, false) => " Select Instrument ",
        (SelectorMode::Instrument, true) => " Select Instrument (scanning…) ",
        (SelectorMode::Effect, false) => " Select Effect ",
        (SelectorMode::Effect, true) => " Select Effect (scanning…) ",
        (SelectorMode::GroupEffect(_), false) => " Select Group Effect ",
        (SelectorMode::GroupEffect(_), true) => " Select Group Effect (scanning…) ",
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

fn render_group_assign_popup(frame: &mut ratatui::Frame, area: Rect, gs: &GroupAssignState) {
    let w = (area.width * 45 / 100).max(30).min(area.width);
    let h = (area.height * 40 / 100).max(7).min(area.height);
    let popup = centered_rect(w, h, area);
    frame.render_widget(Clear, popup);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(" Assign to group ");
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let columns: &[(&str, u16)] = &[("Group", inner.width)];
    frame.render_widget(FilterList::new(&gs.filter, &gs.items, columns), inner);
}

fn render_modulate_popup(frame: &mut ratatui::Frame, area: Rect, ms: &ModulateState) {
    let w = (area.width * 50 / 100).max(36).min(area.width);
    let h = (area.height * 45 / 100).max(8).min(area.height);
    let popup = centered_rect(w, h, area);
    frame.render_widget(Clear, popup);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Magenta))
        .title(format!(" Modulate {} ", ms.param_name));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let columns: &[(&str, u16)] = &[("Modulator", inner.width)];
    frame.render_widget(FilterList::new(&ms.filter, &ms.items, columns), inner);
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

fn render_preset_selector_popup(
    frame: &mut ratatui::Frame,
    area: Rect,
    ps: &PresetSelectorState,
) {
    let w = (area.width * 50 / 100).max(32).min(area.width);
    let h = (area.height * 50 / 100).max(10).min(area.height);
    let popup = centered_rect(w, h, area);
    frame.render_widget(Clear, popup);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(" Select Preset ");
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let columns: &[(&str, u16)] = &[("Name", inner.width)];
    frame.render_widget(FilterList::new(&ps.filter, &ps.items, columns), inner);
}

fn render_scale_selector_popup(frame: &mut ratatui::Frame, area: Rect, ss: &ScaleSelectorState) {
    let w = (area.width * 50 / 100).max(36).min(area.width);
    let h = (area.height * 60 / 100).max(12).min(area.height);
    let popup = centered_rect(w, h, area);
    frame.render_widget(Clear, popup);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(" Select Scale ");
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let columns: &[(&str, u16)] = &[("Scale", inner.width)];
    frame.render_widget(FilterList::new(&ss.filter, &ss.items, columns), inner);
}

fn render_piano_tab(frame: &mut ratatui::Frame, area: Rect, s: &State) {
    // Vertical split:
    //  - 2 row status (scale+mode on top, held notes + chord on second)
    //  - main keyboard area
    //  - 1 row "scale strip" listing the scale's notes
    //  - 1 row hint line at bottom
    let [status_area, kb_area, strip_area, hint_area] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Fill(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(area);
    let [config_row, live_row] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(status_area);

    // ---- Row 1: scale + mode ----
    let scale = s.ctl.piano_filter.scale();
    let mode = s.ctl.piano_filter.mode();
    let config_text = format!(
        " Scale: {}   Mode: {}   Octave view: C{}",
        scale.display(),
        mode.label(),
        s.piano_view_octave,
    );
    frame.render_widget(
        Paragraph::new(config_text).style(
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        config_row,
    );

    // ---- Row 2: held notes + chord detection ----
    let held_notes = s.ctl.held.held();
    let held_names: Vec<String> = held_notes
        .iter()
        .map(|&n| {
            let nm = crate::note_name(n);
            match scale.degree(n) {
                Some(d) => format!("{nm}({d})"),
                None => nm,
            }
        })
        .collect();
    let held_str = if held_names.is_empty() {
        String::from("—")
    } else {
        held_names.join(" ")
    };

    // Chord: top match if 3+ notes, interval if exactly 2, blank for 0–1.
    let chord_text = if held_notes.len() >= 2 {
        let matches = crate::chord::detect(&held_notes, &scale);
        if matches.is_empty() {
            if let Some(iv) = crate::chord::two_note_interval(&held_notes) {
                iv
            } else {
                String::from("—")
            }
        } else {
            let mut parts: Vec<String> = Vec::new();
            for (i, m) in matches.iter().take(2).enumerate() {
                let label = match &m.roman {
                    Some(r) if i == 0 => format!("{} ({})", m.display(), r),
                    _ => m.display(),
                };
                parts.push(label);
            }
            parts.join("  ·  ")
        }
    } else {
        String::from("—")
    };

    let live_line = ratatui::text::Line::from(vec![
        ratatui::text::Span::styled(
            " Held: ",
            Style::default().fg(DIM),
        ),
        ratatui::text::Span::styled(
            held_str,
            Style::default().fg(Color::White),
        ),
        ratatui::text::Span::styled(
            "   Chord: ",
            Style::default().fg(DIM),
        ),
        ratatui::text::Span::styled(
            chord_text,
            Style::default()
                .fg(Color::Rgb(240, 185, 70))
                .add_modifier(Modifier::BOLD),
        ),
    ]);
    frame.render_widget(Paragraph::new(live_line), live_row);

    // ---- Keyboard ----
    frame.render_widget(Clear, kb_area);
    let buf = frame.buffer_mut();
    piano_view::render_keyboard(kb_area, buf, &s.ctl.held, &scale, s.piano_view_octave);

    // ---- Scale strip: degree → note pairs ----
    use crate::scale::{SCALES, SCALE_DEGREES};
    let scale_intervals = SCALES[scale.scale_idx].intervals;
    let mut strip_parts: Vec<(String, Style)> = Vec::new();
    for (i, &iv) in scale_intervals.iter().enumerate() {
        let pc = (scale.root as u16 + iv as u16) % 12;
        let name = NOTE_NAMES[pc as usize];
        let degree = SCALE_DEGREES[iv as usize];
        let is_root = iv == 0;
        let key_style = if is_root {
            Style::default()
                .fg(Color::Rgb(240, 185, 70))
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Rgb(120, 200, 215))
        };
        let degree_style = Style::default().fg(Color::Rgb(120, 120, 120));
        if i > 0 {
            strip_parts.push(("   ".into(), Style::default()));
        }
        strip_parts.push((format!("{degree}:"), degree_style));
        strip_parts.push((name.to_string(), key_style));
    }
    let line: ratatui::text::Line = ratatui::text::Line::from(
        strip_parts
            .into_iter()
            .map(|(t, st)| ratatui::text::Span::styled(t, st))
            .collect::<Vec<_>>(),
    );
    frame.render_widget(
        Paragraph::new(line).alignment(ratatui::layout::Alignment::Center),
        strip_area,
    );

    // ---- Hint line ----
    let hint = " [k] scale  [l] mode  [[/]] octave  [Esc] back ";
    frame.render_widget(
        Paragraph::new(hint).style(Style::default().fg(DIM)),
        hint_area,
    );
}

fn render_help(frame: &mut ratatui::Frame, area: Rect, lines: &[String], offset: usize) {
    let scroll_lines: Vec<ScrollLine> = lines
        .iter()
        .map(|l| {
            if l.starts_with("  ") {
                ScrollLine::raw(l)
            } else if l.starts_with("---") {
                ScrollLine::styled(l, Style::default().fg(DIM))
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

/// What a row of a modulator's pane is called, with the unit it is in.
fn modulator_field_label(field: ModulatorField) -> &'static str {
    match field {
        ModulatorField::Rate => "Rate (Hz)",
        ModulatorField::Waveform => "Waveform",
        ModulatorField::Attack => "Attack (s)",
        ModulatorField::Decay => "Decay (s)",
        ModulatorField::Sustain => "Sustain",
        ModulatorField::Release => "Release (s)",
    }
}

/// The pane of a modulator: its title, and a row for each of what `modulator_row` says its rows are (the
/// kind it is, its waveform, its settings, a separator, a depth for each cable), as parameters to draw.
fn modulator_pane(m: &ModulatorSlot) -> (String, Vec<ParamSlot>) {
    use crate::plugin::chain::LfoWaveform;
    let title = match &m.source {
        ModSourceSlot::Lfo { .. } => m.source.label(),
        ModSourceSlot::Envelope { .. } => "ADSR".to_string(),
        ModSourceSlot::MidiLearn { .. } => "MIDI".to_string(),
    };
    let row = |name: String, index: usize, min: f32, max: f32, default: f32, value: f32, kind: ParamKind| ParamSlot {
        name,
        index: index as u32,
        min,
        max,
        default,
        value,
        kind,
        ..Default::default()
    };
    // What a modulator of each kind starts as, which a row's default is.
    let (new_lfo, new_envelope) = (ModSourceSlot::new_lfo(), ModSourceSlot::new_envelope());
    let rows = (0..modulator_param_len(m))
        .filter_map(|position| {
            Some(match modulator_row(m, position)? {
                ModulatorRow::Type => {
                    let envelope = matches!(m.source, ModSourceSlot::Envelope { .. });
                    row("Type".to_string(), position, 0.0, 1.0, 0.0, if envelope { 1.0 } else { 0.0 }, ParamKind::Enum(vec!["LFO".to_string(), "Envelope".to_string()]))
                }
                ModulatorRow::Waveform => {
                    let (min, max) = ModulatorField::Waveform.range();
                    let value = m.source.setting(ModulatorField::Waveform).unwrap_or(0.0);
                    let names = LfoWaveform::ALL.iter().map(|w| w.name().to_string()).collect();
                    row(modulator_field_label(ModulatorField::Waveform).to_string(), position, min, max, 0.0, value, ParamKind::Enum(names))
                }
                ModulatorRow::Setting(field) => {
                    let (min, max) = field.range();
                    let default = new_lfo.setting(field).or_else(|| new_envelope.setting(field)).unwrap_or(min);
                    row(modulator_field_label(field).to_string(), position, min, max, default, m.source.setting(field)?, ParamKind::Float)
                }
                // A control's source, shown and not edited.
                ModulatorRow::Info => row(format!("Source: {}", m.source.label()), position, 0.0, 0.0, 0.0, 0.0, ParamKind::Separator),
                ModulatorRow::Separator => row("Targets".to_string(), position, 0.0, 0.0, 0.0, 0.0, ParamKind::Separator),
                ModulatorRow::Depth(target) => {
                    let target = m.targets.get(target)?;
                    row(format!("{} depth", target.param_name), position, DEPTH_MIN, DEPTH_MAX, 0.5, target.depth, ParamKind::Float)
                }
            })
        })
        .collect();
    (title, rows)
}

/// Build the value-entry `EditState` for row `pa` of a modulator's pane, or None for a row that is
/// not a number to type (the kind, the waveform, a separator). Scope-independent (lane or group).
fn modulator_edit_state(m: &ModulatorSlot, pa: usize) -> Option<EditState> {
    // (An envelope's numbers are shown to a thousandth, the others' to a hundredth.)
    let places = if matches!(m.source, ModSourceSlot::Envelope { .. }) { 3 } else { 2 };
    let (value, name, min, max) = match modulator_row(m, pa)? {
        ModulatorRow::Setting(field) => {
            let (min, max) = field.range();
            (m.source.setting(field)?, modulator_field_label(field).to_string(), min, max)
        }
        ModulatorRow::Depth(target) => {
            let target = m.targets.get(target)?;
            (target.depth, format!("{} depth", target.param_name), DEPTH_MIN, DEPTH_MAX)
        }
        ModulatorRow::Type | ModulatorRow::Info | ModulatorRow::Waveform | ModulatorRow::Separator => return None,
    };
    Some(EditState { input: TextInputState::new(&format!("{value:.places$}")), param_name: name, param_min: min, param_max: max })
}

fn param_step(s: &State, modifiers: KeyModifiers) -> f32 {
    // Enum params step one value at a time, regardless of modifiers.
    if s.selected_param_is_enum() {
        return 1.0;
    }
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

/// Format a note range as "C4-B5" style string (open-ended: "-B3", "C4-").
fn format_range(range: (u8, u8)) -> String {
    crate::session::format_range(range)
}

/// Render one instrument and its children (pattern, effects, lane mod rack).
/// `prefix` is prepended to every label so the whole sub-tree can be nested
/// under a group header.
fn push_instrument_entries(
    entries: &mut Vec<TreeEntry>,
    inst: &InstrumentNode,
    inst_idx: usize,
    prefix: &str,
) {
    let plugin_label = inst.instrument.as_ref()
        .map(|p| format!("\u{266a} {}  [{}]", p.name, p.format))
        .unwrap_or_else(|| "\u{266a} (empty)".to_string());
    let range_label = inst.range
        .map(|r| format!("  {}", format_range(r)))
        .unwrap_or_default();
    let transpose_label = if inst.transpose != 0 {
        let sign = if inst.transpose > 0 { "+" } else { "" };
        format!("  {sign}{}", inst.transpose)
    } else {
        String::new()
    };
    entries.push(TreeEntry {
        label: format!("{prefix}{plugin_label}{range_label}{transpose_label}"),
        address: TreeAddress::Instrument(inst_idx),
        node: inst.node,
        color: Color::Green,
        indent: 0,
    });

    let has_pattern = inst.pattern.as_ref().is_some_and(|p| p.recording || !p.events.is_empty());
    let child_count = if has_pattern { 1 } else { 0 } + inst.effects.len() + inst.modulators.len();
    let mut child_idx = 0;
    let branch_for = |idx: usize| if idx == child_count - 1 { "╰" } else { "├" };

    if let Some(pat) = &inst.pattern {
        if pat.recording || !pat.events.is_empty() {
            let (icon, color, detail) = if pat.recording {
                ("\u{23fa}", Color::Red, "recording...".to_string())
            } else {
                let n = pat.events.iter().filter(|e| e.1 == 0x90).count();
                let mode = if pat.in_key { ", in-key" } else { "" };
                ("\u{25b6}", Color::Blue, format!("{:.0} beats, {n} notes{mode}", pat.length_beats))
            };
            entries.push(TreeEntry {
                label: format!("{prefix}{} {icon} Pattern  {detail}", branch_for(child_idx)),
                address: TreeAddress::Pattern(inst_idx),
                node: inst.node,
                color,
                indent: 1,
            });
            child_idx += 1;
        }
    }

    for (fx_idx, fx) in inst.effects.iter().enumerate() {
        entries.push(TreeEntry {
            label: format!("{prefix}{} fx {}  [{}]", branch_for(child_idx), fx.name, fx.format),
            address: TreeAddress::Effect { inst: inst_idx, index: fx_idx },
            node: fx.node,
            color: Color::Yellow,
            indent: 1,
        });
        child_idx += 1;
    }

    let slot_name = |slot: usize| -> String {
        if slot == 0 {
            inst.instrument.as_ref().map(|p| p.name.clone()).unwrap_or_else(|| "Instrument".into())
        } else {
            inst.effects.get(slot - 1).map(|p| p.name.clone()).unwrap_or_else(|| format!("fx{slot}"))
        }
    };
    let target_disp = |t: &ModTargetSlot| -> String {
        match t.kind {
            crate::plugin::chain::ModTargetKind::PluginParam { slot, .. } => {
                format!("{}: {}", slot_name(slot), t.param_name)
            }
            _ => t.param_name.clone(),
        }
    };
    for (mod_idx, m) in inst.modulators.iter().enumerate() {
        let source_label = m.source.label();
        let targets = if m.targets.is_empty() {
            String::new()
        } else {
            let list = m
                .targets
                .iter()
                .map(|t| format!("{} ({:.0}%)", target_disp(t), t.depth * 100.0))
                .collect::<Vec<_>>()
                .join(", ");
            format!(" \u{2192} {list}")
        };
        entries.push(TreeEntry {
            label: format!("{prefix}{} ~ {source_label}{targets}", branch_for(child_idx)),
            address: TreeAddress::Modulator { inst: inst_idx, index: mod_idx },
            node: m.node,
            color: Color::Magenta,
            indent: 1,
        });
        child_idx += 1;
    }
}

fn build_tree_entries(instruments: &[InstrumentNode], groups: &[GroupNode]) -> Vec<TreeEntry> {
    let mut entries = Vec::new();

    // Groups first: a header, the member instruments nested under it, then the
    // group's bus effects.
    for (g_idx, group) in groups.iter().enumerate() {
        let name = group.display_name(g_idx);
        let vol = if (group.volume - 1.0).abs() > f32::EPSILON {
            format!("  vol {:.2}", group.volume)
        } else {
            String::new()
        };
        entries.push(TreeEntry {
            label: format!("\u{25a6} {name}{vol}"),
            address: TreeAddress::Group(g_idx),
            node: group.node,
            color: Color::Cyan,
            indent: 0,
        });
        for (inst_idx, inst) in instruments.iter().enumerate() {
            if inst.group == Some(g_idx) {
                push_instrument_entries(&mut entries, inst, inst_idx, "  ");
            }
        }
        for (fx_idx, fx) in group.effects.iter().enumerate() {
            entries.push(TreeEntry {
                label: format!("  fx {}  [{}]  (bus)", fx.name, fx.format),
                address: TreeAddress::GroupEffect { group: g_idx, index: fx_idx },
                node: fx.node,
                color: Color::Yellow,
                indent: 1,
            });
        }

        // Group-scoped modulators (magenta), after the bus effects.
        if !group.modulators.is_empty() {
            // Resolve member ordinal -> plugin name for GroupMember targets.
            let members: Vec<&InstrumentNode> =
                instruments.iter().filter(|n| n.group == Some(g_idx)).collect();
            let member_plugin = |member: usize, slot: usize| -> String {
                let Some(m) = members.get(member) else { return format!("M{member}") };
                if slot == 0 {
                    m.instrument.as_ref().map(|p| p.name.clone()).unwrap_or_else(|| "Instrument".into())
                } else {
                    m.effects.get(slot - 1).map(|p| p.name.clone()).unwrap_or_else(|| format!("fx{slot}"))
                }
            };
            let target_disp = |t: &ModTargetSlot| -> String {
                match t.kind {
                    crate::plugin::chain::ModTargetKind::GroupMember { member, slot, .. } => {
                        format!("M{member} {}: {}", member_plugin(member, slot), t.param_name)
                    }
                    crate::plugin::chain::ModTargetKind::GroupBus { effect_index, .. } => {
                        let name = group.effects.get(effect_index)
                            .map(|p| p.name.clone())
                            .unwrap_or_else(|| format!("fx{effect_index}"));
                        format!("Bus {name}: {}", t.param_name)
                    }
                    crate::plugin::chain::ModTargetKind::GroupMemberMod { member, mod_index, field } => {
                        // Render live from the kind so it survives membership fixups.
                        use crate::plugin::chain::CrossModField;
                        let fname = match field {
                            CrossModField::Rate => "rate".to_string(),
                            CrossModField::Attack => "attack".to_string(),
                            CrossModField::Decay => "decay".to_string(),
                            CrossModField::Sustain => "sustain".to_string(),
                            CrossModField::Release => "release".to_string(),
                            CrossModField::Depth(ti) => format!("depth {ti}"),
                        };
                        format!("M{member} Mod{mod_index} {fname}")
                    }
                    _ => t.param_name.clone(),
                }
            };
            for (mod_idx, m) in group.modulators.iter().enumerate() {
                let source_label = m.source.label();
                let targets = if m.targets.is_empty() {
                    String::new()
                } else {
                    let list = m
                        .targets
                        .iter()
                        .map(|t| format!("{} ({:.0}%)", target_disp(t), t.depth * 100.0))
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!(" \u{2192} {list}")
                };
                entries.push(TreeEntry {
                    label: format!("  ~ {source_label}{targets}  (bus)"),
                    address: TreeAddress::GroupModulator { group: g_idx, index: mod_idx },
                    node: m.node,
                    color: Color::Magenta,
                    indent: 1,
                });
            }
        }
    }

    // Ungrouped instruments at the top level.
    for (inst_idx, inst) in instruments.iter().enumerate() {
        if inst.group.is_none() {
            push_instrument_entries(&mut entries, inst, inst_idx, "");
        }
    }

    entries
}

/// Truncate `s` to at most `max` characters, appending an ellipsis when cut.
/// Operates on `char`s (not bytes) so it never splits a multi-byte character —
/// param names can contain non-ASCII glyphs (e.g. the MIDI-learn source row
/// "MIDI (learning…)"), and byte-slicing those panics.
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else if max == 0 {
        String::new()
    } else {
        let kept: String = s.chars().take(max - 1).collect();
        format!("{kept}…")
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

fn build_help_lines(web: Option<&WebStatus>) -> Vec<String> {
    let mut lines = vec![
        "Tang — Terminal Audio Plugin Host".into(),
        "".into(),
    ];
    if let Some(web) = web {
        lines.extend([
            "Web interface (--web):".into(),
            format!("  {}", web.url),
            "  Open it in a browser: what is changed there shows here, and what is".into(),
            "  changed here shows there.".into(),
        ]);
        lines.extend(web.warnings().into_iter().map(|warning| format!("  {warning}")));
        lines.push("".into());
    }
    lines.extend(vec![
        "Global keybindings:".into(),
        "  1 2 3 4    Switch to tab by number".into(),
        "  Tab        Next tab".into(),
        "  Shift+Tab  Previous tab".into(),
        "  Ctrl+S     Save session".into(),
        "  Ctrl+Shift+S  Save session as (new filename)".into(),
        "  Ctrl+Q     Quit".into(),
        "".into(),
        "Session tab (chain focus):".into(),
        "  Up/Down    Navigate chain".into(),
        "  Shift+↑/↓  Move effect up/down".into(),
        "  Enter      Focus parameter list".into(),
        "  n          Add instrument (split); defaults to oscillator".into(),
        "  i          Replace instrument".into(),
        "  R          Set instrument key range / rename group".into(),
        "  v          Set instrument volume".into(),
        "  a          Add effect after selected".into(),
        "  d          Delete selected".into(),
        "  m          Add modulator (lane rack; group rack on a group node)".into(),
        "  g          Assign instrument to a group (submix bus)".into(),
        "  x          Set effect / group-bus dry/wet mix".into(),
        "  r          Record/stop pattern".into(),
        "  Ctrl+R     Clear pattern".into(),
        "  b          Set BPM".into(),
        "".into(),
        "Modulator (chain focus, lane or group):".into(),
        "  t          Add modulation target".into(),
        "  d          Delete modulator".into(),
        "".into(),
        "Piano tab:".into(),
        "  k          Open scale picker (root + scale)".into(),
        "  [          Shift view octave down".into(),
        "  ]          Shift view octave up".into(),
        "".into(),
        "Session tab (param focus):".into(),
        "  Up/Down    Navigate parameters".into(),
        "  Left/Right Adjust value (5%)".into(),
        "  Shift+←/→  Fine adjust (1%)".into(),
        "  Ctrl+←/→   Coarse adjust (10%)".into(),
        "  Enter      Type a value".into(),
        "  m          Modulate this parameter (new/existing modulator)".into(),
        "  l          MIDI learn: bind the next CC / pitch-bend / aftertouch".into(),
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
    ]);
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_is_char_boundary_safe() {
        // The MIDI-learn "learning" source row: 26 bytes but only 24 chars, so
        // the old byte-length check triggered truncation and `&s[..23]` sliced
        // through the 3-byte ellipsis — a panic when the param pane rendered a
        // still-learning modulator. It must not panic now.
        let s = "Source: MIDI (learning…)";
        assert_eq!(s.len(), 26);
        assert_eq!(s.chars().count(), 24);
        let out = truncate(s, 24); // name_width used by the param pane
        assert_eq!(out, s); // fits in 24 chars → unchanged

        // A genuinely-too-long multi-byte string truncates cleanly on a char
        // boundary and gains an ellipsis.
        let arrows = "→".repeat(30);
        let out = truncate(&arrows, 10);
        assert!(out.chars().count() <= 10);
        assert!(out.ends_with('…'));
        // Round-trips as valid UTF-8 (String is always valid, but assert the cut
        // landed on a boundary by re-counting).
        assert_eq!(out.chars().filter(|&c| c == '→').count(), 9);

        // Degenerate width.
        assert_eq!(truncate("abc", 0), "");
    }

    #[test]
    fn param_action_bar_shows_learn() {
        // Regression for "I can't see the 'l' at the bottom": the parameter pane
        // action bar must advertise MIDI-learn on plugin params.
        let inst = TreeAddress::Instrument(0);
        let acts = action_bar_items(Some(&inst), true);
        assert!(acts.iter().any(|(k, _)| *k == "l"), "learn missing: {acts:?}");
        assert!(acts.iter().any(|(k, _)| *k == "m"), "modulate missing: {acts:?}");
        // Chain focus keeps the chain actions (no bare 'l').
        let chain = action_bar_items(Some(&inst), false);
        assert!(chain.iter().any(|(k, _)| *k == "i"));
        assert!(!chain.iter().any(|(k, _)| *k == "l"));

        // A modulator's own params can be cross-modulated ('m') and learned ('l').
        let m = TreeAddress::Modulator { inst: 0, index: 0 };
        let macts = action_bar_items(Some(&m), true);
        assert!(macts.iter().any(|(k, _)| *k == "m"), "modulate missing on modulator: {macts:?}");
        assert!(macts.iter().any(|(k, _)| *k == "l"), "learn missing on modulator: {macts:?}");
    }

    #[test]
    fn modulator_target_depth_allows_negative() {
        // A depth row must be editable across -1..1 so modulation can invert.
        let m = ModulatorSlot {
            node: crate::app::model::NodeId(0),
            source: ModSourceSlot::Lfo {
                waveform: crate::plugin::chain::LfoWaveform::Sine,
                rate: 1.0,
            },
            targets: vec![ModTargetSlot {
                edge: crate::app::model::EdgeId(0),
                slot: 0,
                param_name: "cutoff".into(),
                kind: crate::plugin::chain::ModTargetKind::PluginParam { slot: 0, param_index: 0 },
                depth: -0.5,
                param_min: 0.0,
                param_max: 1.0,
                effective: None,
            }],
            output: None,
        };
        // LFO rows: 0=Type, 1=Waveform, 2=Rate, 3=Targets separator, 4=first depth.
        assert_eq!(modulator_param_range(&m, 4), Some((-1.0, 1.0)));
        let edit = modulator_edit_state(&m, 4).expect("depth row is editable");
        assert_eq!((edit.param_min, edit.param_max), (-1.0, 1.0));
    }
}
