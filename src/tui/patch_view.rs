//! The Patch tab: the terminal's patchbay (P3e of `docs/patch-graph.md` §11.6, step 79 of `docs/web-ui.md`).
//!
//! The audio cables of a session as a matrix, as the pin matrix of an old modular synthesizer has them: a row for every output of audio (what a lane's instrument puts out, each effect, each
//! group) and a column for every input of audio (each effect, each group's bus, the mix). The pin at a crosspoint is the level of the cable that is drawn there; a dot says that there is
//! none, and a cross that one drawn there would make the audio go round. The links that a lane or a group makes by itself (step 88: an instrument to its first effect, an effect to the next, the end of
//! a chain to its group's bus or the mix) are cables here too, a filled dot at their crosspoints, with no level of their own: Enter cuts one, and Enter where one is cut gives it back. What is done with a pin is done by the intents that the page's cables are (`AddCable`, `SetCableGain`, `RemoveEdge`), and
//! so are the way out of a lane or a group (`SetRoute`), its mute and its solo, which the first columns of a row show: the tab only says which. Everything on it is read from the model on
//! every frame, so what a browser draws is here at once and what is done here is seen there.

use std::collections::{BTreeMap, HashMap};
use std::ops::Range;

use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use view::DIM;

use crate::app::graph::{Endpoint, Port};
use crate::app::intent::Intent;
use crate::app::model::{CABLE_GAIN_MAX, EdgeId, LinkOf, NodeId, SessionModel};
use crate::plugin::chain::{CableFrom, CableProblem, CableTo};
use crate::session::{Feed, Route};

/// How wide a column of the matrix is, in cells of the terminal: a level (`0.60`) and a blank.
pub const COLUMN: u16 = 5;

/// What is drawn quietly, and what is lit (the cursor's row and column).
const QUIET: Style = Style::new().fg(DIM);
const LIT: Style = Style::new().fg(Color::White).add_modifier(Modifier::BOLD);

/// How much `+` and `-` turn the level of a cable: a twentieth of its range, as a step of `Left` or `Right` is on the Session tab.
const LEVEL_STEP: f32 = 0.05;

/// The width of what a row says before its tag: the way out (a mark and where it goes), a blank, the mute, the solo and a blank.
const FLAGS_WIDTH: u16 = 8;

/// The width of a row's tag (`L1`, `G12`) and the blank after it.
const TAG_WIDTH: u16 = 4;

/// The narrowest and the widest that the names of the rows are given, in cells of the terminal.
const LABEL_WIDTHS: (u16, u16) = (10, 26);

/// How many lines the foot of the tab has for what is under the cursor (what it joins, what it is, why not), and the rule above them.
const INFO_LINES: u16 = 3;

/// What a row says of the lane or the group whose output it is: the way out, the mute and the solo, which are the lane's and the group's and not the output's (an effect's row has its lane's).
#[derive(Clone, Debug, PartialEq)]
pub struct Flags {
    pub muted: bool,
    pub solo: bool,
    pub route: Route,
    /// Where the way out goes, when it is not cut: `G2` (the bus of a group) or `Mix`.
    pub goes_to: String,
}

/// An output of audio: a row of the matrix.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub from: CableFrom,
    /// The lane or the group that the output is of: whose way out, mute and solo the keys on the row are.
    pub owner: NodeId,
    /// The first row of its lane or group, which is where the flags are shown.
    pub head: bool,
    /// Whether it is of a group (its output or an effect of its bus) and not of a lane.
    pub group: bool,
    /// `L1` for the first lane, `G1` for the first group.
    pub tag: String,
    /// What the row is called in the matrix.
    pub label: String,
    /// What it is called where it is told in words: its tag and its name.
    pub name: String,
    pub flags: Flags,
}

/// An input of audio: a column of the matrix.
#[derive(Clone, Debug, PartialEq)]
pub struct Column {
    pub to: CableTo,
    pub tag: String,
    /// The first letters of what it is, for the head of the column.
    pub short: String,
    pub name: String,
}

/// What the matrix is of: the outputs and the inputs that the session has, in the order of its lanes and groups, and where its audio cables are.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Bench {
    pub rows: Vec<Row>,
    pub cols: Vec<Column>,
    /// The audio cables: the row and the column of each, by the id.
    pins: BTreeMap<(usize, usize), EdgeId>,
}

/// What is at a crosspoint.
#[derive(Clone, Debug, PartialEq)]
pub enum Cell {
    /// A cable is drawn from the row to the column, at this level.
    Pin { edge: EdgeId, gain: f32 },
    /// The link of the audio path that a lane or a group makes by itself joins the row and the column (step 88): there, or cut. It has no level of its own, and a cable drawn between its ends gives it
    /// back, if it can be given back (where it would make the audio go round the cell is `Refused`).
    Link { of: LinkOf, present: bool },
    /// There is none, and one can be drawn.
    Free,
    /// There is none, and one cannot: the reason.
    Refused(String),
}

/// What a key asks of the program.
#[derive(Clone, Debug, PartialEq)]
pub enum Act {
    /// Nothing: the key moved the cursor, or meant nothing here.
    None,
    /// What the controller is asked, as a browser's request is.
    Apply(Intent),
    /// A number is to be typed for the level of this cable, which has this level now.
    TypeLevel { edge: EdgeId, gain: f32 },
    /// Something to say in the tab, where a refusal of the controller is said too.
    Notice(String),
}

/// What is said of a cable that cannot be drawn: that the audio would go round, without the kernel's account of the way it goes.
fn why(problem: CableProblem) -> String {
    match problem {
        CableProblem::Loop(_) => "the audio would go round".to_string(),
        other => other.to_string(),
    }
}

/// The first letters of a name, for the head of a column.
fn short(name: &str) -> String {
    name.chars().filter(|c| !c.is_whitespace()).take(COLUMN as usize - 1).collect()
}

/// What is said of a link that a key would give a level.
const NO_LEVEL: &str = "A link of the audio path has no level of its own: cut it (Enter) and it is the cables that you draw that have one";

/// What cuts the link `of`: the intent that the cable between its ends is taken out by.
fn cut(of: LinkOf) -> Intent {
    match of {
        LinkOf::WayOut(node) => Intent::SetRoute { node, route: Route::None },
        LinkOf::Feed(node) => Intent::SetFeed { node, feed: Feed::None },
    }
}

/// The node that a cable from `from` is drawn from, as the intents name it.
fn node_from(from: CableFrom) -> NodeId {
    match from {
        CableFrom::Instrument(node) | CableFrom::Effect(node) | CableFrom::Group(node) => node,
    }
}

/// The end that a cable to `to` is drawn to, as the intents name it: the node and its input (the second one of an effect that has two).
fn end_of(to: CableTo) -> Endpoint {
    match to {
        CableTo::Effect(node) | CableTo::GroupBus(node) => Endpoint { node, port: Port::AudioIn },
        CableTo::EffectSide(node) => Endpoint { node, port: Port::AudioInSide },
        CableTo::Master => Endpoint { node: NodeId::MASTER, port: Port::AudioIn },
    }
}

