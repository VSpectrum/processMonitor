use std::collections::{HashMap, VecDeque};
use std::fmt::Write as FmtWrite;
use std::io::{stdout, IsTerminal, Write};
use std::mem::MaybeUninit;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::sleep;
use std::time::{Duration, Instant};

#[repr(C)]
#[derive(Default, Debug, Clone, Copy)]
pub struct ProcTaskInfo {
    pub pti_virtual_size: u64,
    pub pti_resident_size: u64,
    pub pti_total_user: u64,
    pub pti_total_system: u64,
    pub pti_threads_user: u64,
    pub pti_threads_system: u64,
    pub pti_policy: i32,
    pub pti_faults: i32,
    pub pti_pageins: i32,
    pub pti_cow_faults: i32,
    pub pti_messages_sent: i32,
    pub pti_messages_received: i32,
    pub pti_syscalls_mach: i32,
    pub pti_syscalls_unix: i32,
    pub pti_csw: i32,
    pub pti_threadnum: i32,
    pub pti_numrunning: i32,
    pub pti_priority: i32,
}

const PROC_PIDTASKINFO: i32 = 4;
const PROC_ALL_PIDS: u32 = 1;
const MAX_HISTORY_SNAPSHOTS: usize = 60;

extern "C" {
    fn proc_listpids(type_: u32, typeinfo: u32, buffer: *mut std::ffi::c_void, buffersize: i32) -> i32;
    fn proc_pidinfo(pid: i32, flavor: i32, arg: u64, buffer: *mut std::ffi::c_void, buffersize: i32) -> i32;
    fn proc_name(pid: i32, buffer: *mut u8, buffersize: u32) -> i32;
    fn signal(sig: std::ffi::c_int, handler: extern "C" fn(std::ffi::c_int)) -> *mut std::ffi::c_void;
}

static RUNNING: AtomicBool = AtomicBool::new(true);

extern "C" fn handle_termination_signal(_: std::ffi::c_int) {
    RUNNING.store(false, Ordering::SeqCst);
}

/// Historical record tracked across ticks for each process.
#[derive(Debug, Clone)]
pub struct ProcessRecord {
    pub pid: i32,
    pub name: String,
    pub total_cpu_ns: u64,
    pub last_seen_tick: u64,
    pub cpu_percent: f32,
    pub resident_bytes: u64,
    pub peak_cpu_percent: f32,
    pub peak_resident_bytes: u64,
    pub sample_count: u32,
}

/// Point-in-time system aggregate snapshot stored in historical buffer.
#[derive(Debug, Clone)]
pub struct SystemSnapshot {
    pub tick: u64,
    pub timestamp: Instant,
    pub total_processes: usize,
    pub total_resident_bytes: u64,
    pub total_cpu_percent: f32,
}

/// Lightweight borrowed row representation for zero-allocation sorting & display.
pub struct ProcessRow<'a> {
    pub pid: i32,
    pub name: &'a str,
    pub cpu_percent: f32,
    pub peak_cpu_percent: f32,
    pub resident_bytes: u64,
    pub peak_resident_bytes: u64,
    pub sample_count: u32,
}

pub struct ProcessMonitor {
    pub records: HashMap<i32, ProcessRecord>,
    pub snapshots: VecDeque<SystemSnapshot>,
    pub current_tick: u64,
    pub start_time: Instant,
    last_sample_instant: Instant,
    pid_buffer: Vec<i32>,
    active_pids: Vec<i32>,
}

impl ProcessMonitor {
    pub fn new() -> Self {
        Self {
            records: HashMap::with_capacity(512),
            snapshots: VecDeque::with_capacity(MAX_HISTORY_SNAPSHOTS),
            current_tick: 0,
            start_time: Instant::now(),
            last_sample_instant: Instant::now(),
            pid_buffer: vec![0i32; 2048],
            active_pids: Vec::with_capacity(1024),
        }
    }

