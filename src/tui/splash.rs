//! Startup splash screen.
//!
//! Shown while session plugins load (which can take seconds for heavyweight
//! instruments). A small render thread owns the terminal and animates a
//! spinner at ~12 fps while the main thread loads plugins and reports
//! progress over a channel. Dropping the `Splash` stops the thread and
//! restores the terminal, so the error path (`?` during loading) cleans up
//! automatically.

use std::io;
use std::thread::JoinHandle;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender};
use crossterm::execute;
use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Terminal;

const LOGO: &[&str] = &[
    "████████╗ █████╗ ███╗   ██╗ ██████╗ ",
    "╚══██╔══╝██╔══██╗████╗  ██║██╔════╝ ",
    "   ██║   ███████║██╔██╗ ██║██║  ███╗",
    "   ██║   ██╔══██║██║╚██╗██║██║   ██║",
    "   ██║   ██║  ██║██║ ╚████║╚██████╔╝",
    "   ╚═╝   ╚═╝  ╚═╝╚═╝  ╚═══╝ ╚═════╝ ",
];

const TAGLINE: &str = "terminal audio plugin host";

const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

const BAR_WIDTH: usize = 36;

enum Msg {
    Status(String),
    Progress(usize),
    Done,
}

pub struct Splash {
    tx: Sender<Msg>,
    thread: Option<JoinHandle<()>>,
}

impl Splash {
    /// Enter the alternate screen and start the splash render thread.
    /// `total` is the number of plugin slots that will be loaded (drives the
    /// progress bar; 0 hides the bar).
    pub fn start(total: usize) -> anyhow::Result<Self> {
        crossterm::terminal::enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen, crossterm::cursor::Hide)?;
        let mut terminal = Terminal::new(CrosstermBackend::new(stdout))?;

        let (tx, rx) = crossbeam_channel::unbounded();
        let thread = std::thread::spawn(move || {
            render_loop(&mut terminal, &rx, total);
            let _ = execute!(
                terminal.backend_mut(),
                crossterm::cursor::Show,
                LeaveAlternateScreen
            );
            let _ = crossterm::terminal::disable_raw_mode();
        });

        Ok(Self {
            tx,
            thread: Some(thread),
        })
    }

    /// Update the status line (e.g. "Loading Pianoteq 9…").
    pub fn status(&self, msg: impl Into<String>) {
        let _ = self.tx.send(Msg::Status(msg.into()));
    }

    /// Update the number of completed plugin slots.
    pub fn progress(&self, done: usize) {
        let _ = self.tx.send(Msg::Progress(done));
    }
}

impl Drop for Splash {
    fn drop(&mut self) {
        let _ = self.tx.send(Msg::Done);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn render_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    rx: &Receiver<Msg>,
    total: usize,
) {
    let mut status = String::from("Starting…");
    let mut done = 0usize;
    let mut tick = 0usize;
    loop {
        loop {
            match rx.try_recv() {
                Ok(Msg::Status(s)) => status = s,
                Ok(Msg::Progress(d)) => done = d,
                Ok(Msg::Done) | Err(crossbeam_channel::TryRecvError::Disconnected) => return,
                Err(crossbeam_channel::TryRecvError::Empty) => break,
            }
        }
        let _ = terminal.draw(|frame| draw(frame, &status, done, total, tick));
        tick += 1;
        std::thread::sleep(Duration::from_millis(80));
    }
}

fn draw(frame: &mut ratatui::Frame, status: &str, done: usize, total: usize, tick: usize) {
    let area = frame.area();

    let mut lines: Vec<Line> = Vec::new();
    for (i, row) in LOGO.iter().enumerate() {
        // Subtle two-tone logo: top half cyan, bottom half dimmer.
        let color = if i < LOGO.len() / 2 {
            Color::Cyan
        } else {
            Color::DarkGray
        };
        lines.push(Line::from(Span::styled(
            *row,
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        )));
    }
    lines.push(Line::default());
    lines.push(Line::from(Span::styled(
        TAGLINE,
        Style::default().fg(Color::DarkGray),
    )));
    lines.push(Line::default());

    let spinner = SPINNER[tick % SPINNER.len()];
    lines.push(Line::from(vec![
        Span::styled(spinner, Style::default().fg(Color::Yellow)),
        Span::raw(" "),
        Span::styled(status.to_string(), Style::default().fg(Color::White)),
    ]));

    if let Some(filled) = (done * BAR_WIDTH).checked_div(total) {
        lines.push(Line::default());
        let filled = filled.min(BAR_WIDTH);
        let bar: String = "▓".repeat(filled) + &"░".repeat(BAR_WIDTH - filled);
        lines.push(Line::from(vec![
            Span::styled(bar, Style::default().fg(Color::Cyan)),
            Span::styled(
                format!(" {done}/{total}"),
                Style::default().fg(Color::DarkGray),
            ),
        ]));
    }

    let height = lines.len() as u16;
    let top = area.height.saturating_sub(height) / 2;
    let rect = Rect::new(area.x, area.y + top, area.width, height.min(area.height));
    frame.render_widget(
        Paragraph::new(lines).alignment(ratatui::layout::Alignment::Center),
        rect,
    );
}

/// Short human-readable name for a plugin source string, for status lines:
/// strips format prefixes ("vst3:Pianoteq 9" → "Pianoteq 9") and reduces
/// bundle paths to the file stem ("./fx/reverb.lv2" → "reverb").
pub fn display_name(source: &str) -> &str {
    for prefix in ["lv2:", "clap:", "vst3:", "builtin:"] {
        if let Some(rest) = source.strip_prefix(prefix) {
            return rest;
        }
    }
    if source.contains('/') && !source.starts_with("http") {
        std::path::Path::new(source)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(source)
    } else {
        source
    }
}
