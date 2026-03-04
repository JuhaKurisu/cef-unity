// IPC protocol definitions and shared memory layout for CEF Server <-> Unity communication.
//
// Transport: ipc-channel (Mach ports on macOS)
// Pixel data: shared_memory crate (POSIX shm)

use ipc_channel::ipc::{IpcReceiver, IpcSender};
use serde::{Deserialize, Serialize};
use shared_memory::{Shmem, ShmemConf};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

// ---------------------------------------------------------------------------
// Wire protocol
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Debug)]
pub enum Command {
    CreateBrowser {
        width: i32,
        height: i32,
        url: String,
    },
    DestroyBrowser {
        browser_id: u32,
    },
    LoadUrl {
        browser_id: u32,
        url: String,
    },
    Resize {
        browser_id: u32,
        width: i32,
        height: i32,
    },
    Shutdown,
}

#[derive(Serialize, Deserialize, Debug)]
pub enum Response {
    BrowserCreated { browser_id: u32, shm_flink: String },
    Ok,
    Error { msg: String },
}

/// Sent from server to client during bootstrap to establish bidirectional channels.
#[derive(Serialize, Deserialize, Debug)]
pub struct Bootstrap {
    pub cmd_tx: IpcSender<Command>,
    pub resp_rx: IpcReceiver<Response>,
}

// ---------------------------------------------------------------------------
// Shared memory layout (per browser)
// ---------------------------------------------------------------------------

pub const MAX_W: u32 = 3840;
pub const MAX_H: u32 = 2160;
pub const BUFFER_SIZE: usize = (MAX_W * MAX_H * 4) as usize;
pub const SHM_HEADER_SIZE: usize = 64;
pub const SHM_TOTAL_SIZE: usize = SHM_HEADER_SIZE + BUFFER_SIZE * 2;

/// Header at offset 0 of shared memory. 64 bytes, cache-line aligned.
#[repr(C, align(64))]
pub struct ShmHeader {
    pub frame_id: AtomicU64,
    pub width: AtomicU32,
    pub height: AtomicU32,
    /// 0 or 1 — which buffer is currently readable
    pub active_buffer: AtomicU32,
    pub _pad: [u8; 44],
}

const _: () = assert!(std::mem::size_of::<ShmHeader>() == SHM_HEADER_SIZE);

/// Generate a shared memory flink path for a browser.
pub fn shm_flink_path(server_pid: u32, browser_id: u32) -> String {
    let tmp = std::env::temp_dir();
    tmp.join(format!("cef-unity-shm-{}-{}", server_pid, browser_id))
        .to_str()
        .unwrap()
        .to_string()
}

// ---------------------------------------------------------------------------
// High-level shared memory wrappers (shared_memory crate)
// ---------------------------------------------------------------------------

/// Server-side handle for writing frames into shared memory.
pub struct ShmWriter {
    shmem: Shmem,
    pub flink: String,
}

unsafe impl Send for ShmWriter {}

impl ShmWriter {
    pub fn new(flink: &str) -> std::io::Result<Self> {
        // Remove stale flink file
        let _ = std::fs::remove_file(flink);

        let shmem = ShmemConf::new()
            .size(SHM_TOTAL_SIZE)
            .flink(flink)
            .create()
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;

        // Zero-initialize header
        unsafe {
            std::ptr::write_bytes(shmem.as_ptr(), 0, SHM_HEADER_SIZE);
        }

        Ok(ShmWriter {
            shmem,
            flink: flink.to_string(),
        })
    }

    fn header(&self) -> &ShmHeader {
        unsafe { &*(self.shmem.as_ptr() as *const ShmHeader) }
    }

    /// Write a frame. The buffer must be width*height*4 BGRA bytes.
    pub fn write_frame(&self, pixels: &[u8], width: u32, height: u32) {
        let size = (width * height * 4) as usize;
        assert!(size <= BUFFER_SIZE);
        assert_eq!(pixels.len(), size);

        let header = self.header();
        // Write to inactive buffer
        let active = header.active_buffer.load(Ordering::Acquire);
        let target = if active == 0 { 1 } else { 0 };
        let offset = SHM_HEADER_SIZE + target as usize * BUFFER_SIZE;

        unsafe {
            let dst = self.shmem.as_ptr().add(offset);
            std::ptr::copy_nonoverlapping(pixels.as_ptr(), dst, size);
        }

        header.width.store(width, Ordering::Release);
        header.height.store(height, Ordering::Release);
        header.active_buffer.store(target, Ordering::Release);
        header.frame_id.fetch_add(1, Ordering::Release);
    }
}

/// Client-side handle for reading frames from shared memory.
pub struct ShmReader {
    shmem: Shmem,
    pub last_frame_id: u64,
}

unsafe impl Send for ShmReader {}

