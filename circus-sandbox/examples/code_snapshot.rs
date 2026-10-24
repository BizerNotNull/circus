//! Captures code into memory and restores it into a fresh workspace.

use std::io;
use std::process::ExitCode;

use circus_sandbox::{SnapshotBudget, Workspace};

fn main() -> ExitCode {
    match run() {
        Ok(bytes) => {
            println!("{}", String::from_utf8_lossy(&bytes));
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("{err}");
            ExitCode::from(1)
        }
    }
}

fn run() -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let mut source = Workspace::new(64)?;
    source.create_dir_all("src/empty")?;
    source.write_file("src/main.rs", b"snapshot bytes")?;
    let snapshot = source.capture_code_snapshot(SnapshotBudget::new(64, 8)?)?;
    let mut restored = snapshot.restore()?;
    let bytes = restored.read_file("src/main.rs")?;
    restored.write_file("src/empty/proof.txt", b"restored")?;
    restored.cleanup()?;
    source.cleanup()?;
    if bytes != b"snapshot bytes" {
        return Err(io::Error::other("restored bytes differ").into());
    }
    Ok(bytes)
}
