use std::cmp::Ordering;
use std::collections::{HashMap, VecDeque};
use std::fmt::Write as FmtWrite;
use std::fs::{File, OpenOptions};
use std::io::{stdin, stdout, IsTerminal, Write};
use std::mem::MaybeUninit;
use std::path::{Path, PathBuf};
use std::ptr::null_mut;
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
use std::thread::sleep;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

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

/// Darwin `struct termios` (64-bit). Layout checked against the system headers.
#[repr(C)]
#[derive(Clone, Copy)]
struct Termios {
    c_iflag: u64,
    c_oflag: u64,
    c_cflag: u64,
    c_lflag: u64,
    c_cc: [u8; 20],
    _pad: [u8; 4],
    c_ispeed: u64,
    c_ospeed: u64,
}

/// Darwin `struct tm`.
#[repr(C)]
struct Tm {
    tm_sec: i32,
    tm_min: i32,
    tm_hour: i32,
    tm_mday: i32,
    tm_mon: i32,
    tm_year: i32,
    tm_wday: i32,
    tm_yday: i32,
    tm_isdst: i32,
    _pad: i32,
    tm_gmtoff: i64,
    tm_zone: *const std::ffi::c_char,
}

#[repr(C)]
struct PollFd {
    fd: i32,
    events: i16,
    revents: i16,
}

/// Darwin `struct sigaction` without `SA_SIGINFO`. 16 bytes on 64-bit macOS.
#[repr(C)]
struct SigAction {
    sa_handler: *const std::ffi::c_void,
    sa_mask: u32,
    sa_flags: i32,
}

const _: () = assert!(std::mem::size_of::<Termios>() == 72);
const _: () = assert!(std::mem::offset_of!(Termios, c_ispeed) == 56);
const _: () = assert!(std::mem::size_of::<Tm>() == 56);
const _: () = assert!(std::mem::offset_of!(Tm, tm_gmtoff) == 40);
const _: () = assert!(std::mem::size_of::<PollFd>() == 8);
const _: () = assert!(std::mem::size_of::<SigAction>() == 16);

const PROC_PIDTASKINFO: i32 = 4;
const PROC_PIDTBSDINFO: i32 = 3;
const PROC_ALL_PIDS: u32 = 1;
/// `proc_bsdinfo.pbi_start_tvsec` / `pbi_start_tvusec` on current macOS.
const BSD_INFO_SIZE: usize = 136;
const BSD_START_SEC: usize = 120;
const BSD_START_USEC: usize = 128;

const MAX_HISTORY_SNAPSHOTS: usize = 60;
const NAME_COL_WIDTH: usize = 24;
const BOX_WIDTH: usize = 90;
const DEFAULT_SPIKE_CPU: f32 = 25.0;
const SPIKE_REARM_RATIO: f32 = 0.5;
/// Ignore sub-50ms CPU blips when a process lives for only one sample.
const MIN_BURST_CPU_SECS: f32 = 0.05;
const MIN_BURST_CPU_NS: u64 = 50_000_000;
const LOG_FLUSH_BYTES: usize = 8 * 1024;
const LOG_BUFFER_MAX: usize = 64 * 1024;
const DEFAULT_LOG_FLUSH_SECS: u64 = 30;

const POLLIN: i16 = 0x1;
const POLLERR: i16 = 0x8;
const POLLHUP: i16 = 0x10;
const POLLNVAL: i16 = 0x20;
const TCSANOW: i32 = 0;
const TCSAFLUSH: i32 = 2;
const ICANON: u64 = 0x100;
const ECHO: u64 = 0x8;
const VMIN: usize = 16;
const VTIME: usize = 17;
const STDIN_FILENO: i32 = 0;
const SIGTSTP: std::ffi::c_int = 18;

const KEY_HELP: &str =
    " [c] CPU  [m] RAM  [C] peak CPU  [M] peak RAM  [p] PID  [n] name  [t] ticks  [q] quit";
const FLAG_HELP: &str =
    " no hotkeys (not a tty). --sort cpu|ram|peak-cpu|peak-ram|pid|name|ticks  Ctrl-C quits";

const _: () = assert!(KEY_HELP.len() <= BOX_WIDTH);
const _: () = assert!(FLAG_HELP.len() <= BOX_WIDTH);

extern "C" {
    fn proc_listpids(type_: u32, typeinfo: u32, buffer: *mut std::ffi::c_void, buffersize: i32) -> i32;
    fn proc_pidinfo(
        pid: i32,
        flavor: i32,
        arg: u64,
        buffer: *mut std::ffi::c_void,
        buffersize: i32,
    ) -> i32;
    fn proc_name(pid: i32, buffer: *mut u8, buffersize: u32) -> i32;
    fn sigaction(
        sig: std::ffi::c_int,
        act: *const SigAction,
        old: *mut SigAction,
    ) -> std::ffi::c_int;
    fn poll(fds: *mut PollFd, nfds: u32, timeout: i32) -> i32;
    #[link_name = "read"]
    fn libc_read(fd: i32, buf: *mut std::ffi::c_void, count: usize) -> isize;
    fn tcgetattr(fd: i32, termios: *mut Termios) -> i32;
    fn tcsetattr(fd: i32, action: i32, termios: *const Termios) -> i32;
    #[link_name = "time"]
    fn unix_time(tloc: *mut i64) -> i64;
    fn localtime_r(clock: *const i64, result: *mut Tm) -> *mut Tm;
    fn raise(sig: std::ffi::c_int) -> std::ffi::c_int;
}

static RUNNING: AtomicBool = AtomicBool::new(true);
static STOP_REQUESTED: AtomicBool = AtomicBool::new(false);

extern "C" fn handle_termination_signal(_: std::ffi::c_int) {
    RUNNING.store(false, AtomicOrdering::Release);
}

extern "C" fn handle_stop_signal(_: std::ffi::c_int) {
    STOP_REQUESTED.store(true, AtomicOrdering::Release);
}

/// Install a handler that interrupts `poll` instead of restarting it.
fn install_handler(sig: std::ffi::c_int, handler: extern "C" fn(std::ffi::c_int)) {
    let action = SigAction {
        sa_handler: handler as *const std::ffi::c_void,
        sa_mask: 0,
        sa_flags: 0,
    };
    unsafe {
        let _ = sigaction(sig, &action, null_mut());
    }
}

/// Column used to order the on-screen table. Highest value is first, except PID and name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortKey {
    Cpu,
    Ram,
    PeakCpu,
    PeakRam,
    Pid,
    Name,
    Ticks,
}

