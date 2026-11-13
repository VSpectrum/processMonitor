# procmon

A lightweight, zero-dependency real-time terminal process monitor engineered specifically for macOS, written in Rust.

`procmon` inspects system processes directly via native macOS kernel APIs (`libproc`), tracking real-time CPU utilization, resident memory (RSS), and historical peak usage in a flicker-free terminal dashboard.

---

## Features

- **Direct macOS Kernel Telemetry**: Interfaces directly with macOS `libproc` C APIs (`proc_listpids`, `proc_pidinfo`, `proc_name`) without invoking external CLI tools like `ps` or `top`.
- **Zero External Dependencies**: Implemented entirely with Rust's standard library and Darwin libc bindings for instant compilation and a compact binary footprint.
- **Peak Resource Tracking**: Records both current and peak CPU% and RAM (RSS) metrics across process lifespans.
- **Sortable table**: Starts on CPU% (highest first). Change it with `--sort` or with a key while the monitor is running: RAM, peak CPU, peak RAM, PID, name, or ticks.
- **Spike log**: Appends CPU crossings and short-lived bursts to a log so a process that spikes and disappears is still visible. Lines are queued in memory and written in batches.
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
│ Total RAM:   4821.5 MB   CPU:   18.4%   Sort: CPU% (highest first)                     │
├────────┬──────────────────────────┬──────────┬──────────┬───────────┬───────────┬──────┤
│ PID    │ NAME                     │    CPU % │ PEAK CPU │  RAM (MB) │  PEAK RAM │ TICKS│
├────────┼──────────────────────────┬──────────┬──────────┬───────────┬───────────┬──────┤
│ 12845  │ rust-analyzer            │    12.4% │    45.2% │     612.4 │     640.1 │   42 │
│ 943    │ WindowServer             │     4.2% │    15.8% │     820.7 │     850.3 │   42 │
│ 3120   │ Google Chrome Helper     │     1.8% │    22.0% │     340.5 │     410.2 │   42 │
│ 81402  │ procmon                  │     0.1% │     0.3% │       4.2 │       4.5 │   42 │
└────────┴──────────────────────────┴──────────┴──────────┴───────────┴───────────┴──────┘
 log ~/Library/Logs/procmon/spikes.log >=25% queued=0 saved=0
 [c] CPU  [m] RAM  [C] peak CPU  [M] peak RAM  [p] PID  [n] name  [t] ticks  [q] quit
```

---

## Requirements

- **Operating System**: macOS (Darwin), Apple Silicon or Intel.
- **Rust**: Rust 2021 edition or newer (`rustc` / `cargo`).

---

## Quick Start & Building

### 1. Build Standalone macOS App (Recommended)

Run the automated macOS build script to compile the optimized binary and code sign it:

```bash
./build.sh
```

This generates:
- `./bin/procmon` (executable)
- `./procmon` (symlink in root directory)

You can now run it immediately without `cargo run`:

```bash
./procmon
```

### 2. Install to Your PATH (Run from Anywhere)

Install `procmon` directly into `~/.cargo/bin` (or custom `--install-dir`) so you can type `procmon` from any directory in any terminal:

```bash
./build.sh --install
```

Then simply run:

```bash
procmon
procmon -i 2 -n 10
```

### 3. Build Script Options

| Option | Description |
| :--- | :--- |
| `./build.sh` | Builds optimized release binary in `./bin/procmon` and `./procmon` |
| `./build.sh --install` | Installs binary to `~/.cargo/bin` (in your `$PATH`) |
| `./build.sh --install-dir <DIR>` | Installs binary to a custom directory (e.g. `/usr/local/bin`) |
| `./build.sh --universal` | Compiles a Universal binary (Apple Silicon + Intel via `lipo`) |
| `./build.sh --clean` | Runs `cargo clean` before building |
| `./build.sh --help` | Shows usage and options |

---

## How to Run

### Standalone Executable

```bash
# Run local binary
./procmon

# Run with custom update interval (e.g., 2 seconds) and 10 top processes
./procmon -i 2 -n 10

# Start sorted by resident memory instead of CPU
./procmon --sort ram

# Log CPU crossings at 40% or above, holding lines for up to 60 seconds
./procmon --spike 40 --log-flush 60