impl Bench {
    /// The matrix of a session: the rows and the columns that its lanes, its groups and their effects make, and its audio cables.
    pub fn of(model: &SessionModel) -> Bench {
        let mut rows = Vec::new();
        let mut cols = Vec::new();
        for (index, lane) in model.instruments.iter().enumerate() {
            let tag = format!("L{}", index + 1);
            let goes_to = match lane.group.filter(|group| *group < model.groups.len()) {
                Some(group) => format!("G{}", group + 1),
                None => "Mix".to_string(),
            };
            let flags = Flags { muted: lane.muted, solo: lane.solo, route: lane.route, goes_to };
            let (label, name) = match &lane.instrument {
                Some(plugin) => (format!("\u{266a} {}", plugin.name), format!("{tag} {}", plugin.name)),
                None => ("\u{266a} (empty)".to_string(), format!("{tag} (an empty lane)")),
            };
            rows.push(Row { from: CableFrom::Instrument(lane.node), owner: lane.node, head: true, group: false, tag: tag.clone(), label, name, flags: flags.clone() });
            for effect in &lane.effects {
                let name = format!("{tag} {}", effect.name);
                rows.push(Row { from: CableFrom::Effect(effect.node), owner: lane.node, head: false, group: false, tag: tag.clone(), label: format!("  fx {}", effect.name), name: name.clone(), flags: flags.clone() });
                cols.push(Column { to: CableTo::Effect(effect.node), tag: tag.clone(), short: short(&effect.name), name });
                // An effect that has a second input has a column for it beside its first (step 95): `↳B`, and `L1 Math (B)` in the foot.
                if let Some(side) = effect.side {
                    cols.push(Column { to: CableTo::EffectSide(effect.node), tag: tag.clone(), short: format!("\u{21b3}{}", side.side), name: format!("{tag} {} ({})", effect.name, side.side) });
                }
            }
        }
        for (index, group) in model.groups.iter().enumerate() {
            let tag = format!("G{}", index + 1);
            let name = group.display_name(index);
            let flags = Flags { muted: group.muted, solo: group.solo, route: group.route, goes_to: "Mix".to_string() };
            rows.push(Row { from: CableFrom::Group(group.node), owner: group.node, head: true, group: true, tag: tag.clone(), label: format!("\u{25a6} {name}"), name: format!("{tag} {name}"), flags: flags.clone() });
            cols.push(Column { to: CableTo::GroupBus(group.node), tag: tag.clone(), short: "bus".to_string(), name: format!("{tag} {name}'s bus") });
            for effect in &group.effects {
                let name = format!("{tag} {} (bus)", effect.name);
                rows.push(Row { from: CableFrom::Effect(effect.node), owner: group.node, head: false, group: true, tag: tag.clone(), label: format!("  fx {} (bus)", effect.name), name: name.clone(), flags: flags.clone() });
                cols.push(Column { to: CableTo::Effect(effect.node), tag: tag.clone(), short: short(&effect.name), name });
                if let Some(side) = effect.side {
                    cols.push(Column { to: CableTo::EffectSide(effect.node), tag: tag.clone(), short: format!("\u{21b3}{}", side.side), name: format!("{tag} {} (bus, {})", effect.name, side.side) });
                }
            }
        }
        cols.push(Column { to: CableTo::Master, tag: "Mix".to_string(), short: String::new(), name: "the mix".to_string() });
        let pins = model
            .cables
            .iter()
            .filter_map(|cable| {
                let row = rows.iter().position(|row| row.from == cable.from)?;
                let col = cols.iter().position(|col| col.to == cable.to)?;
                Some(((row, col), cable.id))
            })
            .collect();
        Bench { rows, cols, pins }
    }
}

/// The matrix with a cursor on it: where it is, which part of it is in view, and what has been worked out of it.
#[derive(Debug, Default)]
pub struct Patch {
    row: usize,
    col: usize,
    /// What the cursor is on, by what it joins, so that it stays on that when rows and columns come and go (a browser adds a lane above it).
    on: (Option<CableFrom>, Option<CableTo>),
    /// The first row and the first column in view.
    top: usize,
    left: usize,
    /// How many rows and columns the last frame had room for.
    rows_in_view: usize,
    cols_in_view: usize,
    /// Where the last frame drew the crosspoints and the names of the rows, for the mouse.
    cells: Rect,
    names: Rect,
    /// What the last key was refused with, until the next key.
    pub notice: Option<String>,
    /// What the refusals below were worked out of, and why a cable cannot be drawn at a crosspoint (none if it can): a refusal is a compile of the wiring, so it is worked out for the
    /// crosspoints in view and kept until the wiring changes.
    seen: Bench,
    refusals: HashMap<(usize, usize), Option<String>>,
}

impl Patch {
    pub fn new() -> Patch {
        Patch { rows_in_view: 10, cols_in_view: 10, ..Patch::default() }
    }

    /// Bring the cursor and what is kept up to date with the session: the cursor goes with the output and the input it was on if they are still there, and stays within the matrix if not,
    /// and what was worked out of a wiring that is not this one is forgotten.
    fn settle(&mut self, bench: &Bench) {
        if *bench != self.seen {
            self.refusals.clear();
            if let Some(at) = self.on.0.and_then(|from| bench.rows.iter().position(|row| row.from == from)) {
                self.row = at;
            }
            if let Some(at) = self.on.1.and_then(|to| bench.cols.iter().position(|col| col.to == to)) {
                self.col = at;
            }
            self.seen = bench.clone();
        }
        self.row = self.row.min(bench.rows.len().saturating_sub(1));
        self.col = self.col.min(bench.cols.len().saturating_sub(1));
        self.on = (bench.rows.get(self.row).map(|row| row.from), bench.cols.get(self.col).map(|col| col.to));
    }

    /// What is at the crosspoint of row `row` and column `col`.
    pub fn cell(&mut self, model: &SessionModel, bench: &Bench, row: usize, col: usize) -> Cell {
        if let Some(&edge) = bench.pins.get(&(row, col)) {
            let gain = model.audio_cable(edge).map_or(0.0, |cable| cable.gain);
            return Cell::Pin { edge, gain };
        }
        let (Some(from), Some(to)) = (bench.rows.get(row).map(|row| row.from), bench.cols.get(col).map(|col| col.to)) else { return Cell::Free };
        // A link of the audio path: there, or cut and given back by a cable drawn between its ends, which the intents refuse where the audio would go round with the link (and not by the rule of a
        // cable: a group's own link into its first bus effect starts at the group, and as a cable it would go round).
        let link = model.template_link(from, to);
        let problem = self.refusals.entry((row, col)).or_insert_with(|| match link {
            None => model.cable_problem(from, to).map(why),
            Some(link) if link.present => None,
            Some(link) => match link.of {
                LinkOf::WayOut(node) => model.locate(node).and_then(|path| model.loop_if_route_is(path, Route::Out)),
                LinkOf::Feed(node) => model.loop_if_feed_is(node, true),
            }
            .map(|_| "the audio would go round".to_string()),
        });
        match (problem, link) {
            (Some(why), _) => Cell::Refused(why.clone()),
            (None, Some(link)) => Cell::Link { of: link.of, present: link.present },
            (None, None) => Cell::Free,
        }
    }

    /// The cell under the cursor.
    fn here(&mut self, model: &SessionModel, bench: &Bench) -> Cell {
        self.cell(model, bench, self.row, self.col)
    }

    /// What a key does. Movement is the tab's own; the rest is what the controller is asked, which the caller carries out (and says why, if it is refused).
    pub fn press(&mut self, model: &SessionModel, code: KeyCode, modifiers: KeyModifiers) -> Act {
        self.notice = None;
        let bench = Bench::of(model);
        self.settle(&bench);
        let page = self.rows_in_view.saturating_sub(1).max(1);
        // (What goes past the end of the matrix is held in it by `settle`, which also has the cursor go with what it is on.)
        match code {
            KeyCode::Up => self.row = self.row.saturating_sub(1),
            KeyCode::Down => self.row = self.row.saturating_add(1),
            KeyCode::Left => self.col = self.col.saturating_sub(1),
            KeyCode::Right => self.col = self.col.saturating_add(1),
            KeyCode::PageUp => self.row = self.row.saturating_sub(page),
            KeyCode::PageDown => self.row = self.row.saturating_add(page),
            KeyCode::Home => self.col = 0,
            KeyCode::End => self.col = usize::MAX,
            _ => return self.act(model, &bench, code, modifiers),
        }
        self.settle(&bench);
        Act::None
    }

    /// What a key that is not a movement asks.
    fn act(&mut self, model: &SessionModel, bench: &Bench, code: KeyCode, modifiers: KeyModifiers) -> Act {
        if modifiers.contains(KeyModifiers::CONTROL) {
            return Act::None;
        }
        let (Some(row), Some(col)) = (bench.rows.get(self.row), bench.cols.get(self.col)) else { return Act::None };
        let cell = self.here(model, bench);
        match code {
            // A pin is put in, or taken out.
            KeyCode::Enter | KeyCode::Char(' ') => match cell {
                // (A link that is cut is given back by the cable that is asked for between its ends: the controller turns it into the link.)
                Cell::Free | Cell::Link { present: false, .. } => Act::Apply(Intent::AddCable {
                    from: Endpoint { node: node_from(row.from), port: Port::AudioOut },
                    to: end_of(col.to),
                    gain: None,
                }),
                Cell::Pin { edge, .. } => Act::Apply(Intent::RemoveEdge { edge }),
                Cell::Link { of, present: true } => Act::Apply(cut(of)),
                Cell::Refused(why) => Act::Notice(format!("{} cannot send to {}: {why}", row.name, col.name)),
            },
            KeyCode::Char('d') | KeyCode::Delete | KeyCode::Backspace => match cell {
                Cell::Pin { edge, .. } => Act::Apply(Intent::RemoveEdge { edge }),
                Cell::Link { of, present: true } => Act::Apply(cut(of)),
                _ => Act::None,
            },
            KeyCode::Char('+' | '=') => self.turn(cell, LEVEL_STEP),
            KeyCode::Char('-' | '_') => self.turn(cell, -LEVEL_STEP),
            KeyCode::Char('e') => match cell {
                Cell::Pin { edge, gain } => Act::TypeLevel { edge, gain },
                Cell::Link { present: true, .. } => Act::Notice(NO_LEVEL.to_string()),
                _ => Act::None,
            },
            // What is the lane's or the group's whose row it is.
            KeyCode::Char('w') => Act::Apply(Intent::SetRoute { node: row.owner, route: if row.flags.route == Route::Out { Route::None } else { Route::Out } }),
            KeyCode::Char('M') => Act::Apply(Intent::SetMute { node: row.owner, muted: !row.flags.muted }),
            KeyCode::Char('S') => Act::Apply(Intent::SetSolo { node: row.owner, solo: !row.flags.solo }),
            _ => Act::None,
        }
    }