impl SortKey {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.to_ascii_lowercase().as_str() {
            "cpu" => Some(Self::Cpu),
            "ram" | "mem" | "memory" | "rss" => Some(Self::Ram),
            "peak-cpu" | "peakcpu" | "pcpu" => Some(Self::PeakCpu),
            "peak-ram" | "peak-mem" | "peakram" | "pram" | "peak-memory" => Some(Self::PeakRam),
            "pid" => Some(Self::Pid),
            "name" => Some(Self::Name),
            "ticks" | "time" => Some(Self::Ticks),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Cpu => "CPU%",
            Self::Ram => "RAM",
            Self::PeakCpu => "PEAK CPU",
            Self::PeakRam => "PEAK RAM",
            Self::Pid => "PID",
            Self::Name => "NAME",
            Self::Ticks => "TICKS",
        }
    }

    pub fn order_hint(self) -> &'static str {
        match self {
            Self::Pid => "lowest first",
            Self::Name => "A-Z",
            _ => "highest first",
        }
    }

    fn cmp_rows(self, a: &ProcessRow, b: &ProcessRow) -> Ordering {
        let primary = match self {
            Self::Cpu => cmp_desc_f32(a.cpu_percent, b.cpu_percent),
            Self::Ram => cmp_desc_u64(a.resident_bytes, b.resident_bytes),
            Self::PeakCpu => cmp_desc_f32(a.peak_cpu_percent, b.peak_cpu_percent),
            Self::PeakRam => cmp_desc_u64(a.peak_resident_bytes, b.peak_resident_bytes),
            Self::Ticks => b.sample_count.cmp(&a.sample_count),
            Self::Pid => a.pid.cmp(&b.pid),
            Self::Name => Ordering::Equal,
        };
        primary.then_with(|| a.pid.cmp(&b.pid))
    }
}

fn cmp_desc_f32(a: f32, b: f32) -> Ordering {
    b.partial_cmp(&a).unwrap_or(Ordering::Equal)
}

