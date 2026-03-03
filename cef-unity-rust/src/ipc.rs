// IPC protocol definitions and shared memory layout for CEF Server <-> Unity communication.
//
// Transport: Unix domain socket (length-prefixed messages)
// Pixel data: File-backed mmap (double-buffered, lock-free)

use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicU32, AtomicU64};

// ---------------------------------------------------------------------------
// Wire protocol
// ---------------------------------------------------------------------------

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandTag {
    CreateBrowser = 1,
    DestroyBrowser = 2,
    LoadUrl = 3,
    Resize = 4,
    Shutdown = 5,
}

#[derive(Debug)]
pub enum Command {
    CreateBrowser { width: i32, height: i32, url: String },
    DestroyBrowser { browser_id: u32 },
    LoadUrl { browser_id: u32, url: String },
    Resize { browser_id: u32, width: i32, height: i32 },
    Shutdown,
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResponseTag {
    BrowserCreated = 1,
    Ok = 2,
    Error = 3,
}

#[derive(Debug)]
pub enum Response {
    BrowserCreated { browser_id: u32, shm_name: String },
    Ok,
    Error { msg: String },
}

// ---------------------------------------------------------------------------
// Serialization helpers
// ---------------------------------------------------------------------------

fn write_u32(buf: &mut Vec<u8>, v: u32) {
    buf.extend_from_slice(&v.to_le_bytes());
}

fn write_i32(buf: &mut Vec<u8>, v: i32) {
    buf.extend_from_slice(&v.to_le_bytes());
}

fn write_str(buf: &mut Vec<u8>, s: &str) {
    write_u32(buf, s.len() as u32);
    buf.extend_from_slice(s.as_bytes());
}

fn read_u32(data: &[u8], off: &mut usize) -> io::Result<u32> {
    if *off + 4 > data.len() {
        return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "u32"));
    }
    let v = u32::from_le_bytes(data[*off..*off + 4].try_into().unwrap());
    *off += 4;
    Ok(v)
}

fn read_i32(data: &[u8], off: &mut usize) -> io::Result<i32> {
    if *off + 4 > data.len() {
        return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "i32"));
    }
    let v = i32::from_le_bytes(data[*off..*off + 4].try_into().unwrap());
    *off += 4;
    Ok(v)
}

fn read_str(data: &[u8], off: &mut usize) -> io::Result<String> {
    let len = read_u32(data, off)? as usize;
    if *off + len > data.len() {
        return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "str"));
    }
    let s = std::str::from_utf8(&data[*off..*off + len])
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?
        .to_string();
    *off += len;
    Ok(s)
}

// ---------------------------------------------------------------------------
// Command serialization
// ---------------------------------------------------------------------------

impl Command {
    pub fn serialize(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(64);
        match self {
            Command::CreateBrowser { width, height, url } => {
                buf.push(CommandTag::CreateBrowser as u8);
                write_i32(&mut buf, *width);
                write_i32(&mut buf, *height);
                write_str(&mut buf, url);
            }
            Command::DestroyBrowser { browser_id } => {
                buf.push(CommandTag::DestroyBrowser as u8);
                write_u32(&mut buf, *browser_id);
            }
            Command::LoadUrl { browser_id, url } => {
                buf.push(CommandTag::LoadUrl as u8);
                write_u32(&mut buf, *browser_id);
                write_str(&mut buf, url);
            }
            Command::Resize { browser_id, width, height } => {
                buf.push(CommandTag::Resize as u8);
                write_u32(&mut buf, *browser_id);
                write_i32(&mut buf, *width);
                write_i32(&mut buf, *height);
            }
            Command::Shutdown => {
                buf.push(CommandTag::Shutdown as u8);
            }
        }
        buf
    }

