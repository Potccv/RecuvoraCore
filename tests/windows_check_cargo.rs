//! Cargo process substitute for the standalone Windows check-script tests.

use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let records = PathBuf::from(env!("RECUVORA_CHECK_STUB_RECORD"));
    let mut call = 1;
    while records.join(format!("cargo-call-{call}.txt")).exists() {
        call += 1;
    }
    let mut record = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(records.join(format!("cargo-call-{call}.txt")))?;
    let arguments: Vec<OsString> = std::env::args_os().skip(1).collect();
    writeln!(record, "{arguments:?}")?;
    writeln!(record, "{}", std::env::current_dir()?.display())?;
    for name in [
        "CARGO_TARGET_DIR",
        "RECUVORA_TEST_TEMP",
        "RUST_TEST_THREADS",
    ] {
        writeln!(record, "{}", std::env::var(name)?)?;
    }
    for name in ["PATH", "RUSTC"] {
        writeln!(record, "{:?}", std::env::var_os(name))?;
    }
    record.sync_all()?;

    let fail_at = records.join("fail-at.txt");
    if fail_at.exists() && fs::read_to_string(fail_at)?.trim() == call.to_string() {
        std::process::exit(30 + call);
    }
    Ok(())
}
