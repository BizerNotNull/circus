# ADR 0001: Docker Linux + runc initial slice

## Decision

Use bollard 0.21.1 with only its local `pipe` transport feature. The public SDK accepts explicit local Unix socket or Windows named-pipe endpoints and never exposes bollard types. Only `runc` is accepted.

## Security and ownership

Each container gets explicit memory, CPU, pids, read-only root, disabled network, no-new-privileges, all capability drop, bounded tmpfs, no host mounts, and no credentials. A generated owner label and name are fixed before create. Cancellation assumes a request may have been accepted and recovers only that exact, label-matching container. This costs cleanup latency but avoids an orphaned accepted request.

## Workspace and commands

The existing capability-scoped `Workspace` remains the host staging API. Files are archived and uploaded into tmpfs; no second host path is opened. Commands use argv, bounded output, and a timeout. Whole-container destruction on timeout/cancellation is intentional: it terminates descendants without fragile process-group handling.

## Not included

Docker commit snapshots are not claimed: read-only root plus tmpfs would omit workspace changes. Snapshot restore, image pulling, HTTP, Git, pools, health/metrics, and other runtimes remain separate work.