    /// Fetches all active PIDs using proc_listpids, dynamically resizing if needed.
    fn fetch_active_pids(&mut self) -> usize {
        let mut buf_bytes = (self.pid_buffer.len() * std::mem::size_of::<i32>()) as i32;
        let mut bytes_returned = unsafe {
            proc_listpids(
                PROC_ALL_PIDS,
                0,
                self.pid_buffer.as_mut_ptr() as *mut std::ffi::c_void,
                buf_bytes,
            )
        };

        // Grow buffer if the system had more PIDs than allocated
        while bytes_returned as usize >= buf_bytes as usize {
            self.pid_buffer.resize(self.pid_buffer.len() * 2, 0);
            buf_bytes = (self.pid_buffer.len() * std::mem::size_of::<i32>()) as i32;
            bytes_returned = unsafe {
                proc_listpids(
                    PROC_ALL_PIDS,
                    0,
                    self.pid_buffer.as_mut_ptr() as *mut std::ffi::c_void,
                    buf_bytes,
                )
            };
        }

        if bytes_returned <= 0 {
            0
        } else {
            (bytes_returned as usize) / std::mem::size_of::<i32>()
        }
    }

    /// Ticks the monitor: updates all active process metrics, tracks historical peaks,
    /// stores a system snapshot, and cleans up dead processes in O(1) per process.
    pub fn tick(&mut self) {
        let now = Instant::now();
        let elapsed_secs = (now - self.last_sample_instant).as_secs_f32();
        self.last_sample_instant = now;
        self.current_tick += 1;

        let num_pids = self.fetch_active_pids();
        self.active_pids.clear();
        self.active_pids.extend_from_slice(&self.pid_buffer[..num_pids]);

        let info_size = std::mem::size_of::<ProcTaskInfo>() as i32;
        let mut total_system_resident_bytes: u64 = 0;
        let mut total_system_cpu: f32 = 0.0;
        let current_tick = self.current_tick;

        for &pid in &self.active_pids {
            if pid <= 0 {
                continue;
            }

            unsafe {
                let mut info = MaybeUninit::<ProcTaskInfo>::uninit();
                let ret = proc_pidinfo(
                    pid,
                    PROC_PIDTASKINFO,
                    0,
                    info.as_mut_ptr() as *mut std::ffi::c_void,
                    info_size,
                );

                if ret != info_size {
                    continue; // Exited or protected system daemon
                }

                let info = info.assume_init();
                let total_cpu_ns = info.pti_total_user + info.pti_total_system;
                let resident_bytes = info.pti_resident_size;
                total_system_resident_bytes += resident_bytes;

                if let Some(record) = self.records.get_mut(&pid) {
                    let cpu_delta_ns = total_cpu_ns.saturating_sub(record.total_cpu_ns);
                    let cpu_percent = if elapsed_secs > 0.0 {
                        ((cpu_delta_ns as f32 / 1_000_000_000.0) / elapsed_secs) * 100.0
                    } else {
                        0.0
                    };

                    record.total_cpu_ns = total_cpu_ns;
                    record.cpu_percent = cpu_percent;
                    record.resident_bytes = resident_bytes;
                    record.peak_cpu_percent = record.peak_cpu_percent.max(cpu_percent);
                    record.peak_resident_bytes = record.peak_resident_bytes.max(resident_bytes);
                    record.sample_count += 1;
                    record.last_seen_tick = current_tick;

                    total_system_cpu += cpu_percent;
                } else {
                    // Resolve process name only once when first discovered
                    let mut name_buf = [0u8; 64];
                    let len = proc_name(pid, name_buf.as_mut_ptr(), name_buf.len() as u32);
                    let name = if len > 0 {
                        let valid_len = name_buf.iter().position(|&b| b == 0).unwrap_or(len as usize);
                        String::from_utf8_lossy(&name_buf[..valid_len]).into_owned()
                    } else {
                        format!("[pid {}]", pid)
                    };

                    self.records.insert(
                        pid,
                        ProcessRecord {
                            pid,
                            name,
                            total_cpu_ns,
                            last_seen_tick: current_tick,
                            cpu_percent: 0.0,
                            resident_bytes,
                            peak_cpu_percent: 0.0,
                            peak_resident_bytes: resident_bytes,
                            sample_count: 1,
                        },
                    );
                }
            }
        }

        // Clean up terminated processes without O(N^2) searches
        self.records.retain(|_, record| record.last_seen_tick == current_tick);

        // Store aggregated historical snapshot (retaining last MAX_HISTORY_SNAPSHOTS)
        if self.snapshots.len() >= MAX_HISTORY_SNAPSHOTS {
            self.snapshots.pop_front();
        }
        self.snapshots.push_back(SystemSnapshot {
            tick: current_tick,
            timestamp: now,
            total_processes: self.records.len(),
            total_resident_bytes: total_system_resident_bytes,
            total_cpu_percent: total_system_cpu,
        });
    }

