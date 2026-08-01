//! Asynchronous multi-URB bulk stream reader.
//!
//! The synchronous `read_bulk` path has one transfer in flight at a time:
//! between two calls no URB is queued, so the device buffers events in its
//! limited FIFO and can overflow at high rates. This reader keeps
//! `QUEUED_TRANSFERS` bulk transfers submitted at all times, so the kernel
//! fills the next buffer while the pipeline processes the previous one and
//! there is no host-side dead time between reads.
//!
//! Buffer ownership: the reader owns both the transfer buffers and a pool of
//! spares. A completed transfer is re-armed with a spare *immediately*, and its
//! filled buffer is queued for delivery. Re-arming therefore never waits for
//! the pipeline to hand a buffer back, which matters because the pipeline can
//! stall (slow disk) for far longer than the device FIFO survives. While the
//! pipeline has no free buffer it calls [`AsyncBulkStreamReader::service`],
//! which keeps reaping and re-arming so the endpoint stays fed.
//!
//! Threading model: all submission, reaping, and event handling happen on the
//! thread that owns the reader (the pipeline's stream thread). libusb may
//! invoke the completion callback from another thread that happens to be
//! pumping events for the same context (the control thread's synchronous
//! transfers do this), so the completion queue is mutex-protected. Ordering
//! is preserved because bulk IN URBs on one endpoint complete in submission
//! order, libusb fires callbacks in completion order, and a re-armed transfer
//! always goes to the tail of the endpoint's queue.

use std::{
    collections::VecDeque,
    os::raw::c_void,
    sync::Mutex,
    time::{Duration, Instant},
};

use augur_core::{camera::PacketStreamReader, CameraError, Result};
use rusb::constants::{
    LIBUSB_ERROR_INTERRUPTED, LIBUSB_TRANSFER_CANCELLED, LIBUSB_TRANSFER_COMPLETED,
    LIBUSB_TRANSFER_NO_DEVICE, LIBUSB_TRANSFER_STALL, LIBUSB_TRANSFER_TIMED_OUT,
};
use rusb::ffi;

use crate::transport::Transport;

pub const QUEUED_TRANSFERS: usize = 8;
const TRANSFER_BUF_SIZE: usize = 65_536;
const TRANSFER_TIMEOUT_MS: u32 = 100;
const EVENT_POLL_STEP: Duration = Duration::from_millis(10);
const DROP_DRAIN_DEADLINE: Duration = Duration::from_secs(2);

type StreamBuffer = Box<[u8; TRANSFER_BUF_SIZE]>;

fn new_stream_buffer() -> StreamBuffer {
    Box::new([0_u8; TRANSFER_BUF_SIZE])
}

struct CompletionQueue {
    completed: Mutex<VecDeque<usize>>,
}

impl CompletionQueue {
    fn push(&self, index: usize) {
        self.completed
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push_back(index);
    }

    fn pop(&self) -> Option<usize> {
        self.completed
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .pop_front()
    }
}

/// `user_data` payload for one transfer. Boxed so its address is stable for
/// the lifetime of the transfer.
struct CallbackTag {
    queue: *const CompletionQueue,
    index: usize,
}

extern "system" fn stream_transfer_done(transfer: *mut ffi::libusb_transfer) {
    // Safety: `user_data` points to a live `CallbackTag`, and the tagged
    // `CompletionQueue` outlives every submitted transfer (both are leaked if
    // a transfer cannot be reaped on drop).
    unsafe {
        let tag = &*((*transfer).user_data as *const CallbackTag);
        (*tag.queue).push(tag.index);
    }
}

struct TransferSlot {
    transfer: *mut ffi::libusb_transfer,
    /// `None` while the slot's buffer is queued for delivery and no spare was
    /// available to take its place; such a slot is re-armed as soon as a
    /// buffer is recycled.
    buffer: Option<StreamBuffer>,
    tag: Option<Box<CallbackTag>>,
    in_flight: bool,
}

/// Keeps `QUEUED_TRANSFERS` bulk IN transfers pending on the stream endpoint
/// and hands completed buffers to `read_packet` in completion order.
pub struct AsyncBulkStreamReader {
    // Keeps the device handle (and libusb context) alive.
    transport: Transport,
    queue: Option<Box<CompletionQueue>>,
    slots: Vec<TransferSlot>,
    /// Buffers available to re-arm a reaped transfer.
    spare: Vec<StreamBuffer>,
    /// Received payloads awaiting delivery, in completion order.
    ready: VecDeque<(StreamBuffer, usize)>,
    /// Sticky transport failure. Reported only once every already-received
    /// packet has been delivered, so a failing device never costs data the
    /// host already holds.
    fatal: Option<String>,
}

// Safety: the reader is used from one thread at a time (`read_packet` takes
// `&mut self`). The raw transfer pointers are only dereferenced by that
// owning thread; libusb callbacks touch nothing but the mutex-protected
// completion queue. The device handle itself is `Send + Sync` via `Transport`.
unsafe impl Send for AsyncBulkStreamReader {}

