//! The Scope tab: the output as it goes by, drawn as a wave or as a spectrum on a braille canvas. It is the picture of the page's scope
//! (`web/src/client/scope.ts` and `fft.ts`, `docs/web-ui.md` §6) with the same trigger, time bases and spectrum, which are pure functions here with the tests
//! that those have. What is drawn is the last frames of the output mix, which the audio thread keeps in a ring (`telemetry::ScopeRing`) and the loop copies on
//! every frame it draws.

use std::f64::consts::TAU;
use std::time::Instant;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols::Marker;
use ratatui::text::{Line, Span};
use ratatui::widgets::canvas::{Canvas, Context, Line as CanvasLine};
use ratatui::widgets::{Block, Borders, Paragraph};
use view::DIM;

use crate::telemetry::{ScopeRing, SCOPE_READ_MAX, SILENCE};

/// How many frames of the output a picture is made from: what a reader of the ring can take at once.
pub const FRAMES: usize = SCOPE_READ_MAX;

/// What the width of the wave can show, in samples: the time bases, the shortest first.
pub const TIMES: [usize; 4] = [256, 512, 1024, 2048];

/// The time base that a scope starts at (an index into `TIMES`): 1024 samples, 21 ms at 48 kHz.
const TIME_AT_FIRST: usize = 2;

/// The level of a frequency with nothing in it: as low as the picture goes, and not minus infinity.
pub const FLOOR_DB: f32 = -120.0;

/// How fast a level of the spectrum falls when the sound is quieter, as a meter's peak does: 40 dB a second.
const FALL_DB_PER_SECOND: f32 = 40.0;

/// The longest wait between two looks that the fall is made up for, in seconds: a terminal that was in the background does not draw a spectrum that has fallen to nothing, but one that is falling.
const LONGEST_GAP: f32 = 0.25;

/// What a scope can show.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Wave,
    Spectrum,
}

impl Kind {
    fn label(self) -> &'static str {
        match self {
            Kind::Wave => "wave",
            Kind::Spectrum => "spectrum",
        }
    }
}

/// Where in a block of the output a picture of `length` samples starts: at the first place where the wave goes up through `level` after having been under it by
/// `hysteresis` (so that a wobble round the level is not a wave), among the places that leave room for the whole picture. That is what a scope's trigger does, and a
/// wave that goes round again and again is drawn from the same point of its turn every time, and stands still. With no such place (silence, a constant, a wave that
/// stays on one side) it is the latest picture that fits.
pub fn trigger_start(samples: &[f32], length: usize, level: f32, hysteresis: f32) -> usize {
    let Some(last) = samples.len().checked_sub(length).filter(|&last| last > 0) else { return 0 };
    let mut armed = false;
    for (at, &sample) in samples.iter().enumerate().take(last + 1) {
        if sample < level - hysteresis {
            armed = true;
        } else if armed && sample >= level {
            return at;
        }
    }
    last
}

/// The analyser of blocks of one length: a Hann window, a radix-2 FFT and the levels of the frequencies in decibels. The tables that it needs (the window, the turns
/// of the FFT, the order of the samples) are made once, and the work arrays are its own, so that a block is analysed without allocating.
pub struct Spectrum {
    size: usize,
    window: Vec<f64>,
    cos: Vec<f64>,
    sin: Vec<f64>,
    reverse: Vec<usize>,
    re: Vec<f64>,
    im: Vec<f64>,
}

impl Spectrum {
    /// `size` is the length of the blocks: a power of two, 4 or more.
    pub fn new(size: usize) -> Spectrum {
        assert!(size >= 4 && size.is_power_of_two(), "a block of {size} samples is not a power of two of 4 or more");
        let bits = size.trailing_zeros();
        Spectrum {
            size,
            // (The periodic Hann window: a sine that goes round a whole number of times in the block has only the two frequencies beside its own to leak into.)
            window: (0..size).map(|n| 0.5 - 0.5 * (TAU * n as f64 / size as f64).cos()).collect(),
            cos: (0..size / 2).map(|t| (TAU * t as f64 / size as f64).cos()).collect(),
            sin: (0..size / 2).map(|t| (TAU * t as f64 / size as f64).sin()).collect(),
            reverse: (0..size).map(|n| ((n as u32).reverse_bits() >> (32 - bits)) as usize).collect(),
            re: vec![0.0; size],
            im: vec![0.0; size],
        }
    }

