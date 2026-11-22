//! Perfetto IPC transport (`src/ipc/`, `protos/perfetto/ipc/wire_protocol.proto`).
//!
//! Each frame on the UNIX socket is `u32 little-endian length` followed by an
//! `IPCFrame`. File descriptors (the shared memory buffer) travel as
//! `SCM_RIGHTS` ancillary data alongside the frame that needs them.

use crate::protos::IpcFrame;
use prost::Message;
use std::collections::VecDeque;
use std::io::{self, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::sync::Mutex;

/// Maximum frame size accepted (matches `ipc::kIPCBufferSize`).
const MAX_FRAME: usize = 128 * 1024;

pub struct Reader {
    sock: UnixStream,
    buf: Vec<u8>,
    pub fds: VecDeque<OwnedFd>,
}

pub struct Sender {
    sock: Mutex<UnixStream>,
}

pub fn connect(path: &str) -> io::Result<(Reader, Sender)> {
    let sock = UnixStream::connect(path)?;
    let w = sock.try_clone()?;
    Ok((Reader { sock, buf: Vec::new(), fds: VecDeque::new() }, Sender { sock: Mutex::new(w) }))
}

impl Sender {
    pub fn send(&self, frame: &IpcFrame) -> io::Result<()> {
        let body = frame.encode_to_vec();
        let mut out = Vec::with_capacity(body.len() + 4);
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(&body);
        self.sock.lock().unwrap().write_all(&out)
    }

    pub fn shutdown(&self) {
        let _ = self.sock.lock().unwrap().shutdown(std::net::Shutdown::Both);
    }
}

impl Reader {
    /// Blocks until a complete frame is available.
    pub fn next_frame(&mut self) -> io::Result<IpcFrame> {
        loop {
            if self.buf.len() >= 4 {
                let len = u32::from_le_bytes(self.buf[..4].try_into().unwrap()) as usize;
                if len > MAX_FRAME {
                    return Err(io::Error::new(io::ErrorKind::InvalidData, "IPC frame too large"));
                }
                if self.buf.len() >= 4 + len {
                    let f = IpcFrame::decode(&self.buf[4..4 + len])
                        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
                    self.buf.drain(..4 + len);
                    return Ok(f);
                }
            }
            self.recv_more()?;
        }
    }

    fn recv_more(&mut self) -> io::Result<()> {
        let mut data = [0u8; 16 * 1024];
        // Room for up to 4 fds.
        let mut cmsg = [0u64; 8];
        let mut iov = libc::iovec { iov_base: data.as_mut_ptr() as *mut libc::c_void, iov_len: data.len() };
        let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        msg.msg_control = cmsg.as_mut_ptr() as *mut libc::c_void;
        msg.msg_controllen = std::mem::size_of_val(&cmsg) as _;
        let n = loop {
            let n = unsafe { libc::recvmsg(self.sock.as_raw_fd(), &mut msg, libc::MSG_CMSG_CLOEXEC) };
            if n < 0 {
                let e = io::Error::last_os_error();
                if e.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(e);
            }
            break n as usize;
        };
        unsafe {
            let mut c = libc::CMSG_FIRSTHDR(&msg);
            while !c.is_null() {
                if (*c).cmsg_level == libc::SOL_SOCKET && (*c).cmsg_type == libc::SCM_RIGHTS {
                    let payload = (*c).cmsg_len as usize - libc::CMSG_LEN(0) as usize;
                    let p = libc::CMSG_DATA(c) as *const i32;
                    for i in 0..payload / 4 {
                        self.fds.push_back(OwnedFd::from_raw_fd(std::ptr::read_unaligned(p.add(i))));
                    }
                }
                c = libc::CMSG_NXTHDR(&msg, c);
            }
        }
        if n == 0 {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "traced closed the connection"));
        }
        self.buf.extend_from_slice(&data[..n]);
        Ok(())
    }
}