fn cmp_desc_u64(a: u64, b: u64) -> Ordering {
    b.cmp(&a)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyAction {
    Sort(SortKey),
    Quit,
}

fn key_action(byte: u8) -> Option<KeyAction> {
    match byte {
        0x03 | b'q' | b'Q' => Some(KeyAction::Quit),
        b'c' => Some(KeyAction::Sort(SortKey::Cpu)),
        b'm' | b'r' => Some(KeyAction::Sort(SortKey::Ram)),
        b'C' => Some(KeyAction::Sort(SortKey::PeakCpu)),
        b'M' | b'R' => Some(KeyAction::Sort(SortKey::PeakRam)),
        b'p' | b'P' => Some(KeyAction::Sort(SortKey::Pid)),
        b'n' | b'N' => Some(KeyAction::Sort(SortKey::Name)),
        b't' | b'T' => Some(KeyAction::Sort(SortKey::Ticks)),
        _ => None,
    }
}

/// Last sort key wins. Quit wins over a sort key in the same burst.
fn fold_keys(bytes: &[u8]) -> Option<KeyAction> {
    let mut quit = false;
    let mut sort = None;
    for &byte in bytes {
        match key_action(byte) {
            Some(KeyAction::Quit) => quit = true,
            Some(KeyAction::Sort(key)) => sort = Some(key),
            None => {}
        }
    }
    if quit {
        Some(KeyAction::Quit)
    } else {
        sort.map(KeyAction::Sort)
    }
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
    /// CPU% over the process lifetime, from the first sample. Zero when unknown.
    pub lifetime_cpu_percent: f32,
    pub seen_age_secs: f32,
    /// Ready to record the next upward crossing of the spike threshold.
    pub spike_armed: bool,
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

/// Numeric row used for sorting and display. Names stay in the process table.
#[derive(Clone, Copy)]
pub struct ProcessRow {
    pub pid: i32,
    pub cpu_percent: f32,
    pub peak_cpu_percent: f32,
    pub resident_bytes: u64,
    pub peak_resident_bytes: u64,
    pub sample_count: u32,
}

enum ExitKind {
    Ignore,
    Gone,
    Burst,
}

/// `emit` is true on an upward crossing. The bool is the new armed state.
fn spike_transition(armed: bool, cpu_percent: f32, threshold: f32) -> (bool, bool) {
    let rearm_below = threshold * SPIKE_REARM_RATIO;
    if cpu_percent >= threshold {
        if armed {
            (true, false)
        } else {
            (false, false)
        }
    } else if cpu_percent < rearm_below {
        (false, true)
    } else {
        (false, armed)
    }
}

fn is_short_burst(rec: &ProcessRecord, threshold: f32, interval: Duration) -> bool {
    if rec.sample_count != 1 {
        return false;
    }
    let cpu_time_s = rec.total_cpu_ns as f32 / 1_000_000_000.0;
    if cpu_time_s < MIN_BURST_CPU_SECS || rec.lifetime_cpu_percent < threshold {
        return false;
    }
    let max_age = (interval.as_secs_f32() * 3.0).max(1.0);
    rec.seen_age_secs >= 0.001 && rec.seen_age_secs <= max_age
}

fn exit_kind(rec: &ProcessRecord, threshold: f32, interval: Duration) -> ExitKind {
    if rec.peak_cpu_percent >= threshold {
        ExitKind::Gone
    } else if is_short_burst(rec, threshold, interval) {
        ExitKind::Burst
    } else {
        ExitKind::Ignore
    }
}

fn mb(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

/// Start time comes from `proc_bsdinfo`. Called only the first time a PID is seen,
/// and only when that process has already burned enough CPU to be a one-sample burst.
fn fetch_lifetime(pid: i32, total_cpu_ns: u64) -> Option<(f32, f32)> {
    let mut buf = [0u8; BSD_INFO_SIZE];
    let ret = unsafe {
        proc_pidinfo(
            pid,
            PROC_PIDTBSDINFO,
            0,
            buf.as_mut_ptr() as *mut std::ffi::c_void,
            BSD_INFO_SIZE as i32,
        )
    };
    if ret < BSD_INFO_SIZE as i32 {
        return None;
    }
    let sec = u64::from_ne_bytes(buf[BSD_START_SEC..BSD_START_SEC + 8].try_into().ok()?);
    let usec = u64::from_ne_bytes(buf[BSD_START_USEC..BSD_START_USEC + 8].try_into().ok()?);
    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?;
    let start = Duration::from_secs(sec).checked_add(Duration::from_micros(usec))?;
    let age = now.checked_sub(start)?;
    let age_secs = age.as_secs_f64();
    if age_secs < 0.001 {
        return None;
    }
    let pct = (total_cpu_ns as f64 / 1_000_000_000.0) / age_secs * 100.0;
    if !pct.is_finite() {
        return None;
    }
    Some((pct as f32, age_secs as f32))
}

fn format_now(buf: &mut Vec<u8>) {
    let mut secs: i64 = 0;
    let now = unsafe { unix_time(&mut secs) };
    let stamp = if now >= 0 { now } else { secs };
    unsafe {
        let mut tm = MaybeUninit::<Tm>::zeroed();
        if localtime_r(&stamp, tm.as_mut_ptr()).is_null() {
            let _ = write!(buf, "1970-01-01T00:00:00+00:00");
            return;
        }
        let tm = tm.assume_init();
        let off = tm.tm_gmtoff;
        let sign = if off < 0 { '-' } else { '+' };
        let abs = off.unsigned_abs();
        let hh = abs / 3600;
        let mm = (abs % 3600) / 60;
        let _ = write!(
            buf,
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}{}{:02}:{:02}",
            tm.tm_year + 1900,
            tm.tm_mon + 1,
            tm.tm_mday,
            tm.tm_hour,
            tm.tm_min,
            tm.tm_sec,
            sign,
            hh,
            mm,
        );
    }
}

fn push_quoted_name(buf: &mut Vec<u8>, name: &str) {
    buf.push(b'"');
    for ch in name.chars() {
        match ch {
            '"' => buf.extend_from_slice(br#"\""#),
            '\\' => buf.extend_from_slice(br#"\\"#),
            c if (c as u32) < 0x20 => buf.push(b'?'),
            c => {
                let mut tmp = [0u8; 4];
                buf.extend_from_slice(c.encode_utf8(&mut tmp).as_bytes());
            }
        }
    }
    buf.push(b'"');
}

/// Append-only spike log. Events sit in `buf` and reach disk in batches.
struct SpikeLog {
    enabled: bool,
    path: PathBuf,
    path_display: String,
    file: Option<File>,
    buf: Vec<u8>,
    pending_events: u32,
    written_events: u64,
    dropped_events: u64,
    oldest: Option<Instant>,
    flush_every: Duration,
    flush_bytes: usize,
    header_written: bool,
    error: bool,
    spike_cpu: f32,
    interval_secs: f32,
}

impl SpikeLog {
    fn from_config(cfg: &Config) -> Self {
        Self {
            enabled: cfg.log_enabled,
            path: cfg.log_path.clone(),
            path_display: display_log_path(&cfg.log_path),
            file: None,
            buf: Vec::with_capacity(LOG_FLUSH_BYTES),
            pending_events: 0,
            written_events: 0,
            dropped_events: 0,
            oldest: None,
            flush_every: cfg.log_flush,
            flush_bytes: LOG_FLUSH_BYTES,
            header_written: false,
            error: false,
            spike_cpu: cfg.spike_cpu,
            interval_secs: cfg.interval.as_secs_f32(),
        }
    }

    #[cfg(test)]
    fn disabled() -> Self {
        let mut cfg = Config::default();
        cfg.log_enabled = false;
        Self::from_config(&cfg)
    }

    #[cfg(test)]
    fn for_test(path: PathBuf, flush_every: Duration, flush_bytes: usize) -> Self {
        let mut log = Self::from_config(&Config {
            log_enabled: true,
            log_path: path,
            log_flush: flush_every,
            ..Config::default()
        });
        log.flush_bytes = flush_bytes.max(1);
        log.flush_every = flush_every;
        log
    }

    fn begin_line(&mut self, kind: &str) -> bool {
        if !self.enabled {
            return false;
        }
        self.ensure_room();
        if self.buf.is_empty() {
            self.oldest = Some(Instant::now());
        }
        format_now(&mut self.buf);
        let _ = write!(self.buf, " {kind} ");
        true
    }

    fn end_line(&mut self) {
        self.buf.push(b'\n');
        self.pending_events = self.pending_events.saturating_add(1);
    }

    fn log_spike(&mut self, rec: &ProcessRecord) {
        if !self.begin_line("SPIKE") {
            return;
        }
        let _ = write!(
            self.buf,
            "pid={} cpu={:.1} peak_cpu={:.1} ram_mb={:.1} peak_ram_mb={:.1} ticks={} name=",
            rec.pid,
            rec.cpu_percent,
            rec.peak_cpu_percent,
            mb(rec.resident_bytes),
            mb(rec.peak_resident_bytes),
            rec.sample_count,
        );
        push_quoted_name(&mut self.buf, &rec.name);
        self.end_line();
    }

    fn log_gone(&mut self, rec: &ProcessRecord) {
        if !self.begin_line("GONE") {
            return;
        }
        let tracked = rec.sample_count.saturating_sub(1) as f32 * self.interval_secs;
        let _ = write!(
            self.buf,
            "pid={} last_cpu={:.1} peak_cpu={:.1} ram_mb={:.1} peak_ram_mb={:.1} ticks={} tracked_s={:.1} name=",
            rec.pid,
            rec.cpu_percent,
            rec.peak_cpu_percent,
            mb(rec.resident_bytes),
            mb(rec.peak_resident_bytes),
            rec.sample_count,
            tracked,
        );
        push_quoted_name(&mut self.buf, &rec.name);
        self.end_line();
    }

    fn log_burst(&mut self, rec: &ProcessRecord) {
        if !self.begin_line("BURST") {
            return;
        }
        let cpu_time = rec.total_cpu_ns as f64 / 1_000_000_000.0;
        let _ = write!(
            self.buf,
            "pid={} lifetime_cpu={:.1} age_s={:.3} cpu_time_s={:.3} ram_mb={:.1} ticks={} name=",
            rec.pid,
            rec.lifetime_cpu_percent,
            rec.seen_age_secs,
            cpu_time,
            mb(rec.resident_bytes),
            rec.sample_count,
        );
        push_quoted_name(&mut self.buf, &rec.name);
        self.end_line();
    }

    fn ensure_room(&mut self) {
        if self.buf.len() < LOG_BUFFER_MAX {
            return;
        }
        if self.flush() && self.buf.len() < LOG_BUFFER_MAX {
            return;
        }
        self.dropped_events = self.dropped_events.saturating_add(self.pending_events as u64);
        self.buf.clear();
        self.pending_events = 0;
        self.oldest = None;
    }

    fn build_header(&self) -> Vec<u8> {
        let mut header = Vec::with_capacity(160);
        header.extend_from_slice(b"# ");
        format_now(&mut header);
        let _ = writeln!(
            header,
            " procmon start threshold={:.1}% interval={:.2}s flush={:.2}s",
            self.spike_cpu,
            self.interval_secs,
            self.flush_every.as_secs_f32(),
        );
        header
    }

    fn open_file(&mut self) -> bool {
        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() && std::fs::create_dir_all(parent).is_err() {
                self.error = true;
                return false;
            }
        }
        match OpenOptions::new().create(true).append(true).open(&self.path) {
            Ok(file) => {
                self.file = Some(file);
                true
            }
            Err(_) => {
                self.error = true;
                false
            }
        }
    }

    /// One `write` of the queued bytes. No fsync — the kernel can coalesce the pages.
    fn flush(&mut self) -> bool {
        if self.buf.is_empty() {
            return true;
        }
        if self.file.is_none() && !self.open_file() {
            return false;
        }
        let header = if self.header_written {
            None
        } else {
            Some(self.build_header())
        };
        let payload = std::mem::take(&mut self.buf);
        let pending = self.pending_events;
        let write_result = match self.file.as_mut() {
            Some(file) => {
                let header_ok = match header.as_ref() {
                    Some(bytes) => file.write_all(bytes).is_ok(),
                    None => true,
                };
                header_ok && file.write_all(&payload).is_ok()
            }
            None => false,
        };
        if write_result {
            self.pending_events = 0;
            self.written_events = self.written_events.saturating_add(pending as u64);
            self.oldest = None;
            self.header_written = true;
            self.error = false;
            true
        } else {
            self.buf = payload;
            self.error = true;
            false
        }
    }

    fn maybe_flush(&mut self, force: bool) {
        if !self.enabled || self.buf.is_empty() {
            return;
        }
        let aged = self
            .oldest
            .map(|t| t.elapsed() >= self.flush_every)
            .unwrap_or(false);
        let big = self.buf.len() >= self.flush_bytes;
        if force || aged || big {
            let _ = self.flush();
        }
    }
}

impl Drop for SpikeLog {
    fn drop(&mut self) {
        if self.enabled {
            let _ = self.flush();
        }
    }
}

fn display_log_path(path: &Path) -> String {
    let text = path.to_string_lossy();
    let shown = if let Some(home) = std::env::var_os("HOME") {
        let home = home.to_string_lossy();
        if let Some(rest) = text.strip_prefix(home.as_ref()) {
            format!("~/{}", rest.trim_start_matches('/'))
        } else {
            text.into_owned()
        }
    } else {
        text.into_owned()
    };
    clip_chars(&shown, 48)
}

fn clip_chars(text: &str, max: usize) -> String {
    let count = text.chars().count();
    if count <= max {
        return text.to_string();
    }
    if max < 4 {
        return text.chars().take(max).collect();
    }
    let tail = max / 3;
    let head = max - tail - 3;
    let head: String = text.chars().take(head).collect();
    let skip = count - tail;
    let tail: String = text.chars().skip(skip).collect();
    format!("{head}...{tail}")
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

    /// Drop time spent stopped (Ctrl-Z) so the next CPU delta is not diluted.
    fn rebase_clock(&mut self) {
        self.last_sample_instant = Instant::now();
    }

    /// Ticks the monitor: updates all active process metrics, tracks historical peaks,
    /// stores a system snapshot, and drops processes that disappeared.
    fn tick(&mut self, log: &mut SpikeLog, spike_cpu: f32, interval: Duration) {
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
        let logging = log.enabled;

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
                    continue;
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

                    if logging {
                        let (emit, armed) = spike_transition(record.spike_armed, cpu_percent, spike_cpu);
                        record.spike_armed = armed;
                        if emit {
                            log.log_spike(record);
                        }
                    }
                } else {
                    let mut name_buf = [0u8; 64];
                    let len = proc_name(pid, name_buf.as_mut_ptr(), name_buf.len() as u32);
                    let name = if len > 0 {
                        let valid_len = name_buf.iter().position(|&b| b == 0).unwrap_or(len as usize);
                        String::from_utf8_lossy(&name_buf[..valid_len]).into_owned()
                    } else {
                        format!("[pid {pid}]")
                    };

                    let (lifetime_cpu_percent, seen_age_secs) =
                        if logging && total_cpu_ns >= MIN_BURST_CPU_NS {
                            fetch_lifetime(pid, total_cpu_ns).unwrap_or((0.0, 0.0))
                        } else {
                            (0.0, 0.0)
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
                            lifetime_cpu_percent,
                            seen_age_secs,
                            spike_armed: true,
                        },
                    );
                }
            }
        }

        if logging {
            self.records.retain(|_, record| {
                if record.last_seen_tick == current_tick {
                    return true;
                }
                match exit_kind(record, spike_cpu, interval) {
                    ExitKind::Gone => log.log_gone(record),
                    ExitKind::Burst => log.log_burst(record),
                    ExitKind::Ignore => {}
                }
                false
            });
        } else {
            self.records.retain(|_, record| record.last_seen_tick == current_tick);
        }

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

    fn cmp_rows(&self, sort: SortKey, a: &ProcessRow, b: &ProcessRow) -> Ordering {
        let primary = if sort == SortKey::Name {
            self.name_of(a.pid).cmp(self.name_of(b.pid))
        } else {
            sort.cmp_rows(a, b)
        };
        primary.then_with(|| a.pid.cmp(&b.pid))
    }

    fn name_of(&self, pid: i32) -> &str {
        self.records.get(&pid).map(|rec| rec.name.as_str()).unwrap_or("")
    }

    /// Fills `rows` with the top `k` processes for `sort`.
    /// Quickselect keeps this linear in the process count; only the visible slice is fully sorted.
    pub fn top(&self, rows: &mut Vec<ProcessRow>, k: usize, sort: SortKey) {
        rows.clear();
        if k == 0 {
            return;
        }
        rows.reserve(self.records.len());

        for record in self.records.values() {
            rows.push(ProcessRow {
                pid: record.pid,
                cpu_percent: record.cpu_percent,
                peak_cpu_percent: record.peak_cpu_percent,
                resident_bytes: record.resident_bytes,
                peak_resident_bytes: record.peak_resident_bytes,
                sample_count: record.sample_count,
            });
        }

        if rows.len() > k {
            rows.select_nth_unstable_by(k, |a, b| self.cmp_rows(sort, a, b));
            rows.truncate(k);
        }

        rows.sort_by(|a, b| self.cmp_rows(sort, a, b));
    }
}

