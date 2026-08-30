//! dmabuf の file descriptor をプロセス間で受け渡すチャネル (Linux 専用)。
//!
//! CEF の `on_accelerated_paint` が渡してくる dmabuf は file descriptor なので、
//! 共有メモリにバイト列として書くことができない。Unix ドメインソケットの
//! `SCM_RIGHTS` 補助データに載せて転送する。
//!
//! macOS の Mach port 転送 (`mach_iosurface.c`)、Windows の NT 共有ハンドル
//! (`DuplicateHandle`) に相当する層。
//!
//! `ipc-channel` の高レベル API は任意の file descriptor を運べないため別建てにしてある。

use std::mem::size_of;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};

/// 1 メッセージで運べる file descriptor の上限。dmabuf のプレーン数上限に合わせてある。
const MAXIMUM_FILE_DESCRIPTORS: usize = 4;

/// `SCM_RIGHTS` で file descriptor を運べる双方向チャネル。
pub struct FileDescriptorChannel {
    socket: std::os::unix::net::UnixStream,
}

impl FileDescriptorChannel {
    /// 接続済みの 1 対を作る。テストと、親子プロセスを直接繋ぐ用途に使う。
    pub fn pair() -> std::io::Result<(Self, Self)> {
        let (first, second) = std::os::unix::net::UnixStream::pair()?;
        Ok((Self { socket: first }, Self { socket: second }))
    }

    /// 待ち受けを開始する。既存のソケットファイルは作り直す。
    pub fn listen(path: &str) -> std::io::Result<FileDescriptorListener> {
        // 前回の残置があると bind が EADDRINUSE で失敗する。
        let _ = std::fs::remove_file(path);
        Ok(FileDescriptorListener {
            listener: std::os::unix::net::UnixListener::bind(path)?,
            path: path.to_string(),
        })
    }

    /// 待ち受けているサーバへ接続する。
    pub fn connect(path: &str) -> std::io::Result<Self> {
        Ok(Self {
            socket: std::os::unix::net::UnixStream::connect(path)?,
        })
    }

    /// バイト列と file descriptor をまとめて送る。
    ///
    /// file descriptor は複製されて相手に渡るため、呼び出し側は送信後に
    /// 自分の分を閉じてよい。
    pub fn send(&self, payload: &[u8], file_descriptors: &[RawFd]) -> std::io::Result<()> {
        if file_descriptors.len() > MAXIMUM_FILE_DESCRIPTORS {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "file descriptor が多すぎる",
            ));
        }

        let control_length =
            unsafe { libc::CMSG_SPACE((size_of::<RawFd>() * file_descriptors.len()) as u32) }
                as usize;
        let mut control_buffer = vec![0u8; control_length.max(1)];
        let mut io_vector = libc::iovec {
            iov_base: payload.as_ptr() as *mut libc::c_void,
            iov_len: payload.len(),
        };
        let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
        message.msg_iov = &mut io_vector;
        message.msg_iovlen = 1;

        if !file_descriptors.is_empty() {
            message.msg_control = control_buffer.as_mut_ptr() as *mut libc::c_void;
            message.msg_controllen = control_length;
            unsafe {
                let control_message = libc::CMSG_FIRSTHDR(&message);
                (*control_message).cmsg_level = libc::SOL_SOCKET;
                (*control_message).cmsg_type = libc::SCM_RIGHTS;
                (*control_message).cmsg_len =
                    libc::CMSG_LEN((size_of::<RawFd>() * file_descriptors.len()) as u32) as usize;
                std::ptr::copy_nonoverlapping(
                    file_descriptors.as_ptr(),
                    libc::CMSG_DATA(control_message) as *mut RawFd,
                    file_descriptors.len(),
                );
            }
        }

        let sent = unsafe { libc::sendmsg(self.socket.as_raw_fd(), &message, 0) };
        if sent < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }

    /// バイト列と file descriptor を受け取る。戻り値はペイロードの長さと file descriptor。
    ///
    /// 受け取った file descriptor の所有権は呼び出し側に移る (`OwnedFd` が閉じる)。
    pub fn receive(&self, payload_buffer: &mut [u8]) -> std::io::Result<(usize, Vec<OwnedFd>)> {
        let control_length =
            unsafe { libc::CMSG_SPACE((size_of::<RawFd>() * MAXIMUM_FILE_DESCRIPTORS) as u32) }
                as usize;
        let mut control_buffer = vec![0u8; control_length];
        let mut io_vector = libc::iovec {
            iov_base: payload_buffer.as_mut_ptr() as *mut libc::c_void,
            iov_len: payload_buffer.len(),
        };
        let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
        message.msg_iov = &mut io_vector;
        message.msg_iovlen = 1;
        message.msg_control = control_buffer.as_mut_ptr() as *mut libc::c_void;
        message.msg_controllen = control_length;

        let received = unsafe { libc::recvmsg(self.socket.as_raw_fd(), &mut message, 0) };
        if received < 0 {
            return Err(std::io::Error::last_os_error());
        }

        let mut file_descriptors = Vec::new();
        unsafe {
            let mut control_message = libc::CMSG_FIRSTHDR(&message);
            while !control_message.is_null() {
                if (*control_message).cmsg_level == libc::SOL_SOCKET
                    && (*control_message).cmsg_type == libc::SCM_RIGHTS
                {
                    let payload_length =
                        (*control_message).cmsg_len - libc::CMSG_LEN(0) as usize;
                    let count = payload_length / size_of::<RawFd>();
                    let data = libc::CMSG_DATA(control_message) as *const RawFd;
                    for index in 0..count {
                        file_descriptors.push(OwnedFd::from_raw_fd(*data.add(index)));
                    }
                }
                control_message = libc::CMSG_NXTHDR(&message, control_message);
            }
        }
        Ok((received as usize, file_descriptors))
    }
}