    pub fn deserialize(data: &[u8]) -> io::Result<Command> {
        if data.is_empty() {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "empty"));
        }
        let tag = data[0];
        let mut off = 1;
        match tag {
            t if t == CommandTag::CreateBrowser as u8 => {
                let width = read_i32(data, &mut off)?;
                let height = read_i32(data, &mut off)?;
                let url = read_str(data, &mut off)?;
                Ok(Command::CreateBrowser { width, height, url })
            }
            t if t == CommandTag::DestroyBrowser as u8 => {
                let browser_id = read_u32(data, &mut off)?;
                Ok(Command::DestroyBrowser { browser_id })
            }
            t if t == CommandTag::LoadUrl as u8 => {
                let browser_id = read_u32(data, &mut off)?;
                let url = read_str(data, &mut off)?;
                Ok(Command::LoadUrl { browser_id, url })
            }
            t if t == CommandTag::Resize as u8 => {
                let browser_id = read_u32(data, &mut off)?;
                let width = read_i32(data, &mut off)?;
                let height = read_i32(data, &mut off)?;
                Ok(Command::Resize { browser_id, width, height })
            }
            t if t == CommandTag::Shutdown as u8 => Ok(Command::Shutdown),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unknown command tag: {}", tag),
            )),
        }
    }
}

// ---------------------------------------------------------------------------
// Response serialization
// ---------------------------------------------------------------------------

impl Response {
    pub fn serialize(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(64);
        match self {
            Response::BrowserCreated { browser_id, shm_name } => {
                buf.push(ResponseTag::BrowserCreated as u8);
                write_u32(&mut buf, *browser_id);
                write_str(&mut buf, shm_name);
            }
            Response::Ok => {
                buf.push(ResponseTag::Ok as u8);
            }
            Response::Error { msg } => {
                buf.push(ResponseTag::Error as u8);
                write_str(&mut buf, msg);
            }
        }
        buf
    }

    pub fn deserialize(data: &[u8]) -> io::Result<Response> {
        if data.is_empty() {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "empty"));
        }
        let tag = data[0];
        let mut off = 1;
        match tag {
            t if t == ResponseTag::BrowserCreated as u8 => {
                let browser_id = read_u32(data, &mut off)?;
                let shm_name = read_str(data, &mut off)?;
                Ok(Response::BrowserCreated { browser_id, shm_name })
            }
            t if t == ResponseTag::Ok as u8 => Ok(Response::Ok),
            t if t == ResponseTag::Error as u8 => {
                let msg = read_str(data, &mut off)?;
                Ok(Response::Error { msg })
            }
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unknown response tag: {}", tag),
            )),
        }
    }
}

// ---------------------------------------------------------------------------
// Framed I/O (length-prefixed)
// ---------------------------------------------------------------------------

pub fn send_message<W: Write>(w: &mut W, payload: &[u8]) -> io::Result<()> {
    let len = payload.len() as u32;
    w.write_all(&len.to_le_bytes())?;
    w.write_all(payload)?;
    w.flush()
}

pub fn recv_message<R: Read>(r: &mut R) -> io::Result<Vec<u8>> {
    let mut len_buf = [0u8; 4];
    r.read_exact(&mut len_buf)?;
    let len = u32::from_le_bytes(len_buf) as usize;
    if len > 16 * 1024 * 1024 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("message too large: {} bytes", len),
        ));
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf)?;
    Ok(buf)
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

/// Generate a shared memory file path for a browser.
pub fn shm_name(server_pid: u32, browser_id: u32) -> String {
    let tmp = std::env::temp_dir();
    tmp.join(format!("cef-unity-shm-{}-{}", server_pid, browser_id))
        .to_str()
        .unwrap()
        .to_string()
}

// ---------------------------------------------------------------------------
// File-backed mmap helpers (macOS compatible)
// ---------------------------------------------------------------------------

mod mmap_file {
    use super::*;
    use std::fs;
    use std::os::unix::io::AsRawFd;

    unsafe extern "C" {
        fn mmap(
            addr: *mut u8,
            len: usize,
            prot: i32,
            flags: i32,
            fd: i32,
            offset: i64,
        ) -> *mut u8;
        fn munmap(addr: *mut u8, len: usize) -> i32;
    }

    const PROT_READ: i32 = 0x01;
    const PROT_WRITE: i32 = 0x02;
    const MAP_SHARED: i32 = 0x0001;
    const MAP_FAILED: *mut u8 = !0usize as *mut u8;