impl Default for ProcessMonitor {
    fn default() -> Self {
        Self::new()
    }
}

struct Hud<'a> {
    sort: SortKey,
    interactive: bool,
    spike_cpu: f32,
    log_enabled: bool,
    log_path: &'a str,
    log_pending: u32,
    log_written: u64,
    log_dropped: u64,
    log_error: bool,
}

/// RAII terminal manager: alternate screen, cbreak keys, and clean restore on exit.
pub struct TerminalView {
    is_tty: bool,
    interactive: bool,
    buffer: String,
    prev_frame: String,
    orig_termios: Option<Termios>,
}

impl TerminalView {
    pub fn new() -> Self {
        let stdout_tty = stdout().is_terminal();
        let stdin_tty = stdin().is_terminal();
        if stdout_tty {
            print!("\x1b[?1049h\x1b[2J\x1b[?25l");
            let _ = stdout().flush();
        }

        let mut interactive = false;
        let mut orig_termios = None;

        if stdout_tty && stdin_tty {
            unsafe {
                let mut term = MaybeUninit::<Termios>::zeroed();
                if tcgetattr(STDIN_FILENO, term.as_mut_ptr()) == 0 {
                    let orig = term.assume_init();
                    let mut raw = orig;
                    // VMIN/VTIME 0: read() returns immediately, so stdin stays blocking
                    // and stdout (often the same tty) is not switched to nonblocking.
                    raw.c_lflag &= !(ICANON | ECHO);
                    raw.c_cc[VMIN] = 0;
                    raw.c_cc[VTIME] = 0;
                    if tcsetattr(STDIN_FILENO, TCSANOW, &raw) == 0 {
                        orig_termios = Some(orig);
                        interactive = true;
                    }
                }
            }
        }

        Self {
            is_tty: stdout_tty,
            interactive,
            buffer: String::with_capacity(4096),
            prev_frame: String::with_capacity(4096),
            orig_termios,
        }
    }

