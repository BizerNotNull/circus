//! Writes, reads, removes, and cleans up a real temporary workspace.
//!
//! Stdout is the byte string actually returned by `read_file`, followed by a
//! newline. A failed step prints that error to stderr and exits 1.

use std::io::{self, Write};
use std::process::ExitCode;

use circus_sandbox::{Workspace, WorkspaceError};

fn main() -> ExitCode {
    match run() {
        Ok(read) => {
            let mut stdout = io::stdout().lock();
            if stdout
                .write_all(&read)
                .and_then(|()| stdout.write_all(b"\n"))
                .is_err()
            {
                return ExitCode::from(1);
            }
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("{err}");
            ExitCode::from(1)
        }
    }
}

fn run() -> Result<Vec<u8>, WorkspaceError> {
    let mut workspace = Workspace::new(64)?;
    workspace.create_dir_all("notes")?;
    let written = b"circus-sandbox";
    workspace.write_file("notes/note.txt", written)?;
    let read = workspace.read_file("notes/note.txt")?;
    if read.as_slice() != written {
        return Err(io::Error::other(format!("read mismatch: {read:?}")).into());
    }
    workspace.remove_file("notes/note.txt")?;
    workspace.cleanup()?;
    match workspace.read_file("notes/note.txt") {
        Err(WorkspaceError::Closed) => Ok(read),
        Err(err) => Err(err),
        Ok(bytes) => Err(io::Error::other(format!("expected Closed, read {bytes:?}")).into()),
    }
}
