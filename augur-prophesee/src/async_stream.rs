//! Asynchronous multi-URB bulk stream reader.
//!
//! The synchronous `read_bulk` path has one transfer in flight at a time:
//! between two calls no URB is queued, so the device buffers events in its
//! limited FIFO and can overflow at high rates. This reader keeps
//! `QUEUED_TRANSFERS` bulk transfers submitted at all times, so the kernel
//! fills the next buffer while the pipeline processes the previous one and
//! there is no host-side dead time between reads.
//!
//! Threading model: all submission, reaping, and event handling happen on the
//! thread that owns the reader (the pipeline's stream thread). libusb may
//! invoke the completion callback from another thread that happens to be
//! pumping events for the same context (the control thread's synchronous
//! transfers do this), so the completion queue is mutex-protected. Ordering
//! is preserved because bulk IN URBs on one endpoint complete in submission
//! order and libusb fires callbacks in completion order.

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
    buffer: Option<Box<[u8; TRANSFER_BUF_SIZE]>>,
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
}

// Safety: the reader is used from one thread at a time (`read_packet` takes
// `&mut self`). The raw transfer pointers are only dereferenced by that
// owning thread; libusb callbacks touch nothing but the mutex-protected
// completion queue. The device handle itself is `Send + Sync` via `Transport`.
unsafe impl Send for AsyncBulkStreamReader {}

impl AsyncBulkStreamReader {
    pub fn new(transport: Transport, queued_transfers: usize) -> Result<Self> {
        let queue = Box::new(CompletionQueue {
            completed: Mutex::new(VecDeque::with_capacity(queued_transfers)),
        });
        let mut reader = Self {
            transport,
            queue: Some(queue),
            slots: Vec::with_capacity(queued_transfers),
        };

        for index in 0..queued_transfers.max(1) {
            // Safety: libusb_alloc_transfer returns an owned transfer or null.
            let transfer = unsafe { ffi::libusb_alloc_transfer(0) };
            if transfer.is_null() {
                return Err(CameraError::Transport(
                    "libusb_alloc_transfer failed".into(),
                ));
            }
            reader.slots.push(TransferSlot {
                transfer,
                buffer: Some(Box::new([0_u8; TRANSFER_BUF_SIZE])),
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
        let buffer = slot
            .buffer
            .as_mut()
            .expect("transfer buffer is present unless leaked on drop");
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
        let tv = libc::timeval {
            tv_sec: wait.as_secs() as libc::time_t,
            tv_usec: wait.subsec_micros() as libc::suseconds_t,
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

    fn consume(&mut self, index: usize, out: &mut [u8]) -> Result<usize> {
        self.slots[index].in_flight = false;
        // Safety: the transfer is reaped (callback fired), so libusb no
        // longer touches it and its fields are stable.
        let (status, actual_length) = unsafe {
            let transfer = &*self.slots[index].transfer;
            (transfer.status, transfer.actual_length.max(0) as usize)
        };

        match status {
            LIBUSB_TRANSFER_COMPLETED | LIBUSB_TRANSFER_TIMED_OUT => {
                let len = actual_length.min(out.len());
                if len > 0 {
                    let buffer = self.slots[index]
                        .buffer
                        .as_ref()
                        .expect("reaped transfer still owns its buffer");
                    out[..len].copy_from_slice(&buffer[..len]);
                }
                self.submit(index)?;
                if len == 0 {
                    return Err(CameraError::Timeout("USB stream read timed out".into()));
                }
                Ok(len)
            }
            LIBUSB_TRANSFER_CANCELLED => {
                Err(CameraError::Timeout("stream transfer cancelled".into()))
            }
            LIBUSB_TRANSFER_STALL => Err(CameraError::Transport("stream endpoint stalled".into())),
            LIBUSB_TRANSFER_NO_DEVICE => {
                Err(CameraError::Transport("USB device disconnected".into()))
            }
            other => Err(CameraError::Transport(format!(
                "stream transfer failed with status {other}"
            ))),
        }
    }
}

impl PacketStreamReader for AsyncBulkStreamReader {
    fn read_packet(&mut self, out: &mut [u8]) -> Result<usize> {
        let deadline = Instant::now() + self.transport.stream_timeout();
        loop {
            if let Some(index) = self.pop_completed() {
                return self.consume(index, out);
            }
            if Instant::now() >= deadline {
                return Err(CameraError::Timeout("USB stream read timed out".into()));
            }
            self.pump_events(EVENT_POLL_STEP)?;
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