    /// Block in the kernel until a key arrives or `timeout` elapses. No busy wait.
    fn poll_key(&mut self, timeout: Duration) -> Option<KeyAction> {
        let mut ms = timeout_ms(timeout);
        if ms == 0 {
            ms = 1;
        }
        if !self.interactive {
            let rc = unsafe { poll(null_mut(), 0, ms) };
            // EINTR means a signal fired. Return so the loop can quit or stop.
            // Sleep only when poll itself fails, so a broken wait cannot spin.
            if rc < 0 && std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted
            {
                sleep(timeout);
            }
            return None;
        }

        let mut pfd = PollFd {
            fd: STDIN_FILENO,
            events: POLLIN,
            revents: 0,
        };
        let rc = unsafe { poll(&mut pfd, 1, ms) };
        if rc < 0 {
            let interrupted = std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted;
            if !interrupted {
                self.interactive = false;
                sleep(timeout);
            }
            return None;
        }
        if rc == 0 {
            return None;
        }
        if (pfd.revents & POLLIN) != 0 {
            return self.read_keys();
        }
        if (pfd.revents & (POLLERR | POLLHUP | POLLNVAL)) != 0 {
            self.interactive = false;
        }
        None
    }

    /// Leave the alternate screen and cooked terminal, stop, then come back raw.
    fn suspend(&mut self) {
        self.restore_tty();
        let default_action = SigAction {
            sa_handler: std::ptr::null(),
            sa_mask: 0,
            sa_flags: 0,
        };
        unsafe {
            let _ = sigaction(SIGTSTP, &default_action, null_mut());
            let _ = raise(SIGTSTP);
        }
        install_handler(SIGTSTP, handle_stop_signal);
        if self.is_tty {
            print!("\x1b[?1049h\x1b[2J\x1b[?25l");
            let _ = stdout().flush();
        }
        self.apply_cbreak();
        self.prev_frame.clear();
    }

    fn restore_tty(&mut self) {
        unsafe {
            if let Some(ref term) = self.orig_termios {
                let _ = tcsetattr(STDIN_FILENO, TCSAFLUSH, term);
            }
        }
        if self.is_tty {
            print!("\x1b[?25h\x1b[?1049l");
            let _ = stdout().flush();
        }
        self.interactive = false;
    }

    fn apply_cbreak(&mut self) {
        let Some(orig) = self.orig_termios else {
            return;
        };
        let mut raw = orig;
        raw.c_lflag &= !(ICANON | ECHO);
        raw.c_cc[VMIN] = 0;
        raw.c_cc[VTIME] = 0;
        unsafe {
            if tcsetattr(STDIN_FILENO, TCSANOW, &raw) == 0 {
                self.interactive = true;
            }
        }
    }

    fn read_keys(&mut self) -> Option<KeyAction> {
        let mut buf = [0u8; 64];
        let mut quit = false;
        let mut sort = None;
        loop {
            let n = unsafe { libc_read(STDIN_FILENO, buf.as_mut_ptr() as *mut std::ffi::c_void, buf.len()) };
            if n == 0 {
                self.interactive = false;
                break;
            }
            if n < 0 {
                break;
            }
            match fold_keys(&buf[..n as usize]) {
                Some(KeyAction::Quit) => quit = true,
                Some(KeyAction::Sort(key)) => sort = Some(key),
                None => {}
            }
            if (n as usize) < buf.len() {
                break;
            }
        }
        if quit {
            Some(KeyAction::Quit)
        } else {
            sort.map(KeyAction::Sort)
        }
    }

    fn render(&mut self, monitor: &ProcessMonitor, top_rows: &[ProcessRow], hud: &Hud) {
        self.buffer.clear();

        if self.is_tty {
            self.buffer.push_str("\x1b[H");
        }

        let uptime_secs = monitor.start_time.elapsed().as_secs();
        let hours = uptime_secs / 3600;
        let mins = (uptime_secs % 3600) / 60;
        let secs = uptime_secs % 60;

        let latest_snapshot = monitor.snapshots.back();
        let total_ram_mb = latest_snapshot.map(|s| mb(s.total_resident_bytes)).unwrap_or(0.0);
        let total_cpu = latest_snapshot.map(|s| s.total_cpu_percent).unwrap_or(0.0);

        let _ = writeln!(
            self.buffer,
            "┌────────────────────────────────────────────────────────────────────────────────────────┐\x1b[K"
        );
        let _ = writeln!(
            self.buffer,
            "│ \x1b[1;36mPROCMON\x1b[0m — macOS Process Monitor         Tick: #{:<6} Active PIDs: {:<6} Uptime: {:02}:{:02}:{:02} │\x1b[K",
            monitor.current_tick, monitor.records.len(), hours, mins, secs
        );

        let line_start = self.buffer.len();
        self.buffer.push('│');
        let _ = write!(
            self.buffer,
            " Total RAM: {total_ram_mb:>8.1} MB   CPU: {total_cpu:>6.1}%   Sort: {} ({})",
            hud.sort.label(),
            hud.sort.order_hint(),
        );
        finish_box_line(&mut self.buffer, line_start);

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

        for row in top_rows {
            let name = monitor.records.get(&row.pid).map(|rec| rec.name.as_str()).unwrap_or("");
            let _ = write!(self.buffer, "│ {:<6} │ ", row.pid);
            push_fixed_width(&mut self.buffer, name, NAME_COL_WIDTH);
            let _ = write!(
                self.buffer,
                " │ {:>7.1}% │ {:>7.1}% │ {:>9.1} │ {:>9.1} │ {:>4} │\x1b[K\n",
                row.cpu_percent,
                row.peak_cpu_percent,
                mb(row.resident_bytes),
                mb(row.peak_resident_bytes),
                row.sample_count
            );
        }

        let _ = writeln!(
            self.buffer,
            "└────────┴──────────────────────────┴──────────┴──────────┴───────────┴───────────┴──────┘\x1b[K"
        );

        let status_start = self.buffer.len();
        if !hud.log_enabled {
            self.buffer.push_str(" log off");
        } else if hud.log_error {
            let _ = write!(
                self.buffer,
                " log FAILED {} queued={}",
                hud.log_path, hud.log_pending
            );
        } else {
            self.buffer.push_str(" log ");
            self.buffer.push_str(hud.log_path);
            self.buffer.push_str(" >=");
            if (hud.spike_cpu - hud.spike_cpu.round()).abs() < 0.05 {
                let _ = write!(self.buffer, "{:.0}", hud.spike_cpu);
            } else {
                let _ = write!(self.buffer, "{:.1}", hud.spike_cpu);
            }
            let _ = write!(
                self.buffer,
                "% queued={} saved={}",
                hud.log_pending, hud.log_written
            );
            if hud.log_dropped > 0 {
                let _ = write!(self.buffer, " dropped={}", hud.log_dropped);
            }
        }
        trim_to_width(&mut self.buffer, status_start, BOX_WIDTH);
        self.buffer.push_str("\x1b[K\n");

        if hud.interactive {
            self.buffer.push_str(KEY_HELP);
        } else {
            self.buffer.push_str(FLAG_HELP);
        }
        self.buffer.push_str("\x1b[K\n\x1b[J");

        if self.is_tty && self.buffer == self.prev_frame {
            return;
        }

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
        self.restore_tty();
    }
}