/// 接続を待ち受ける側。落とすとソケットファイルを削除する。
pub struct FileDescriptorListener {
    listener: std::os::unix::net::UnixListener,
    path: String,
}

impl FileDescriptorListener {
    /// クライアントの接続を 1 つ受け付ける。
    pub fn accept(&self) -> std::io::Result<FileDescriptorChannel> {
        let (socket, _address) = self.listener.accept()?;
        Ok(FileDescriptorChannel { socket })
    }
}

impl Drop for FileDescriptorListener {
    fn drop(&mut self) {
        // 残置するとサーバ再起動時に bind が EADDRINUSE で失敗する。
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    #[test]
    fn 送った_fd_で受信側が同じファイルを読める() {
        let path = std::env::temp_dir().join("cef_unity_fd_channel_test.txt");
        std::fs::File::create(&path)
            .unwrap()
            .write_all(b"dmabuf")
            .unwrap();
        let file = std::fs::File::open(&path).unwrap();

        let (sender, receiver) = FileDescriptorChannel::pair().unwrap();
        sender.send(b"x", &[file.as_raw_fd()]).unwrap();

        let mut payload_buffer = [0u8; 8];
        let (_length, file_descriptors) = receiver.receive(&mut payload_buffer).unwrap();

        assert_eq!(file_descriptors.len(), 1);
        let mut received = String::new();
        std::fs::File::from(file_descriptors.into_iter().next().unwrap())
            .read_to_string(&mut received)
            .unwrap();
        assert_eq!(received, "dmabuf");
    }

    #[test]
    fn listen_と_connect_で_fd_を渡せる() {
        let path = std::env::temp_dir()
            .join("cef_unity_fd_channel_listen_test.sock")
            .to_string_lossy()
            .into_owned();
        let listener = FileDescriptorChannel::listen(&path).unwrap();

        let connect_path = path.clone();
        let client_thread = std::thread::spawn(move || {
            let channel = FileDescriptorChannel::connect(&connect_path).unwrap();
            let mut payload_buffer = [0u8; 8];
            let (_length, file_descriptors) = channel.receive(&mut payload_buffer).unwrap();
            file_descriptors.len()
        });

        let accepted = listener.accept().unwrap();
        let file = std::fs::File::open("/dev/null").unwrap();
        accepted.send(b"y", &[file.as_raw_fd()]).unwrap();

        assert_eq!(client_thread.join().unwrap(), 1);
    }

    #[test]
    fn listener_を落とすとソケットファイルが消える() {
        // 残置すると次回の bind が EADDRINUSE で失敗する。
        let path = std::env::temp_dir()
            .join("cef_unity_fd_channel_unlink_test.sock")
            .to_string_lossy()
            .into_owned();
        {
            let _listener = FileDescriptorChannel::listen(&path).unwrap();
            assert!(std::path::Path::new(&path).exists());
        }
        assert!(!std::path::Path::new(&path).exists());
    }

    #[test]
    fn ペイロードが往復する() {
        let (sender, receiver) = FileDescriptorChannel::pair().unwrap();
        sender.send(b"hello", &[]).unwrap();

        let mut payload_buffer = [0u8; 16];
        let (length, file_descriptors) = receiver.receive(&mut payload_buffer).unwrap();

        assert_eq!(&payload_buffer[..length], b"hello");
        assert!(file_descriptors.is_empty());
    }
}