    /// The levels of the frequencies of `samples`, from nothing to half the sample rate (`size / 2 + 1` of them, the sample rate over `size` apart), in decibels: 0 dB
    /// is a sine of full scale, which a window and a block that go round it a whole number of times show at its own frequency. Written into `out`.
    pub fn decibels(&mut self, samples: &[f32], out: &mut [f32]) {
        let size = self.size;
        assert_eq!(samples.len(), size, "this analyser is of blocks of {size} samples, and this one has {}", samples.len());
        assert!(out.len() > size / 2, "{} levels are not room for the {} of a block of {size} samples", out.len(), size / 2 + 1);
        for (n, (&sample, &window)) in samples.iter().zip(&self.window).enumerate() {
            let place = self.reverse[n];
            self.re[place] = f64::from(sample) * window;
            self.im[place] = 0.0;
        }
        let mut half = 1;
        while half < size {
            let stride = size / (2 * half);
            for start in (0..size).step_by(2 * half) {
                for k in 0..half {
                    let turn = k * stride;
                    let (wr, wi) = (self.cos[turn], -self.sin[turn]);
                    let a = start + k;
                    let b = a + half;
                    let (br, bi) = (self.re[b], self.im[b]);
                    let (tr, ti) = (br * wr - bi * wi, br * wi + bi * wr);
                    self.re[b] = self.re[a] - tr;
                    self.im[b] = self.im[a] - ti;
                    self.re[a] += tr;
                    self.im[a] += ti;
                }
            }
            half *= 2;
        }
        // (A sine of full scale, windowed, is a quarter of the block's length in the frequency it is at.)
        let full = (size / 4) as f64;
        for (bin, level) in out.iter_mut().enumerate().take(size / 2 + 1) {
            let magnitude = self.re[bin].hypot(self.im[bin]) / full;
            *level = if magnitude > 0.0 { (20.0 * magnitude.log10()).max(f64::from(FLOOR_DB)) as f32 } else { FLOOR_DB };
        }
    }
}

/// What a picture of a spectrum covers: the lowest and the highest frequency across it, and the levels at its bottom and its top.
pub struct SpectrumRange {
    pub min_hz: f64,
    pub max_hz: f64,
    pub floor_db: f64,
    pub top_db: f64,
}

/// How far up a picture of a spectrum a level is, 0 at the floor and 1 at the top, and no further than that.
pub fn level_height(db: f64, range: &SpectrumRange) -> f64 {
    ((db - range.floor_db) / (range.top_db - range.floor_db)).clamp(0.0, 1.0)
}

/// The column, across a picture `width` wide, where the frequency `hz` is on the decades of `range`: for the lines of a spectrum's graticule.
pub fn frequency_column(hz: f64, range: &SpectrumRange, width: f64) -> f64 {
    width * (hz / range.min_hz).ln() / (range.max_hz / range.min_hz).ln()
}

/// The heights of a spectrum (`levels` in dB of the frequencies from nothing to half of `sample_rate`, as `Spectrum::decibels` gives them) in a picture `width` wide:
/// a height, from 0 to 1, for each column from 0 to `width` (the first at the left edge and the last at the right one), the frequencies spread over the decades
/// of `range` (equal ratios are equally far apart, as a musician hears them). A column that is narrower than the space between two levels has the level between
/// them, drawn as a line; one that holds more of them has the highest of them (each level is in one column, the one that its frequency is in), so that a peak is not
/// hidden at the high end, where there are many to a column. Written into `out`, which is cleared first.
pub fn spectrum_heights(levels: &[f32], sample_rate: f32, width: f64, range: &SpectrumRange, out: &mut Vec<f64>) {
    out.clear();
    let bins = levels.len().saturating_sub(1);
    if bins == 0 {
        return;
    }
    let bin_hz = f64::from(sample_rate) / 2.0 / bins as f64;
    let decades = (range.max_hz / range.min_hz).ln();
    let at = |column: f64| range.min_hz * (decades * column / width).exp();
    let in_bins = |hz: f64| (hz / bin_hz).clamp(0.0, bins as f64);
    let level_at = |bin: usize| f64::from(levels.get(bin).copied().unwrap_or(range.floor_db as f32));
    for column in 0..=width.floor() as usize {
        let column = column as f64;
        let (low, high) = (in_bins(at(column - 0.5)), in_bins(at(column + 0.5)));
        let level = if high - low >= 1.0 {
            (low.ceil() as usize..=high.floor() as usize).map(level_at).fold(f64::NEG_INFINITY, f64::max)
        } else {
            let middle = (low + high) / 2.0;
            let bin = middle.floor() as usize;
            let along = middle - bin as f64;
            level_at(bin) * (1.0 - along) + level_at((bin + 1).min(bins)) * along
        };
        out.push(level_height(level, range));
    }
}

/// How long a time base is, as it is told: in milliseconds at the sample rate.
pub fn time_label(time: usize, sample_rate: f32) -> String {
    let samples = TIMES.get(time).copied().unwrap_or(FRAMES);
    format!("{} ms", (samples as f32 / sample_rate * 1000.0).round())
}

/// The loudest sample of the picture as it is told: in decibels below full scale (above it, with a plus, when it clips), and a dash for silence, which is what the
/// meters call it (`telemetry::SILENCE`): the tail of a filter that has been left alone is never exactly nothing.
pub fn peak_text(peak: f32) -> String {
    if peak < SILENCE {
        return "—".to_string();
    }
    // (In tenths, so that what rounds to nothing has no sign.)
    let tenths = (200.0 * peak.log10()).round() as i32;
    let sign = match tenths.signum() {
        1 => "+",
        -1 => "-",
        _ => "",
    };
    format!("{sign}{}.{} dB", tenths.abs() / 10, tenths.abs() % 10)
}