impl Default for TerminalView {
    fn default() -> Self {
        Self::new()
    }
}

fn timeout_ms(timeout: Duration) -> i32 {
    let ms = timeout.as_millis();
    if ms > i32::MAX as u128 {
        i32::MAX
    } else {
        ms as i32
    }
}

fn trim_to_width(buf: &mut String, start: usize, max_chars: usize) {
    let mut count = 0usize;
    for (i, _) in buf[start..].char_indices() {
        if count == max_chars {
            buf.truncate(start + i);
            return;
        }
        count += 1;
    }
}

fn finish_box_line(buf: &mut String, line_start: usize) {
    trim_to_width(buf, line_start, BOX_WIDTH - 1);
    let visible = buf[line_start..].chars().count();
    for _ in visible..(BOX_WIDTH - 1) {
        buf.push(' ');
    }
    buf.push('│');
    buf.push_str("\x1b[K\n");
}

fn push_fixed_width(buf: &mut String, text: &str, width: usize) {
    let mut end = 0usize;
    let mut count = 0usize;
    let mut overflow = false;
    for (i, ch) in text.char_indices() {
        if count >= width {
            overflow = true;
            break;
        }
        end = i + ch.len_utf8();
        count += 1;
    }
    if !overflow {
        buf.push_str(&text[..end]);
        while count < width {
            buf.push(' ');
            count += 1;
        }
        return;
    }
    let keep = width.saturating_sub(3);
    end = 0;
    count = 0;
    for (i, ch) in text.char_indices() {
        if count >= keep {
            break;
        }
        end = i + ch.len_utf8();
        count += 1;
    }
    buf.push_str(&text[..end]);
    if width >= 3 {
        buf.push_str("...");
        count += 3;
    }
    while count < width {
        buf.push(' ');
        count += 1;
    }
}

/// Command-line configuration for procmon.
#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub interval: Duration,
    pub top_n: usize,
    pub sort: SortKey,
    pub spike_cpu: f32,
    pub log_enabled: bool,
    pub log_path: PathBuf,
    pub log_flush: Duration,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(1),
            top_n: 15,
            sort: SortKey::Cpu,
            spike_cpu: DEFAULT_SPIKE_CPU,
            log_enabled: true,
            log_path: default_log_path(),
            log_flush: Duration::from_secs(DEFAULT_LOG_FLUSH_SECS),
        }
    }
}

fn default_log_path() -> PathBuf {
    match std::env::var_os("HOME") {
        Some(home) => PathBuf::from(home).join("Library/Logs/procmon/spikes.log"),
        None => PathBuf::from("procmon-spikes.log"),
    }
}

impl Config {
    pub fn parse_from<I, T>(args: I) -> Result<Option<Self>, String>
    where
        I: IntoIterator<Item = T>,
        T: AsRef<str>,
    {
        let mut cfg = Config::default();
        let mut iter = args.into_iter();

        while let Some(arg) = iter.next() {
            let s = arg.as_ref();
            match s {
                "-i" | "--interval" => {
                    let val = iter
                        .next()
                        .ok_or_else(|| "Missing value for --interval / -i".to_string())?;
                    let interval_secs = val
                        .as_ref()
                        .parse::<f64>()
                        .map_err(|_| format!("Invalid interval number: '{}'", val.as_ref()))?;
                    if interval_secs < 0.05 {
                        return Err("Interval must be at least 0.05 seconds".to_string());
                    }
                    cfg.interval = Duration::from_secs_f64(interval_secs);
                }
                "-n" | "--top" => {
                    let val = iter
                        .next()
                        .ok_or_else(|| "Missing value for --top / -n".to_string())?;
                    let top_n = val
                        .as_ref()
                        .parse::<usize>()
                        .map_err(|_| format!("Invalid top process count: '{}'", val.as_ref()))?;
                    if top_n == 0 {
                        return Err("Top process count must be greater than 0".to_string());
                    }
                    cfg.top_n = top_n;
                }
                "-s" | "--sort" => {
                    let val = iter
                        .next()
                        .ok_or_else(|| "Missing value for --sort / -s".to_string())?;
                    cfg.sort = SortKey::parse(val.as_ref()).ok_or_else(|| {
                        format!(
                            "Invalid sort '{}'. Use cpu, ram, peak-cpu, peak-ram, pid, name, or ticks.",
                            val.as_ref()
                        )
                    })?;
                }
                "--spike" => {
                    let val = iter
                        .next()
                        .ok_or_else(|| "Missing value for --spike".to_string())?;
                    let spike = val
                        .as_ref()
                        .parse::<f32>()
                        .map_err(|_| format!("Invalid spike percent: '{}'", val.as_ref()))?;
                    if !(spike >= 0.0) {
                        return Err("Spike percent must be zero or positive".to_string());
                    }
                    cfg.spike_cpu = spike;
                }
                "--log" => {
                    let val = iter
                        .next()
                        .ok_or_else(|| "Missing value for --log".to_string())?;
                    cfg.log_path = PathBuf::from(val.as_ref());
                    cfg.log_enabled = true;
                }
                "--no-log" => {
                    cfg.log_enabled = false;
                }
                "--log-flush" => {
                    let val = iter
                        .next()
                        .ok_or_else(|| "Missing value for --log-flush".to_string())?;
                    let secs = val
                        .as_ref()
                        .parse::<f64>()
                        .map_err(|_| format!("Invalid log flush seconds: '{}'", val.as_ref()))?;
                    if secs < 1.0 {
                        return Err("Log flush interval must be at least 1 second".to_string());
                    }
                    cfg.log_flush = Duration::from_secs_f64(secs);
                }
                "-h" | "--help" => {
                    print_help();
                    return Ok(None);
                }
                unknown => {
                    return Err(format!(
                        "Unknown option: '{unknown}'. Run with --help for options."
                    ));
                }
            }
        }

        Ok(Some(cfg))
    }
}

fn print_help() {
    println!("PROCMON — macOS Real-Time Process Monitor\n");
    println!("Usage: procmon [OPTIONS]\n");
    println!("Options:");
    println!("  -i, --interval <SECS>   Sample interval in seconds (default: 1.0, min 0.05)");
    println!("  -n, --top <COUNT>       Rows to show (default: 15)");
    println!("  -s, --sort <KEY>        cpu (default), ram, peak-cpu, peak-ram, pid, name, ticks");
    println!("      --spike <PERCENT>   Record CPU at or above this percent (default: 25)");
    println!("      --log <PATH>        Spike log path (default: ~/Library/Logs/procmon/spikes.log)");
    println!("      --no-log            Disable the spike log");
    println!("      --log-flush <SECS>  Hold log lines in memory at most this long (default: 30, min 1)");
    println!("  -h, --help              Show this help and exit");
    println!();
    println!("Keys while running (terminal only):");
    println!("  c CPU     m or r RAM     C peak CPU     M or R peak RAM");
    println!("  p PID     n name         t ticks        q or Ctrl-C quit");
    println!();
    println!("The spike log records a process when its CPU crosses the threshold, again");
    println!("after it drops below half that threshold and crosses it once more, and when");
    println!("a process that spiked then exits. A process that vanishes inside one sample");
    println!("is recorded only if it used at least 50ms of CPU. Lines stay in memory and");
    println!("are written every 8 KiB, every flush interval, or on exit. No file is created");
    println!("until the first event. Ctrl-C flushes the queue; kill -9 can drop it.");
}