    /// Create a file-backed mmap (server side, read-write).
    pub fn shm_create(path: &str, size: usize) -> io::Result<*mut u8> {
        // Remove stale file
        let _ = fs::remove_file(path);

        // Create file with read-write permissions for owner+group+other
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)?;
        // Set permissions so other processes can read
        fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o644))?;
        file.set_len(size as u64)?;

        let fd = file.as_raw_fd();
        let ptr = unsafe {
            mmap(
                std::ptr::null_mut(),
                size,
                PROT_READ | PROT_WRITE,
                MAP_SHARED,
                fd,
                0,
            )
        };
        // File can be closed after mmap; the mapping stays alive
        drop(file);

        if ptr == MAP_FAILED {
            let _ = fs::remove_file(path);
            return Err(io::Error::last_os_error());
        }
        // Zero-initialize header
        unsafe {
            std::ptr::write_bytes(ptr, 0, std::mem::size_of::<ShmHeader>());
        }
        Ok(ptr)
    }

    /// Open an existing file-backed mmap (client side, read-only).
    pub fn shm_open_ro(path: &str, size: usize) -> io::Result<*const u8> {
        let file = fs::OpenOptions::new().read(true).open(path)?;
        let fd = file.as_raw_fd();
        let ptr = unsafe {
            mmap(
                std::ptr::null_mut(),
                size,
                PROT_READ,
                MAP_SHARED,
                fd,
                0,
            )
        };
        drop(file);

        if ptr == MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        Ok(ptr as *const u8)
    }

    pub unsafe fn shm_unmap(ptr: *mut u8, size: usize) {
        unsafe { munmap(ptr, size); }
    }

    pub unsafe fn shm_unmap_ro(ptr: *const u8, size: usize) {
        unsafe { munmap(ptr as *mut u8, size); }
    }

    pub fn shm_remove(path: &str) {
        let _ = fs::remove_file(path);
    }
}

pub use mmap_file::{shm_create, shm_open_ro, shm_remove, shm_unmap, shm_unmap_ro};

// ---------------------------------------------------------------------------
// High-level shared memory wrappers
// ---------------------------------------------------------------------------

use std::sync::atomic::Ordering;

/// Server-side handle for writing frames into shared memory.
pub struct ShmWriter {
    pub ptr: *mut u8,
    pub name: String,
}

unsafe impl Send for ShmWriter {}

impl ShmWriter {
    pub fn new(name: &str) -> io::Result<Self> {
        let ptr = shm_create(name, SHM_TOTAL_SIZE)?;
        Ok(ShmWriter {
            ptr,
            name: name.to_string(),
        })
    }

    fn header(&self) -> &ShmHeader {
        unsafe { &*(self.ptr as *const ShmHeader) }
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
            std::ptr::copy_nonoverlapping(pixels.as_ptr(), self.ptr.add(offset), size);
        }

        header.width.store(width, Ordering::Release);
        header.height.store(height, Ordering::Release);
        header.active_buffer.store(target, Ordering::Release);
        header.frame_id.fetch_add(1, Ordering::Release);
    }
}

impl Drop for ShmWriter {
    fn drop(&mut self) {
        unsafe { shm_unmap(self.ptr, SHM_TOTAL_SIZE) };
        shm_remove(&self.name);
    }
}

/// Client-side handle for reading frames from shared memory.
pub struct ShmReader {
    pub ptr: *const u8,
    pub name: String,
    pub last_frame_id: u64,
}

unsafe impl Send for ShmReader {}

impl ShmReader {
    pub fn open(name: &str) -> io::Result<Self> {
        let ptr = shm_open_ro(name, SHM_TOTAL_SIZE)?;
        Ok(ShmReader {
            ptr,
            name: name.to_string(),
            last_frame_id: 0,
        })
    }

    /// Check if there's a new frame. If so, copy it into `dst` and return (width, height).
    /// Returns None if no new frame since last call.
    pub fn read_frame(&mut self, dst: &mut Vec<u8>) -> Option<(u32, u32)> {
        // Read header fields directly via ptr to avoid borrowing self
        let header = unsafe { &*(self.ptr as *const ShmHeader) };
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
            std::ptr::copy_nonoverlapping(self.ptr.add(offset), dst.as_mut_ptr(), size);
        }
        Some((width, height))
    }
}

impl Drop for ShmReader {
    fn drop(&mut self) {
        unsafe { shm_unmap_ro(self.ptr, SHM_TOTAL_SIZE) };
    }
}