# View all CLI options
./procmon --help
```

### Development Mode

If working directly on the source code, you can also run using Cargo:

```bash
cargo run -- -i 2
cargo run --release -- -i 2 -n 10
```

### Running with Elevated Privileges (Optional)

By default, macOS restricts querying detailed task information for certain protected root daemons. Running with `sudo` grants access to inspect all system processes:

```bash
sudo ./target/release/procmon
```

---

## Why is procmon so Lightweight and Fast?

`procmon` is specifically designed to have a negligible system footprint (typically **`0.1% – 0.3% CPU`** and **`~1 – 4 MB RAM`** on macOS). It avoids the common architectural bottlenecks found in conventional monitoring scripts:

### 1. Direct macOS Kernel FFI vs. Process Spawning
- **The slow way (Shell tools)**: Many monitoring tools repeatedly spawn subshells to run `ps aux` or `top -l 1`, pipe stdout into text parsers, and allocate thousands of strings every second. Spawning processes (`fork`/`exec`) and parsing text can easily consume 5%–15% CPU on its own.
- **The `procmon` way**: `procmon` talks directly to the Darwin kernel via native C FFI (`proc_listpids` and `proc_pidinfo` with `PROC_PIDTASKINFO`). It retrieves binary task structs directly from the kernel in memory. Querying all 400+ active PIDs takes only **~0.2 to 0.5 milliseconds** of execution time per tick.

### 2. Zero External Dependencies & No Heavy Runtimes
- Built purely with the Rust standard library and raw libc bindings.
- No heavy asynchronous runtimes (e.g., Tokio), no background thread pools, and no garbage collector (GC) pauses.
- Compiles down to a single compact native binary.

### 3. One-Time Process Name Caching
- Process names ([`proc_name`](src/main.rs)) are resolved through a kernel syscall only **once** when a PID is first detected.
- On subsequent ticks, `procmon` only updates numeric counters (CPU timestamp deltas and resident memory bytes) with simple arithmetic, eliminating string re-allocations for existing processes.

### 4. $O(N)$ Quickselect for Top Processes
- Rather than sorting all 400+ processes every tick with a full $O(N \log N)$ sort, `procmon` uses Rust's `select_nth_unstable_by` (Quickselect algorithm).
- It partitions the top $K$ processes in linear $O(N)$ time, and only sorts the final $K$ displayed rows (e.g., $K = 15$).

### 5. Flicker-Free Double-Buffered Terminal Rendering
- Allocates a single reusable string buffer for formatting the entire screen frame.
- Emits a frame-dirty check: if the rendered content has not changed, it avoids re-flushing to terminal stdout.
- Uses atomic stdout locking and single-call buffer flushing to prevent tearing and cursor flicker.

### 6. True Kernel Sleep (Zero Busy-Waiting)
- Between sample cycles, `procmon` blocks in `poll`, yielding the CPU back to the macOS scheduler.
- The same wait returns early for a sort key or for Ctrl-C. There is no input-polling thread and no busy loop.
- The thread stays dormant until the kernel wakes it up, using no CPU between ticks.

### 7. Batched Spike Log
- A spike line is formatted into an in-memory buffer. The file is opened once, on the first flush, and kept open.
- Disk writes happen when the buffer reaches 8 KiB, when the oldest queued line is 30 seconds old (`--log-flush`), or when the process exits.
- There is no `fsync`. A quiet run with no spikes never creates the file.

---

## Mitigating CPU Usage for 24/7 Monitoring

If you plan to run `procmon` continuously in the background or want to maximize battery life on Apple Silicon laptops:

1. **Use `--release` builds**:
   Always compile with `cargo run --release` or `cargo build --release`. LLVM release optimizations vectorize loops, inline critical syscall wrappers, and strip runtime debug overhead.
2. **Increase the Refresh Interval**:
   System monitors like macOS *Activity Monitor* default to 2–5 second intervals. Running with `-i 2` or `-i 3` cuts kernel syscalls and CPU wakeups by 50% to 66%, reducing CPU usage to under **0.1%**:
   ```bash
   procmon -i 3
   ```
3. **Limit Displayed Rows**:
   If running on a smaller terminal, lowering the row count (e.g., `-n 10`) reduces terminal buffer formatting overhead.

---

## Sorting

The table starts sorted by current CPU%, highest first. `--sort` / `-s` picks the starting column. In a terminal, the keys below switch columns immediately and the next sample still happens on the original interval.

| `--sort` value | Keys | Top of the table |
| :--- | :--- | :--- |
| `cpu` (default) | `c` | Highest CPU% |
| `ram`, `mem`, `memory`, `rss` | `m` or `r` | Highest resident memory |
| `peak-cpu` | `C` | Highest CPU% seen for that process |
| `peak-ram`, `peak-mem` | `M` or `R` | Highest resident memory seen |
| `pid` | `p` | Lowest PID |
| `name` | `n` | Name, A to Z |
| `ticks` | `t` | Longest time on screen |

`Ctrl-Z` suspends the monitor and puts the terminal back the way it was. `fg` resumes it.

## Spike Log

`procmon` keeps a log of CPU spikes so a process that flares up and exits is not lost when it drops out of the table.

| Event | When it is recorded |
| :--- | :--- |
| `SPIKE` | CPU% crosses `--spike` (default 25). It is not recorded again until CPU falls below half that threshold and crosses it once more. |
| `GONE` | A process whose peak CPU reached the threshold has exited. Includes the peak and how long it was watched. |
| `BURST` | A process vanished inside a single sample, was only a few seconds old, and had already used at least 50ms of CPU at or above the threshold. |

Very short spikes can still hide between samples. A smaller `-i` (for example `0.5`) shortens that gap. The default 1 second interval is what keeps `procmon` itself well under 1% CPU.

Lines sit in memory and are written together. The default file is `~/Library/Logs/procmon/spikes.log`. Nothing is created until the first event. `--log <PATH>` moves the file, `--no-log` turns it off, and `--log-flush <SECS>` caps how long a line waits (minimum 1 second, default 30). Ctrl-C writes the queue before exit. `kill -9` can drop whatever is still only in memory.

```text
2026-11-13T05:27:01-05:00 SPIKE pid=12845 cpu=82.4 peak_cpu=82.4 ram_mb=120.5 peak_ram_mb=120.5 ticks=4 name="rustc"
2026-11-13T05:27:03-05:00 GONE pid=12845 last_cpu=4.0 peak_cpu=82.4 ram_mb=20.1 peak_ram_mb=140.2 ticks=6 tracked_s=5.0 name="rustc"
```

## Controls

| Key / Action | Description |
| :--- | :--- |
| `c` / `m` / `C` / `M` | Sort by CPU, RAM, peak CPU, or peak RAM |
| `p` / `n` / `t` | Sort by PID, name, or ticks seen |
| `q` or `Ctrl + C` | Quit and flush the spike log |
| `Ctrl + Z` | Suspend and restore the terminal; `fg` resumes |

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
