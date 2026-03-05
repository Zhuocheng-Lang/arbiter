//! Listens to the Linux kernel's process-connector (`CN_PROC`) via a raw
//! `NETLINK_CONNECTOR` socket.  No BPF involved.
//!
//! Kernel reference: `include/uapi/linux/cn_proc.h`

use anyhow::{Result, bail};
use std::io;
use std::mem;
use std::os::raw::c_int;
use tokio::sync::mpsc;

// ── ProcEvent (public) ────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum ProcEvent {
    /// A process called `execve` (new binary loaded).
    Exec { pid: u32, tgid: u32 },
    /// A process forked.
    Fork { parent_pid: u32, child_pid: u32 },
    /// A process exited.
    Exit { pid: u32, exit_code: u32 },
}

// ── Netlink / CN_PROC constants ───────────────────────────────────────────────

const NETLINK_CONNECTOR: c_int = 11;
const CN_IDX_PROC: u32 = 1;
const CN_VAL_PROC: u32 = 1;
const PROC_CN_MCAST_LISTEN: u32 = 1;

const PROC_EVENT_EXEC: u32 = 0x00000002;
const PROC_EVENT_FORK: u32 = 0x00000001;
const PROC_EVENT_EXIT: u32 = 0x80000000;

const RECV_BUF: usize = 4096;

// ── Wire structs (all #[repr(C)] for correct ABI layout) ─────────────────────

#[repr(C)]
#[derive(Copy, Clone)]
struct NlMsgHdr {
    nlmsg_len: u32,
    nlmsg_type: u16,
    nlmsg_flags: u16,
    nlmsg_seq: u32,
    nlmsg_pid: u32,
}

#[repr(C)]
#[derive(Copy, Clone)]
struct CbId {
    idx: u32,
    val: u32,
}

#[repr(C)]
#[derive(Copy, Clone)]
struct CnMsg {
    id: CbId,
    seq: u32,
    ack: u32,
    len: u16,
    flags: u16,
}

/// The first three fields of every `proc_event` union.
#[repr(C)]
#[derive(Copy, Clone)]
struct ProcEventHdr {
    what: u32,
    cpu: u32,
    timestamp_ns: u64,
}

/// Subscribe message sent to the kernel to start receiving proc events.
#[repr(C)]
struct SubscribeMsg {
    nl_hdr: NlMsgHdr,
    cn_msg: CnMsg,
    op: u32,
}

// ── Socket helpers ────────────────────────────────────────────────────────────

fn create_nl_socket() -> Result<c_int> {
    let fd = unsafe {
        libc::socket(
            libc::AF_NETLINK,
            libc::SOCK_DGRAM | libc::SOCK_CLOEXEC,
            NETLINK_CONNECTOR,
        )
    };
    if fd < 0 {
        bail!("socket(NETLINK_CONNECTOR): {}", io::Error::last_os_error());
    }

    // Use zeroed() to avoid depending on the exact field layout / padding types
    // in different libc versions.
    let mut addr: libc::sockaddr_nl = unsafe { std::mem::zeroed() };
    addr.nl_family = libc::AF_NETLINK as u16;
    addr.nl_pid = unsafe { libc::getpid() as u32 };
    addr.nl_groups = CN_IDX_PROC;

    let ret = unsafe {
        libc::bind(
            fd,
            &addr as *const _ as *const libc::sockaddr,
            mem::size_of::<libc::sockaddr_nl>() as libc::socklen_t,
        )
    };
    if ret < 0 {
        unsafe { libc::close(fd) };
        bail!("bind(NETLINK_CONNECTOR): {}", io::Error::last_os_error());
    }

    Ok(fd)
}

