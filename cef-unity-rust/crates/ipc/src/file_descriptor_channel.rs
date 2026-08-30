//! dmabuf の file descriptor をプロセス間で受け渡すチャネル (Linux 専用)。
//!
//! CEF の `on_accelerated_paint` が渡してくる dmabuf は file descriptor なので、
//! 共有メモリにバイト列として書くことができない。Unix ドメインソケットの
//! `SCM_RIGHTS` 補助データに載せて転送する。
//!
//! macOS の Mach port 転送 (`mach_iosurface.c`)、Windows の NT 共有ハンドル
//! (`DuplicateHandle`) に相当する層。

use std::os::fd::{OwnedFd, RawFd};

/// `SCM_RIGHTS` で file descriptor を運べる双方向チャネル。
pub struct FileDescriptorChannel {
    _socket: std::os::unix::net::UnixStream,
}

impl FileDescriptorChannel {
    /// 接続済みの 1 対を作る。
    pub fn pair() -> std::io::Result<(Self, Self)> {
        todo!("not implemented")
    }

    /// バイト列と file descriptor をまとめて送る。
    pub fn send(&self, _payload: &[u8], _file_descriptors: &[RawFd]) -> std::io::Result<()> {
        todo!("not implemented")
    }

    /// バイト列と file descriptor を受け取る。戻り値はペイロードの長さと fd。
    pub fn receive(&self, _payload_buffer: &mut [u8]) -> std::io::Result<(usize, Vec<OwnedFd>)> {
        todo!("not implemented")
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
        sender.send(b"x", &[std::os::fd::AsRawFd::as_raw_fd(&file)]).unwrap();

        let mut payload_buffer = [0u8; 8];
        let (_length, file_descriptors) = receiver.receive(&mut payload_buffer).unwrap();

        assert_eq!(file_descriptors.len(), 1);
        let mut received = String::new();
        std::fs::File::from(file_descriptors.into_iter().next().unwrap())
            .read_to_string(&mut received)
            .unwrap();
        assert_eq!(received, "dmabuf");
    }
}