impl AsyncBulkStreamReader {
    pub fn new(transport: Transport, queued_transfers: usize) -> Result<Self> {
        let queued_transfers = queued_transfers.max(1);
        let queue = Box::new(CompletionQueue {
            completed: Mutex::new(VecDeque::with_capacity(queued_transfers)),
        });
        let mut reader = Self {
            transport,
            queue: Some(queue),
            slots: Vec::with_capacity(queued_transfers),
            // One spare per transfer: a full round of completions can be
            // re-armed without the pipeline handing anything back.
            spare: (0..queued_transfers).map(|_| new_stream_buffer()).collect(),
            ready: VecDeque::with_capacity(queued_transfers * 2),
            fatal: None,
        };

        for index in 0..queued_transfers {
            // Safety: libusb_alloc_transfer returns an owned transfer or null.
            let transfer = unsafe { ffi::libusb_alloc_transfer(0) };
            if transfer.is_null() {
                return Err(CameraError::Transport(
                    "libusb_alloc_transfer failed".into(),
                ));
            }
            reader.slots.push(TransferSlot {
                transfer,
                buffer: Some(new_stream_buffer()),
                tag: Some(Box::new(CallbackTag {
                    queue: reader
                        .queue
                        .as_deref()
                        .map(|queue| queue as *const CompletionQueue)
                        .expect("queue is present during construction"),
                    index,
                })),
                in_flight: false,
            });
            reader.submit(index)?;
        }
        Ok(reader)
    }

    fn submit(&mut self, index: usize) -> Result<()> {
        let raw_handle = self.transport.raw_handle();
        let endpoint = self.transport.stream_endpoint();
        let slot = &mut self.slots[index];
        debug_assert!(!slot.in_flight, "transfer resubmitted while in flight");
        let Some(buffer) = slot.buffer.as_mut() else {
            // The slot's buffer is queued for delivery and no spare was free.
            // `rearm_idle_slots` picks it up once one is recycled.
            return Ok(());
        };
        let tag = slot
            .tag
            .as_ref()
            .expect("transfer tag is present unless leaked on drop");
        // Safety: transfer, handle, buffer, and tag are all live; the buffer
        // and tag stay alive (or are deliberately leaked) until the transfer
        // is reaped.
        unsafe {
            ffi::libusb_fill_bulk_transfer(
                slot.transfer,
                raw_handle,
                endpoint,
                buffer.as_mut_ptr(),
                TRANSFER_BUF_SIZE as i32,
                stream_transfer_done,
                (tag.as_ref() as *const CallbackTag as *mut CallbackTag).cast::<c_void>(),
                TRANSFER_TIMEOUT_MS,
            );
            let rc = ffi::libusb_submit_transfer(slot.transfer);
            if rc != 0 {
                return Err(CameraError::Transport(format!(
                    "libusb_submit_transfer failed with {rc}"
                )));
            }
        }
        slot.in_flight = true;
        Ok(())
    }

    fn pop_completed(&self) -> Option<usize> {
        self.queue.as_deref().and_then(CompletionQueue::pop)
    }

    fn pump_events(&self, wait: Duration) -> Result<()> {
        // `timeval`'s field types differ across platforms - `time_t`/`suseconds_t`
        // on unix, `c_long` on Windows - so let inference pick them rather than
        // naming types that only exist on one of the two.
        let tv = libc::timeval {
            tv_sec: wait.as_secs() as _,
            tv_usec: wait.subsec_micros() as _,
        };
        // Safety: the context outlives the reader via the shared transport.
        let rc = unsafe { ffi::libusb_handle_events_timeout(self.transport.raw_context(), &tv) };
        if rc < 0 && rc != LIBUSB_ERROR_INTERRUPTED {
            return Err(CameraError::Transport(format!(
                "libusb event handling failed with {rc}"
            )));
        }
        Ok(())
    }

    /// Collects every completion the kernel already reported: queues the
    /// received bytes for delivery and puts the transfer back on the endpoint.
    ///
    /// Never blocks and never waits for a downstream buffer, so it is safe to
    /// call while the pipeline is stalled.
    fn reap(&mut self) {
        while let Some(index) = self.pop_completed() {
            self.slots[index].in_flight = false;
            // Safety: the transfer is reaped (callback fired), so libusb no
            // longer touches it and its fields are stable.
            let (status, actual_length) = unsafe {
                let transfer = &*self.slots[index].transfer;
                (transfer.status, transfer.actual_length.max(0) as usize)
            };

            match status {
                // A timed-out or cancelled bulk IN transfer still returns the
                // bytes the device already sent; discarding them would punch a
                // hole in the recording.
                LIBUSB_TRANSFER_COMPLETED
                | LIBUSB_TRANSFER_TIMED_OUT
                | LIBUSB_TRANSFER_CANCELLED => {
                    if actual_length > 0 {
                        let replacement = self.spare.pop();
                        let filled = match replacement {
                            Some(replacement) => self.slots[index].buffer.replace(replacement),
                            None => self.slots[index].buffer.take(),
                        };
                        if let Some(filled) = filled {
                            self.ready.push_back((filled, actual_length));
                        }
                    }
                }
                LIBUSB_TRANSFER_STALL => {
                    self.set_fatal("stream endpoint stalled");
                    continue;
                }
                LIBUSB_TRANSFER_NO_DEVICE => {
                    self.set_fatal("USB device disconnected");
                    continue;
                }
                other => {
                    self.set_fatal(&format!("stream transfer failed with status {other}"));
                    continue;
                }
            }

            if status == LIBUSB_TRANSFER_CANCELLED {
                // Cancellation only happens while tearing the reader down; do
                // not put the transfer back on the endpoint.
                continue;
            }
            if let Err(err) = self.submit(index) {
                self.set_fatal(&err.to_string());
            }
        }
    }