    /// Returns top K processes by CPU percentage without full O(N log N) sorting.
    pub fn top_by_cpu<'a>(&'a self, rows: &mut Vec<ProcessRow<'a>>, k: usize) {
        rows.clear();
        rows.reserve(self.records.len());

        for record in self.records.values() {
            rows.push(ProcessRow {
                pid: record.pid,
                name: &record.name,
                cpu_percent: record.cpu_percent,
                peak_cpu_percent: record.peak_cpu_percent,
                resident_bytes: record.resident_bytes,
                peak_resident_bytes: record.peak_resident_bytes,
                sample_count: record.sample_count,
            });
        }

        if rows.len() > k {
            // O(N) selection of top K elements
            rows.select_nth_unstable_by(k, |a, b| {
                b.cpu_percent
                    .partial_cmp(&a.cpu_percent)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            rows.truncate(k);
        }

        // Sort only the top K elements
        rows.sort_by(|a, b| {
            b.cpu_percent
                .partial_cmp(&a.cpu_percent)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    }
}

impl Default for ProcessMonitor {
    fn default() -> Self {
        Self::new()
    }
}

/// RAII Terminal Manager for flicker-free alternate screen rendering and clean exit.
pub struct TerminalView {
    is_tty: bool,
    buffer: String,
    prev_frame: String,
}

impl TerminalView {
    pub fn new() -> Self {
        let is_tty = stdout().is_terminal();
        if is_tty {
            // Switch to alternate screen buffer, clear screen, and hide cursor
            print!("\x1b[?1049h\x1b[2J\x1b[?25l");
            let _ = stdout().flush();
        }
        Self {
            is_tty,
            buffer: String::with_capacity(4096),
            prev_frame: String::with_capacity(4096),
        }
    }

    /// Renders the dashboard in-place using double buffering to avoid excessive repaints and terminal scrolling.
    pub fn render(&mut self, monitor: &ProcessMonitor, top_rows: &[ProcessRow]) {
        self.buffer.clear();

        if self.is_tty {
            // Cursor Home: overwrite from (1, 1) without scrolling terminal
            self.buffer.push_str("\x1b[H");
        }

        let uptime_secs = monitor.start_time.elapsed().as_secs();
        let hours = uptime_secs / 3600;
        let mins = (uptime_secs % 3600) / 60;
        let secs = uptime_secs % 60;

        let latest_snapshot = monitor.snapshots.back();
        let total_ram_mb = latest_snapshot
            .map(|s| s.total_resident_bytes as f64 / (1024.0 * 1024.0))
            .unwrap_or(0.0);
        let total_cpu = latest_snapshot.map(|s| s.total_cpu_percent).unwrap_or(0.0);

        // Header & Summary
        let _ = writeln!(
            self.buffer,
            "┌────────────────────────────────────────────────────────────────────────────────────────┐\x1b[K"
        );
        let _ = writeln!(
            self.buffer,
            "│ \x1b[1;36mPROCMON\x1b[0m — macOS Process Monitor         Tick: #{:<6} Active PIDs: {:<6} Uptime: {:02}:{:02}:{:02} │\x1b[K",
            monitor.current_tick,
            monitor.records.len(),
            hours,
            mins,
            secs
        );
        let _ = writeln!(
            self.buffer,
            "│ Total Monitored RAM: {:>8.1} MB         Total System Process CPU: {:>6.1}%              │\x1b[K",
            total_ram_mb,
            total_cpu
        );
        let _ = writeln!(
            self.buffer,
            "├────────┬──────────────────────────┬──────────┬──────────┬───────────┬───────────┬──────┤\x1b[K"
        );
        let _ = writeln!(
            self.buffer,
            "│ {:<6} │ {:<24} │ {:>8} │ {:>8} │ {:>9} │ {:>9} │ {:>4} │\x1b[K",
            "PID", "NAME", "CPU %", "PEAK CPU", "RAM (MB)", "PEAK RAM", "TICKS"
        );
        let _ = writeln!(
            self.buffer,
            "├────────┼──────────────────────────┼──────────┼──────────┼───────────┼───────────┼──────┤\x1b[K"
        );

        // Process rows
        for row in top_rows {
            let truncated_name = if row.name.chars().count() > 24 {
                let s: String = row.name.chars().take(21).collect();
                format!("{}...", s)
            } else {
                row.name.to_string()
            };

            let ram_mb = row.resident_bytes as f64 / (1024.0 * 1024.0);
            let peak_ram_mb = row.peak_resident_bytes as f64 / (1024.0 * 1024.0);

            let _ = writeln!(
                self.buffer,
                "│ {:<6} │ {:<24} │ {:>7.1}% │ {:>7.1}% │ {:>9.1} │ {:>9.1} │ {:>4} │\x1b[K",
                row.pid,
                truncated_name,
                row.cpu_percent,
                row.peak_cpu_percent,
                ram_mb,
                peak_ram_mb,
                row.sample_count
            );
        }

        let _ = writeln!(
            self.buffer,
            "└────────┴──────────────────────────┴──────────┴──────────┴───────────┴───────────┴──────┘\x1b[K"
        );
        let _ = write!(
            self.buffer,
            " [Ctrl+C to quit]  Tracking {} historical snapshots\x1b[K\n\x1b[J",
            monitor.snapshots.len()
        );

        // Frame dirty-check: avoid excessive terminal repaints if identical
        if self.is_tty && self.buffer == self.prev_frame {
            return;
        }

        // Single atomic flush to stdout
        let mut out = stdout().lock();
        let _ = out.write_all(self.buffer.as_bytes());
        let _ = out.flush();

        if self.is_tty {
            self.prev_frame.clone_from(&self.buffer);
        }
    }
}

impl Drop for TerminalView {
    fn drop(&mut self) {
        if self.is_tty {
            // Restore normal screen buffer and make cursor visible again
            print!("\x1b[?25h\x1b[?1049l");
            let _ = stdout().flush();
        }
    }
}

impl Default for TerminalView {
    fn default() -> Self {
        Self::new()
    }
}

/// Command-line configuration for procmon.
#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub interval: Duration,
    pub top_n: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(1),
            top_n: 15,
        }
    }
}

