//! The program as it is run from a shell: what it prints, and the status it ends with.
//!
//! `main` ends the process itself, after everything has been shut down in order (see `main.rs`), so
//! that what it says and the status it leaves are the program's to keep: these tests hold them.

use std::process::{Command, Output};

fn tang(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_tang")).args(args).output().expect("tang runs")
}

#[test]
fn a_command_that_works_says_all_it_has_to_say_and_ends_with_status_0() {
    let output = tang(&["describe", "builtin:osc"]);
    let said = String::from_utf8_lossy(&output.stdout);
    assert_eq!(output.status.code(), Some(0), "{said}{}", String::from_utf8_lossy(&output.stderr));
    assert!(said.starts_with("Oscillator\n"), "{said}");
    assert!(said.ends_with("  Presets:       (none)\n"), "the last line of it is there: {said}");
}

#[test]
fn describe_says_how_late_a_plugin_is() {
    // The drive is late by the 23 frames of its oversampling filters (0.48 ms at 48 kHz); the oscillator is not late.
    let said = |plugin: &str| String::from_utf8_lossy(&tang(&["describe", plugin]).stdout).to_string();
    assert!(said("builtin:drive").contains("\n  Latency:       23 frames (0.48 ms at 48000 Hz)\n"), "{}", said("builtin:drive"));
    assert!(said("builtin:osc").contains("\n  Latency:       none\n"), "{}", said("builtin:osc"));
}

#[test]
fn describe_says_which_effect_has_a_second_input_and_what_it_calls_the_two() {
    let said = |plugin: &str| String::from_utf8_lossy(&tang(&["describe", plugin]).stdout).to_string();
    assert!(said("builtin:math").contains("\n  Audio inputs:  two: A (what the chain feeds) and B (cables only)\n"), "{}", said("builtin:math"));
    assert!(said("builtin:math").contains("[0] mode (ring|am|fold|warp|compare|min|max|xor|and|or|hold|follow, default=ring)"), "{}", said("builtin:math"));
    assert!(!said("builtin:filter").contains("Audio inputs"), "{}", said("builtin:filter"));
}

#[test]
fn a_command_that_fails_says_why_on_stderr_and_ends_with_status_1() {
    let output = tang(&["describe", "builtin:nope"]);
    assert_eq!(output.status.code(), Some(1));
    let said = String::from_utf8_lossy(&output.stderr);
    assert!(said.starts_with("Error: Unknown built-in plugin: \"nope\"\n"), "{said}");
    assert!(output.stdout.is_empty(), "it printed nothing but the error: {}", String::from_utf8_lossy(&output.stdout));
}

#[test]
fn a_command_line_that_is_not_one_says_so_and_ends_with_status_2() {
    let output = tang(&["--nope"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("unexpected argument '--nope'"));
}
