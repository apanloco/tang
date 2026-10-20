//! The benchmark (P0 of `docs/patch-graph.md` §11.6): how long a block of the engine takes against the time it has, and whether it asks for memory.
//!
//! `cargo test --release oracle::bench -- --ignored --nocapture --test-threads=1` prints the table. A block is what the audio callback does, `Renderer::render` and nothing else
//! (the loading of the session, the controller's polling and the script's notes are outside the clock); the time it has is `frames / sample rate` (1.33 ms at 64 frames, 5.33 ms
//! at 256, 21.3 ms at 1024), and a block that takes longer is a dropout. Numbers are only for the machine and the build they were made on: the docs say which.

use std::fmt::Write;

use super::*;

/// How many blocks are rendered and not timed, so that the first notes' voices, the caches and the branch predictors are where they will be.
const WARM_UP: usize = 100;

/// The frame at which a held chord begins: after the first block of every block size, so that a lane that is muted from the start has been let go of (over that block) before
/// anything sounds, and inside the warm-up.
const CHORD_AT: u64 = 1024;

/// `lanes` lanes that are a lot for the engine, in two groups that have a reverb and a filter on their buses: each is a saw with a unison of seven (and a little FM, a sub and a
/// drift) through a ladder filter, a delay with a wobble, a reverb and a drive, with an LFO on the cutoff, a faster one on the pitch, an envelope on the cutoff and a step sequencer
/// on the delay's time, so that every lane has four modulators and four plugins. With `muted` every lane is muted.
pub fn stress_session(lanes: usize, muted: bool) -> String {
    let mut out = String::from("bpm = 120.0\n");
    for _ in 0..2 {
        out.push_str("[[group]]\nvolume = 0.8\n[[group.effect]]\nplugin = \"builtin:reverb\"\nmix = 0.3\n[[group.effect]]\nplugin = \"builtin:filter\"\nmix = 1.0\n");
        out.push_str("[[group.modulator]]\ntype = \"lfo\"\nrate = 0.3\n[[group.modulator.target]]\nbus = 1\nparam = \"cutoff\"\ndepth = 0.3\n");
    }
    for lane in 0..lanes {
        writeln!(out, "[[instrument]]\nplugin = \"builtin:osc\"\nvolume = 0.2\ngroup = {}", lane % 2).unwrap();
        if muted {
            out.push_str("mute = true\n");
        }
        out.push_str("params = { \"waveform\" = 2.0, \"unison\" = 6.0, \"spread\" = 0.4, \"width\" = 0.8, \"fm\" = 0.1, \"sub\" = 0.3, \"drift\" = 0.3, \"release\" = 0.3 }\n");
        out.push_str("[[instrument.effect]]\nplugin = \"builtin:filter\"\nmix = 1.0\nparams = { \"type\" = 4.0, \"cutoff\" = 0.6, \"resonance\" = 4.0, \"drive\" = 3.0 }\n");
        out.push_str("[[instrument.effect]]\nplugin = \"builtin:delay\"\nmix = 0.3\nparams = { \"time\" = 0.25, \"feedback\" = 0.5, \"mod_depth\" = 2.0 }\n");
        out.push_str("[[instrument.effect]]\nplugin = \"builtin:reverb\"\nmix = 0.3\n");
        out.push_str("[[instrument.effect]]\nplugin = \"builtin:drive\"\nmix = 1.0\nparams = { \"type\" = 3.0, \"drive\" = 0.4 }\n");
        out.push_str("[[instrument.modulator]]\ntype = \"lfo\"\nrate = 0.5\n[[instrument.modulator.target]]\nparam = \"cutoff\"\nslot = 1\ndepth = 0.3\n");
        out.push_str("[[instrument.modulator]]\ntype = \"lfo\"\nwaveform = \"triangle\"\nrate = 4.0\n[[instrument.modulator.target]]\nparam = \"detune\"\nslot = 0\ndepth = 0.02\n");
        out.push_str("[[instrument.modulator]]\ntype = \"envelope\"\nattack = 0.05\ndecay = 0.3\nsustain = 0.6\nrelease = 0.4\n[[instrument.modulator.target]]\nparam = \"cutoff\"\nslot = 1\ndepth = 0.5\n");
        out.push_str("[[instrument.modulator]]\ntype = \"steps\"\nsteps = [0.0, 0.5, 0.25, -0.25, 0.75, 0.0, -0.5, 0.25]\nrate = 6.0\n[[instrument.modulator.target]]\nparam = \"time\"\nslot = 2\ndepth = 0.05\n");
    }
    out
}

/// What a scenario of the benchmark is: a session, and whether a chord is held from the start (a session with no notes shows what the engine does when nothing sounds).
struct Case {
    name: &'static str,
    session: String,
    notes: bool,
}

fn cases() -> Vec<Case> {
    let basic = std::fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/oracle/sessions/basic.toml")).expect("the basic session");
    vec![
        Case { name: "one oscillator, a chord", session: basic, notes: true },
        Case { name: "8 lanes of the stress session, nothing played", session: stress_session(8, false), notes: false },
        Case { name: "8 lanes of the stress session, a chord of 6", session: stress_session(8, false), notes: true },
        Case { name: "the same, every lane muted", session: stress_session(8, true), notes: true },
    ]
}

