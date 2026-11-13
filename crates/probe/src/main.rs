//! Phase 0 read only probe for the Corsair K95 RGB (VID 1B1C, PID 1B11).
//!
//! This binary never writes to the keyboard. It declares no HID write function:
//! no WriteFile, no HidD_SetFeature, no HidD_SetOutputReport, no
//! HidD_SetNumInputBuffers, no DeviceIoControl. Audit it with:
//!
//!     dumpbin /imports probe.exe
//!
//! Against HID.DLL that shows only HidD_Get* and HidP_Get*. Note that
//! NtWriteFile does appear in the import table: it belongs to the Rust standard
//! library, which uses it for console output. The checkable claim is "no HID
//! write surface", not "no write syscall is present in the process".
//!
//! Every handle is opened with FILE_SHARE_READ | FILE_SHARE_WRITE so that
//! SignalRGB, which drives this keyboard's lighting, is never locked out.
//!
//! Subcommands:
//!     probe list            enumerate HID collections with their report sizes
//!     probe listen [secs]   dump every input report that arrives, with a diff
//!     probe map             guided capture that builds the G1..G18 bit mapping

use std::collections::BTreeMap;
use std::io::Write;
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// Win32 FFI, declared by hand. Read only surface only.
// ---------------------------------------------------------------------------

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Guid {
    data1: u32,
    data2: u16,
    data3: u16,
    data4: [u8; 8],
}

#[repr(C)]
struct SpDeviceInterfaceData {
    cb_size: u32,
    interface_class_guid: Guid,
    flags: u32,
    reserved: usize,
}

#[repr(C)]
#[derive(Default)]
struct HiddAttributes {
    size: u32,
    vendor_id: u16,
    product_id: u16,
    version_number: u16,
}

/// HIDP_CAPS is 64 bytes. Field order matters; do not reorder.
#[repr(C)]
#[derive(Default)]
struct HidpCaps {
    usage: u16,
    usage_page: u16,
    input_report_byte_length: u16,
    output_report_byte_length: u16,
    feature_report_byte_length: u16,
    reserved: [u16; 17],
    number_link_collection_nodes: u16,
    number_input_button_caps: u16,
    number_input_value_caps: u16,
    number_input_data_indices: u16,
    number_output_button_caps: u16,
    number_output_value_caps: u16,
    number_output_data_indices: u16,
    number_feature_button_caps: u16,
    number_feature_value_caps: u16,
    number_feature_data_indices: u16,
}

#[repr(C)]
struct Overlapped {
    internal: usize,
    internal_high: usize,
    offset: u32,
    offset_high: u32,
    h_event: isize,
}

impl Overlapped {
    fn new(ev: isize) -> Self {
        Overlapped {
            internal: 0,
            internal_high: 0,
            offset: 0,
            offset_high: 0,
            h_event: ev,
        }
    }
}

const INVALID_HANDLE_VALUE: isize = -1;
const DIGCF_PRESENT: u32 = 0x0000_0002;
const DIGCF_DEVICEINTERFACE: u32 = 0x0000_0010;
const GENERIC_READ: u32 = 0x8000_0000;
const FILE_SHARE_READ: u32 = 0x0000_0001;
const FILE_SHARE_WRITE: u32 = 0x0000_0002;
const OPEN_EXISTING: u32 = 3;
const FILE_FLAG_OVERLAPPED: u32 = 0x4000_0000;
const ERROR_IO_PENDING: u32 = 997;
const WAIT_OBJECT_0: u32 = 0;
const HIDP_STATUS_SUCCESS: i32 = 0x0011_0000;

extern "system" {
    fn CreateFileW(
        name: *const u16,
        access: u32,
        share: u32,
        sa: *const core::ffi::c_void,
        disposition: u32,
        flags: u32,
        template: isize,
    ) -> isize;
    fn CloseHandle(h: isize) -> i32;
    fn ReadFile(h: isize, buf: *mut u8, len: u32, read: *mut u32, ov: *mut Overlapped) -> i32;
    fn CancelIoEx(h: isize, ov: *mut Overlapped) -> i32;
    fn GetOverlappedResult(h: isize, ov: *mut Overlapped, got: *mut u32, wait: i32) -> i32;
    fn CreateEventW(
        sa: *const core::ffi::c_void,
        manual: i32,
        initial: i32,
        name: *const u16,
    ) -> isize;
    fn ResetEvent(h: isize) -> i32;
    fn WaitForSingleObject(h: isize, ms: u32) -> u32;
    fn GetLastError() -> u32;
}