    fn set_fatal(&mut self, message: &str) {
        if self.fatal.is_none() {
            self.fatal = Some(message.to_owned());
        }
    }

    /// Returns a delivered buffer to the spare pool and re-arms any transfer
    /// that had to give its buffer up.
    fn recycle(&mut self, buffer: StreamBuffer) {
        self.spare.push(buffer);
        self.rearm_idle_slots();
    }

    fn rearm_idle_slots(&mut self) {
        for index in 0..self.slots.len() {
            if self.spare.is_empty() {
                break;
            }
            let slot = &mut self.slots[index];
            if slot.in_flight || slot.buffer.is_some() {
                continue;
            }
            slot.buffer = self.spare.pop();
            if let Err(err) = self.submit(index) {
                self.set_fatal(&err.to_string());
            }
        }
    }

    /// Moves the oldest received payload into `out`, if one is queued.
    fn deliver(&mut self, out: &mut [u8]) -> Option<Result<usize>> {
        let (buffer, len) = self.ready.pop_front()?;
        if len > out.len() {
            // Truncating here would silently drop recorded events. The caller's
            // buffer is sized to `TRANSFER_BUF_SIZE`, so this is a wiring bug.
            let capacity = out.len();
            self.recycle(buffer);
            return Some(Err(CameraError::Transport(format!(
                "stream packet of {len} bytes does not fit the {capacity}-byte pipeline buffer"
            ))));
        }
        out[..len].copy_from_slice(&buffer[..len]);
        self.recycle(buffer);
        Some(Ok(len))
    }

    fn take_fatal(&mut self) -> Option<CameraError> {
        self.fatal.take().map(CameraError::Transport)
    }
}

impl PacketStreamReader for AsyncBulkStreamReader {
    fn read_packet(&mut self, out: &mut [u8]) -> Result<usize> {
        let deadline = Instant::now() + self.transport.stream_timeout();
        loop {
            self.reap();
            if let Some(result) = self.deliver(out) {
                return result;
            }
            // Only surface a transport failure once nothing received is left.
            if let Some(err) = self.take_fatal() {
                return Err(err);
            }
            if Instant::now() >= deadline {
                return Err(CameraError::Timeout("USB stream read timed out".into()));
            }
            self.pump_events(EVENT_POLL_STEP)?;
        }
    }

    fn service(&mut self, budget: Duration) {
        // Errors are recorded and reported by the next `read_packet`; there is
        // nothing useful to do with them here, and the point of this call is to
        // keep the endpoint fed.
        if let Err(err) = self.pump_events(budget.min(EVENT_POLL_STEP)) {
            self.set_fatal(&err.to_string());
        }
        self.reap();
    }

    fn take_buffered_packet(&mut self, out: &mut [u8]) -> Result<usize> {
        // Non-blocking: collect what the kernel already finished, then hand
        // over one queued payload.
        let _ = self.pump_events(Duration::ZERO);
        self.reap();
        match self.deliver(out) {
            Some(result) => result,
            None => Ok(0),
        }
    }
}

impl Drop for AsyncBulkStreamReader {
    fn drop(&mut self) {
        // Cancel everything still pending and reap the cancellations before
        // freeing. A transfer the kernel may still write into must never be
        // freed; if draining times out, leak its resources instead.
        unsafe {
            for slot in &self.slots {
                if slot.in_flight {
                    ffi::libusb_cancel_transfer(slot.transfer);
                }
            }
            let deadline = Instant::now() + DROP_DRAIN_DEADLINE;
            while self.slots.iter().any(|slot| slot.in_flight) && Instant::now() < deadline {
                if self.pump_events(EVENT_POLL_STEP).is_err() {
                    break;
                }
                while let Some(index) = self.pop_completed() {
                    self.slots[index].in_flight = false;
                }
            }
            let mut leak_queue = false;
            for slot in &mut self.slots {
                if slot.in_flight {
                    // The kernel may still own this transfer: leak transfer,
                    // buffer, and tag so nothing dangles if it ever completes.
                    std::mem::forget(slot.buffer.take());
                    std::mem::forget(slot.tag.take());
                    leak_queue = true;
                } else {
                    ffi::libusb_free_transfer(slot.transfer);
                }
            }
            if leak_queue {
                std::mem::forget(self.queue.take());
            }
        }
    }
}