/// What the Scope tab shows and has taken from the output: the choices (the view and the time base), the latest frames of both sides and, for the spectrum, the levels as
/// they are drawn.
pub struct Scope {
    kind: Kind,
    time: usize,
    left: Vec<f32>,
    right: Vec<f32>,
    peak: f32,
    analyser: Spectrum,
    mono: Vec<f32>,
    level: Vec<f32>,
    /// The levels as they are drawn: what the last frames were, each falling from where it was.
    shown: Vec<f32>,
    taken_at: Option<Instant>,
}

impl Default for Scope {
    fn default() -> Self {
        Scope::new()
    }
}

impl Scope {
    pub fn new() -> Scope {
        Scope {
            kind: Kind::Wave,
            time: TIME_AT_FIRST,
            left: vec![0.0; FRAMES],
            right: vec![0.0; FRAMES],
            peak: 0.0,
            analyser: Spectrum::new(FRAMES),
            mono: vec![0.0; FRAMES],
            level: vec![FLOOR_DB; FRAMES / 2 + 1],
            shown: vec![FLOOR_DB; FRAMES / 2 + 1],
            taken_at: None,
        }
    }

    pub fn kind(&self) -> Kind {
        self.kind
    }

    /// Which time base the wave is drawn at, an index into `TIMES`.
    pub fn time(&self) -> usize {
        self.time
    }

    /// The left side of the output, `FRAMES` samples, the latest last.
    pub fn left(&self) -> &[f32] {
        &self.left
    }

    /// The right side of it.
    pub fn right(&self) -> &[f32] {
        &self.right
    }

    /// The loudest sample on either side.
    pub fn peak(&self) -> f32 {
        self.peak
    }

    /// The levels of the spectrum as they are drawn, in dB.
    pub fn levels(&self) -> &[f32] {
        &self.shown
    }

    /// The wave, or the spectrum.
    pub fn toggle_kind(&mut self) {
        self.kind = match self.kind {
            Kind::Wave => Kind::Spectrum,
            Kind::Spectrum => Kind::Wave,
        };
        // (A spectrum that was drawn some time ago has levels that are not the sound's now, and they would fall from there.)
        self.shown.fill(FLOOR_DB);
    }

    /// A longer time base, to the longest there is.
    pub fn slower(&mut self) {
        self.time = (self.time + 1).min(TIMES.len() - 1);
    }

    /// A shorter time base, to the shortest there is.
    pub fn faster(&mut self) {
        self.time = self.time.saturating_sub(1);
    }

    /// Copy the latest frames of the output from `ring`, and, for the spectrum, work out the levels of them and let the levels that are drawn fall to them. `now` is when
    /// this look is.
    pub fn take(&mut self, ring: &ScopeRing, now: Instant) {
        ring.latest(&mut self.left, &mut self.right);
        self.peak = self.left.iter().chain(&self.right).fold(0.0, |peak: f32, sample| peak.max(sample.abs()));
        if self.kind == Kind::Spectrum {
            for ((mono, left), right) in self.mono.iter_mut().zip(&self.left).zip(&self.right) {
                *mono = (left + right) / 2.0;
            }
            self.analyser.decibels(&self.mono, &mut self.level);
            let fall = FALL_DB_PER_SECOND * self.taken_at.map_or(0.0, |then| now.saturating_duration_since(then).as_secs_f32().min(LONGEST_GAP));
            for (shown, level) in self.shown.iter_mut().zip(&self.level) {
                *shown = level.max(*shown - fall);
            }
        }
        self.taken_at = Some(now);
    }
}

// The colours: the left side in the teal that the Piano tab has for the notes of the scale, the right in a green, the graticule quiet.
const LEFT: Color = Color::Rgb(120, 200, 215);
const RIGHT: Color = Color::Rgb(95, 200, 111);
const GRID: Color = Color::Rgb(70, 74, 88);
const FILL: Color = Color::Rgb(40, 78, 90);
const CLIPPING: Color = Color::Rgb(235, 80, 80);

/// The frequencies that have a line down the spectrum and a label under it, and what the label says.
const GRID_HZ: [(f64, &str); 3] = [(100.0, "100"), (1000.0, "1k"), (10_000.0, "10k")];

/// The levels, in dB, that have a line across the spectrum.
const GRID_DB: [f64; 4] = [-20.0, -40.0, -60.0, -80.0];

/// Whether the right side of the picture that starts at `start` and is `length` samples long is not the same as the left.
fn sides_differ(scope: &Scope, start: usize, length: usize) -> bool {
    scope.left[start..start + length].iter().zip(&scope.right[start..start + length]).any(|(l, r)| (l - r).abs() > 1e-4)
}