/// A chord of six notes that is held to the end.
fn held_chord() -> Vec<(u64, [u8; 3])> {
    [48, 52, 55, 60, 64, 67].iter().map(|&key| (CHORD_AT, [0x90, key, 100])).collect()
}

/// How long each of `blocks` blocks took, and how many times the engine asked for memory in them, after a warm-up.
pub struct Timing {
    /// Sorted, the shortest first.
    pub took: Vec<Duration>,
    pub allocations: usize,
    /// The blocks that asked for memory.
    pub allocating_blocks: usize,
}

pub fn measure(session: &str, notes: bool, block: usize, blocks: usize, make: &dyn Fn(Wiring) -> Box<dyn Engine>) -> Timing {
    let scenario = Scenario {
        id: format!("bench@{block}"),
        session: session.to_string(),
        block,
        rate: RATE,
        frames: ((WARM_UP + blocks) * block) as u64,
        script: if notes { held_chord() } else { Vec::new() },
        edits: Vec::new(),
    };
    let mut rig = Rig::new(&scenario, make);
    for _ in 0..WARM_UP {
        rig.block();
        rig.poll();
    }
    let mut took = Vec::with_capacity(blocks);
    let (mut allocations, mut allocating_blocks) = (0, 0);
    for _ in 0..blocks {
        let b = rig.block();
        took.push(b.took);
        allocations += b.allocations;
        allocating_blocks += usize::from(b.allocations > 0);
        rig.poll();
    }
    took.sort();
    Timing { took, allocations, allocating_blocks }
}

/// The `p`-th percentile (0 to 100) of a sorted list, the value that `p` percent of the list is not above.
pub fn percentile(sorted: &[Duration], p: f64) -> Duration {
    assert!(!sorted.is_empty() && (0.0..=100.0).contains(&p));
    let at = (p * sorted.len() as f64 / 100.0).ceil() as usize;
    sorted[at.clamp(1, sorted.len()) - 1]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn micros(n: &[u64]) -> Vec<Duration> {
        n.iter().map(|&n| Duration::from_micros(n)).collect()
    }

    #[test]
    fn a_percentile_is_the_value_that_that_part_of_the_list_is_not_above() {
        let list = micros(&[1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
        assert_eq!(percentile(&list, 50.0), Duration::from_micros(5));
        assert_eq!(percentile(&list, 90.0), Duration::from_micros(9));
        assert_eq!(percentile(&list, 99.0), Duration::from_micros(10));
        assert_eq!(percentile(&list, 100.0), Duration::from_micros(10));
        assert_eq!(percentile(&list, 0.0), Duration::from_micros(1), "the shortest is the least a percentile is");
        assert_eq!(percentile(&micros(&[7]), 95.0), Duration::from_micros(7));
    }

    #[test]
    fn the_stress_session_loads_and_is_heard_and_muted_it_is_not() {
        for muted in [false, true] {
            let scenario = Scenario {
                id: "stress".to_string(),
                session: stress_session(8, muted),
                block: BLOCK,
                rate: RATE,
                frames: 24_000,
                script: held_chord(),
                edits: Vec::new(),
            };
            let rendered = render(&scenario, &engine);
            let peak = rendered.samples.iter().fold(0.0f32, |m, s| m.max(s.abs()));
            if muted {
                assert_eq!(peak, 0.0, "every lane is muted before anything is played");
            } else {
                assert!(peak > 0.05 && peak < 4.0, "peak {peak}");
            }
        }
    }

    #[test]
    #[ignore = "a benchmark: cargo test --release oracle::bench -- --ignored --nocapture --test-threads=1"]
    fn the_engine_against_its_deadline() {
        eprintln!("{:<48} {:>6} {:>5} {:>9} {:>9} {:>9} {:>9} {:>9} {:>8}", "session", "engine", "block", "median", "p95", "p99", "max", "of budget", "allocs");
        for case in cases() {
            for (block, blocks) in [(64, 4000), (256, 2000), (1024, 500)] {
                // The engine as the program has it (the plan) and the loop it replaced, one after the other on the same session.
                let engines: [(&str, &dyn Fn(Wiring) -> Box<dyn Engine>); 2] = [("loop", &legacy), ("plan", &engine)];
                for (which, make) in engines {
                    let timing = measure(&case.session, case.notes, block, blocks, make);
                    let budget = Duration::from_secs_f64(block as f64 / f64::from(RATE));
                    let us = |d: Duration| d.as_secs_f64() * 1e6;
                    let (median, p95, p99, max) = (percentile(&timing.took, 50.0), percentile(&timing.took, 95.0), percentile(&timing.took, 99.0), *timing.took.last().unwrap());
                    eprintln!(
                        "{:<48} {:>6} {:>5} {:>7.0}µs {:>7.0}µs {:>7.0}µs {:>7.0}µs {:>8.1}% {:>8}",
                        case.name,
                        which,
                        block,
                        us(median),
                        us(p95),
                        us(p99),
                        us(max),
                        100.0 * median.as_secs_f64() / budget.as_secs_f64(),
                        timing.allocations
                    );
                    assert_eq!(timing.allocating_blocks == 0, timing.allocations == 0);
                }
            }
        }
    }
}
