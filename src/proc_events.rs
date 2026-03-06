//! Listens to the Linux kernel's process-connector (`CN_PROC`) via a raw
//! `NETLINK_CONNECTOR` socket.  No BPF involved.
//!
//! Kernel reference: `include/uapi/linux/cn_proc.h`

use anyhow::{Result, bail};
use std::io;
use std::mem;
use std::os::raw::c_int;
use std::ptr;
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

const NLMSG_NOOP: u16 = 0x1;
const NLMSG_ERROR: u16 = 0x2;
const NLMSG_DONE: u16 = 0x3;

const NLM_F_REQUEST: u16 = 0x1;
const NLM_F_ACK: u16 = 0x4;

const CAP_NET_ADMIN: u32 = 12;
const RECV_BUF: usize = 8192;
const SOCKET_RCVBUF: c_int = 1 << 20;

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

#[repr(C)]
#[derive(Copy, Clone)]
struct NlMsgErr {
    error: i32,
    msg: NlMsgHdr,
}

enum ParsedNetlinkMessage {
    ProcEvent(ProcEvent),
    Ack,
    Error(i32),
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

    let rcvbuf: c_int = SOCKET_RCVBUF;
    let ret = unsafe {
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_RCVBUF,
            &rcvbuf as *const _ as *const libc::c_void,
            mem::size_of_val(&rcvbuf) as libc::socklen_t,
        )
    };
    if ret < 0 {
        tracing::debug!(
            "setsockopt(SO_RCVBUF) failed for CN_PROC socket: {}",
            io::Error::last_os_error()
        );
    }

    Ok(fd)
}