/// The line through the samples, in the coordinates of a canvas that has the sample's place across and its value up (what is beyond full scale at the edges).
fn draw_wave(ctx: &mut Context, samples: &[f32], color: Color) {
    for (at, pair) in samples.windows(2).enumerate() {
        let (from, to) = (f64::from(pair[0].clamp(-1.0, 1.0)), f64::from(pair[1].clamp(-1.0, 1.0)));
        ctx.draw(&CanvasLine::new(at as f64, from, at as f64 + 1.0, to, color));
    }
}

fn paint_wave(ctx: &mut Context, scope: &Scope, length: usize) {
    let last = (length - 1) as f64;
    for y in [0.5, 0.0, -0.5] {
        ctx.draw(&CanvasLine::new(0.0, y, last, y, GRID));
    }
    ctx.layer();
    let start = trigger_start(scope.left(), length, 0.0, 0.02);
    // (Where the right side is not the same it is drawn first, and the left over it.)
    if sides_differ(scope, start, length) {
        draw_wave(ctx, &scope.right()[start..start + length], RIGHT);
        ctx.layer();
    }
    draw_wave(ctx, &scope.left()[start..start + length], LEFT);
}

fn paint_spectrum(ctx: &mut Context, scope: &Scope, sample_rate: f32, dots: usize) {
    let range = SpectrumRange { min_hz: 20.0, max_hz: 20_000f64.min(f64::from(sample_rate) / 2.0), floor_db: -90.0, top_db: 0.0 };
    let width = (dots - 1) as f64;
    for (hz, _) in GRID_HZ {
        let x = frequency_column(hz, &range, width);
        ctx.draw(&CanvasLine::new(x, 0.0, x, 1.0, GRID));
    }
    for db in GRID_DB {
        let y = level_height(db, &range);
        ctx.draw(&CanvasLine::new(0.0, y, width, y, GRID));
    }
    ctx.layer();
    let mut heights = Vec::new();
    spectrum_heights(scope.levels(), sample_rate, width, &range, &mut heights);
    for (column, &height) in heights.iter().enumerate() {
        ctx.draw(&CanvasLine::new(column as f64, 0.0, column as f64, height, FILL));
    }
    ctx.layer();
    for (column, pair) in heights.windows(2).enumerate() {
        ctx.draw(&CanvasLine::new(column as f64, pair[0], column as f64 + 1.0, pair[1], LEFT));
    }
    for (hz, label) in GRID_HZ {
        ctx.print(frequency_column(hz, &range, width) + 1.0, 0.0, Span::styled(label, Style::default().fg(DIM)));
    }
}

/// The Scope tab, in `area`: a line that says what it shows, the picture in a box, and the keys. The frames are what `Scope::take` last took.
pub fn render(frame: &mut Frame, area: Rect, scope: &Scope, sample_rate: f32) {
    let [status_area, picture_area, hint_area] = Layout::vertical([Constraint::Length(1), Constraint::Fill(1), Constraint::Length(1)]).areas(area);

    let label = Style::default().fg(DIM);
    let value = Style::default().fg(Color::White).add_modifier(Modifier::BOLD);
    let mut status = vec![Span::styled(" Scope: ", label), Span::styled(scope.kind().label(), value)];
    if scope.kind() == Kind::Wave {
        status.push(Span::styled("   Time: ", label));
        status.push(Span::styled(time_label(scope.time(), sample_rate), value));
    }
    status.push(Span::styled("   Peak: ", label));
    status.push(Span::styled(peak_text(scope.peak()), if scope.peak() > 1.0 { value.fg(CLIPPING) } else { value }));
    frame.render_widget(Paragraph::new(Line::from(status)), status_area);

    let block = Block::default().borders(Borders::ALL).border_style(Style::default().fg(DIM));
    let inner = block.inner(picture_area);
    match scope.kind() {
        Kind::Wave => {
            let length = TIMES[scope.time()];
            let canvas = Canvas::default().block(block).marker(Marker::Braille).x_bounds([0.0, (length - 1) as f64]).y_bounds([-1.0, 1.0]);
            frame.render_widget(canvas.paint(|ctx| paint_wave(ctx, scope, length)), picture_area);
        }
        Kind::Spectrum => {
            // (A cell of the canvas is two dots across.)
            let dots = (2 * usize::from(inner.width)).max(2);
            let canvas = Canvas::default().block(block).marker(Marker::Braille).x_bounds([0.0, (dots - 1) as f64]).y_bounds([0.0, 1.0]);
            frame.render_widget(canvas.paint(|ctx| paint_spectrum(ctx, scope, sample_rate, dots)), picture_area);
        }
    }

    let hint = match scope.kind() {
        Kind::Wave => " [v] wave/spectrum  [←/→] time base ",
        Kind::Spectrum => " [v] wave/spectrum ",
    };
    frame.render_widget(Paragraph::new(hint).style(Style::default().fg(DIM)), hint_area);
}

