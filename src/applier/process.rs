use std::fs::File;
use std::io::Write;
use std::os::fd::FromRawFd;

use anyhow::{Context, Result, bail};

pub(super) fn set_nice(pid: u32, nice: i32) -> Result<()> {
    let ret = unsafe { libc::setpriority(libc::PRIO_PROCESS, pid, nice) };
    if ret != 0 {
        bail!("setpriority: {}", std::io::Error::last_os_error());
    }
    Ok(())
}

pub(super) fn set_oom_score_adj(pid: u32, score: i32) -> Result<()> {
    let path = std::ffi::CString::new(format!("/proc/{pid}/oom_score_adj"))
        .expect("proc path is always valid ASCII");
    let value = format!("{score}\n");
    let flags = libc::O_WRONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW;
    let fd = unsafe { libc::open(path.as_ptr(), flags) };
    if fd < 0 {
        bail!(
            "open /proc/{pid}/oom_score_adj: {}",
            std::io::Error::last_os_error()
        );
    }
    let mut file = unsafe { File::from_raw_fd(fd) };
    file.write_all(value.as_bytes())
        .with_context(|| format!("write /proc/{pid}/oom_score_adj"))?;
    Ok(())
}