fn send_subscribe(fd: c_int) -> Result<()> {
    let cn_data_len = mem::size_of::<u32>() as u16; // just the `op` field
    let total_len = mem::size_of::<SubscribeMsg>() as u32;

    let msg = SubscribeMsg {
        nl_hdr: NlMsgHdr {
            nlmsg_len: total_len,
            nlmsg_type: 0, // NLMSG_NOOP — kernel ignores this for connectors
            nlmsg_flags: 0,
            nlmsg_seq: 0,
            nlmsg_pid: unsafe { libc::getpid() as u32 },
        },
        cn_msg: CnMsg {
            id: CbId {
                idx: CN_IDX_PROC,
                val: CN_VAL_PROC,
            },
            seq: 0,
            ack: 0,
            len: cn_data_len,
            flags: 0,
        },
        op: PROC_CN_MCAST_LISTEN,
    };

    let ret = unsafe {
        libc::send(
            fd,
            &msg as *const _ as *const libc::c_void,
            mem::size_of::<SubscribeMsg>(),
            0,
        )
    };
    if ret < 0 {
        bail!("send(subscribe): {}", io::Error::last_os_error());
    }
    Ok(())
}

// ── Parsing ───────────────────────────────────────────────────────────────────

fn parse_event(buf: &[u8]) -> Option<ProcEvent> {
    let nl_sz = mem::size_of::<NlMsgHdr>();
    let cn_sz = mem::size_of::<CnMsg>();
    let evhdr_sz = mem::size_of::<ProcEventHdr>();
    let header_end = nl_sz + cn_sz + evhdr_sz;

    if buf.len() < header_end {
        return None;
    }

    // SAFETY: buf is aligned to at least u8; we use read_unaligned.
    let evhdr: ProcEventHdr =
        unsafe { std::ptr::read_unaligned(buf[nl_sz + cn_sz..].as_ptr() as *const ProcEventHdr) };

    let data = &buf[header_end..];

    match evhdr.what {
        PROC_EVENT_EXEC => {
            if data.len() < 8 {
                return None;
            }
            let pid = u32::from_ne_bytes(data[0..4].try_into().ok()?);
            let tgid = u32::from_ne_bytes(data[4..8].try_into().ok()?);
            Some(ProcEvent::Exec { pid, tgid })
        }
        PROC_EVENT_FORK => {
            if data.len() < 16 {
                return None;
            }
            let parent_pid = u32::from_ne_bytes(data[0..4].try_into().ok()?);
            // skip parent_tgid at [4..8]
            let child_pid = u32::from_ne_bytes(data[8..12].try_into().ok()?);
            Some(ProcEvent::Fork {
                parent_pid,
                child_pid,
            })
        }
        PROC_EVENT_EXIT => {
            if data.len() < 8 {
                return None;
            }
            let pid = u32::from_ne_bytes(data[0..4].try_into().ok()?);
            // skip tgid at [4..8]
            let exit_code = if data.len() >= 12 {
                u32::from_ne_bytes(data[8..12].try_into().ok()?)
            } else {
                0
            };
            Some(ProcEvent::Exit { pid, exit_code })
        }
        _ => None,
    }
}

// ── Public entry point ────────────────────────────────────────────────────────

/// Spawn a background thread that reads CN_PROC events and forwards them to
/// `tx`.  Returns immediately; the thread runs until the channel is closed.
///
/// Requires `CAP_NET_ADMIN` (or root) to receive system-wide events.
pub async fn start_event_stream(tx: mpsc::Sender<ProcEvent>) -> Result<()> {
    // Check for root / CAP_NET_ADMIN early so the error is visible.
    let uid = unsafe { libc::getuid() };
    if uid != 0 {
        tracing::warn!(
            uid,
            "Not running as root: proc-connector events will be limited \
             to processes owned by this user. Run arbiter with sudo or \
             grant CAP_NET_ADMIN for full system-wide coverage."
        );
    }

    let fd = create_nl_socket()?;
    send_subscribe(fd)?;
    tracing::info!("Subscribed to kernel proc-connector (CN_PROC)");

    tokio::task::spawn_blocking(move || {
        let mut buf = vec![0u8; RECV_BUF];
        loop {
            let n = unsafe { libc::recv(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len(), 0) };
            if n < 0 {
                let err = io::Error::last_os_error();
                if err.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                tracing::error!("recv(CN_PROC): {err}");
                break;
            }
            if n == 0 {
                break;
            }

            if let Some(event) = parse_event(&buf[..n as usize]) {
                if tx.blocking_send(event).is_err() {
                    break; // daemon shut down
                }
            }
        }
        unsafe { libc::close(fd) };
    });

    Ok(())
}
