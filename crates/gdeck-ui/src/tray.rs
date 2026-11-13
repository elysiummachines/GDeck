//! The notification area icon.
//!
//! A tray icon needs a window procedure to receive its callback messages, and
//! winit owns the one belonging to the app window. So this runs a tiny
//! message only window of its own on a dedicated thread, which exists purely
//! to receive tray clicks and hand them back as [`TrayEvent`].
//!
//! The icon is drawn at runtime rather than embedded, which keeps the build to
//! plain `cargo build` with no resource compiler step.

#![allow(non_snake_case)]

use core::ffi::c_void;
use core::ptr;
use std::sync::mpsc::Sender;
use std::sync::OnceLock;

/// What the person did with the tray icon.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TrayEvent {
    /// Show the window again.
    Open,
    /// Leave for good.
    Quit,
}

static SENDER: OnceLock<Sender<TrayEvent>> = OnceLock::new();

const WM_APP_TRAY: u32 = 0x8000 + 1;
/// Asks the tray thread to take the icon down and stop.
const WM_APP_CLOSE: u32 = 0x8000 + 2;
/// Posted by a second launch of G Deck at the copy already running.
const WM_APP_SHOW: u32 = 0x8000 + 3;
const WM_DESTROY: u32 = 0x0002;
const WM_COMMAND: u32 = 0x0111;
const WM_LBUTTONUP: u32 = 0x0202;
const WM_RBUTTONUP: u32 = 0x0205;

const NIM_ADD: u32 = 0;
const NIM_DELETE: u32 = 2;
const NIF_MESSAGE: u32 = 0x0001;
const NIF_ICON: u32 = 0x0002;
const NIF_TIP: u32 = 0x0004;

const HWND_MESSAGE: isize = -3;

const MF_STRING: u32 = 0x0000;
const MF_SEPARATOR: u32 = 0x0800;
const TPM_RIGHTBUTTON: u32 = 0x0002;
const TPM_RETURNCMD: u32 = 0x0100;

const ID_OPEN: usize = 1;
const ID_QUIT: usize = 2;

#[repr(C)]
struct WndClassW {
    style: u32,
    proc_: Option<extern "system" fn(isize, u32, usize, isize) -> isize>,
    cls_extra: i32,
    wnd_extra: i32,
    instance: isize,
    icon: isize,
    cursor: isize,
    background: isize,
    menu_name: *const u16,
    class_name: *const u16,
}

#[repr(C)]
struct NotifyIconDataW {
    cb_size: u32,
    hwnd: isize,
    id: u32,
    flags: u32,
    callback_message: u32,
    icon: isize,
    tip: [u16; 128],
    state: u32,
    state_mask: u32,
    info: [u16; 256],
    timeout_or_version: u32,
    info_title: [u16; 64],
    info_flags: u32,
    guid: [u8; 16],
    balloon_icon: isize,
}

#[repr(C)]
struct IconInfo {
    is_icon: i32,
    x_hotspot: u32,
    y_hotspot: u32,
    mask: isize,
    colour: isize,
}

#[repr(C)]
struct Point {
    x: i32,
    y: i32,
}

#[repr(C)]
struct Msg {
    hwnd: isize,
    message: u32,
    wparam: usize,
    lparam: isize,
    time: u32,
    point: Point,
    private: u32,
}

extern "system" {
    fn GetModuleHandleW(name: *const u16) -> isize;
}

#[link(name = "user32")]
extern "system" {
    fn RegisterClassW(class: *const WndClassW) -> u16;
    fn CreateWindowExW(
        ex_style: u32,
        class: *const u16,
        name: *const u16,
        style: u32,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        parent: isize,
        menu: isize,
        instance: isize,
        param: *mut c_void,
    ) -> isize;
    fn DefWindowProcW(hwnd: isize, message: u32, wparam: usize, lparam: isize) -> isize;
    fn GetMessageW(msg: *mut Msg, hwnd: isize, min: u32, max: u32) -> i32;
    fn TranslateMessage(msg: *const Msg) -> i32;
    fn DispatchMessageW(msg: *const Msg) -> isize;
    fn PostQuitMessage(code: i32);
    fn DestroyWindow(hwnd: isize) -> i32;
    fn CreatePopupMenu() -> isize;
    fn AppendMenuW(menu: isize, flags: u32, id: usize, text: *const u16) -> i32;
    fn DestroyMenu(menu: isize) -> i32;
    fn TrackPopupMenu(
        menu: isize,
        flags: u32,
        x: i32,
        y: i32,
        reserved: i32,
        hwnd: isize,
        rect: *const c_void,
    ) -> i32;
    fn GetCursorPos(point: *mut Point) -> i32;
    fn SetForegroundWindow(hwnd: isize) -> i32;
    fn CreateIconIndirect(info: *mut IconInfo) -> isize;
    fn DestroyIcon(icon: isize) -> i32;
    fn PostMessageW(hwnd: isize, message: u32, wparam: usize, lparam: isize) -> i32;
    fn FindWindowExW(parent: isize, after: isize, class: *const u16, window: *const u16) -> isize;
}