fn send_subscribe(fd: c_int) -> Result<()> {
    let cn_data_len = mem::size_of::<u32>() as u16; // just the `op` field
    let total_len = mem::size_of::<SubscribeMsg>() as u32;

    let msg = SubscribeMsg {
        nl_hdr: NlMsgHdr {
            nlmsg_len: total_len,
            nlmsg_type: NLMSG_DONE,
            nlmsg_flags: NLM_F_REQUEST | NLM_F_ACK,
            nlmsg_seq: 1,
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

    let mut kernel_addr: libc::sockaddr_nl = unsafe { std::mem::zeroed() };
    kernel_addr.nl_family = libc::AF_NETLINK as u16;
    kernel_addr.nl_pid = 0;
    kernel_addr.nl_groups = CN_IDX_PROC;

    let ret = unsafe {
        libc::sendto(
            fd,
            &msg as *const _ as *const libc::c_void,
            mem::size_of::<SubscribeMsg>(),
            0,
            &kernel_addr as *const _ as *const libc::sockaddr,
            mem::size_of::<libc::sockaddr_nl>() as libc::socklen_t,
        )
    };
    if ret < 0 {
        bail!("send(subscribe): {}", io::Error::last_os_error());
    }
    Ok(())
}

// ── Parsing ───────────────────────────────────────────────────────────────────

fn read_copy<T: Copy>(buf: &[u8]) -> Option<T> {
    if buf.len() < mem::size_of::<T>() {
        return None;
    }

    Some(unsafe { ptr::read_unaligned(buf.as_ptr() as *const T) })
}

fn nlmsg_align(len: usize) -> usize {
    (len + 3) & !3
}

fn parse_connector_event(payload: &[u8]) -> Option<ProcEvent> {
    let cn_sz = mem::size_of::<CnMsg>();
    let evhdr_sz = mem::size_of::<ProcEventHdr>();

    if payload.len() < cn_sz + evhdr_sz {
        return None;
    }

    let cn_msg: CnMsg = read_copy(payload)?;
    if cn_msg.id.idx != CN_IDX_PROC || cn_msg.id.val != CN_VAL_PROC {
        return None;
    }

    let cn_payload_len = cn_msg.len as usize;
    if cn_payload_len < evhdr_sz || cn_sz + cn_payload_len > payload.len() {
        return None;
    }

    let header_end = cn_sz + evhdr_sz;
    let evhdr: ProcEventHdr = read_copy(&payload[cn_sz..])?;
    let data = &payload[header_end..cn_sz + cn_payload_len];

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

fn parse_nlmsg_error(payload: &[u8]) -> Option<i32> {
    let err: NlMsgErr = read_copy(payload)?;
    Some(err.error)
}

fn parse_messages(buf: &[u8]) -> Vec<ParsedNetlinkMessage> {
    let nl_sz = mem::size_of::<NlMsgHdr>();
    let mut messages = Vec::new();
    let mut offset = 0usize;

    while offset + nl_sz <= buf.len() {
        let hdr: NlMsgHdr = match read_copy(&buf[offset..]) {
            Some(hdr) => hdr,
            None => break,
        };

        let msg_len = hdr.nlmsg_len as usize;
        if msg_len < nl_sz || offset + msg_len > buf.len() {
            tracing::debug!(nlmsg_len = hdr.nlmsg_len, remaining = buf.len() - offset, "dropping malformed netlink message");
            break;
        }

        let payload = &buf[offset + nl_sz..offset + msg_len];
        match hdr.nlmsg_type {
            NLMSG_NOOP | NLMSG_DONE => {
                // NLMSG_DONE marks end-of-multipart-dump; NLMSG_NOOP is a no-op.
                // Neither is an ACK for our subscription request — only
                // NLMSG_ERROR(error=0) constitutes an ACK per netlink convention.
            }
            NLMSG_ERROR => match parse_nlmsg_error(payload) {
                Some(0) => messages.push(ParsedNetlinkMessage::Ack),
                Some(errno) => messages.push(ParsedNetlinkMessage::Error(errno)),
                None => tracing::debug!("received malformed NLMSG_ERROR from kernel"),
            },
            _ => {
                if let Some(event) = parse_connector_event(payload) {
                    messages.push(ParsedNetlinkMessage::ProcEvent(event));
                }
            }
        }

        let aligned_len = nlmsg_align(msg_len);
        if aligned_len == 0 {
            break;
        }
        offset += aligned_len;
    }

    messages
}

fn recv_subscribe_ack(fd: c_int) -> Result<()> {
    let mut buf = vec![0u8; RECV_BUF];

    loop {
        let n = unsafe { libc::recv(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len(), 0) };
        if n < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            if err.kind() == io::ErrorKind::WouldBlock || err.kind() == io::ErrorKind::TimedOut {
                bail!("timed out waiting for CN_PROC subscription ACK from kernel (5 s)");
            }
            bail!("recv(subscribe-ack): {err}");
        }
        if n == 0 {
            bail!("recv(subscribe-ack): kernel closed the netlink socket unexpectedly");
        }

        for message in parse_messages(&buf[..n as usize]) {
            match message {
                ParsedNetlinkMessage::Ack => return Ok(()),
                ParsedNetlinkMessage::Error(errno) => {
                    bail!("kernel rejected CN_PROC subscription: {}", io::Error::from_raw_os_error(-errno));
                }
                ParsedNetlinkMessage::ProcEvent(_) => {
                    tracing::debug!("received CN_PROC event before subscription ACK");
                }
            }
        }
    }
}

fn has_effective_capability(cap: u32) -> io::Result<bool> {
    let status = std::fs::read_to_string("/proc/self/status")?;
    let raw = status
        .lines()
        .find_map(|line| line.strip_prefix("CapEff:\t").or_else(|| line.strip_prefix("CapEff:")))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "CapEff missing from /proc/self/status"))?
        .trim();

    let effective = u64::from_str_radix(raw, 16)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;

    Ok((effective & (1u64 << cap)) != 0)
}

// ── Public entry point ────────────────────────────────────────────────────────