#[cfg(test)]
mod tests {
    use std::f64::consts::TAU;
    use std::time::{Duration, Instant};

    use super::*;
    use crate::telemetry::ScopeRing;

    /// `length` samples of a cosine that goes round once in `period` samples, from sample `from` on (half a sample later than that, so that none is exactly on the middle).
    fn cosine(length: usize, period: f64, from: usize, amplitude: f64) -> Vec<f32> {
        (0..length).map(|n| (amplitude * (TAU * ((n + from) as f64 + 0.5) / period).cos()) as f32).collect()
    }

    #[test]
    fn a_picture_of_the_wave_starts_where_it_goes_up_through_the_middle_the_first_time_it_does() {
        // A cosine that goes round in 100 samples (half a sample late) is under the middle from the 25th to the 75th: it goes up through at the 75th.
        let samples = cosine(2048, 100.0, 0, 1.0);
        let start = trigger_start(&samples, 300, 0.0, 0.02);
        assert_eq!(start, 75);
        assert!(samples[start - 1] < 0.0);
        assert!(samples[start] >= 0.0);
    }

    #[test]
    fn a_wave_stands_still_whatever_the_block_began_in() {
        let pictures: Vec<Vec<f32>> = [0, 17, 41, 93]
            .into_iter()
            .map(|from| {
                let samples = cosine(2048, 100.0, from, 1.0);
                let start = trigger_start(&samples, 300, 0.0, 0.02);
                samples[start..start + 100].iter().map(|s| (s * 1e4).round() / 1e4).collect()
            })
            .collect();
        for picture in &pictures {
            assert_eq!(picture, &pictures[0]);
        }
    }

    #[test]
    fn with_nothing_to_start_from_a_picture_is_the_latest_that_fits() {
        // Silence, a constant, a wave that stays on one side.
        assert_eq!(trigger_start(&[0.0; 2048], 300, 0.0, 0.02), 2048 - 300);
        assert_eq!(trigger_start(&[0.5; 2048], 300, 0.0, 0.02), 2048 - 300);
        assert_eq!(trigger_start(&[-0.5; 2048], 300, 0.0, 0.02), 2048 - 300);
        let above: Vec<f32> = cosine(2048, 100.0, 0, 0.4).iter().map(|s| s + 1.0).collect();
        assert_eq!(trigger_start(&above, 300, 0.0, 0.02), 2048 - 300);
    }

    #[test]
    fn a_crossing_too_near_the_end_of_the_block_to_leave_room_for_the_picture_is_not_one() {
        // Under the middle for 1000 samples and then over it, from the 1000th on: a picture of 1500 does not fit after it.
        let samples: Vec<f32> = (0..2048).map(|n| if n < 1000 { -0.5 } else { 0.5 }).collect();
        assert_eq!(trigger_start(&samples, 1000, 0.0, 0.02), 1000);
        assert_eq!(trigger_start(&samples, 1500, 0.0, 0.02), 2048 - 1500);
    }

    #[test]
    fn a_picture_as_long_as_the_block_or_longer_starts_at_the_start_of_it() {
        assert_eq!(trigger_start(&cosine(512, 100.0, 0, 1.0), 512, 0.0, 0.02), 0);
        assert_eq!(trigger_start(&cosine(512, 100.0, 0, 1.0), 2000, 0.0, 0.02), 0);
    }

    #[test]
    fn a_wobble_round_the_middle_is_not_a_wave_that_goes_up_through_it() {
        // A hum of 0.01 either way is under the 0.02 that the wave has to go below before it may go up through.
        let hum: Vec<f32> = (0..2048).map(|n| if n % 2 == 0 { 0.01 } else { -0.01 }).collect();
        assert_eq!(trigger_start(&hum, 300, 0.0, 0.02), 2048 - 300);
        let louder: Vec<f32> = (0..2048).map(|n| if n % 2 == 0 { 0.05 } else { -0.05 }).collect();
        assert_eq!(trigger_start(&louder, 300, 0.0, 0.02), 2);
    }

    #[test]
    fn a_picture_can_start_where_the_wave_goes_up_through_another_level() {
        let samples = cosine(2048, 100.0, 0, 1.0);
        let start = trigger_start(&samples, 300, 0.5, 0.02);
        assert!(samples[start - 1] < 0.5);
        assert!(samples[start] >= 0.5);
        assert_eq!(start, 83);
    }

    /// The decibels of a block of samples, as an analyser of that block's length has them.
    fn levels_of(samples: &[f32]) -> Vec<f32> {
        let mut out = vec![0.0; samples.len() / 2 + 1];
        Spectrum::new(samples.len()).decibels(samples, &mut out);
        out
    }

