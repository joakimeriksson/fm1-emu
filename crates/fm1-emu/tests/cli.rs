//! fm1-emu as a script runs it: its exit status, and the files it was asked for.
//!
//! The image is zeroed memory: `nop`s, and after 24 of them the run halts ("nop slide"); fewer
//! steps end at the step limit.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// A new folder for a test, with `zeros.bin` in it.
fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("fm1-emu-cli-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("zeros.bin"), [0u8; 256]).unwrap();
    dir
}

/// fm1-emu on the zeroed image, run in `dir` (where `work/` is, or is not).
fn emu(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_fm1-emu")).current_dir(dir).args(["--bin", "zeros.bin"]).args(args).output().unwrap()
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

#[test]
fn a_halt_exits_1_and_the_step_limit_0() {
    let dir = scratch("status");
    let halted = emu(&dir, &["--steps", "100"]);
    assert!(text(&halted.stdout).contains("halt reason:"), "{}", text(&halted.stdout));
    assert_eq!(halted.status.code(), Some(1));
    let ran = emu(&dir, &["--steps", "10"]);
    assert!(text(&ran.stdout).contains("step limit"), "{}", text(&ran.stdout));
    assert_eq!(ran.status.code(), Some(0));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn quiet_still_writes_the_wav_and_the_screen_and_says_why_it_halted() {
    let dir = scratch("quiet");
    std::fs::create_dir_all(dir.join("work")).unwrap();
    let out = emu(&dir, &["--steps", "100", "--quiet", "--wav", "out.wav", "--dump", "1c00000", "8"]);
    assert_eq!(out.status.code(), Some(1));
    let wav = std::fs::read(dir.join("out.wav")).expect("the WAV asked for");
    assert_eq!(&wav[..4], b"RIFF");
    assert!(dir.join("work/lcd.bmp").exists(), "the screen at the end");
    // the reason on stderr, with the halting core's registers; stdout stays the dump's
    let (said, dump) = (text(&out.stderr), text(&out.stdout));
    assert!(said.contains("halt reason:") && said.contains("r15="), "{said}");
    assert!(!dump.contains("halt reason") && dump.contains("dump 0x01c00000 (8 words):"), "{dump}");
    assert!(!dump.contains("stopped after"), "--quiet: no report\n{dump}");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_file_that_cannot_be_written_is_said_and_exits_1() {
    let dir = scratch("unwritable");
    // (no halt: only the files fail) a WAV into a folder that is not there
    let out = emu(&dir, &["--steps", "10", "--wav", "nowhere/out.wav"]);
    assert!(text(&out.stderr).contains("cannot write nowhere/out.wav"), "{}", text(&out.stderr));
    assert_eq!(out.status.code(), Some(1));
    // --snap writes work/lcd_<ms>.bmp, and here there is no work/
    let out = emu(&dir, &["--steps", "10", "--quiet", "--snap", "0"]);
    assert!(text(&out.stderr).contains("cannot write work/lcd_0.bmp"), "{}", text(&out.stderr));
    assert_eq!(out.status.code(), Some(1));
    // with work/ there, the same run is fine
    std::fs::create_dir_all(dir.join("work")).unwrap();
    let out = emu(&dir, &["--steps", "10", "--quiet", "--snap", "0"]);
    assert!(dir.join("work/lcd_0.bmp").exists());
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    std::fs::remove_dir_all(&dir).unwrap();
}
