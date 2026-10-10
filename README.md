# procmon

A lightweight, zero-dependency real-time terminal process monitor engineered specifically for macOS, written in Rust.

`procmon` inspects system processes directly via native macOS kernel APIs (`libproc`), tracking real-time CPU utilization, resident memory (RSS), and historical peak usage in a flicker-free terminal dashboard.

---

## Features

- **Direct macOS Kernel Telemetry**: Interfaces directly with macOS `libproc` C APIs (`proc_listpids`, `proc_pidinfo`, `proc_name`) without invoking external CLI tools like `ps` or `top`.
- **Zero External Dependencies**: Implemented entirely with Rust's standard library and Darwin libc bindings for instant compilation and a compact binary footprint.
- **Peak Resource Tracking**: Records both current and peak CPU% and RAM (RSS) metrics across process lifespans.
- **System Aggregates**: Displays total monitored memory, aggregated CPU load, active PID count, monitor ticks, and uptime.
- **Flicker-Free Terminal UI**: Uses the alternate screen buffer (`\x1b[?1049h`), in-place cursor repositioning, and double-buffering / frame dirty checking to avoid flickering and terminal scroll pollution.
- **Low-Overhead Performance**:
  - $O(N)$ top-$K$ selection using `select_nth_unstable_by` rather than full $O(N \log N)$ sorting.
  - Zero-allocation row references for rendering.
  - Process name resolution cached on first discovery.
  - Single atomic buffer flush per refresh cycle.
- **Graceful Shutdown**: Installs `SIGINT` (Ctrl+C) and `SIGTERM` signal handlers with RAII terminal teardown to reliably restore the normal terminal buffer and unhide the cursor on exit.

---

## Preview

```text
┌────────────────────────────────────────────────────────────────────────────────────────┐
│ PROCMON — macOS Process Monitor         Tick: #42     Active PIDs: 485    Uptime: 00:00:42 │
│ Total Monitored RAM:   4821.5 MB         Total System Process CPU:   18.4%              │
├────────┬──────────────────────────┬──────────┬──────────┬───────────┬───────────┬──────┤
│ PID    │ NAME                     │    CPU % │ PEAK CPU │  RAM (MB) │  PEAK RAM │ TICKS│
├────────┼──────────────────────────┬──────────┬──────────┬───────────┬───────────┬──────┤
│ 12845  │ rust-analyzer            │    12.4% │    45.2% │     612.4 │     640.1 │   42 │
│ 943    │ WindowServer             │     4.2% │    15.8% │     820.7 │     850.3 │   42 │
│ 3120   │ Google Chrome Helper     │     1.8% │    22.0% │     340.5 │     410.2 │   42 │
│ 81402  │ procmon                  │     0.1% │     0.3% │       4.2 │       4.5 │   42 │
└────────┴──────────────────────────┴──────────┴──────────┴───────────┴───────────┴──────┘
 [Ctrl+C to quit]  Tracking 42 historical snapshots
```

---

## Requirements

- **Operating System**: macOS (Darwin), Apple Silicon or Intel.
- **Rust**: Rust 2021 edition or newer (`rustc` / `cargo`).

---

## How to Run

### Development Mode

Run directly using Cargo:

```bash
cargo run
```

### Release Mode (Recommended)

Build and execute the optimized binary for minimal CPU overhead:

```bash
cargo run --release
```

### Building a Standalone Binary

Compile the standalone executable:

```bash
cargo build --release
```

The resulting binary will be located at:

```bash
./target/release/procmon
```

You can copy or symlink this binary into your `$PATH` (e.g. `/usr/local/bin` or `~/.cargo/bin`) to run it from anywhere.

### Running with Elevated Privileges (Optional)

By default, macOS restricts querying detailed task information for certain protected root daemons. Running with `sudo` grants access to inspect all system processes:

```bash
sudo ./target/release/procmon
```

---

## Controls

| Key / Action | Description |
| :--- | :--- |
| `Ctrl + C` | Gracefully quit the monitor and restore terminal state |

---

## Running Tests

Run the built-in unit tests:

```bash
cargo test
```

---

## Project Structure

```text
procmon/
├── Cargo.toml      # Package configuration (zero third-party dependencies)
├── src/
│   └── main.rs     # Process monitor, libproc FFI, terminal renderer, and tests
└── README.md       # Project documentation
```