    #[test]
    fn a_full_scale_sine_is_0_db_at_its_own_frequency_and_6_db_down_at_the_two_beside_it() {
        // 64 times round the block of 2048 samples: the bin 64, with the Hann window's two neighbours at half.
        let samples: Vec<f32> = (0..2048).map(|n| (TAU * 64.0 * n as f64 / 2048.0).cos() as f32).collect();
        let db = levels_of(&samples);
        assert!(db[64].abs() < 0.01, "{}", db[64]);
        assert!((db[63] + 6.0206).abs() < 0.01, "{}", db[63]);
        assert!((db[65] + 6.0206).abs() < 0.01, "{}", db[65]);
        assert!(db[60] < -100.0 && db[70] < -100.0, "nothing further off: {} {}", db[60], db[70]);
        // (The rounding of the samples to 32 bits leaves noise far below -120 dB: it is the floor.)
        assert!(db.iter().all(|&level| level >= FLOOR_DB), "{:?}", db.iter().cloned().fold(0.0, f32::min));
    }

    #[test]
    fn a_sine_a_tenth_as_high_is_20_db_lower() {
        let samples: Vec<f32> = (0..2048).map(|n| (0.1 * (TAU * 100.0 * n as f64 / 2048.0).sin()) as f32).collect();
        let db = levels_of(&samples);
        assert!((db[100] + 20.0).abs() < 0.01, "{}", db[100]);
    }

    #[test]
    fn silence_is_the_floor_at_every_frequency() {
        let db = levels_of(&[0.0; 256]);
        assert_eq!(db.len(), 129);
        assert!(db.iter().all(|&level| level == FLOOR_DB), "{db:?}");
    }

    #[test]
    fn the_levels_are_those_of_the_discrete_fourier_transform_of_the_windowed_block() {
        // A block that is none of the simple ones, against the sum that the transform is defined by.
        let size = 64;
        let samples: Vec<f32> = (0..size).map(|n| (((n * 37 + 11) % 29) as f32 / 14.5 - 1.0) * 0.8).collect();
        let db = levels_of(&samples);
        for (bin, &level) in db.iter().enumerate() {
            let (mut re, mut im) = (0.0f64, 0.0f64);
            for (n, &sample) in samples.iter().enumerate() {
                let window = 0.5 - 0.5 * (TAU * n as f64 / size as f64).cos();
                let angle = -TAU * (bin * n) as f64 / size as f64;
                re += sample as f64 * window * angle.cos();
                im += sample as f64 * window * angle.sin();
            }
            let expected = (20.0 * (re.hypot(im) / (size as f64 / 4.0)).log10()).max(FLOOR_DB as f64);
            assert!((f64::from(level) - expected).abs() < 1e-3, "bin {bin}: {level} against {expected}");
        }
    }

    #[test]
    #[should_panic(expected = "not a power of two")]
    fn a_block_that_is_not_a_power_of_two_is_refused() {
        let _ = Spectrum::new(100);
    }

    #[test]
    #[should_panic(expected = "blocks of 8 samples")]
    fn a_block_of_another_length_than_the_analyser_has_is_refused() {
        let mut out = [0.0; 5];
        Spectrum::new(8).decibels(&[0.0; 9], &mut out);
    }

    const RANGE: SpectrumRange = SpectrumRange { min_hz: 20.0, max_hz: 20_000.0, floor_db: -90.0, top_db: 0.0 };
    const SAMPLE_RATE: f32 = 48_000.0;

    /// The levels of a block of 2048 samples (a bin is 23.4 Hz): at the lowest there is, but for the given bins.
    fn floor_with(levels: &[(usize, f32)]) -> Vec<f32> {
        let mut db = vec![-120.0; 1025];
        for &(bin, level) in levels {
            db[bin] = level;
        }
        db
    }

    fn heights(db: &[f32], width: f64) -> Vec<f64> {
        let mut out = Vec::new();
        spectrum_heights(db, SAMPLE_RATE, width, &RANGE, &mut out);
        out
    }

    #[test]
    fn a_spectrum_has_a_point_for_each_column_from_the_left_edge_to_the_right_one_at_the_bottom_where_there_is_nothing() {
        let out = heights(&floor_with(&[]), 100.0);
        assert_eq!(out.len(), 101);
        assert!(out.iter().all(|&height| height == 0.0));
    }

    #[test]
    fn the_frequencies_of_a_spectrum_are_spread_over_the_decades() {
        // A level at 1 kHz (between the bins 42 and 43) is 100 * ln(1000 / 20) / ln(20000 / 20) = 56.6 of the way across: the column of 1026 Hz is the 57th.
        // The level halfway between the floor and the top is halfway up.
        let out = heights(&floor_with(&[(42, -45.0), (43, -45.0)]), 100.0);
        assert!((out[57] - 0.5).abs() < 1e-9, "{}", out[57]);
        assert_eq!(out[20], 0.0);
        assert_eq!(out[90], 0.0);
    }

