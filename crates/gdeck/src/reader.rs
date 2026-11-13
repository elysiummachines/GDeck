//! The background reader: one thread, one handle, cancellable without polling.

use core::ptr;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};

use crate::device;
use crate::ffi::*;
use crate::keys::{decode_state, GKey, KeyState, G_KEY_COUNT, MIN_REPORT_LEN};
use crate::{Error, Result};

/// How long to wait before looking for the keyboard again after it goes away.
const RECONNECT_DELAY_MS: u32 = 1000;

/// Everything the reader reports upward.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Event {
    Connected,
    Disconnected,
    Pressed(GKey),
    Released(GKey),
}

/// Why a read loop ended.
enum Stop {
    /// `Reader::stop` was called, or the receiver was dropped.
    Cancelled,
    /// The handle failed. Usually the keyboard was unplugged.
    DeviceLost,
}

/// A running reader. Dropping it stops the thread and waits for it to finish.
pub struct Reader {
    cancel: isize,
    thread: Option<JoinHandle<()>>,
}

impl Reader {
    /// Spawn the reader thread. Returns immediately, whether or not the
    /// keyboard is currently attached: if it is missing, the thread waits for
    /// it and emits `Connected` when it appears.
    pub fn start() -> Result<(Reader, Receiver<Event>)> {
        // Manual reset, initially unsignalled.
        let cancel = unsafe { CreateEventW(ptr::null(), 1, 0, ptr::null()) };
        if cancel == 0 {
            return Err(Error::Win32 {
                what: "CreateEventW",
                code: unsafe { GetLastError() },
            });
        }

        let (tx, rx) = mpsc::channel();
        let thread = thread::spawn(move || run(cancel, tx));

        Ok((
            Reader {
                cancel,
                thread: Some(thread),
            },
            rx,
        ))
    }