/// Spawn a background thread that reads CN_PROC events and forwards them to
/// `tx`.  Returns immediately; the thread runs until the channel is closed.
///
/// Requires `CAP_NET_ADMIN` (or root) to receive system-wide events.
pub async fn start_event_stream(tx: mpsc::Sender<ProcEvent>) -> Result<()> {
    match has_effective_capability(CAP_NET_ADMIN) {
        Ok(true) => {}
        Ok(false) => {
            tracing::warn!(
                "CAP_NET_ADMIN is not effective: proc-connector events may be limited \
                 to processes visible to the current user or namespace"
            );
        }
        Err(err) => {
            tracing::debug!("Failed to inspect effective capabilities: {err}");
        }
    }

    let fd = create_nl_socket()?;
    if let Err(err) = send_subscribe(fd) {
        unsafe { libc::close(fd) };
        return Err(err);
    }
    // Bound the blocking recv in recv_subscribe_ack: if the kernel never ACKs
    // (old kernel, unusual config), we must not stall the tokio worker thread
    // indefinitely.  5 s is generous; in practice the ACK arrives in < 1 ms.
    let ack_timeout = libc::timeval { tv_sec: 5, tv_usec: 0 };
    unsafe {
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_RCVTIMEO,
            &ack_timeout as *const _ as *const libc::c_void,
            mem::size_of::<libc::timeval>() as libc::socklen_t,
        );
    }
    if let Err(err) = recv_subscribe_ack(fd) {
        unsafe { libc::close(fd) };
        return Err(err);
    }
    // Clear the timeout before handing fd to the streaming thread.
    let no_timeout = libc::timeval { tv_sec: 0, tv_usec: 0 };
    unsafe {
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_RCVTIMEO,
            &no_timeout as *const _ as *const libc::c_void,
            mem::size_of::<libc::timeval>() as libc::socklen_t,
        );
    }
    tracing::info!("Subscribed to kernel proc-connector (CN_PROC)");

    tokio::task::spawn_blocking(move || {
        let mut buf = vec![0u8; RECV_BUF];
        'recv_loop: loop {
            let n = unsafe { libc::recv(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len(), 0) };
            if n < 0 {
                let err = io::Error::last_os_error();
                if err.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                if err.raw_os_error() == Some(libc::ENOBUFS) {
                    tracing::warn!("CN_PROC receive buffer overflowed; some process events were dropped");
                    continue;
                }
                tracing::error!("recv(CN_PROC): {err}");
                break;
            }
            if n == 0 {
                break;
            }

            for message in parse_messages(&buf[..n as usize]) {
                match message {
                    ParsedNetlinkMessage::ProcEvent(event) => {
                        if tx.blocking_send(event).is_err() {
                            break 'recv_loop;
                        }
                    }
                    ParsedNetlinkMessage::Ack => {
                        tracing::debug!("received unexpected CN_PROC ACK while streaming");
                    }
                    ParsedNetlinkMessage::Error(errno) => {
                        tracing::warn!(
                            errno,
                            "kernel reported a CN_PROC netlink error while streaming"
                        );
                    }
                }
            }
        }
        unsafe { libc::close(fd) };
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nl_hdr(nlmsg_type: u16, payload_len: usize) -> Vec<u8> {
        let hdr = NlMsgHdr {
            nlmsg_len: (mem::size_of::<NlMsgHdr>() + payload_len) as u32,
            nlmsg_type,
            nlmsg_flags: 0,
            nlmsg_seq: 1,
            nlmsg_pid: 0,
        };

        unsafe {
            std::slice::from_raw_parts(
                &hdr as *const _ as *const u8,
                mem::size_of::<NlMsgHdr>(),
            )
        }
        .to_vec()
    }

    fn cn_msg_bytes(event_len: usize) -> Vec<u8> {
        let msg = CnMsg {
            id: CbId {
                idx: CN_IDX_PROC,
                val: CN_VAL_PROC,
            },
            seq: 0,
            ack: 0,
            len: event_len as u16,
            flags: 0,
        };

        unsafe {
            std::slice::from_raw_parts(&msg as *const _ as *const u8, mem::size_of::<CnMsg>())
        }
        .to_vec()
    }

    fn event_hdr_bytes(what: u32) -> Vec<u8> {
        let hdr = ProcEventHdr {
            what,
            cpu: 1,
            timestamp_ns: 99,
        };

        unsafe {
            std::slice::from_raw_parts(
                &hdr as *const _ as *const u8,
                mem::size_of::<ProcEventHdr>(),
            )
        }
        .to_vec()
    }

    fn build_exec_message(pid: u32, tgid: u32) -> Vec<u8> {
        let mut payload = cn_msg_bytes(mem::size_of::<ProcEventHdr>() + 8);
        payload.extend_from_slice(&event_hdr_bytes(PROC_EVENT_EXEC));
        payload.extend_from_slice(&pid.to_ne_bytes());
        payload.extend_from_slice(&tgid.to_ne_bytes());

        let mut message = nl_hdr(0x10, payload.len());
        message.extend_from_slice(&payload);
        message
    }

    fn build_ack_message(error: i32) -> Vec<u8> {
        let err = NlMsgErr {
            error,
            msg: NlMsgHdr {
                nlmsg_len: mem::size_of::<NlMsgHdr>() as u32,
                nlmsg_type: NLMSG_DONE,
                nlmsg_flags: 0,
                nlmsg_seq: 1,
                nlmsg_pid: 0,
            },
        };

        let payload = unsafe {
            std::slice::from_raw_parts(
                &err as *const _ as *const u8,
                mem::size_of::<NlMsgErr>(),
            )
        };

        let mut message = nl_hdr(NLMSG_ERROR, payload.len());
        message.extend_from_slice(payload);
        message
    }

    fn build_exit_message(pid: u32, exit_code: u32) -> Vec<u8> {
        let mut payload = cn_msg_bytes(mem::size_of::<ProcEventHdr>() + 12);
        payload.extend_from_slice(&event_hdr_bytes(PROC_EVENT_EXIT));
        payload.extend_from_slice(&pid.to_ne_bytes());
        payload.extend_from_slice(&pid.to_ne_bytes());
        payload.extend_from_slice(&exit_code.to_ne_bytes());

        let mut message = nl_hdr(0x10, payload.len());
        message.extend_from_slice(&payload);
        message
    }

    #[test]
    fn parses_exec_event_message() {
        let parsed = parse_messages(&build_exec_message(123, 123));
        assert_eq!(parsed.len(), 1);
        match &parsed[0] {
            ParsedNetlinkMessage::ProcEvent(ProcEvent::Exec { pid, tgid }) => {
                assert_eq!((*pid, *tgid), (123, 123));
            }
            _ => panic!("expected exec event"),
        }
    }

    #[test]
    fn parses_ack_message() {
        let parsed = parse_messages(&build_ack_message(0));
        assert_eq!(parsed.len(), 1);
        assert!(matches!(parsed[0], ParsedNetlinkMessage::Ack));
    }

    #[test]
    fn parses_error_message() {
        let parsed = parse_messages(&build_ack_message(-libc::EPERM));
        assert_eq!(parsed.len(), 1);
        assert!(matches!(parsed[0], ParsedNetlinkMessage::Error(errno) if errno == -libc::EPERM));
    }

    #[test]
    fn ignores_truncated_connector_payload() {
        let mut payload = cn_msg_bytes(mem::size_of::<ProcEventHdr>() + 8);
        payload.extend_from_slice(&event_hdr_bytes(PROC_EVENT_EXEC));
        payload.extend_from_slice(&123u32.to_ne_bytes());

        let mut message = nl_hdr(0x10, payload.len());
        message.extend_from_slice(&payload);

        assert!(parse_messages(&message).is_empty());
    }

    #[test]
    fn parses_multiple_netlink_messages_from_single_buffer() {
        let mut buffer = build_exec_message(123, 123);
        buffer.extend_from_slice(&build_exit_message(123, 42));

        let parsed = parse_messages(&buffer);
        assert_eq!(parsed.len(), 2);
        assert!(matches!(parsed[0], ParsedNetlinkMessage::ProcEvent(ProcEvent::Exec { pid: 123, tgid: 123 })));
        assert!(matches!(parsed[1], ParsedNetlinkMessage::ProcEvent(ProcEvent::Exit { pid: 123, exit_code: 42 })));
    }
}
