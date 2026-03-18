use std::ffi::{CStr, CString};
use std::fs::File;
use std::io::Write;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path};

use anyhow::{Context, Result, bail};

pub(super) fn move_to_cgroup(
    pid: u32,
    cgroup: &str,
    weight: Option<u64>,
    io_weight: Option<u16>,
) -> Result<Option<u16>> {
    let components = validated_cgroup_components(cgroup)?;
    let cg_dir =
        open_cgroup_dir(&components).with_context(|| format!("open cgroup dir for '{cgroup}'"))?;

    let pid_value = format!("{pid}\n");
    write_control_file(&cg_dir, c"cgroup.procs", pid_value.as_bytes())
        .with_context(|| format!("write cgroup.procs for '{cgroup}'"))?;

    if let Some(w) = weight {
        let w = w.clamp(1, 10_000);
        let weight_value = format!("{w}\n");
        match write_control_file(&cg_dir, c"cpu.weight", weight_value.as_bytes()) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                tracing::debug!(cgroup, "cpu.weight missing, skipping weight write");
            }
            Err(err) => {
                return Err(err).with_context(|| format!("write cpu.weight for '{cgroup}'"));
            }
        }
    }

    let mut applied_io_weight = None;
    if let Some(w) = io_weight {
        let io_weight_value = format!("{w}\n");
        match write_control_file(&cg_dir, c"io.weight", io_weight_value.as_bytes()) {
            Ok(()) => applied_io_weight = Some(w),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                tracing::debug!(cgroup, "io.weight missing, skipping io weight write");
            }
            Err(err) => {
                return Err(err).with_context(|| format!("write io.weight for '{cgroup}'"));
            }
        }
    }

    Ok(applied_io_weight)
}

pub(super) fn validated_cgroup_components(cgroup: &str) -> Result<Vec<CString>> {
    if cgroup.is_empty() {
        bail!("Refusing empty cgroup path");
    }
    if cgroup.starts_with('/') {
        bail!("Refusing absolute cgroup path: '{cgroup}'");
    }

    let mut components = Vec::new();
    for component in Path::new(cgroup).components() {
        match component {
            Component::Normal(part) => {
                let bytes = part.as_bytes();
                if bytes.is_empty() {
                    bail!("Refusing empty cgroup path component in '{cgroup}'");
                }
                components.push(CString::new(bytes).context("cgroup component contains NUL")?);
            }
            Component::CurDir
            | Component::ParentDir
            | Component::RootDir
            | Component::Prefix(_) => {
                bail!("Refusing unsafe cgroup path: '{cgroup}'");
            }
        }
    }

    if components.is_empty() {
        bail!("Refusing empty cgroup path");
    }

    Ok(components)
}

pub(super) fn cgroup_base_components() -> Result<Vec<CString>> {
    let uid = unsafe { libc::geteuid() };
    cgroup_base_components_for_uid(uid)
}

pub(super) fn cgroup_base_components_for_uid(uid: u32) -> Result<Vec<CString>> {
    let user_slice = format!("user-{uid}.slice");
    let user_service = format!("user@{uid}.service");

    Ok(vec![
        CString::new("user.slice").expect("static string without NUL"),
        CString::new(user_slice).context("invalid user slice name")?,
        CString::new(user_service).context("invalid user service name")?,
        CString::new("arbiter.slice").expect("static string without NUL"),
    ])
}

fn open_cgroup_dir(components: &[CString]) -> Result<OwnedFd> {
    let root = CString::new("/sys/fs/cgroup").expect("static path without NUL");
    let root_fd = open_dir_at(libc::AT_FDCWD, &root).context("open /sys/fs/cgroup")?;
    let base_components = cgroup_base_components().context("resolve user cgroup scope")?;

    let mut current = root_fd;
    for component in base_components.iter().chain(components.iter()) {
        let mkdir_ret = unsafe { libc::mkdirat(current.as_raw_fd(), component.as_ptr(), 0o755) };
        if mkdir_ret != 0 {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() != Some(libc::EEXIST) {
                return Err(err).with_context(|| {
                    format!("mkdirat component '{}'", component.to_string_lossy())
                });
            }
        }

        current = open_dir_at(current.as_raw_fd(), component)
            .with_context(|| format!("openat component '{}'", component.to_string_lossy()))?;
    }

    Ok(current)
}

fn open_dir_at(base_fd: libc::c_int, path: &CStr) -> Result<OwnedFd> {
    let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW;
    let fd = unsafe { libc::openat(base_fd, path.as_ptr(), flags) };
    if fd < 0 {
        bail!("openat: {}", std::io::Error::last_os_error());
    }

    let owned = unsafe { OwnedFd::from_raw_fd(fd) };
    Ok(owned)
}

fn write_control_file(
    dir_fd: &OwnedFd,
    name: &CStr,
    contents: &[u8],
) -> Result<(), std::io::Error> {
    let flags = libc::O_WRONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW;
    let fd = unsafe { libc::openat(dir_fd.as_raw_fd(), name.as_ptr(), flags) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }

    let mut file = unsafe { File::from_raw_fd(fd) };
    file.write_all(contents)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{cgroup_base_components_for_uid, validated_cgroup_components};

    #[test]
    fn rejects_unsafe_cgroup_paths() {
        for path in [
            "",
            "/system.slice",
            "../escape",
            "games/../escape",
            "./games",
        ] {
            assert!(
                validated_cgroup_components(path).is_err(),
                "path {path} should fail"
            );
        }
    }

    #[test]
    fn accepts_nested_relative_cgroup_paths() {
        let parts = validated_cgroup_components("apps/games.slice").unwrap();
        let text: Vec<String> = parts
            .iter()
            .map(|part| part.to_string_lossy().into_owned())
            .collect();
        assert_eq!(text, vec!["apps".to_string(), "games.slice".to_string()]);
    }

    #[test]
    fn cgroup_base_scope_is_user_local_and_arbiter_scoped() {
        let parts = cgroup_base_components_for_uid(1000).unwrap();
        let text: Vec<String> = parts
            .iter()
            .map(|part| part.to_string_lossy().into_owned())
            .collect();

        assert_eq!(
            text,
            vec![
                "user.slice".to_string(),
                "user-1000.slice".to_string(),
                "user@1000.service".to_string(),
                "arbiter.slice".to_string(),
            ]
        );
    }
}