#[link(name = "setupapi")]
extern "system" {
    fn SetupDiGetClassDevsW(
        guid: *const Guid,
        enumerator: *const u16,
        hwnd: isize,
        flags: u32,
    ) -> isize;
    fn SetupDiEnumDeviceInterfaces(
        set: isize,
        devinfo: *mut core::ffi::c_void,
        guid: *const Guid,
        index: u32,
        data: *mut SpDeviceInterfaceData,
    ) -> i32;
    fn SetupDiGetDeviceInterfaceDetailW(
        set: isize,
        data: *mut SpDeviceInterfaceData,
        detail: *mut u8,
        detail_size: u32,
        required: *mut u32,
        devinfo: *mut core::ffi::c_void,
    ) -> i32;
    fn SetupDiDestroyDeviceInfoList(set: isize) -> i32;
}

#[link(name = "hid")]
extern "system" {
    fn HidD_GetHidGuid(guid: *mut Guid);
    fn HidD_GetAttributes(h: isize, attrs: *mut HiddAttributes) -> u8;
    fn HidD_GetPreparsedData(h: isize, pp: *mut isize) -> u8;
    fn HidD_FreePreparsedData(pp: isize) -> u8;
    fn HidP_GetCaps(pp: isize, caps: *mut HidpCaps) -> i32;
    fn HidD_GetProductString(h: isize, buf: *mut u16, len: u32) -> u8;
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn wide_from_bytes(buf: &[u8], offset: usize) -> String {
    let mut units = Vec::new();
    let mut i = offset;
    while i + 1 < buf.len() {
        let u = u16::from_le_bytes([buf[i], buf[i + 1]]);
        if u == 0 {
            break;
        }
        units.push(u);
        i += 2;
    }
    String::from_utf16_lossy(&units)
}

#[derive(Clone)]
struct Collection {
    path: String,
    product: String,
    vid: u16,
    pid: u16,
    usage_page: u16,
    usage: u16,
    in_len: usize,
    out_len: usize,
    feat_len: usize,
}

impl Collection {
    /// Short stable tag, e.g. "FFC0/0002", used in listen output.
    fn tag(&self) -> String {
        format!("{:04X}/{:04X}", self.usage_page, self.usage)
    }

    fn is_vendor(&self) -> bool {
        self.usage_page >= 0xFF00
    }

    /// Readable candidate: vendor defined and actually carries input reports.
    fn is_input_candidate(&self) -> bool {
        self.is_vendor() && self.in_len > 1
    }

    fn kind(&self) -> &'static str {
        match (self.usage_page, self.usage) {
            (0x0001, 0x0006) => "keyboard (OS owned, exclusively claimed)",
            (0x0001, 0x0002) => "mouse (OS owned, exclusively claimed)",
            (0x000C, _) => "consumer control (media keys, OS owned)",
            (p, _) if p >= 0xFF00 => "vendor defined",
            _ => "other",
        }
    }
}

/// Open a collection without asking for read access, purely to query metadata.
/// dwDesiredAccess = 0 cannot disturb any other process holding the device.
unsafe fn open_query(path: &str) -> isize {
    CreateFileW(
        wide(path).as_ptr(),
        0,
        FILE_SHARE_READ | FILE_SHARE_WRITE,
        ptr::null(),
        OPEN_EXISTING,
        0,
        0,
    )
}

/// Open a collection for reading input reports, shared so SignalRGB keeps working.
unsafe fn open_read(path: &str) -> isize {
    CreateFileW(
        wide(path).as_ptr(),
        GENERIC_READ,
        FILE_SHARE_READ | FILE_SHARE_WRITE,
        ptr::null(),
        OPEN_EXISTING,
        FILE_FLAG_OVERLAPPED,
        0,
    )
}