    /// The level of the cable under the cursor, `delta` more (held to what a level can be, and to hundredths, so that a run of steps does not drift); nothing for a crosspoint with no cable and
    /// for a level that is at the end.
    fn turn(&self, cell: Cell, delta: f32) -> Act {
        if matches!(cell, Cell::Link { present: true, .. }) {
            return Act::Notice(NO_LEVEL.to_string());
        }
        let Cell::Pin { edge, gain } = cell else { return Act::None };
        let next = (((gain + delta) * 100.0).round() / 100.0).clamp(0.0, CABLE_GAIN_MAX);
        if next == gain {
            return Act::None;
        }
        Act::Apply(Intent::SetCableGain { edge, gain: next })
    }

    /// A click on the crosspoint or on the name of the row that it is at: the cursor goes there. Whether it was on the matrix.
    pub fn click(&mut self, model: &SessionModel, x: u16, y: u16) -> bool {
        let bench = Bench::of(model);
        self.settle(&bench);
        let inside = |area: Rect| area.contains((x, y).into());
        if inside(self.cells) {
            self.row = (self.top + (y - self.cells.y) as usize).min(bench.rows.len().saturating_sub(1));
            self.col = (self.left + ((x - self.cells.x) / COLUMN) as usize).min(bench.cols.len().saturating_sub(1));
        } else if inside(self.names) {
            self.row = (self.top + (y - self.names.y) as usize).min(bench.rows.len().saturating_sub(1));
        } else {
            return false;
        }
        self.settle(&bench);
        true
    }

    /// The wheel: the cursor goes `rows` rows down (up, below 0).
    pub fn scroll(&mut self, model: &SessionModel, rows: isize) {
        let bench = Bench::of(model);
        self.settle(&bench);
        self.row = self.row.saturating_add_signed(rows).min(bench.rows.len().saturating_sub(1));
        self.settle(&bench);
    }

    /// The lines at the foot of the tab: what the cursor is on, what is there, and what the last key was told.
    fn info(&mut self, model: &SessionModel, bench: &Bench) -> [Line<'static>; INFO_LINES as usize] {
        let (Some(row), Some(col)) = (bench.rows.get(self.row), bench.cols.get(self.col)) else { return Default::default() };
        let what = match row.from {
            CableFrom::Instrument(_) => "the instrument's output, after its volume",
            CableFrom::Effect(_) => "the output of the effect",
            CableFrom::Group(_) => "the output of the group",
        };
        let plain = Style::default().fg(Color::White);
        let joined = Line::from(vec![Span::styled(row.name.clone(), LIT), Span::styled(format!(" ({what})  \u{25b8}  "), QUIET), Span::styled(col.name.clone(), LIT)]);
        let cell = match self.here(model, bench) {
            Cell::Pin { gain, .. } => Line::from(Span::styled(format!("A cable at level {gain:.2}: Enter takes it out, + and - turn it, e types a level"), Style::default().fg(Color::Yellow))),
            Cell::Link { present: true, .. } => Line::from(Span::styled("A link of the audio path (no level of its own): Enter cuts it", Style::default().fg(Color::Yellow))),
            Cell::Link { present: false, .. } => Line::from(Span::styled("The link of the audio path is cut: Enter gives it back", plain)),
            Cell::Free => Line::from(Span::styled("No cable: Enter draws one at level 1.00", plain)),
            Cell::Refused(why) => Line::from(Span::styled(format!("No cable can go here: {why}"), Style::default().fg(Color::Red))),
        };
        let way = {
            let mut text = format!("{}'s way out ", row.tag);
            text.push_str(&if row.flags.route == Route::Out { format!("goes to {} (w cuts it)", if row.flags.goes_to == "Mix" { "the mix".to_string() } else { row.flags.goes_to.clone() }) } else { "is cut: only its cables are heard (w gives it back)".to_string() });
            if row.flags.muted {
                text.push_str(", muted");
            }
            if row.flags.solo {
                text.push_str(", soloed");
            }
            Line::from(Span::styled(text, QUIET))
        };
        let notice = match &self.notice {
            Some(why) => Line::from(Span::styled(why.clone(), Style::default().fg(Color::Red).add_modifier(Modifier::BOLD))),
            None => way,
        };
        [joined, cell, notice]
    }

    /// What is in view follows the cursor, and does not leave room at the end that there is something to fill.
    fn follow_cursor(&mut self, bench: &Bench) {
        self.top = self.top.min(self.row).max((self.row + 1).saturating_sub(self.rows_in_view));
        self.left = self.left.min(self.col).max((self.col + 1).saturating_sub(self.cols_in_view));
        self.top = self.top.min(bench.rows.len().saturating_sub(self.rows_in_view));
        self.left = self.left.min(bench.cols.len().saturating_sub(self.cols_in_view));
    }

    /// The title of the box: where the tab is, and when the matrix does not fit, which of it is in view.
    fn title(&self, bench: &Bench, rows: &Range<usize>, cols: &Range<usize>) -> String {
        if bench.rows.len() > self.rows_in_view || bench.cols.len() > self.cols_in_view {
            format!(" Patch  \u{b7}  outputs {}\u{2013}{} of {}  \u{b7}  inputs {}\u{2013}{} of {} ", rows.start + 1, rows.end, bench.rows.len(), cols.start + 1, cols.end, bench.cols.len())
        } else {
            " Patch ".to_string()
        }
    }

    /// The two lines over the crosspoints: the tag of each input in view, and the first letters of what it is. The cursor's is lit.
    fn head(&self, bench: &Bench, layout: &Layout, cols: Range<usize>) -> Vec<Line<'static>> {
        let before = layout.before_cells() as usize;
        let mut tags = vec![Span::styled(format!("{:<before$}", "outputs \u{2193}  inputs \u{2192}"), QUIET)];
        let mut names = vec![Span::raw(" ".repeat(before))];
        for col in cols {
            let style = if col == self.col { LIT } else { QUIET };
            tags.push(Span::styled(format!("{:>4} ", bench.cols[col].tag), style));
            names.push(Span::styled(format!("{:>4} ", bench.cols[col].short), style));
        }
        vec![Line::from(tags), Line::from(names)]
    }

    /// The line of row `row`: what its lane or group says of its way out, mute and solo, then its tag and its name, then its crosspoints in `cols`.
    fn row_line(&mut self, model: &SessionModel, bench: &Bench, layout: &Layout, row: usize, cols: Range<usize>) -> Line<'static> {
        let description = &bench.rows[row];
        let on_it = row == self.row;
        let flags = &description.flags;
        let way = match (description.head, flags.route) {
            (false, _) => "    ".to_string(),
            (true, Route::Out) => format!("\u{25aa}{:<3}", flags.goes_to),
            (true, Route::None) => "\u{25ab}   ".to_string(),
        };
        let marks = if description.head { format!("{}{}", if flags.muted { 'M' } else { ' ' }, if flags.solo { 'S' } else { ' ' }) } else { "  ".to_string() };
        let name_style = match (on_it, description.group) {
            (true, true) => Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            (true, false) => LIT,
            (false, true) => Style::default().fg(Color::Cyan).add_modifier(Modifier::DIM),
            (false, false) => QUIET,
        };
        let label: String = description.label.chars().take(layout.label_width as usize).collect();
        let mut spans = vec![
            Span::styled(format!("{way} "), if on_it { LIT } else { QUIET }),
            Span::styled(format!("{marks} "), LIT),
            Span::styled(format!("{:<tag$}", description.tag, tag = TAG_WIDTH as usize), name_style),
            Span::styled(format!("{label:<width$}  ", width = layout.label_width as usize), name_style),
        ];
        for col in cols {
            let (text, mut style) = match self.cell(model, bench, row, col) {
                Cell::Pin { gain, .. } => (format!("{gain:>4.2} "), Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
                Cell::Link { present: true, .. } => (format!("{:>4} ", "\u{25cf}"), Style::default().fg(Color::Yellow)),
                Cell::Free | Cell::Link { present: false, .. } => (format!("{:>4} ", "\u{b7}"), QUIET),
                Cell::Refused(_) => (format!("{:>4} ", "\u{d7}"), Style::default().fg(Color::Red).add_modifier(Modifier::DIM)),
            };
            if on_it && col == self.col {
                style = style.add_modifier(Modifier::REVERSED);
            }
            spans.push(Span::styled(text, style));
        }
        Line::from(spans)
    }

    /// Draw the tab into `area`.
    pub fn render(&mut self, frame: &mut Frame, area: Rect, model: &SessionModel) {
        let bench = Bench::of(model);
        self.settle(&bench);
        // (A click is on what the last frame drew, and a frame with no room draws nothing.)
        (self.cells, self.names) = (Rect::default(), Rect::default());

        let layout = Layout::of(area, &bench);
        self.rows_in_view = (layout.rows_height as usize).max(1);
        self.cols_in_view = ((layout.cols_width / COLUMN) as usize).max(1);
        self.follow_cursor(&bench);
        let rows = self.top..(self.top + self.rows_in_view).min(bench.rows.len());
        let cols = self.left..(self.left + self.cols_in_view).min(bench.cols.len());

        let block = Block::default().borders(Borders::ALL).border_style(Style::default().fg(Color::White)).title(self.title(&bench, &rows, &cols));
        frame.render_widget(block, area);
        let inner = layout.inner;
        if inner.width == 0 || inner.height < 3 {
            return;
        }

        frame.render_widget(Paragraph::new(self.head(&bench, &layout, cols.clone())), Rect::new(inner.x, inner.y, inner.width, 2));
        let lines: Vec<Line> = rows.clone().map(|row| self.row_line(model, &bench, &layout, row, cols.clone())).collect();
        let body = Rect::new(inner.x, inner.y + 2, inner.width, layout.rows_height.min(inner.height.saturating_sub(2)));
        frame.render_widget(Paragraph::new(lines), body);
        self.cells = Rect::new(layout.cells_x(), body.y, (cols.len() as u16 * COLUMN).min(layout.cols_width), rows.len() as u16);
        self.names = Rect::new(inner.x, body.y, layout.before_cells() - 2, rows.len() as u16);

        // The foot: a rule, and what the cursor is on.
        if layout.foot > 0 {
            let at = Rect::new(inner.x, inner.y + inner.height - layout.foot, inner.width, layout.foot);
            let rule = Block::default().borders(Borders::TOP).border_style(QUIET);
            let text = rule.inner(at);
            frame.render_widget(rule, at);
            frame.render_widget(Paragraph::new(self.info(model, &bench).to_vec()), text);
        }
    }
}