fn draw(
    view: &mut TerminalView,
    monitor: &ProcessMonitor,
    rows: &mut Vec<ProcessRow>,
    log: &SpikeLog,
    sort: SortKey,
    top_n: usize,
) {
    monitor.top(rows, top_n, sort);
    view.render(
        monitor,
        rows,
        &Hud {
            sort,
            interactive: view.interactive,
            spike_cpu: log.spike_cpu,
            log_enabled: log.enabled,
            log_path: &log.path_display,
            log_pending: log.pending_events,
            log_written: log.written_events,
            log_dropped: log.dropped_events,
            log_error: log.error,
        },
    );
}

fn main() {
    let config = match Config::parse_from(std::env::args().skip(1)) {
        Ok(Some(cfg)) => cfg,
        Ok(None) => return,
        Err(err) => {
            eprintln!("Error: {err}");
            std::process::exit(1);
        }
    };

    install_handler(2, handle_termination_signal);
    install_handler(15, handle_termination_signal);
    install_handler(SIGTSTP, handle_stop_signal);

    let mut monitor = ProcessMonitor::new();
    let mut view = TerminalView::new();
    let mut log = SpikeLog::from_config(&config);
    let mut sort = config.sort;
    let mut rows: Vec<ProcessRow> = Vec::with_capacity(1024);

    monitor.tick(&mut log, config.spike_cpu, config.interval);
    draw(&mut view, &monitor, &mut rows, &log, sort, config.top_n);

    let mut next_sample = Instant::now() + config.interval;

    while RUNNING.load(AtomicOrdering::Acquire) {
        if STOP_REQUESTED.swap(false, AtomicOrdering::AcqRel) {
            view.suspend();
            monitor.rebase_clock();
            next_sample = Instant::now() + config.interval;
            draw(&mut view, &monitor, &mut rows, &log, sort, config.top_n);
        }
        let now = Instant::now();
        if now < next_sample {
            let remaining = next_sample.saturating_duration_since(now);
            if let Some(action) = view.poll_key(remaining) {
                match action {
                    KeyAction::Quit => break,
                    KeyAction::Sort(new_sort) => {
                        if new_sort != sort {
                            sort = new_sort;
                            draw(&mut view, &monitor, &mut rows, &log, sort, config.top_n);
                        }
                    }
                }
            }
            if !RUNNING.load(AtomicOrdering::Acquire) || Instant::now() < next_sample {
                continue;
            }
        }

        rows.clear();
        monitor.tick(&mut log, config.spike_cpu, config.interval);
        log.maybe_flush(false);
        draw(&mut view, &monitor, &mut rows, &log, sort, config.top_n);
        next_sample = Instant::now() + config.interval;
    }

    log.maybe_flush(true);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_record(pid: i32, name: &str, cpu: f32, ram: u64) -> ProcessRecord {
        ProcessRecord {
            pid,
            name: name.to_string(),
            total_cpu_ns: 0,
            last_seen_tick: 1,
            cpu_percent: cpu,
            resident_bytes: ram,
            peak_cpu_percent: cpu,
            peak_resident_bytes: ram,
            sample_count: 2,
            lifetime_cpu_percent: 0.0,
            seen_age_secs: 0.0,
            spike_armed: true,
        }
    }

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
        let mut log = SpikeLog::disabled();
        monitor.tick(&mut log, 25.0, Duration::from_secs(1));
        assert_eq!(monitor.current_tick, 1);
        assert!(!monitor.records.is_empty());
        assert_eq!(monitor.snapshots.len(), 1);

        let mut rows = Vec::new();
        monitor.top(&mut rows, 10, SortKey::Cpu);
        assert!(rows.len() <= 10);

        for i in 1..rows.len() {
            assert!(rows[i - 1].cpu_percent >= rows[i].cpu_percent);
        }
    }

    #[test]
    fn test_sort_keys_order_rows() {
        let mut monitor = ProcessMonitor::new();
        monitor.records.insert(1, sample_record(1, "zeta", 90.0, 1_000_000));
        monitor.records.insert(2, sample_record(2, "alpha", 5.0, 9_000_000_000));
        monitor.records.insert(3, sample_record(3, "mid", 40.0, 50_000_000));
        monitor.records.get_mut(&1).unwrap().peak_cpu_percent = 10.0;
        monitor.records.get_mut(&2).unwrap().peak_cpu_percent = 80.0;
        monitor.records.get_mut(&3).unwrap().sample_count = 9;

        let mut rows = Vec::new();
        monitor.top(&mut rows, 3, SortKey::Cpu);
        assert_eq!(rows[0].pid, 1);
        monitor.top(&mut rows, 3, SortKey::Ram);
        assert_eq!(rows[0].pid, 2);
        monitor.top(&mut rows, 3, SortKey::PeakCpu);
        assert_eq!(rows[0].pid, 2);
        monitor.top(&mut rows, 3, SortKey::PeakRam);
        assert_eq!(rows[0].pid, 2);
        monitor.top(&mut rows, 3, SortKey::Name);
        assert_eq!(rows[0].pid, 2);
        monitor.top(&mut rows, 3, SortKey::Pid);
        assert_eq!(rows[0].pid, 1);
        monitor.top(&mut rows, 3, SortKey::Ticks);
        assert_eq!(rows[0].pid, 3);
        monitor.top(&mut rows, 1, SortKey::Ram);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].pid, 2);
    }

    #[test]
    fn test_spike_edges_and_exits() {
        let (emit, armed) = spike_transition(true, 30.0, 25.0);
        assert!(emit && !armed);
        let (emit, armed) = spike_transition(false, 40.0, 25.0);
        assert!(!emit && !armed);
        let (emit, armed) = spike_transition(false, 20.0, 25.0);
        assert!(!emit && !armed);
        let (emit, armed) = spike_transition(false, 10.0, 25.0);
        assert!(!emit && armed);

        let mut rec = sample_record(4, "gone", 10.0, 1024);
        rec.peak_cpu_percent = 40.0;
        assert!(matches!(
            exit_kind(&rec, 25.0, Duration::from_secs(1)),
            ExitKind::Gone
        ));

        rec.peak_cpu_percent = 5.0;
        rec.sample_count = 1;
        rec.lifetime_cpu_percent = 80.0;
        rec.seen_age_secs = 0.4;
        rec.total_cpu_ns = 200_000_000;
        assert!(matches!(
            exit_kind(&rec, 25.0, Duration::from_secs(1)),
            ExitKind::Burst
        ));

        rec.seen_age_secs = 30.0;
        assert!(matches!(
            exit_kind(&rec, 25.0, Duration::from_secs(1)),
            ExitKind::Ignore
        ));

        rec.seen_age_secs = 0.4;
        rec.total_cpu_ns = 10_000_000;
        assert!(matches!(
            exit_kind(&rec, 25.0, Duration::from_secs(1)),
            ExitKind::Ignore
        ));
    }

    #[test]
    fn test_keys_and_name_width() {
        assert_eq!(key_action(b'm'), Some(KeyAction::Sort(SortKey::Ram)));
        assert_eq!(key_action(b'R'), Some(KeyAction::Sort(SortKey::PeakRam)));
        assert_eq!(fold_keys(b"cm"), Some(KeyAction::Sort(SortKey::Ram)));
        assert_eq!(fold_keys(b"mq"), Some(KeyAction::Quit));
        assert_eq!(fold_keys(b"\x1b[A"), None);

        let mut clipped = String::new();
        push_fixed_width(&mut clipped, "short", 24);
        assert_eq!(clipped.chars().count(), 24);
        assert!(clipped.starts_with("short"));
        clipped.clear();
        push_fixed_width(&mut clipped, "abcdefghijklmnopqrstuvwxyz012345", 24);
        assert_eq!(clipped.chars().count(), 24);
        assert!(clipped.ends_with("..."));

        let mut line = String::new();
        let start = line.len();
        line.push('│');
        line.push_str(" Total RAM: test");
        finish_box_line(&mut line, start);
        let visible = line.split('\u{1b}').next().unwrap().trim_end_matches('\n');
        assert_eq!(visible.chars().count(), BOX_WIDTH);
    }

    #[test]
    fn test_config_parsing() {
        let default_cfg = Config::parse_from(&[] as &[&str]).unwrap().unwrap();
        assert_eq!(default_cfg.interval, Duration::from_secs(1));
        assert_eq!(default_cfg.top_n, 15);
        assert_eq!(default_cfg.sort, SortKey::Cpu);
        assert_eq!(default_cfg.spike_cpu, 25.0);
        assert!(default_cfg.log_enabled);
        assert_eq!(default_cfg.log_flush, Duration::from_secs(30));
        assert!(default_cfg.log_path.ends_with("spikes.log"));

        let custom_cfg = Config::parse_from(&[
            "-i",
            "2.5",
            "-n",
            "20",
            "-s",
            "mem",
            "--spike",
            "40",
            "--no-log",
            "--log-flush",
            "10",
        ])
        .unwrap()
        .unwrap();
        assert_eq!(custom_cfg.interval, Duration::from_millis(2500));
        assert_eq!(custom_cfg.top_n, 20);
        assert_eq!(custom_cfg.sort, SortKey::Ram);
        assert_eq!(custom_cfg.spike_cpu, 40.0);
        assert!(!custom_cfg.log_enabled);
        assert_eq!(custom_cfg.log_flush, Duration::from_secs(10));

        let logged = Config::parse_from(&["--no-log", "--log", "/tmp/p.log"])
            .unwrap()
            .unwrap();
        assert!(logged.log_enabled);
        assert_eq!(logged.log_path, PathBuf::from("/tmp/p.log"));
        let quiet = Config::parse_from(&["--log", "a.log", "--no-log"])
            .unwrap()
            .unwrap();
        assert!(!quiet.log_enabled);

        let help = Config::parse_from(&["--help"]).unwrap();
        assert!(help.is_none());
    }

    #[test]
    fn test_config_validation() {
        assert!(Config::parse_from(&["-i", "-1"]).is_err());
        assert!(Config::parse_from(&["-n", "0"]).is_err());
        assert!(Config::parse_from(&["--invalid-flag"]).is_err());
        assert!(Config::parse_from(&["--sort", "nope"]).is_err());
        assert!(Config::parse_from(&["--spike", "-5"]).is_err());
        assert!(Config::parse_from(&["--log-flush", "0.2"]).is_err());
    }

    #[test]
    fn test_log_batches_until_flush_or_drop() {
        let dir = std::env::temp_dir().join(format!("procmon-log-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let rec = sample_record(7, "rust\"c", 80.0, 50 * 1024 * 1024);
        let path = dir.join("spikes.log");
        {
            let mut log = SpikeLog::for_test(path.clone(), Duration::from_secs(30), 8 * 1024);
            log.log_spike(&rec);
            log.log_gone(&rec);
            log.maybe_flush(false);
            assert!(!path.exists(), "queued events must not touch disk yet");
            assert_eq!(log.pending_events, 2);
            log.maybe_flush(true);
            let text = std::fs::read_to_string(&path).unwrap();
            assert!(text.contains("SPIKE"), "{text}");
            assert!(text.contains("GONE"), "{text}");
            assert!(text.contains(r#"name="rust\"c""#), "{text}");
            assert!(text.lines().next().unwrap().starts_with("# "));
            assert_eq!(log.pending_events, 0);
            assert_eq!(log.written_events, 2);
        }

        let path_timed = dir.join("timed.log");
        {
            let mut log = SpikeLog::for_test(path_timed.clone(), Duration::from_millis(30), 8 * 1024);
            log.log_spike(&rec);
            log.maybe_flush(false);
            assert!(!path_timed.exists());
            sleep(Duration::from_millis(50));
            log.maybe_flush(false);
            assert!(path_timed.exists());
        }

        let path_sized = dir.join("sized.log");
        {
            let mut log = SpikeLog::for_test(path_sized.clone(), Duration::from_secs(60), 32);
            log.log_spike(&rec);
            log.maybe_flush(false);
            assert!(path_sized.exists());
        }

        let path_drop = dir.join("drop.log");
        {
            let mut burst = sample_record(9, "brief", 0.0, 1024);
            burst.sample_count = 1;
            burst.lifetime_cpu_percent = 150.0;
            burst.seen_age_secs = 0.2;
            burst.total_cpu_ns = 80_000_000;
            let mut log = SpikeLog::for_test(path_drop.clone(), Duration::from_secs(60), 8 * 1024);
            log.log_burst(&burst);
            assert!(!path_drop.exists());
        }
        let dropped = std::fs::read_to_string(&path_drop).unwrap();
        assert!(dropped.contains("BURST"), "{dropped}");
        assert!(dropped.contains("brief"), "{dropped}");

        let mut disabled = SpikeLog::disabled();
        disabled.log_spike(&rec);
        disabled.maybe_flush(true);
        assert_eq!(disabled.pending_events, 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_high_threshold_does_not_create_a_log() {
        let dir = std::env::temp_dir().join(format!("procmon-quiet-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("spikes.log");
        let mut log = SpikeLog::for_test(path.clone(), Duration::from_millis(1), 1);
        let mut monitor = ProcessMonitor::new();
        monitor.tick(&mut log, 100_000.0, Duration::from_secs(1));
        sleep(Duration::from_millis(20));
        monitor.tick(&mut log, 100_000.0, Duration::from_secs(1));
        log.maybe_flush(true);
        assert!(
            !path.exists(),
            "ordinary processes should not create a spike log"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