    /// Signal the thread, wait for it, then release the event handle.
    ///
    /// The thread is blocked in `WaitForMultipleObjects` on the read event and
    /// this cancel event, so it wakes at once rather than after a poll
    /// interval. Idempotent.
    pub fn stop(&mut self) {
        if self.cancel == 0 {
            return;
        }
        unsafe {
            SetEvent(self.cancel);
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        unsafe {
            CloseHandle(self.cancel);
        }
        self.cancel = 0;
    }
}

impl Drop for Reader {
    fn drop(&mut self) {
        self.stop();
    }
}

/// True if cancellation has been signalled within `millis`.
fn cancelled(cancel: isize, millis: u32) -> bool {
    unsafe { WaitForSingleObject(cancel, millis) == WAIT_OBJECT_0 }
}

fn run(cancel: isize, tx: Sender<Event>) {
    loop {
        if cancelled(cancel, 0) {
            return;
        }

        let opened = device::find_g_key_pipe()
            .and_then(|collection| {
                device::open_for_read(&collection)
                    .ok()
                    .map(|handle| (handle, collection.input_len))
            });

        if let Some((handle, input_len)) = opened {
            if tx.send(Event::Connected).is_err() {
                unsafe { CloseHandle(handle) };
                return;
            }

            let stop = read_loop(handle, input_len, cancel, &tx);
            unsafe { CloseHandle(handle) };

            match stop {
                Stop::Cancelled => return,
                Stop::DeviceLost => {
                    let _ = tx.send(Event::Disconnected);
                }
            }
        }

        // Nothing to read from. Back off, but stay instantly cancellable.
        if cancelled(cancel, RECONNECT_DELAY_MS) {
            return;
        }
    }
}

fn read_loop(handle: isize, input_len: usize, cancel: isize, tx: &Sender<Event>) -> Stop {
    let read_event = unsafe { CreateEventW(ptr::null(), 1, 0, ptr::null()) };
    if read_event == 0 {
        return Stop::DeviceLost;
    }

    let mut buf = vec![0u8; input_len.max(MIN_REPORT_LEN)];
    let mut state: KeyState = 0;

    let outcome = loop {
        let mut overlapped = Overlapped::new(read_event);
        let mut transferred = 0u32;

        unsafe { ResetEvent(read_event) };

        let completed_inline = unsafe {
            ReadFile(
                handle,
                buf.as_mut_ptr(),
                buf.len() as u32,
                &mut transferred,
                &mut overlapped,
            )
        };

        let len: usize = if completed_inline != 0 {
            transferred as usize
        } else {
            if unsafe { GetLastError() } != ERROR_IO_PENDING {
                break Stop::DeviceLost;
            }

            let handles = [read_event, cancel];
            let which = unsafe { WaitForMultipleObjects(2, handles.as_ptr(), 0, INFINITE) };

            if which == WAIT_OBJECT_0 {
                let mut got = 0u32;
                if unsafe { GetOverlappedResult(handle, &mut overlapped, &mut got, 0) } == 0 {
                    break Stop::DeviceLost;
                }
                got as usize
            } else if which == WAIT_OBJECT_0 + 1 {
                // Cancel the pending read and wait for the cancellation to
                // land. `overlapped` lives on this stack frame, and the kernel
                // writes to it on completion, so returning before it finishes
                // would let it write into a dead frame.
                unsafe {
                    CancelIoEx(handle, &mut overlapped);
                    let mut got = 0u32;
                    GetOverlappedResult(handle, &mut overlapped, &mut got, 1);
                }
                break Stop::Cancelled;
            } else {
                break Stop::DeviceLost;
            }
        };

        // Anything that is not a well formed report on this pipe is skipped
        // rather than treated as "all keys released".
        if let Some(next) = decode_state(&buf[..len]) {
            if next != state {
                if !emit_transitions(state, next, tx) {
                    break Stop::Cancelled;
                }
                state = next;
            }
        }
    };

    unsafe { CloseHandle(read_event) };
    outcome
}

/// Turn a change of state into press and release events.
///
/// Returns false if the receiver has gone away.
fn emit_transitions(previous: KeyState, next: KeyState, tx: &Sender<Event>) -> bool {
    let changed = previous ^ next;

    for index in 0..G_KEY_COUNT {
        let bit = 1u32 << index;
        if changed & bit == 0 {
            continue;
        }
        let key = match GKey::from_index(index) {
            Some(key) => key,
            None => continue,
        };
        let event = if next & bit != 0 {
            Event::Pressed(key)
        } else {
            Event::Released(key)
        };
        if tx.send(event).is_err() {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect(previous: KeyState, next: KeyState) -> Vec<Event> {
        let (tx, rx) = mpsc::channel();
        assert!(emit_transitions(previous, next, &tx));
        drop(tx);
        rx.into_iter().collect()
    }

    #[test]
    fn a_press_emits_once() {
        let events = collect(0b000, 0b001);
        assert_eq!(events, vec![Event::Pressed(GKey::from_index(0).unwrap())]);
    }

    #[test]
    fn a_release_emits_once() {
        let events = collect(0b001, 0b000);
        assert_eq!(events, vec![Event::Released(GKey::from_index(0).unwrap())]);
    }

    #[test]
    fn an_identical_report_emits_nothing() {
        // Duplicate reports were observed 1 ms apart on the wire. Because the
        // format is a state snapshot, they must be a no op for free.
        assert!(collect(0b101, 0b101).is_empty());
    }

    #[test]
    fn simultaneous_changes_all_emit() {
        // G1 and G2 held, then G1 released while G3 goes down.
        let events = collect(0b011, 0b110);
        assert_eq!(events.len(), 2);
        assert!(events.contains(&Event::Released(GKey::from_index(0).unwrap())));
        assert!(events.contains(&Event::Pressed(GKey::from_index(2).unwrap())));
    }

    #[test]
    fn all_eighteen_can_be_held_at_once() {
        let all = (1u32 << G_KEY_COUNT) - 1;
        let events = collect(0, all);
        assert_eq!(events.len(), G_KEY_COUNT);
    }
}