/// Where the parts of the tab go in the room it has.
struct Layout {
    /// What the border of the box leaves.
    inner: Rect,
    /// How many lines the foot has, the rule over it included: none when the room is too small for it.
    foot: u16,
    /// How many lines the outputs have.
    rows_height: u16,
    /// How wide the names of the outputs are drawn.
    label_width: u16,
    /// How wide the room for the crosspoints is.
    cols_width: u16,
}

impl Layout {
    fn of(area: Rect, bench: &Bench) -> Layout {
        let inner = Block::default().borders(Borders::ALL).inner(area);
        let foot = if inner.height >= 12 { INFO_LINES + 1 } else { 0 };
        let widest = bench.rows.iter().map(|row| row.label.chars().count() as u16).max().unwrap_or(0);
        let label_width = widest.clamp(LABEL_WIDTHS.0, LABEL_WIDTHS.1).min(inner.width.saturating_sub(FLAGS_WIDTH + TAG_WIDTH + 2 + COLUMN).max(1));
        let mut layout = Layout { inner, foot, rows_height: inner.height.saturating_sub(2 + foot), label_width, cols_width: 0 };
        layout.cols_width = (inner.x + inner.width).saturating_sub(layout.cells_x());
        layout
    }

    /// How many cells of a line come before its first crosspoint: the flags, the tag, the name and the blanks after it.
    fn before_cells(&self) -> u16 {
        FLAGS_WIDTH + TAG_WIDTH + self.label_width + 2
    }