impl Config {
    pub fn parse_from<I, T>(args: I) -> Result<Option<Self>, String>
    where
        I: IntoIterator<Item = T>,
        T: AsRef<str>,
    {
        let mut interval_secs: f64 = 1.0;
        let mut top_n: usize = 15;
        let mut iter = args.into_iter();

        while let Some(arg) = iter.next() {
            let s = arg.as_ref();
            match s {
                "-i" | "--interval" => {
                    let val = iter
                        .next()
                        .ok_or_else(|| "Missing value for --interval / -i".to_string())?;
                    interval_secs = val
                        .as_ref()
                        .parse::<f64>()
                        .map_err(|_| format!("Invalid interval number: '{}'", val.as_ref()))?;
                    if interval_secs <= 0.05 {
                        return Err("Interval must be at least 0.05 seconds".to_string());
                    }
                }
                "-n" | "--top" => {
                    let val = iter
                        .next()
                        .ok_or_else(|| "Missing value for --top / -n".to_string())?;
                    top_n = val
                        .as_ref()
                        .parse::<usize>()
                        .map_err(|_| format!("Invalid top process count: '{}'", val.as_ref()))?;
                    if top_n == 0 {
                        return Err("Top process count must be greater than 0".to_string());
                    }
                }
                "-h" | "--help" => {
                    println!("PROCMON — macOS Real-Time Process Monitor\n");
                    println!("Usage: procmon [OPTIONS]\n");
                    println!("Options:");
                    println!("  -i, --interval <SECS>   Update interval in seconds (default: 1.0, e.g. 2.0)");
                    println!("  -n, --top <COUNT>       Number of top CPU processes to display (default: 15)");
                    println!("  -h, --help              Show this help message and exit");
                    return Ok(None);
                }
                unknown => {
                    return Err(format!("Unknown option: '{}'. Run with --help for options.", unknown));
                }
            }
        }

        Ok(Some(Self {
            interval: Duration::from_secs_f64(interval_secs),
            top_n,
        }))
    }
}