#[link(name = "gdi32")]
extern "system" {
    fn CreateBitmap(
        width: i32,
        height: i32,
        planes: u32,
        bits_per_pixel: u32,
        bits: *const c_void,
    ) -> isize;
    fn DeleteObject(object: isize) -> i32;
}

#[link(name = "shell32")]
extern "system" {
    fn Shell_NotifyIconW(message: u32, data: *mut NotifyIconDataW) -> i32;
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(core::iter::once(0)).collect()
}

fn fill(buffer: &mut [u16], text: &str) {
    for (slot, value) in buffer.iter_mut().zip(text.encode_utf16()) {
        *slot = value;
    }
}

/// The tray icon, from the same artwork `build.rs` compiles into the
/// executable and `main` gives the window, so the three cannot drift apart.
unsafe fn draw_icon() -> isize {
    const SIZE: i32 = 32;
    let art = crate::iconart::rgba(SIZE as u32);

    // A 32 bit icon is premultiplied BGRA, while the artwork is straight RGBA.
    let mut pixels = vec![0u8; art.len()];
    for (out, src) in pixels.chunks_exact_mut(4).zip(art.chunks_exact(4)) {
        let alpha = src[3] as u32;
        out[0] = (src[2] as u32 * alpha / 255) as u8;
        out[1] = (src[1] as u32 * alpha / 255) as u8;
        out[2] = (src[0] as u32 * alpha / 255) as u8;
        out[3] = alpha as u8;
    }

    let colour = CreateBitmap(SIZE, SIZE, 1, 32, pixels.as_ptr() as *const c_void);
    // An all zero mask means "take every pixel from the colour bitmap".
    let mask_bits = vec![0u8; (SIZE * SIZE / 8) as usize];
    let mask = CreateBitmap(SIZE, SIZE, 1, 1, mask_bits.as_ptr() as *const c_void);

    let mut info = IconInfo {
        is_icon: 1,
        x_hotspot: 0,
        y_hotspot: 0,
        mask,
        colour,
    };
    let icon = CreateIconIndirect(&mut info);

    DeleteObject(colour);
    DeleteObject(mask);
    icon
}

fn send(event: TrayEvent) {
    if let Some(sender) = SENDER.get() {
        let _ = sender.send(event);
    }
}

extern "system" fn window_proc(hwnd: isize, message: u32, wparam: usize, lparam: isize) -> isize {
    unsafe {
        match message {
            WM_APP_TRAY => {
                match lparam as u32 {
                    WM_LBUTTONUP => send(TrayEvent::Open),
                    WM_RBUTTONUP => {
                        let menu = CreatePopupMenu();
                        if menu != 0 {
                            AppendMenuW(menu, MF_STRING, ID_OPEN, wide("Open G Deck").as_ptr());
                            AppendMenuW(menu, MF_SEPARATOR, 0, ptr::null());
                            AppendMenuW(menu, MF_STRING, ID_QUIT, wide("Quit").as_ptr());

                            let mut point = Point { x: 0, y: 0 };
                            GetCursorPos(&mut point);
                            // Documented requirement: without this the menu
                            // never closes when you click elsewhere.
                            SetForegroundWindow(hwnd);

                            let chosen = TrackPopupMenu(
                                menu,
                                TPM_RIGHTBUTTON | TPM_RETURNCMD,
                                point.x,
                                point.y,
                                0,
                                hwnd,
                                ptr::null(),
                            );
                            DestroyMenu(menu);

                            match chosen as usize {
                                ID_OPEN => send(TrayEvent::Open),
                                ID_QUIT => send(TrayEvent::Quit),
                                _ => {}
                            }
                        }
                    }
                    _ => {}
                }
                0
            }
            WM_APP_SHOW => {
                send(TrayEvent::Open);
                0
            }
            WM_COMMAND => {
                match wparam & 0xFFFF {
                    ID_OPEN => send(TrayEvent::Open),
                    ID_QUIT => send(TrayEvent::Quit),
                    _ => {}
                }
                0
            }
            WM_APP_CLOSE | WM_DESTROY => {
                PostQuitMessage(0);
                0
            }
            _ => DefWindowProcW(hwnd, message, wparam, lparam),
        }
    }
}