    #[test]
    fn a_column_narrower_than_a_bin_has_the_level_between_the_two_bins_that_it_is_between() {
        // The lowest columns are a few hertz wide, and a bin 23.4: the line goes from one bin's level to the next's.
        let out = heights(&floor_with(&[(1, -90.0), (2, 0.0)]), 100.0);
        // (Column 0 is 20 Hz, which is nearly the bin 1 (23.4 Hz); the bin 2 is 46.9 Hz, near column 14.)
        assert!(out[0].abs() < 0.01, "{}", out[0]);
        // Column 8 is 34.8 Hz, which is 0.48 of the way from the bin 1 (-90) to the bin 2 (0): 0.484 of the way up.
        assert!((out[8] - 0.484).abs() < 0.01, "{}", out[8]);
        let lows = &out[..30];
        let top = lows.iter().cloned().fold(0.0, f64::max);
        assert!(top > 0.9, "{top}");
        let highest = lows.iter().position(|&height| height == top).unwrap();
        for column in 1..=highest {
            assert!(lows[column] >= lows[column - 1] - 1e-9, "rising up to the top: {lows:?}");
        }
    }

    #[test]
    fn a_column_that_holds_many_bins_has_the_highest_level_of_them_so_that_a_peak_is_not_hidden() {
        // The bin 700 (16.4 kHz) is near the right edge, where a column holds twenty bins.
        let out = heights(&floor_with(&[(700, 0.0)]), 100.0);
        assert_eq!(out.iter().cloned().fold(0.0, f64::max), 1.0);
    }

    #[test]
    fn each_level_is_in_one_column_the_one_that_its_frequency_is_in() {
        // A level between two bins widens to the bins beside it, and a column of the low end is narrower than a bin: from the bin where a column holds
        // more than one, 43 (1 kHz, a column is 70 Hz there), a bin is in one column and only that.
        for bin in 43..=700 {
            let out = heights(&floor_with(&[(bin, -45.0)]), 100.0);
            assert_eq!(out.iter().filter(|&&height| height > 0.0).count(), 1, "the bin {bin}");
        }
    }

    #[test]
    fn levels_beyond_the_floor_and_the_top_are_kept_between_the_bottom_and_the_top() {
        let out = heights(&floor_with(&[(43, 20.0), (100, -200.0)]), 100.0);
        assert!(out.iter().all(|&height| (0.0..=1.0).contains(&height)), "{out:?}");
        assert_eq!(out.iter().cloned().fold(0.0, f64::max), 1.0);
    }

    #[test]
    fn the_lines_of_the_graticule_are_at_the_columns_of_the_frequencies_a_decade_as_far_as_the_next() {
        let [low, hundred, thousand, ten, high] = [20.0, 100.0, 1000.0, 10_000.0, 20_000.0].map(|hz| frequency_column(hz, &RANGE, 300.0));
        assert!(low.abs() < 1e-9);
        assert!((high - 300.0).abs() < 1e-9);
        assert!((thousand - 300.0 * 50f64.ln() / 1000f64.ln()).abs() < 1e-9);
        // Equal ratios are equally far apart: 100 to 1000 and 1000 to 10000.
        assert!((thousand - hundred - (ten - thousand)).abs() < 1e-9);
    }

