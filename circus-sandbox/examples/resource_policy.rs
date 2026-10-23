use circus_sandbox::runtime::{ResourceLimits, ResourceLimitsError};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    const MIB: u64 = 1024 * 1024;

    // 64 MiB memory, one quarter of a CPU, 32 processes, 16 MiB total
    // workspace capacity, and at most 1 MiB per file.
    // Per-file limits use the same 1..usize::MAX bounds as Workspace.
    let limits = ResourceLimits::new(64 * MIB, 250, 32, 16 * MIB, 1024 * 1024)?;
    println!(
        "Valid request: memory={} bytes, CPU={} milli-CPU (1000 = one CPU), \
         processes={}, workspace={} bytes, per-file={} bytes",
        limits.memory_bytes(),
        limits.cpu_millis(),
        limits.pids(),
        limits.workspace_bytes(),
        limits.max_file_bytes(),
    );
    println!("Validation does not enforce limits, isolate execution, or admit aggregate demand.");

    // A 65 MiB workspace conflicts with the 64 MiB memory request.
    match ResourceLimits::new(64 * MIB, 250, 32, 65 * MIB, 1024 * 1024) {
        Err(error @ ResourceLimitsError::WorkspaceExceedsMemory { .. }) => {
            println!("Typed rejection (WorkspaceExceedsMemory): {error}");
        }
        Err(error) => return Err(error.into()),
        Ok(_) => return Err("conflicting resource request was unexpectedly accepted".into()),
    }
    Ok(())
}