fn main() {
    let config = match Config::parse_from(std::env::args().skip(1)) {
        Ok(Some(cfg)) => cfg,
        Ok(None) => return,
        Err(err) => {
            eprintln!("Error: {}", err);
            std::process::exit(1);
        }
    };

    // Install SIGINT and SIGTERM handlers for clean exit
    unsafe {
        signal(2, handle_termination_signal);  // SIGINT (Ctrl+C)
        signal(15, handle_termination_signal); // SIGTERM
    }

    let mut monitor = ProcessMonitor::new();
    let mut view = TerminalView::new();

    // Warm-up tick to establish baseline CPU delta
    monitor.tick();

    while RUNNING.load(Ordering::Relaxed) {
        sleep(config.interval);
        if !RUNNING.load(Ordering::Relaxed) {
            break;
        }

        monitor.tick();
        let mut top_rows = Vec::with_capacity(config.top_n);
        monitor.top_by_cpu(&mut top_rows, config.top_n);
        view.render(&monitor, &top_rows);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_monitor_initialization() {
        let monitor = ProcessMonitor::new();
        assert_eq!(monitor.current_tick, 0);
        assert!(monitor.records.is_empty());
        assert!(monitor.snapshots.is_empty());
    }

    #[test]
    fn test_monitor_tick_and_top() {
        let mut monitor = ProcessMonitor::new();
        monitor.tick();
        assert_eq!(monitor.current_tick, 1);
        assert!(!monitor.records.is_empty());
        assert_eq!(monitor.snapshots.len(), 1);

        let mut rows = Vec::new();
        monitor.top_by_cpu(&mut rows, 10);
        assert!(rows.len() <= 10);

        // Verify rows are sorted descending by CPU percent
        for i in 1..rows.len() {
            assert!(rows[i - 1].cpu_percent >= rows[i].cpu_percent);
        }
    }

    #[test]
    fn test_config_parsing() {
        let default_cfg = Config::parse_from(&[] as &[&str]).unwrap().unwrap();
        assert_eq!(default_cfg.interval, Duration::from_secs(1));
        assert_eq!(default_cfg.top_n, 15);

        let custom_cfg = Config::parse_from(&["-i", "2.5", "-n", "20"]).unwrap().unwrap();
        assert_eq!(custom_cfg.interval, Duration::from_millis(2500));
        assert_eq!(custom_cfg.top_n, 20);

        let help = Config::parse_from(&["--help"]).unwrap();
        assert!(help.is_none());
    }

    #[test]
    fn test_config_validation() {
        assert!(Config::parse_from(&["-i", "-1"]).is_err());
        assert!(Config::parse_from(&["-n", "0"]).is_err());
        assert!(Config::parse_from(&["--invalid-flag"]).is_err());
    }
}