    #[test]
    fn the_line_of_a_frequency_is_where_the_spectrum_puts_the_level_of_it() {
        // The column of 1 kHz, as the spectrum puts a level of one bin there (a bin of 2048 is 23.4 Hz: the 43rd is 1008 Hz), is the one that the line is at.
        let out = heights(&floor_with(&[(43, 0.0)]), 300.0);
        let peak = out.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1)).unwrap().0;
        let line = frequency_column(43.0 * 23.4375, &RANGE, 300.0);
        assert!((line - peak as f64).abs() < 3.0, "{line} {peak}");
    }

    #[test]
    fn the_rows_of_the_levels_are_a_fraction_of_the_way_up_from_the_floor_to_the_top() {
        let rows: Vec<f64> = [0.0, -45.0, -90.0].into_iter().map(|db| level_height(db, &RANGE)).collect();
        assert_eq!(rows, [1.0, 0.5, 0.0]);
    }

    /// A ring that has had `frames` of a sine of `amplitude` pushed into it: `hertz` on the left and silence on the right.
    fn ring_with(frames: usize, hertz: f64, amplitude: f64) -> ScopeRing {
        let ring = ScopeRing::new();
        let left: Vec<f32> = (0..frames).map(|n| (amplitude * (TAU * hertz * n as f64 / f64::from(SAMPLE_RATE)).sin()) as f32).collect();
        ring.push(&[left, vec![0.0; frames]]);
        ring
    }

    #[test]
    fn a_scope_takes_the_latest_frames_of_both_sides_and_the_loudest_sample() {
        let ring = ScopeRing::new();
        let left: Vec<f32> = (0..3000).map(|n| n as f32 / 10_000.0).collect();
        let right: Vec<f32> = left.iter().map(|s| -s * 2.0).collect();
        ring.push(&[left.clone(), right.clone()]);
        let mut scope = Scope::new();
        scope.take(&ring, Instant::now());
        assert_eq!(scope.left(), &left[3000 - FRAMES..]);
        assert_eq!(scope.right(), &right[3000 - FRAMES..]);
        // The loudest is the right side's, below the middle: twice the left side's highest.
        assert_eq!(scope.peak(), 2.0 * 0.2999);
    }

    #[test]
    fn before_any_frames_have_come_a_scope_is_silent() {
        let mut scope = Scope::new();
        scope.take(&ScopeRing::new(), Instant::now());
        assert_eq!(scope.peak(), 0.0);
        assert!(scope.left().iter().chain(scope.right()).all(|&sample| sample == 0.0));
        assert_eq!(scope.left().len(), FRAMES);
    }

    #[test]
    fn the_choices_of_a_scope_are_a_view_and_a_time_base_that_stop_at_the_ends() {
        let mut scope = Scope::new();
        assert_eq!((scope.kind(), TIMES[scope.time()]), (Kind::Wave, 1024));
        scope.toggle_kind();
        assert_eq!(scope.kind(), Kind::Spectrum);
        scope.toggle_kind();
        assert_eq!(scope.kind(), Kind::Wave);
        scope.slower();
        scope.slower();
        scope.slower();
        assert_eq!(TIMES[scope.time()], 2048, "no slower than the longest");
        for _ in 0..10 {
            scope.faster();
        }
        assert_eq!(TIMES[scope.time()], 256, "no faster than the shortest");
    }

    #[test]
    fn the_time_bases_are_told_in_milliseconds_from_the_sample_rate() {
        let told: Vec<String> = (0..TIMES.len()).map(|time| time_label(time, 48_000.0)).collect();
        assert_eq!(told, ["5 ms", "11 ms", "21 ms", "43 ms"]);
        assert_eq!(time_label(2, 44_100.0), "23 ms");
    }

    #[test]
    fn the_spectrum_of_a_scope_is_that_of_the_mix_of_the_two_sides() {
        // A sine of 0.8 on the left and silence on the right is a sine of 0.4 in the mix: 20 * log10(0.4) = -7.96 dB at its bin (the bin 64 is 64 * 48000 / 2048 Hz).
        let ring = ring_with(2048, 64.0 * 48_000.0 / 2048.0, 0.8);
        let mut scope = Scope::new();
        scope.toggle_kind();
        scope.take(&ring, Instant::now());
        assert!((scope.levels()[64] + 7.9588).abs() < 0.01, "{}", scope.levels()[64]);
    }

    #[test]
    fn the_levels_of_a_spectrum_fall_slowly_where_the_sound_is_quieter_than_it_was() {
        let ring = ring_with(2048, 64.0 * 48_000.0 / 2048.0, 0.8);
        let mut scope = Scope::new();
        scope.toggle_kind();
        let then = Instant::now();
        scope.take(&ring, then);
        let loud = scope.levels()[64];
        // The sound stops: a block of silence is what the ring has now. A tenth of a second later the level has fallen 4 dB (40 dB a second) and not to the floor.
        ring.push(&[vec![0.0; 2048], vec![0.0; 2048]]);
        scope.take(&ring, then + Duration::from_millis(100));
        assert!((scope.levels()[64] - (loud - 4.0)).abs() < 1e-3, "{} from {loud}", scope.levels()[64]);
        // A long wait between two looks (the terminal was in the background) is made up for no more than a quarter of a second: 10 dB.
        scope.take(&ring, then + Duration::from_secs(60));
        assert!((scope.levels()[64] - (loud - 14.0)).abs() < 1e-3, "{} from {loud}", scope.levels()[64]);
    }

    #[test]
    fn a_spectrum_that_is_chosen_again_starts_from_nothing_and_not_from_the_levels_that_it_had() {
        let ring = ring_with(2048, 64.0 * 48_000.0 / 2048.0, 0.8);
        let mut scope = Scope::new();
        scope.toggle_kind();
        scope.take(&ring, Instant::now());
        assert!(scope.levels()[64] > -10.0);
        scope.toggle_kind();
        scope.toggle_kind();
        assert!(scope.levels().iter().all(|&level| level == FLOOR_DB));
    }

    #[test]
    fn the_peak_is_told_in_decibels_below_full_scale_and_silence_is_a_dash() {
        assert_eq!(peak_text(0.0), "—");
        // What the meters call silence (under -120 dB) is silence here: the tail of a filter that has been left alone, which is never exactly nothing, is not a level.
        assert_eq!(peak_text(5e-7), "—");
        assert_eq!(peak_text(1.5e-6), "-116.5 dB");
        assert_eq!(peak_text(1.0), "0.0 dB");
        assert_eq!(peak_text(0.5), "-6.0 dB");
        assert_eq!(peak_text(0.1), "-20.0 dB");
        assert_eq!(peak_text(2.0), "+6.0 dB", "a plus above full scale, where it clips");
        assert_eq!(peak_text(0.9999), "0.0 dB", "and no sign on what rounds to nothing");
        assert_eq!(peak_text(1.0001), "0.0 dB");
    }
}