/// Ask a G Deck that is already running to show its window.
///
/// Called by a second launch, which then exits. Returns false when no
/// running copy could be found to tell, in which case there is nothing
/// useful this process can do except leave.
pub fn wake_existing() -> bool {
    unsafe {
        let class = wide("GDeckTrayWindow");
        let hwnd = FindWindowExW(HWND_MESSAGE, 0, class.as_ptr(), ptr::null());
        if hwnd == 0 {
            return false;
        }
        PostMessageW(hwnd, WM_APP_SHOW, 0, 0) != 0
    }
}

/// A handle that removes the icon when it is dropped.
pub struct Tray {
    hwnd: isize,
    /// Signalled once the icon has actually been removed.
    done: std::sync::mpsc::Receiver<()>,
}

impl Drop for Tray {
    fn drop(&mut self) {
        if self.hwnd == 0 {
            return;
        }
        unsafe { PostMessageW(self.hwnd, WM_APP_CLOSE, 0, 0) };
        // Wait for the removal to actually happen. Posting and walking away
        // lets the process exit first, which leaves a dead icon sitting in
        // the tray until something hovers over it. Bounded, so a wedged
        // message loop delays the exit rather than preventing it.
        let _ = self
            .done
            .recv_timeout(std::time::Duration::from_millis(750));
    }
}

/// Put an icon in the notification area and report clicks on it.
///
/// Returns a handle whose drop removes the icon, and the channel the window
/// procedure sends through.
pub fn start(tooltip: &str) -> Option<(Tray, std::sync::mpsc::Receiver<TrayEvent>)> {
    let (sender, receiver) = std::sync::mpsc::channel();
    if SENDER.set(sender).is_err() {
        return None;
    }

    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<isize>();
    let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
    let tooltip = tooltip.to_string();

    std::thread::spawn(move || unsafe {
        let class_name = wide("GDeckTrayWindow");
        let instance = GetModuleHandleW(ptr::null());

        let class = WndClassW {
            style: 0,
            proc_: Some(window_proc),
            cls_extra: 0,
            wnd_extra: 0,
            instance,
            icon: 0,
            cursor: 0,
            background: 0,
            menu_name: ptr::null(),
            class_name: class_name.as_ptr(),
        };
        RegisterClassW(&class);

        let hwnd = CreateWindowExW(
            0,
            class_name.as_ptr(),
            class_name.as_ptr(),
            0,
            0,
            0,
            0,
            0,
            HWND_MESSAGE,
            0,
            instance,
            ptr::null_mut(),
        );
        let _ = ready_tx.send(hwnd);
        if hwnd == 0 {
            return;
        }

        let icon = draw_icon();
        let mut data: NotifyIconDataW = core::mem::zeroed();
        data.cb_size = core::mem::size_of::<NotifyIconDataW>() as u32;
        data.hwnd = hwnd;
        data.id = 1;
        data.flags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
        data.callback_message = WM_APP_TRAY;
        data.icon = icon;
        fill(&mut data.tip, &tooltip);
        Shell_NotifyIconW(NIM_ADD, &mut data);

        let mut message: Msg = core::mem::zeroed();
        while GetMessageW(&mut message, 0, 0, 0) > 0 {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }

        Shell_NotifyIconW(NIM_DELETE, &mut data);
        if icon != 0 {
            DestroyIcon(icon);
        }
        DestroyWindow(hwnd);
        let _ = done_tx.send(());
    });

    let hwnd = ready_rx.recv().ok()?;
    if hwnd == 0 {
        return None;
    }
    Some((
        Tray {
            hwnd,
            done: done_rx,
        },
        receiver,
    ))
}
