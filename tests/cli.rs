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