unsafe fn enumerate(vid_filter: u16, pid_filter: u16) -> Vec<Collection> {
    let mut found = Vec::new();

    let mut guid = Guid::default();
    HidD_GetHidGuid(&mut guid);

    let set = SetupDiGetClassDevsW(&guid, ptr::null(), 0, DIGCF_PRESENT | DIGCF_DEVICEINTERFACE);
    if set == INVALID_HANDLE_VALUE {
        eprintln!(
            "SetupDiGetClassDevsW failed, win32 error {}",
            GetLastError()
        );
        return found;
    }

    let mut index = 0u32;
    loop {
        let mut did = SpDeviceInterfaceData {
            cb_size: std::mem::size_of::<SpDeviceInterfaceData>() as u32,
            interface_class_guid: Guid::default(),
            flags: 0,
            reserved: 0,
        };

        if SetupDiEnumDeviceInterfaces(set, ptr::null_mut(), &guid, index, &mut did) == 0 {
            break;
        }
        index += 1;

        let mut buf = vec![0u8; 2048];
        // SP_DEVICE_INTERFACE_DETAIL_DATA_W.cbSize: 8 on 64 bit, 6 on 32 bit.
        // DevicePath always begins at byte offset 4.
        let cb: u32 = if cfg!(target_pointer_width = "64") {
            8
        } else {
            6
        };
        buf[0..4].copy_from_slice(&cb.to_le_bytes());

        let mut required = 0u32;
        if SetupDiGetDeviceInterfaceDetailW(
            set,
            &mut did,
            buf.as_mut_ptr(),
            buf.len() as u32,
            &mut required,
            ptr::null_mut(),
        ) == 0
        {
            continue;
        }

        let path = wide_from_bytes(&buf, 4);
        if path.is_empty() {
            continue;
        }

        let h = open_query(&path);
        if h == INVALID_HANDLE_VALUE {
            continue;
        }

        let mut attrs = HiddAttributes {
            size: std::mem::size_of::<HiddAttributes>() as u32,
            ..Default::default()
        };
        let got_attrs = HidD_GetAttributes(h, &mut attrs) != 0;

        if got_attrs && attrs.vendor_id == vid_filter && attrs.product_id == pid_filter {
            let mut pbuf = vec![0u16; 256];
            let product = if HidD_GetProductString(h, pbuf.as_mut_ptr(), 512) != 0 {
                let end = pbuf.iter().position(|&c| c == 0).unwrap_or(pbuf.len());
                String::from_utf16_lossy(&pbuf[..end])
            } else {
                String::from("(unavailable)")
            };

            let mut pp: isize = 0;
            if HidD_GetPreparsedData(h, &mut pp) != 0 {
                let mut caps = HidpCaps::default();
                if HidP_GetCaps(pp, &mut caps) == HIDP_STATUS_SUCCESS {
                    found.push(Collection {
                        path: path.clone(),
                        product: product.trim().to_string(),
                        vid: attrs.vendor_id,
                        pid: attrs.product_id,
                        usage_page: caps.usage_page,
                        usage: caps.usage,
                        in_len: caps.input_report_byte_length as usize,
                        out_len: caps.output_report_byte_length as usize,
                        feat_len: caps.feature_report_byte_length as usize,
                    });
                }
                HidD_FreePreparsedData(pp);
            }
        }

        CloseHandle(h);
    }

    SetupDiDestroyDeviceInfoList(set);
    found.sort_by(|a, b| a.path.cmp(&b.path));
    found
}

/// Distinguishing a quiet pipe from a broken one matters: if both looked the
/// same we could conclude "the keyboard sends nothing" when the truth was
/// "our read failed".
enum ReadOutcome {
    Data(usize),
    Timeout,
    Failed(u32),
}