impl ShmReader {
    pub fn open(flink: &str) -> std::io::Result<Self> {
        let shmem = ShmemConf::new()
            .flink(flink)
            .open()
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;

        Ok(ShmReader {
            shmem,
            last_frame_id: 0,
        })
    }

    /// Check if there's a new frame. If so, copy it into `dst` and return (width, height).
    /// Returns None if no new frame since last call.
    pub fn read_frame(&mut self, dst: &mut Vec<u8>) -> Option<(u32, u32)> {
        let header = unsafe { &*(self.shmem.as_ptr() as *const ShmHeader) };
        let frame_id = header.frame_id.load(Ordering::Acquire);
        if frame_id == self.last_frame_id {
            return None;
        }
        self.last_frame_id = frame_id;

        let active = header.active_buffer.load(Ordering::Acquire);
        let width = header.width.load(Ordering::Acquire);
        let height = header.height.load(Ordering::Acquire);
        let size = (width * height * 4) as usize;
        if size == 0 || size > BUFFER_SIZE {
            return None;
        }

        let offset = SHM_HEADER_SIZE + active as usize * BUFFER_SIZE;
        dst.resize(size, 0);
        unsafe {
            let src = self.shmem.as_ptr().add(offset);
            std::ptr::copy_nonoverlapping(src, dst.as_mut_ptr(), size);
        }
        Some((width, height))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ipc_channel::ipc;

    #[test]
    fn shm_write_read_roundtrip() {
        let flink = std::env::temp_dir()
            .join("cef-unity-test-shm-roundtrip")
            .to_str()
            .unwrap()
            .to_string();

        let writer = ShmWriter::new(&flink).expect("ShmWriter::new");
        let mut reader = ShmReader::open(&flink).expect("ShmReader::open");

        // No frame yet
        let mut buf = Vec::new();
        assert!(reader.read_frame(&mut buf).is_none());

        // Write a 2x2 BGRA frame (16 bytes)
        let pixels: Vec<u8> = (0..16).collect();
        writer.write_frame(&pixels, 2, 2);

        // Read it back
        let result = reader.read_frame(&mut buf);
        assert_eq!(result, Some((2, 2)));
        assert_eq!(buf, pixels);

        // No new frame if we read again
        assert!(reader.read_frame(&mut buf).is_none());

        // Write another frame, verify double-buffer swap
        let pixels2: Vec<u8> = (16..32).collect();
        writer.write_frame(&pixels2, 2, 2);
        let result2 = reader.read_frame(&mut buf);
        assert_eq!(result2, Some((2, 2)));
        assert_eq!(buf, pixels2);
    }

    #[test]
    fn ipc_channel_command_roundtrip() {
        let (tx, rx) = ipc::channel::<Command>().unwrap();
        tx.send(Command::CreateBrowser {
            width: 800,
            height: 600,
            url: "https://example.com".to_string(),
        })
        .unwrap();
        let cmd = rx.recv().unwrap();
        match cmd {
            Command::CreateBrowser { width, height, url } => {
                assert_eq!(width, 800);
                assert_eq!(height, 600);
                assert_eq!(url, "https://example.com");
            }
            _ => panic!("unexpected command variant"),
        }
    }

    #[test]
    fn ipc_channel_response_roundtrip() {
        let (tx, rx) = ipc::channel::<Response>().unwrap();
        tx.send(Response::BrowserCreated {
            browser_id: 42,
            shm_flink: "/tmp/test-shm".to_string(),
        })
        .unwrap();
        let resp = rx.recv().unwrap();
        match resp {
            Response::BrowserCreated {
                browser_id,
                shm_flink,
            } => {
                assert_eq!(browser_id, 42);
                assert_eq!(shm_flink, "/tmp/test-shm");
            }
            _ => panic!("unexpected response variant"),
        }
    }

    #[test]
    fn ipc_bootstrap_roundtrip() {
        // IpcOneShotServer bootstrap requires separate processes on macOS (Mach ports).
        // Here we test the channel transfer pattern using regular IPC channels instead.
        let (bootstrap_tx, bootstrap_rx) = ipc::channel::<Bootstrap>().unwrap();

        let (cmd_tx, cmd_rx) = ipc::channel::<Command>().unwrap();
        let (resp_tx, resp_rx) = ipc::channel::<Response>().unwrap();

        // Simulate server sending bootstrap
        bootstrap_tx.send(Bootstrap { cmd_tx, resp_rx }).unwrap();

        // Client receives bootstrap
        let bootstrap = bootstrap_rx.recv().unwrap();

        // Send a command through the bootstrapped channel
        bootstrap.cmd_tx.send(Command::Shutdown).unwrap();
        let cmd = cmd_rx.recv().unwrap();
        assert!(matches!(cmd, Command::Shutdown));

        // Send a response back
        resp_tx.send(Response::Ok).unwrap();
        let resp = bootstrap.resp_rx.recv().unwrap();
        assert!(matches!(resp, Response::Ok));
    }
}