    /// The column of the terminal where the crosspoints begin.
    fn cells_x(&self) -> u16 {
        self.inner.x + self.before_cells()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::model::AudioCable;
    use crate::app::testutil::{self, PATCHED, SENDS, SIDED, Sends, Sided};

    fn bench_of(text: &str) -> (testutil::Run, Bench) {
        let run = testutil::load(text);
        let bench = Bench::of(&run.model);
        (run, bench)
    }

    /// The row and the column of the crosspoint that joins `from` and `to`.
    fn at(bench: &Bench, from: CableFrom, to: CableTo) -> (usize, usize) {
        (bench.rows.iter().position(|row| row.from == from).expect("the output is a row"), bench.cols.iter().position(|col| col.to == to).expect("the input is a column"))
    }

    /// A patch whose cursor is on the crosspoint of `from` and `to`.
    fn on(bench: &Bench, from: CableFrom, to: CableTo) -> Patch {
        let mut patch = Patch::new();
        (patch.row, patch.col) = at(bench, from, to);
        patch.settle(bench);
        patch
    }

    fn key(patch: &mut Patch, model: &SessionModel, code: KeyCode) -> Act {
        patch.press(model, code, KeyModifiers::NONE)
    }

    #[test]
    fn the_rows_are_the_outputs_and_the_columns_the_inputs_of_the_lanes_and_the_groups_in_their_order() {
        let (run, bench) = bench_of(SENDS);
        let n = Sends::of(&run.model);
        let rows: Vec<CableFrom> = bench.rows.iter().map(|row| row.from).collect();
        assert_eq!(
            rows,
            [
                CableFrom::Instrument(n.lane[0]),
                CableFrom::Effect(n.fx[0]),
                CableFrom::Effect(n.fx[1]),
                CableFrom::Instrument(n.lane[1]),
                CableFrom::Effect(n.fx[2]),
                CableFrom::Group(n.group),
                CableFrom::Effect(n.bus),
            ]
        );
        let cols: Vec<CableTo> = bench.cols.iter().map(|col| col.to).collect();
        assert_eq!(cols, [CableTo::Effect(n.fx[0]), CableTo::Effect(n.fx[1]), CableTo::Effect(n.fx[2]), CableTo::GroupBus(n.group), CableTo::Effect(n.bus), CableTo::Master]);
    }

    #[test]
    fn an_effect_that_has_a_second_input_has_a_column_for_it_beside_its_first_and_the_cable_at_it_is_a_pin_there() {
        let (run, bench) = bench_of(SIDED);
        let n = Sided::of(&run.model);
        let cols: Vec<CableTo> = bench.cols.iter().map(|col| col.to).collect();
        assert_eq!(
            cols,
            [CableTo::Effect(n.filter), CableTo::Effect(n.math), CableTo::EffectSide(n.math), CableTo::GroupBus(n.group), CableTo::Effect(n.bus_math), CableTo::EffectSide(n.bus_math), CableTo::Master]
        );
        let names: Vec<&str> = bench.cols.iter().map(|col| col.name.as_str()).collect();
        assert_eq!(names, ["L1 Filter", "L1 Math", "L1 Math (B)", "G1 Group 1's bus", "G1 Math (bus)", "G1 Math (bus, B)", "the mix"]);
        let shorts: Vec<&str> = bench.cols.iter().map(|col| col.short.as_str()).collect();
        assert_eq!(shorts, ["Filt", "Math", "\u{21b3}B", "bus", "Math", "\u{21b3}B", ""]);
        assert!(bench.cols.iter().all(|col| col.tag != "L2"), "the lane that has no effect has no column");
        // The cable of the file is a pin at lane 2's instrument and the second input of lane 1's math effect, and nowhere else.
        let (row, col) = at(&bench, CableFrom::Instrument(n.lane[1]), CableTo::EffectSide(n.math));
        assert_eq!(bench.pins.get(&(row, col)), Some(&run.model.cables[0].id));
        assert_eq!(bench.pins.len(), 1);
    }

    #[test]
    fn enter_at_the_column_of_a_second_input_draws_a_cable_there_and_one_at_the_first_input_is_another_cable() {
        let (run, bench) = bench_of(SIDED);
        let n = Sided::of(&run.model);
        let model = &run.model;
        let mut patch = on(&bench, CableFrom::Instrument(n.lane[0]), CableTo::EffectSide(n.bus_math));
        assert_eq!(
            key(&mut patch, model, KeyCode::Enter),
            Act::Apply(Intent::AddCable { from: Endpoint { node: n.lane[0], port: Port::AudioOut }, to: Endpoint { node: n.bus_math, port: Port::AudioInSide }, gain: None })
        );
        let mut patch = on(&bench, CableFrom::Instrument(n.lane[1]), CableTo::Effect(n.math));
        assert_eq!(
            key(&mut patch, model, KeyCode::Enter),
            Act::Apply(Intent::AddCable { from: Endpoint { node: n.lane[1], port: Port::AudioOut }, to: Endpoint { node: n.math, port: Port::AudioIn }, gain: None }),
            "the same lane to the first input of the same effect: the cable of the file is at the second"
        );
        // Enter on the pin takes it out.
        let mut patch = on(&bench, CableFrom::Instrument(n.lane[1]), CableTo::EffectSide(n.math));
        assert_eq!(key(&mut patch, model, KeyCode::Enter), Act::Apply(Intent::RemoveEdge { edge: model.cables[0].id }));
    }

    #[test]
    fn a_crosspoint_at_the_second_input_of_an_effect_from_that_effect_or_from_what_it_feeds_is_refused_as_a_loop() {
        let (run, bench) = bench_of(SIDED);
        let n = Sided::of(&run.model);
        let mut patch = Patch::new();
        for from in [CableFrom::Effect(n.math), CableFrom::Group(n.group)] {
            let to = if from == CableFrom::Effect(n.math) { CableTo::EffectSide(n.math) } else { CableTo::EffectSide(n.bus_math) };
            (patch.row, patch.col) = at(&bench, from, to);
            patch.settle(&bench);
            assert!(matches!(patch.cell(&run.model, &bench, patch.row, patch.col), Cell::Refused(why) if why.contains("go round")), "{from:?} to {to:?}");
        }
        // What is not a loop is free: the filter before the effect, to its second input.
        let (row, col) = at(&bench, CableFrom::Effect(n.filter), CableTo::EffectSide(n.math));
        assert_eq!(patch.cell(&run.model, &bench, row, col), Cell::Free);
    }

    #[test]
    fn a_row_is_of_the_lane_or_the_group_whose_output_it_is_and_says_so_in_its_tag() {
        let (run, bench) = bench_of(SENDS);
        let n = Sends::of(&run.model);
        let owners: Vec<(&str, NodeId, bool, bool)> = bench.rows.iter().map(|row| (row.tag.as_str(), row.owner, row.head, row.group)).collect();
        assert_eq!(
            owners,
            [
                ("L1", n.lane[0], true, false),
                ("L1", n.lane[0], false, false),
                ("L1", n.lane[0], false, false),
                ("L2", n.lane[1], true, false),
                ("L2", n.lane[1], false, false),
                ("G1", n.group, true, true),
                ("G1", n.group, false, true)
            ]
        );
        let tags: Vec<&str> = bench.cols.iter().map(|col| col.tag.as_str()).collect();
        assert_eq!(tags, ["L1", "L1", "L2", "G1", "G1", "Mix"]);
    }

    #[test]
    fn the_names_say_what_each_row_and_column_is() {
        let (_, bench) = bench_of(SENDS);
        let labels: Vec<&str> = bench.rows.iter().map(|row| row.label.as_str()).collect();
        assert_eq!(labels, ["\u{266a} Oscillator", "  fx Filter", "  fx Delay", "\u{266a} Oscillator", "  fx Drive", "\u{25a6} Group 1", "  fx Reverb (bus)"]);
        let names: Vec<&str> = bench.rows.iter().map(|row| row.name.as_str()).collect();
        assert_eq!(names, ["L1 Oscillator", "L1 Filter", "L1 Delay", "L2 Oscillator", "L2 Drive", "G1 Group 1", "G1 Reverb (bus)"]);
        let shorts: Vec<&str> = bench.cols.iter().map(|col| col.short.as_str()).collect();
        assert_eq!(shorts, ["Filt", "Dela", "Driv", "bus", "Reve", ""]);
        let words: Vec<&str> = bench.cols.iter().map(|col| col.name.as_str()).collect();
        assert_eq!(words, ["L1 Filter", "L1 Delay", "L2 Drive", "G1 Group 1's bus", "G1 Reverb (bus)", "the mix"]);
    }

    #[test]
    fn a_lane_with_no_instrument_has_a_row_and_no_column() {
        let (_, bench) = bench_of(PATCHED);
        let last = bench.rows.iter().position(|row| row.tag == "L3").expect("the empty lane has a row");
        assert_eq!(bench.rows[last].label, "\u{266a} (empty)");
        assert_eq!(bench.rows[last].name, "L3 (an empty lane)");
        assert!(bench.cols.iter().all(|col| col.tag != "L3"), "a lane has no input");
    }

    #[test]
    fn a_row_says_where_the_way_out_of_its_lane_or_group_goes() {
        let (_, bench) = bench_of(SENDS);
        let goes: Vec<&str> = bench.rows.iter().map(|row| row.flags.goes_to.as_str()).collect();
        assert_eq!(goes, ["G1", "G1", "G1", "Mix", "Mix", "Mix", "Mix"], "the first lane is in the group, the second in none, a group goes to the mix");
        assert!(bench.rows.iter().all(|row| row.flags.route == Route::Out && !row.flags.muted && !row.flags.solo));
    }

    #[test]
    fn the_cables_of_a_session_are_pins_at_their_crosspoints_at_their_levels() {
        let (mut run, bench) = bench_of(PATCHED);
        let (lane1, filter0, delay0, group, bus_filter, bus_reverb) = {
            let m = &run.model;
            (m.instruments[1].node, m.instruments[0].effects[0].node, m.instruments[0].effects[1].node, m.groups[0].node, m.groups[0].effects[0].node, m.groups[0].effects[1].node)
        };
        let mut patch = Patch::new();
        let model = &mut run.model;
        let pins = [
            (CableFrom::Instrument(lane1), CableTo::Effect(filter0), 0.5),
            (CableFrom::Effect(filter0), CableTo::Effect(delay0), 0.75),
            (CableFrom::Effect(delay0), CableTo::GroupBus(group), 1.0),
            (CableFrom::Effect(bus_filter), CableTo::Effect(bus_reverb), 1.0),
            (CableFrom::Effect(bus_reverb), CableTo::Master, 0.5),
        ];
        for (from, to, level) in pins {
            let (row, col) = at(&bench, from, to);
            assert!(matches!(patch.cell(model, &bench, row, col), Cell::Pin { gain, .. } if gain == level), "{from:?} to {to:?}");
        }
        // The cable of the group to the drive of the lane that is in no group, which putting the lane in the group would make a loop of.
        let (row, col) = at(&bench, CableFrom::Group(group), CableTo::Effect(model.instruments[1].effects[0].node));
        assert!(matches!(patch.cell(model, &bench, row, col), Cell::Pin { gain, .. } if gain == 1.5));
    }

    #[test]
    fn a_crosspoint_that_has_no_cable_is_free_unless_a_cable_there_would_make_the_audio_go_round() {
        let (run, bench) = bench_of(SENDS);
        let n = Sends::of(&run.model);
        let mut patch = Patch::new();
        // Forward, down a lane's chain or into the mix: free.
        for (from, to) in [(CableFrom::Instrument(n.lane[0]), CableTo::Effect(n.fx[1])), (CableFrom::Instrument(n.lane[1]), CableTo::Effect(n.bus)), (CableFrom::Effect(n.fx[0]), CableTo::Master)] {
            let (row, col) = at(&bench, from, to);
            assert_eq!(patch.cell(&run.model, &bench, row, col), Cell::Free, "{from:?} to {to:?}");
        }
        // Back to where it came from: round.
        for (from, to) in [(CableFrom::Effect(n.fx[1]), CableTo::Effect(n.fx[0])), (CableFrom::Effect(n.fx[0]), CableTo::Effect(n.fx[0])), (CableFrom::Effect(n.bus), CableTo::Effect(n.fx[0]))] {
            let (row, col) = at(&bench, from, to);
            match patch.cell(&run.model, &bench, row, col) {
                Cell::Refused(why) => assert!(why.contains("go round"), "{from:?} to {to:?}: {why}"),
                other => panic!("{from:?} to {to:?}: {other:?}"),
            }
        }
    }

    #[test]
    fn the_links_of_the_audio_path_are_cables_at_their_crosspoints_with_no_level_of_their_own() {
        let (run, bench) = bench_of(SENDS);
        let n = Sends::of(&run.model);
        let mut patch = Patch::new();
        // Lane 0 is in the group: its instrument into its filter, the filter into its delay, the delay into the group's bus. Lane 1: its instrument into its drive and the drive to the mix. The group: into
        // its bus effect (the link starts at the group), and from the bus effect to the mix.
        let links = [
            (CableFrom::Instrument(n.lane[0]), CableTo::Effect(n.fx[0]), LinkOf::Feed(n.fx[0])),
            (CableFrom::Effect(n.fx[0]), CableTo::Effect(n.fx[1]), LinkOf::Feed(n.fx[1])),
            (CableFrom::Effect(n.fx[1]), CableTo::GroupBus(n.group), LinkOf::WayOut(n.lane[0])),
            (CableFrom::Instrument(n.lane[1]), CableTo::Effect(n.fx[2]), LinkOf::Feed(n.fx[2])),
            (CableFrom::Effect(n.fx[2]), CableTo::Master, LinkOf::WayOut(n.lane[1])),
            (CableFrom::Group(n.group), CableTo::Effect(n.bus), LinkOf::Feed(n.bus)),
            (CableFrom::Effect(n.bus), CableTo::Master, LinkOf::WayOut(n.group)),
        ];
        for (from, to, of) in links {
            let (row, col) = at(&bench, from, to);
            assert_eq!(patch.cell(&run.model, &bench, row, col), Cell::Link { of, present: true }, "{from:?} to {to:?}");
        }
        // No other crosspoint is one.
        let all = (0..bench.rows.len()).flat_map(|row| (0..bench.cols.len()).map(move |col| (row, col)));
        assert_eq!(all.filter(|&(row, col)| matches!(patch.cell(&run.model, &bench, row, col), Cell::Link { .. })).count(), links.len());
    }

    #[test]
    fn a_cable_that_is_drawn_at_the_ends_of_a_link_is_the_pin_and_the_link_is_not_seen_under_it() {
        let (mut run, _) = bench_of(SENDS);
        let n = Sends::of(&run.model);
        run.model.instruments[1].route = Route::None;
        run.model.insert_audio_cable(AudioCable { id: EdgeId(7), from: CableFrom::Effect(n.fx[2]), to: CableTo::Master, gain: 0.5 });
        let bench = Bench::of(&run.model);
        let mut patch = Patch::new();
        let (row, col) = at(&bench, CableFrom::Effect(n.fx[2]), CableTo::Master);
        assert_eq!(patch.cell(&run.model, &bench, row, col), Cell::Pin { edge: EdgeId(7), gain: 0.5 });
    }

    #[test]
    fn enter_and_d_cut_a_link_by_the_intent_that_cuts_it_and_enter_where_it_is_cut_draws_the_cable_that_gives_it_back() {
        let (mut run, bench) = bench_of(SENDS);
        let n = Sends::of(&run.model);
        // The way out of lane 1 (its drive to the mix), cut by every key that takes a cable out; the feed of its drive (its instrument into it), and a group's.
        let mut patch = on(&bench, CableFrom::Effect(n.fx[2]), CableTo::Master);
        for code in [KeyCode::Enter, KeyCode::Char(' '), KeyCode::Char('d'), KeyCode::Delete, KeyCode::Backspace] {
            assert_eq!(key(&mut patch, &run.model, code), Act::Apply(Intent::SetRoute { node: n.lane[1], route: Route::None }), "{code:?}");
        }
        let mut patch = on(&bench, CableFrom::Instrument(n.lane[1]), CableTo::Effect(n.fx[2]));
        assert_eq!(key(&mut patch, &run.model, KeyCode::Enter), Act::Apply(Intent::SetFeed { node: n.fx[2], feed: Feed::None }));
        let mut patch = on(&bench, CableFrom::Group(n.group), CableTo::Effect(n.bus));
        assert_eq!(key(&mut patch, &run.model, KeyCode::Char('d')), Act::Apply(Intent::SetFeed { node: n.bus, feed: Feed::None }));
        // Cut, they are crosspoints that a cable can be drawn at, which gives them back (the controller turns it into the link), and there is nothing to take out.
        run.model.instruments[1].route = Route::None;
        run.model.instruments[1].effects[0].fed = false;
        let mut patch = on(&bench, CableFrom::Effect(n.fx[2]), CableTo::Master);
        assert_eq!(patch.here(&run.model, &bench), Cell::Link { of: LinkOf::WayOut(n.lane[1]), present: false });
        let draws = |from: NodeId, to: NodeId| Act::Apply(Intent::AddCable { from: Endpoint { node: from, port: Port::AudioOut }, to: Endpoint { node: to, port: Port::AudioIn }, gain: None });
        assert_eq!(key(&mut patch, &run.model, KeyCode::Enter), draws(n.fx[2], NodeId::MASTER));
        assert_eq!(key(&mut patch, &run.model, KeyCode::Char('d')), Act::None);
        let mut patch = on(&bench, CableFrom::Instrument(n.lane[1]), CableTo::Effect(n.fx[2]));
        assert_eq!(key(&mut patch, &run.model, KeyCode::Char(' ')), draws(n.lane[1], n.fx[2]));
    }

    #[test]
    fn a_link_has_no_level_to_turn_or_to_type_and_the_keys_that_would_say_so() {
        let (run, bench) = bench_of(SENDS);
        let n = Sends::of(&run.model);
        let mut patch = on(&bench, CableFrom::Effect(n.fx[2]), CableTo::Master);
        for code in [KeyCode::Char('+'), KeyCode::Char('='), KeyCode::Char('-'), KeyCode::Char('_'), KeyCode::Char('e')] {
            assert!(matches!(key(&mut patch, &run.model, code), Act::Notice(why) if why.contains("no level")), "{code:?}");
        }
        // Where there is nothing, they are as they were: nothing.
        let mut patch = on(&bench, CableFrom::Effect(n.fx[0]), CableTo::Master);
        for code in [KeyCode::Char('+'), KeyCode::Char('-'), KeyCode::Char('e')] {
            assert_eq!(key(&mut patch, &run.model, code), Act::None, "{code:?}");
        }
    }

    #[test]
    fn a_link_that_is_cut_is_refused_where_giving_it_back_would_make_the_audio_go_round_and_not_where_a_cable_would() {
        let (mut run, bench) = bench_of(SENDS);
        let n = Sends::of(&run.model);
        // The group's own link into its bus effect starts at the group, and a cable from the group into the effect would go round (the effect is in the group): the link is given back all the same.
        run.model.groups[0].effects[0].fed = false;
        let mut patch = Patch::new();
        let (row, col) = at(&bench, CableFrom::Group(n.group), CableTo::Effect(n.bus));
        assert_eq!(patch.cell(&run.model, &bench, row, col), Cell::Link { of: LinkOf::Feed(n.bus), present: false });
        // Lane 1 is put in the group with its way out cut, and the group's output goes into lane 1's drive: the way out given back would feed the group that feeds it. (The group's chain is whole.)
        run.model.groups[0].effects[0].fed = true;
        run.model.instruments[1].route = Route::None;
        run.model.instruments[1].group = Some(0);
        run.model.insert_audio_cable(AudioCable { id: EdgeId(3), from: CableFrom::Group(n.group), to: CableTo::Effect(n.fx[2]), gain: 1.0 });
        let bench = Bench::of(&run.model);
        let mut patch = Patch::new();
        let (row, col) = at(&bench, CableFrom::Effect(n.fx[2]), CableTo::GroupBus(n.group));
        assert_eq!(patch.cell(&run.model, &bench, row, col), Cell::Refused("the audio would go round".to_string()));
        patch.row = row;
        patch.col = col;
        patch.settle(&bench);
        assert!(matches!(key(&mut patch, &run.model, KeyCode::Enter), Act::Notice(why) if why.contains("cannot send") && why.contains("go round")));
    }

    #[test]
    fn the_input_of_an_effect_that_is_cut_is_refused_where_giving_it_back_would_make_the_audio_go_round_and_given_back_where_it_would_not() {
        let (mut run, _) = bench_of(SENDS);
        let n = Sends::of(&run.model);
        // The delay after lane 0's filter is sent back into the filter by a cable, and its own input (the link from the filter) is cut: the audio does not go round, and the link given back would
        // (filter, delay, the cable, filter).
        run.model.instruments[0].effects[1].fed = false;
        run.model.insert_audio_cable(AudioCable { id: EdgeId(3), from: CableFrom::Effect(n.fx[1]), to: CableTo::Effect(n.fx[0]), gain: 1.0 });
        let bench = Bench::of(&run.model);
        let mut patch = Patch::new();
        let (row, col) = at(&bench, CableFrom::Effect(n.fx[0]), CableTo::Effect(n.fx[1]));
        assert_eq!(patch.cell(&run.model, &bench, row, col), Cell::Refused("the audio would go round".to_string()));
        patch.row = row;
        patch.col = col;
        patch.settle(&bench);
        assert!(matches!(key(&mut patch, &run.model, KeyCode::Enter), Act::Notice(why) if why.contains("cannot send") && why.contains("go round")));
        // With no cable back into the filter the same link is given back, as any cut link is.
        run.model.remove_audio_cable(EdgeId(3));
        let bench = Bench::of(&run.model);
        let mut patch = Patch::new();
        let (row, col) = at(&bench, CableFrom::Effect(n.fx[0]), CableTo::Effect(n.fx[1]));
        assert_eq!(patch.cell(&run.model, &bench, row, col), Cell::Link { of: LinkOf::Feed(n.fx[1]), present: false });
    }

    #[test]
    fn the_cursor_moves_by_arrows_and_stays_in_the_matrix() {
        let (run, bench) = bench_of(SENDS);
        let model = &run.model;
        let mut patch = Patch::new();
        for _ in 0..20 {
            assert_eq!(key(&mut patch, model, KeyCode::Down), Act::None);
        }
        for _ in 0..20 {
            key(&mut patch, model, KeyCode::Right);
        }
        assert_eq!((patch.row, patch.col), (bench.rows.len() - 1, bench.cols.len() - 1), "the last row and the last column");
        for _ in 0..20 {
            key(&mut patch, model, KeyCode::Up);
            key(&mut patch, model, KeyCode::Left);
        }
        assert_eq!((patch.row, patch.col), (0, 0));
        key(&mut patch, model, KeyCode::End);
        assert_eq!(patch.col, bench.cols.len() - 1);
        key(&mut patch, model, KeyCode::Home);
        assert_eq!(patch.col, 0);
        patch.rows_in_view = 4;
        key(&mut patch, model, KeyCode::PageDown);
        assert_eq!(patch.row, 3, "a page is the rows in view less one");
        key(&mut patch, model, KeyCode::PageDown);
        key(&mut patch, model, KeyCode::PageDown);
        assert_eq!(patch.row, bench.rows.len() - 1);
        key(&mut patch, model, KeyCode::PageUp);
        assert_eq!(patch.row, bench.rows.len() - 4);
    }

    #[test]
    fn enter_and_space_draw_a_cable_where_there_is_none_with_the_ports_of_its_ends() {
        let (run, bench) = bench_of(SENDS);
        let n = Sends::of(&run.model);
        for code in [KeyCode::Enter, KeyCode::Char(' ')] {
            let mut patch = on(&bench, CableFrom::Instrument(n.lane[1]), CableTo::Effect(n.bus));
            assert_eq!(
                key(&mut patch, &run.model, code),
                Act::Apply(Intent::AddCable {
                    from: Endpoint { node: n.lane[1], port: Port::AudioOut },
                    to: Endpoint { node: n.bus, port: Port::AudioIn },
                    gain: None
                }),
                "{code:?}"
            );
        }
        // To the mix, which is not a node of the session.
        let mut patch = on(&bench, CableFrom::Group(n.group), CableTo::Master);
        assert_eq!(
            key(&mut patch, &run.model, KeyCode::Enter),
            Act::Apply(Intent::AddCable { from: Endpoint { node: n.group, port: Port::AudioOut }, to: Endpoint { node: NodeId::MASTER, port: Port::AudioIn }, gain: None })
        );
    }

    #[test]
    fn enter_takes_a_cable_that_is_there_out_and_so_do_d_delete_and_backspace() {
        let (run, bench) = bench_of(PATCHED);
        let model = &run.model;
        let edge = model.cables[0].id;
        for code in [KeyCode::Enter, KeyCode::Char(' '), KeyCode::Char('d'), KeyCode::Delete, KeyCode::Backspace] {
            let mut patch = on(&bench, model.cables[0].from, model.cables[0].to);
            assert_eq!(key(&mut patch, model, code), Act::Apply(Intent::RemoveEdge { edge }), "{code:?}");
        }
    }

    #[test]
    fn d_where_there_is_no_cable_does_nothing() {
        let (run, bench) = bench_of(SENDS);
        let n = Sends::of(&run.model);
        let mut patch = on(&bench, CableFrom::Instrument(n.lane[1]), CableTo::Effect(n.bus));
        assert_eq!(key(&mut patch, &run.model, KeyCode::Char('d')), Act::None);
        // Nor where it could not be drawn.
        let mut patch = on(&bench, CableFrom::Effect(n.fx[1]), CableTo::Effect(n.fx[0]));
        assert_eq!(key(&mut patch, &run.model, KeyCode::Char('d')), Act::None);
    }

    #[test]
    fn enter_where_a_cable_would_make_the_audio_go_round_asks_for_nothing_and_says_why() {
        let (run, bench) = bench_of(SENDS);
        let n = Sends::of(&run.model);
        let mut patch = on(&bench, CableFrom::Effect(n.fx[1]), CableTo::Effect(n.fx[0]));
        match key(&mut patch, &run.model, KeyCode::Enter) {
            Act::Notice(text) => assert!(text.starts_with("L1 Delay cannot send to L1 Filter: ") && text.contains("go round"), "{text}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn plus_and_minus_turn_the_level_of_a_cable_by_a_twentieth_and_hold_it_to_what_a_level_can_be() {
        let (run, bench) = bench_of(PATCHED);
        let model = &run.model;
        // 0.5, the first of the cables.
        let (from, to, edge) = (model.cables[0].from, model.cables[0].to, model.cables[0].id);
        let mut patch = on(&bench, from, to);
        for (code, level) in [(KeyCode::Char('+'), 0.55), (KeyCode::Char('='), 0.55), (KeyCode::Char('-'), 0.45), (KeyCode::Char('_'), 0.45)] {
            assert_eq!(key(&mut patch, model, code), Act::Apply(Intent::SetCableGain { edge, gain: level }), "{code:?}");
        }
        // 1.5, the cable of the group.
        let (from, to, edge) = (model.cables[2].from, model.cables[2].to, model.cables[2].id);
        let mut patch = on(&bench, from, to);
        assert_eq!(key(&mut patch, model, KeyCode::Char('+')), Act::Apply(Intent::SetCableGain { edge, gain: 1.55 }));
        // Not a crosspoint that has no cable.
        let mut patch = on(&bench, CableFrom::Instrument(model.instruments[1].node), CableTo::Master);
        assert_eq!(key(&mut patch, model, KeyCode::Char('+')), Act::None);
    }

    #[test]
    fn the_level_stops_at_the_ends_and_a_key_that_would_not_move_it_asks_for_nothing() {
        let model_text = format!("{SENDS}\n[[cable]]\nfrom = \"instrument[1]\"\nto = \"master\"\ngain = 2.0\n\n[[cable]]\nfrom = \"instrument[0].effect[0]\"\nto = \"master\"\ngain = 0.0\n");
        let (run, bench) = bench_of(&model_text);
        let model = &run.model;
        let top = &model.cables[0];
        let mut patch = on(&bench, top.from, top.to);
        assert_eq!(key(&mut patch, model, KeyCode::Char('+')), Act::None, "a level of 2 is the highest");
        assert!(matches!(key(&mut patch, model, KeyCode::Char('-')), Act::Apply(Intent::SetCableGain { gain, .. }) if gain == 1.95));
        let bottom = &model.cables[1];
        let mut patch = on(&bench, bottom.from, bottom.to);
        assert_eq!(key(&mut patch, model, KeyCode::Char('-')), Act::None, "a level of 0 is the lowest");
        assert!(matches!(key(&mut patch, model, KeyCode::Char('+')), Act::Apply(Intent::SetCableGain { gain, .. }) if gain == 0.05));
    }

    #[test]
    fn a_run_of_steps_goes_through_the_hundredths_with_no_drift_from_nothing_to_the_highest_level() {
        let edge = EdgeId(7);
        let patch = Patch::new();
        let mut gain = 0.0f32;
        for step in 1..=40 {
            let Act::Apply(Intent::SetCableGain { gain: next, .. }) = patch.turn(Cell::Pin { edge, gain }, LEVEL_STEP) else { panic!("step {step} from {gain}") };
            assert_eq!(next, (step as f32 * 5.0).round() / 100.0, "step {step}");
            gain = next;
        }
        assert_eq!(gain, CABLE_GAIN_MAX);
        for step in (0..40).rev() {
            let Act::Apply(Intent::SetCableGain { gain: next, .. }) = patch.turn(Cell::Pin { edge, gain }, -LEVEL_STEP) else { panic!("down from {gain}") };
            assert_eq!(next, (step as f32 * 5.0).round() / 100.0, "step down to {step}");
            gain = next;
        }
        assert_eq!(gain, 0.0);
    }

    #[test]
    fn e_asks_for_a_number_for_the_level_of_the_cable_that_is_there_and_for_nothing_else() {
        let (run, bench) = bench_of(PATCHED);
        let model = &run.model;
        let cable = &model.cables[1];
        let mut patch = on(&bench, cable.from, cable.to);
        assert_eq!(key(&mut patch, model, KeyCode::Char('e')), Act::TypeLevel { edge: cable.id, gain: 0.75 });
        let mut patch = on(&bench, CableFrom::Instrument(model.instruments[1].node), CableTo::Master);
        assert_eq!(key(&mut patch, model, KeyCode::Char('e')), Act::None);
    }

    #[test]
    fn w_m_and_s_are_of_the_lane_or_the_group_whose_row_the_cursor_is_on_whichever_of_its_rows_it_is() {
        let (run, bench) = bench_of(SENDS);
        let n = Sends::of(&run.model);
        let model = &run.model;
        // The row of an effect of the lane is the lane's, and so is its instrument's.
        for from in [CableFrom::Instrument(n.lane[0]), CableFrom::Effect(n.fx[1])] {
            let mut patch = on(&bench, from, CableTo::Master);
            assert_eq!(key(&mut patch, model, KeyCode::Char('w')), Act::Apply(Intent::SetRoute { node: n.lane[0], route: Route::None }));
            assert_eq!(key(&mut patch, model, KeyCode::Char('M')), Act::Apply(Intent::SetMute { node: n.lane[0], muted: true }));
            assert_eq!(key(&mut patch, model, KeyCode::Char('S')), Act::Apply(Intent::SetSolo { node: n.lane[0], solo: true }));
        }
        // The group's bus effect is the group's.
        let mut patch = on(&bench, CableFrom::Effect(n.bus), CableTo::Master);
        assert_eq!(key(&mut patch, model, KeyCode::Char('w')), Act::Apply(Intent::SetRoute { node: n.group, route: Route::None }));
        assert_eq!(key(&mut patch, model, KeyCode::Char('M')), Act::Apply(Intent::SetMute { node: n.group, muted: true }));
    }

    #[test]
    fn w_m_and_s_toggle_what_the_session_has() {
        let text = format!("{SENDS}\n").replace("[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument.effect]]\nplugin = \"builtin:drive\"", "[[instrument]]\nplugin = \"builtin:osc\"\nmute = true\nsolo = true\nroute = \"none\"\n[[instrument.effect]]\nplugin = \"builtin:drive\"");
        let (run, bench) = bench_of(&text);
        let model = &run.model;
        let lane = model.instruments[1].node;
        let mut patch = on(&bench, CableFrom::Instrument(lane), CableTo::Master);
        assert_eq!(key(&mut patch, model, KeyCode::Char('w')), Act::Apply(Intent::SetRoute { node: lane, route: Route::Out }), "a way out that is cut is given back");
        assert_eq!(key(&mut patch, model, KeyCode::Char('M')), Act::Apply(Intent::SetMute { node: lane, muted: false }));
        assert_eq!(key(&mut patch, model, KeyCode::Char('S')), Act::Apply(Intent::SetSolo { node: lane, solo: false }));
    }

    #[test]
    fn a_letter_with_control_held_is_none_of_the_tab_s_keys() {
        let (run, bench) = bench_of(SENDS);
        let n = Sends::of(&run.model);
        let mut patch = on(&bench, CableFrom::Instrument(n.lane[1]), CableTo::Effect(n.bus));
        for letter in ['d', 'e', 'w', 'M', 'S', '+', '-', ' '] {
            assert_eq!(patch.press(&run.model, KeyCode::Char(letter), KeyModifiers::CONTROL), Act::None, "Ctrl+{letter}");
        }
    }

    #[test]
    fn the_cursor_stays_on_its_crosspoint_when_a_lane_comes_in_above_it() {
        let (mut run, bench) = bench_of(SENDS);
        let n = Sends::of(&run.model);
        let mut patch = on(&bench, CableFrom::Effect(n.fx[2]), CableTo::Effect(n.bus));
        let before = (patch.row, patch.col);
        // A lane is put at the front, by what the model does for the load of a session: a lane first has the first row of the matrix.
        let mut lane = testutil::load(SENDS).model.instruments.remove(1);
        lane.node = run.model.alloc_node();
        lane.effects.clear();
        lane.group = None;
        run.model.instruments.insert(0, lane);
        let moved = Bench::of(&run.model);
        patch.settle(&moved);
        assert_eq!((patch.row, patch.col), (before.0 + 1, before.1), "one row further down, the same column: the lane has no input");
        assert_eq!(moved.rows[patch.row].from, CableFrom::Effect(n.fx[2]));
    }

    #[test]
    fn the_cursor_stays_on_its_input_when_a_column_comes_in_before_it() {
        let (mut run, bench) = bench_of(SENDS);
        let n = Sends::of(&run.model);
        let mut patch = on(&bench, CableFrom::Effect(n.fx[2]), CableTo::Effect(n.bus));
        let before = (patch.row, patch.col);
        // A lane with an effect comes in at the front: two rows, and a column for its effect, before the cursor's.
        let mut lane = testutil::load(SENDS).model.instruments.remove(1);
        lane.node = run.model.alloc_node();
        lane.effects[0].node = run.model.alloc_node();
        lane.group = None;
        run.model.instruments.insert(0, lane);
        let moved = Bench::of(&run.model);
        patch.settle(&moved);
        assert_eq!((patch.row, patch.col), (before.0 + 2, before.1 + 1));
        assert_eq!((moved.rows[patch.row].from, moved.cols[patch.col].to), (CableFrom::Effect(n.fx[2]), CableTo::Effect(n.bus)));
    }

    #[test]
    fn the_cursor_stays_in_the_matrix_when_the_output_and_the_input_it_was_on_go() {
        let (mut run, bench) = bench_of(SENDS);
        let n = Sends::of(&run.model);
        let mut patch = on(&bench, CableFrom::Effect(n.bus), CableTo::Effect(n.bus));
        assert_eq!((patch.row, patch.col), (bench.rows.len() - 1, bench.cols.len() - 2));
        run.model.groups.clear();
        for lane in &mut run.model.instruments {
            lane.group = None;
        }
        let smaller = Bench::of(&run.model);
        patch.settle(&smaller);
        assert_eq!((patch.row, patch.col), (smaller.rows.len() - 1, smaller.cols.len() - 1), "on the last row and the last column, the mix");
    }

    #[test]
    fn a_refusal_is_worked_out_once_and_kept_until_the_wiring_changes() {
        let (mut run, bench) = bench_of(SENDS);
        let n = Sends::of(&run.model);
        let mut patch = Patch::new();
        let (row, col) = at(&bench, CableFrom::Effect(n.fx[1]), CableTo::Effect(n.fx[0]));
        patch.settle(&bench);
        assert!(matches!(patch.cell(&run.model, &bench, row, col), Cell::Refused(_)));
        assert_eq!(patch.refusals.len(), 1);
        patch.cell(&run.model, &bench, row, col);
        assert_eq!(patch.refusals.len(), 1, "asked again, not worked out again");
        // A lane takes the effect's place in no group: another wiring.
        run.model.instruments[0].group = None;
        let changed = Bench::of(&run.model);
        patch.settle(&changed);
        assert!(patch.refusals.is_empty(), "what was worked out of the old wiring is forgotten");
    }

    #[test]
    fn a_click_puts_the_cursor_on_the_crosspoint_or_the_row_that_it_is_on_and_on_nothing_else() {
        let (run, _) = bench_of(SENDS);
        let model = &run.model;
        let mut patch = Patch::new();
        patch.cells = Rect::new(40, 10, 30, 7);
        patch.names = Rect::new(2, 10, 36, 7);
        assert!(patch.click(model, 40 + COLUMN * 2 + 1, 10 + 3), "a crosspoint");
        assert_eq!((patch.row, patch.col), (3, 2));
        assert!(patch.click(model, 5, 10 + 5), "the name of a row");
        assert_eq!((patch.row, patch.col), (5, 2), "the column stays");
        assert!(!patch.click(model, 100, 10), "not on the matrix");
        assert!(!patch.click(model, 40, 9), "above it");
        assert_eq!((patch.row, patch.col), (5, 2));
    }

    #[test]
    fn a_click_is_on_the_row_and_the_column_that_are_in_view_when_the_view_is_scrolled() {
        let (run, _) = bench_of(PATCHED);
        let mut patch = Patch::new();
        (patch.top, patch.left) = (2, 3);
        patch.cells = Rect::new(40, 10, 25, 6);
        patch.names = Rect::new(2, 10, 36, 6);
        assert!(patch.click(&run.model, 40 + COLUMN + 1, 10 + 2));
        assert_eq!((patch.row, patch.col), (4, 4), "the second column and the third row of the view");
        assert!(patch.click(&run.model, 3, 10 + 5));
        assert_eq!(patch.row, 7, "the last row of the view");
    }

    #[test]
    fn the_wheel_moves_the_cursor_by_rows_and_stops_at_the_ends() {
        let (run, bench) = bench_of(SENDS);
        let mut patch = Patch::new();
        patch.scroll(&run.model, 3);
        assert_eq!(patch.row, 3);
        patch.scroll(&run.model, -1);
        assert_eq!(patch.row, 2);
        patch.scroll(&run.model, 100);
        assert_eq!(patch.row, bench.rows.len() - 1);
        patch.scroll(&run.model, -100);
        assert_eq!(patch.row, 0);
    }
}