/// One overlapped read with a timeout.
unsafe fn read_timeout(h: isize, ev: isize, buf: &mut [u8], ms: u32) -> ReadOutcome {
    let mut ov = Overlapped::new(ev);
    ResetEvent(ev);

    let mut read = 0u32;
    if ReadFile(h, buf.as_mut_ptr(), buf.len() as u32, &mut read, &mut ov) != 0 {
        return ReadOutcome::Data(read as usize);
    }
    let err = GetLastError();
    if err != ERROR_IO_PENDING {
        return ReadOutcome::Failed(err);
    }

    if WaitForSingleObject(ev, ms) == WAIT_OBJECT_0 {
        let mut got = 0u32;
        if GetOverlappedResult(h, &mut ov, &mut got, 0) != 0 {
            return ReadOutcome::Data(got as usize);
        }
        return ReadOutcome::Failed(GetLastError());
    }

    // Timed out: cancel and wait for the cancellation to land before `ov` dies.
    // Reports arriving while no read is pending are not lost; the HID class
    // driver queues them per handle (32 by default) and the next read drains them.
    CancelIoEx(h, &mut ov);
    let mut got = 0u32;
    GetOverlappedResult(h, &mut ov, &mut got, 1);
    ReadOutcome::Timeout
}

struct Report {
    tag: String,
    at: Duration,
    bytes: Vec<u8>,
}

/// What each collection is to this experiment.
fn role(c: &Collection) -> &'static str {
    match (c.usage_page, c.usage) {
        (0x0001, 0x0006) => "keyboard: Windows should REFUSE us, which is the point",
        (0x0001, 0x0002) => "mouse: Windows should refuse us",
        (0x000C, _) => "consumer control: control experiment, proves reading works",
        (p, _) if p >= 0xFF00 && c.out_len > 1 => {
            "vendor command channel, SignalRGB's, read only here"
        }
        (p, _) if p >= 0xFF00 => "vendor input pipe: the question under test",
        _ => "other",
    }
}

/// Spawn one reader thread per collection. Each owns its handle and event.
fn spawn_readers(
    cols: &[Collection],
    stop: Arc<AtomicBool>,
    tx: mpsc::Sender<Report>,
    t0: Instant,
) -> Vec<thread::JoinHandle<()>> {
    let mut handles = Vec::new();

    for col in cols {
        let path = col.path.clone();
        let tag = col.tag();
        let role = role(col).to_string();
        let len = col.in_len.max(1);
        let stop = Arc::clone(&stop);
        let tx = tx.clone();

        handles.push(thread::spawn(move || unsafe {
            let h = open_read(&path);
            if h == INVALID_HANDLE_VALUE {
                let e = GetLastError();
                let note = if e == 5 { " (ACCESS_DENIED)" } else { "" };
                println!("  [{}] OPEN REFUSED, win32 error {}{}", tag, e, note);
                println!("            {}", role);
                return;
            }
            let ev = CreateEventW(ptr::null(), 1, 0, ptr::null());
            if ev == 0 {
                println!("  [{}] CreateEventW failed, win32 error {}", tag, GetLastError());
                CloseHandle(h);
                return;
            }
            println!("  [{}] OPEN OK, {} byte reports", tag, len);
            println!("            {}", role);

            let mut buf = vec![0u8; len];
            loop {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                match read_timeout(h, ev, &mut buf, 1000) {
                    ReadOutcome::Data(n) if n > 0 => {
                        let _ = tx.send(Report {
                            tag: tag.clone(),
                            at: t0.elapsed(),
                            bytes: buf[..n].to_vec(),
                        });
                    }
                    ReadOutcome::Data(_) => {}
                    ReadOutcome::Timeout => {}
                    ReadOutcome::Failed(e) => {
                        println!(
                            "  [{}] READ FAILED, win32 error {}. Silence here means a broken read, not a quiet keyboard.",
                            tag, e
                        );
                        break;
                    }
                }
            }

            CloseHandle(ev);
            CloseHandle(h);
        }));
    }

    handles
}

fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{:02X}", b))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Bits that are set in `now` but not in `prev`, as (byte index, bit index).
///
/// Byte 0 is skipped deliberately: on a collection that uses report IDs it is
/// the ID itself, constant for every report (0x03 on this keyboard's input
/// pipe). Diffing it against an empty baseline reports its bits as "rising" on
/// the very first report, which would otherwise be captured as a key.
fn rising_bits(prev: &[u8], now: &[u8]) -> Vec<(usize, u8)> {
    let mut out = Vec::new();
    for (i, &value) in now.iter().enumerate().skip(1) {
        let p = prev.get(i).copied().unwrap_or(0);
        let changed = value & !p;
        for bit in 0..8u8 {
            if changed & (1 << bit) != 0 {
                out.push((i, bit));
            }
        }
    }
    out
}

fn describe_bits(bits: &[(usize, u8)]) -> String {
    bits.iter()
        .map(|(byte, bit)| {
            format!(
                "byte {} bit {} (linear bit {})",
                byte,
                bit,
                byte * 8 + *bit as usize
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

// ---------------------------------------------------------------------------
// Subcommands
// ---------------------------------------------------------------------------

const VID: u16 = 0x1B1C;
const PID: u16 = 0x1B11;

fn cmd_list() {
    let cols = unsafe { enumerate(VID, PID) };
    if cols.is_empty() {
        println!(
            "No HID collection found for {:04X}:{:04X}. Is the keyboard plugged in?",
            VID, PID
        );
        return;
    }

    println!(
        "Corsair K95 RGB, {} HID collections ({:04X}:{:04X})",
        cols.len(),
        cols[0].vid,
        cols[0].pid
    );
    println!("Product string: {}", cols[0].product);
    println!();

    for c in &cols {
        println!("{}", "-".repeat(76));
        println!(
            "  usage page 0x{:04X}  usage 0x{:04X}   {}",
            c.usage_page,
            c.usage,
            c.kind()
        );
        println!(
            "  report lengths: input {}  output {}  feature {}",
            c.in_len, c.out_len, c.feat_len
        );
        if c.is_input_candidate() {
            println!(
                "  >> READ CANDIDATE: {} payload bytes after the report ID",
                c.in_len - 1
            );
        } else if c.is_vendor() && c.out_len > 1 {
            println!("  >> command channel (writable). Probably SignalRGB's. We do not touch it.");
        }
        println!("  {}", c.path);
    }
    println!("{}", "-".repeat(76));
}

fn cmd_listen(secs: u64) {
    // Open everything, including the collections Windows will refuse us. Being
    // refused on the keyboard is itself a result worth recording.
    let cols = unsafe { enumerate(VID, PID) };

    if cols.is_empty() {
        println!(
            "No HID collection found for {:04X}:{:04X}. Is the keyboard plugged in?",
            VID, PID
        );
        return;
    }

    // Vendor collections that only carry input are the ones under test.
    // The writable vendor collection is the command channel, counted separately
    // so that SignalRGB's own traffic cannot be mistaken for a G key.
    let input_tags: Vec<String> = cols
        .iter()
        .filter(|c| c.is_vendor() && c.out_len <= 1)
        .map(|c| c.tag())
        .collect();
    let cmd_tags: Vec<String> = cols
        .iter()
        .filter(|c| c.is_vendor() && c.out_len > 1)
        .map(|c| c.tag())
        .collect();
    let consumer_tags: Vec<String> = cols
        .iter()
        .filter(|c| c.usage_page == 0x000C)
        .map(|c| c.tag())
        .collect();

    println!(
        "Listening read only for {} seconds. No byte is ever written to the keyboard.",
        secs
    );
    println!();
    println!("Please do these in order, pausing between each:");
    println!("  1. press G1, then G2, then G3");
    println!("  2. press mute, or roll the volume wheel      <-- control experiment");
    println!("  3. press M1, M2, M3, then MR");
    println!("  4. type a few normal letters");
    println!();
    println!("Step 2 is the one that matters. If it produces reports while the G keys do");
    println!("not, then reading provably works and the G keys are genuinely silent.");
    println!();

    let stop = Arc::new(AtomicBool::new(false));
    let (tx, rx) = mpsc::channel::<Report>();
    let t0 = Instant::now();
    let readers = spawn_readers(&cols, Arc::clone(&stop), tx, t0);

    // Let the open messages land, then wait for the operator. The window must
    // not count down while they are still reading the instructions: that is
    // what voided the first mapping attempt.
    thread::sleep(Duration::from_millis(400));
    println!();
    print!("Press Enter to start the {} second window. ", secs);
    let _ = std::io::stdout().flush();
    let mut ready = String::new();
    let _ = std::io::stdin().read_line(&mut ready);
    println!();

    let mut last: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    while let Ok(r) = rx.try_recv() {
        last.insert(r.tag.clone(), r.bytes);
    }
    let deadline = Instant::now() + Duration::from_secs(secs);

    while Instant::now() < deadline {
        let left = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left.min(Duration::from_millis(500))) {
            Ok(r) => {
                *counts.entry(r.tag.clone()).or_insert(0) += 1;
                let prev = last.entry(r.tag.clone()).or_default();
                let bits = rising_bits(prev, &r.bytes);
                println!(
                    "[{:>7.3}s] {} len {}  {}",
                    r.at.as_secs_f64(),
                    r.tag,
                    r.bytes.len(),
                    hex(&r.bytes)
                );
                if !bits.is_empty() {
                    println!("            rising: {}", describe_bits(&bits));
                }
                *prev = r.bytes;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    stop.store(true, Ordering::Relaxed);
    for h in readers {
        let _ = h.join();
    }

    let sum = |tags: &Vec<String>| -> usize { tags.iter().filter_map(|t| counts.get(t)).sum() };
    let input_total = sum(&input_tags);
    let cmd_total = sum(&cmd_tags);
    let consumer_total = sum(&consumer_tags);
    let total: usize = counts.values().sum();

    println!();
    println!("{}", "=".repeat(76));
    println!("REPORTS PER COLLECTION");
    for c in &cols {
        let n = counts.get(&c.tag()).copied().unwrap_or(0);
        println!("  {:<11} {:>5}   {}", c.tag(), n, role(c));
    }
    println!("{}", "=".repeat(76));
    println!("total {} reports in {} seconds", total, secs);
    println!();

    if cmd_total > 0 {
        println!(
            "NOTE: {} report(s) on the command channel. That is most likely SignalRGB,",
            cmd_total
        );
        println!("so check the hex above before reading anything into it.");
        println!();
    }

    if input_total > 0 {
        println!("VERDICT: a vendor input pipe is already emitting, with no write from us.");
        println!("Next: run `probe map` to pin each G key to its bit. No write needed at all.");
    } else if consumer_total > 0 {
        println!("VERDICT: reading provably works, because the consumer control collection");
        println!("delivered reports. The vendor input pipes stayed silent the whole time, so");
        println!("the G keys are not emitting Corsair events in the keyboard's current state.");
        println!("Enabling them needs the routing config written to the command channel,");
        println!("which is a separate decision and needs sign off first.");
    } else {
        println!("VERDICT: INCONCLUSIVE. Nothing arrived on any collection, not even the");
        println!("control experiment, so this run cannot say whether the G keys emit.");
        println!("Most likely nothing was pressed inside the window, or only normal keys");
        println!("were, and those stay on the collection Windows refuses us.");
        println!("Re-run, and be sure to press mute or roll the volume wheel this time.");
    }
}

fn cmd_map() {
    let cols: Vec<Collection> = unsafe { enumerate(VID, PID) }
        .into_iter()
        .filter(|c| c.is_input_candidate())
        .collect();

    if cols.is_empty() {
        println!("No vendor input collection found. Nothing to map.");
        return;
    }

    println!("Guided mapping. Press only the key named, once, then wait for the next prompt.");
    println!("Each key waits 15 seconds before it is skipped. Nothing is written to the device.");
    println!();

    let stop = Arc::new(AtomicBool::new(false));
    let (tx, rx) = mpsc::channel::<Report>();
    let t0 = Instant::now();
    let readers = spawn_readers(&cols, Arc::clone(&stop), tx, t0);
    println!();

    // Let the open messages land before prompting, so a failure is visible first.
    thread::sleep(Duration::from_millis(400));

    // Ready gate. One blocking read, on the main thread, before the loop starts.
    // The per key countdown must not begin until the person is actually at the
    // keyboard, or every key times out while they are still reading.
    println!();
    print!("Press Enter when you are ready to begin. ");
    let _ = std::io::stdout().flush();
    let mut ready = String::new();
    let _ = std::io::stdin().read_line(&mut ready);
    println!();

    // Establish the idle baseline, discarding anything from before we began.
    let mut last: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    while let Ok(r) = rx.try_recv() {
        last.insert(r.tag.clone(), r.bytes);
    }

    let mut mapping: Vec<(String, String, usize, u8)> = Vec::new();
    let mut consecutive_misses = 0u32;

    for key in 1..=18 {
        print!("  G{:<2} press it now: ", key);
        let _ = std::io::stdout().flush();

        // Drain anything stale before we start watching.
        while let Ok(r) = rx.try_recv() {
            last.insert(r.tag.clone(), r.bytes);
        }

        let mut hit: Option<(String, usize, u8, Vec<u8>)> = None;
        let wait_until = Instant::now() + Duration::from_secs(15);

        while Instant::now() < wait_until {
            match rx.recv_timeout(Duration::from_millis(150)) {
                Ok(r) => {
                    let prev = last.entry(r.tag.clone()).or_default();
                    let bits = rising_bits(prev, &r.bytes);
                    *prev = r.bytes.clone();
                    if let Some(&(byte, bit)) = bits.first() {
                        hit = Some((r.tag.clone(), byte, bit, r.bytes.clone()));
                        break;
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }

        match hit {
            Some((tag, byte, bit, raw)) => {
                consecutive_misses = 0;
                println!(
                    "captured {} byte {} bit {} (mask 0x{:02X})",
                    tag,
                    byte,
                    bit,
                    1u8 << bit
                );
                println!("        raw {}", hex(&raw));
                mapping.push((format!("G{}", key), tag, byte, bit));
                // Let the release settle so it is not mistaken for the next press.
                thread::sleep(Duration::from_millis(250));
                while let Ok(r) = rx.try_recv() {
                    last.insert(r.tag.clone(), r.bytes);
                }
            }
            None => {
                consecutive_misses += 1;
                println!("nothing arrived, skipped");
                if consecutive_misses >= 3 {
                    println!();
                    println!("Three keys in a row produced nothing. Stopping early.");
                    break;
                }
            }
        }
    }

    stop.store(true, Ordering::Relaxed);
    for h in readers {
        let _ = h.join();
    }

    println!();
    println!("{}", "=".repeat(76));
    println!("MAPPING RESULT");
    println!("{}", "=".repeat(76));
    if mapping.is_empty() {
        println!("Nothing captured. The G keys are not reaching the vendor pipe as configured.");
        return;
    }
    println!(
        "{:<5} {:<12} {:>5} {:>4} {:>12}",
        "key", "collection", "byte", "bit", "linear bit"
    );
    for (k, tag, byte, bit) in &mapping {
        println!(
            "{:<5} {:<12} {:>5} {:>4} {:>12}",
            k,
            tag,
            byte,
            bit,
            byte * 8 + *bit as usize
        );
    }
    println!();
    println!("Rust table for phase 1:");
    println!("pub const G_KEY_BITS: [(u8, u8); {}] = [", mapping.len());
    for (k, _, byte, bit) in &mapping {
        println!("    ({}, {}), // {}", byte, bit, k);
    }
    println!("];");
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let cmd = args.get(1).map(|s| s.as_str()).unwrap_or("list");

    println!("probe: read only. No HID write function is linked into this binary.");
    println!();

    match cmd {
        "list" => cmd_list(),
        "listen" => {
            let secs = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(45);
            cmd_listen(secs);
        }
        "map" => cmd_map(),
        other => {
            println!("unknown subcommand: {}", other);
            println!("usage: probe list | probe listen [seconds] | probe map");
        }
    }
}
