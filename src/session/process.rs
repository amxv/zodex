#[cfg(any(target_os = "linux", target_os = "windows"))]
use std::collections::HashMap;
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
use std::collections::VecDeque;
#[cfg(target_os = "linux")]
use std::path::PathBuf;

use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};

#[cfg(unix)]
use nix::errno::Errno;
#[cfg(unix)]
use nix::sys::signal::{Signal, kill, killpg};
#[cfg(unix)]
use nix::unistd::Pid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProcessIdentity {
    pub pid: i32,
    pub birth: ProcessBirthIdentity,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProcessBirthIdentity {
    LinuxProcStartTicks { ticks: u64 },
    MacOsStartTime { seconds: u64, microseconds: u64 },
    WindowsCreationTime { ticks_100ns: u64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessSignal {
    Terminate,
    Kill,
}

pub trait ProcessInspector: Send + Sync {
    fn identity(&self, pid: i32) -> Result<Option<ProcessIdentity>>;
    fn live_cwd(&self, pid: i32) -> Option<String>;

    fn descendants(&self, _pid: i32, _limit: usize) -> Result<Vec<ProcessIdentity>> {
        Ok(Vec::new())
    }

    fn process_group_members(&self, _pgid: i32, _limit: usize) -> Result<Vec<ProcessIdentity>> {
        Ok(Vec::new())
    }
}

pub trait ProcessControl: ProcessInspector {
    fn signal_group(&self, pid: i32, signal: ProcessSignal) -> Result<()>;
}

#[derive(Debug, Default)]
pub struct SystemProcessInspector;

impl ProcessInspector for SystemProcessInspector {
    fn identity(&self, pid: i32) -> Result<Option<ProcessIdentity>> {
        system_process_identity(pid)
    }

    fn live_cwd(&self, pid: i32) -> Option<String> {
        system_live_cwd(pid)
    }

    fn descendants(&self, pid: i32, limit: usize) -> Result<Vec<ProcessIdentity>> {
        system_descendant_identities(pid, limit)
    }

    fn process_group_members(&self, pgid: i32, limit: usize) -> Result<Vec<ProcessIdentity>> {
        system_process_group_members(pgid, limit)
    }
}

impl ProcessControl for SystemProcessInspector {
    fn signal_group(&self, pid: i32, signal: ProcessSignal) -> Result<()> {
        signal_process_group(pid, signal)
    }
}

pub fn identity_matches(
    inspector: &dyn ProcessInspector,
    expected: &ProcessIdentity,
) -> Result<bool> {
    Ok(inspector.identity(expected.pid)?.as_ref() == Some(expected))
}

pub fn signal_process_if_matching(
    inspector: &dyn ProcessInspector,
    expected: &ProcessIdentity,
    signal: ProcessSignal,
) -> Result<bool> {
    if !identity_matches(inspector, expected)? {
        return Ok(false);
    }
    signal_process(expected.pid, signal)?;
    Ok(true)
}

#[cfg(unix)]
pub(crate) fn signal_process_group(pid: i32, signal: ProcessSignal) -> Result<()> {
    let signal = match signal {
        ProcessSignal::Terminate => Signal::SIGTERM,
        ProcessSignal::Kill => Signal::SIGKILL,
    };
    match killpg(Pid::from_raw(pid), signal) {
        Ok(_) | Err(Errno::ESRCH) => Ok(()),
        Err(error) => Err(anyhow!(
            "failed to send {signal:?} to process group {pid}: {error}"
        )),
    }
}

#[cfg(target_os = "windows")]
pub(crate) fn signal_process_group(pid: i32, signal: ProcessSignal) -> Result<()> {
    let inspector = SystemProcessInspector;
    let mut descendants = system_descendant_identities(pid, 1024)?;
    descendants.reverse();
    for descendant in descendants {
        let _ = signal_process_if_matching(&inspector, &descendant, signal);
    }
    signal_process(pid, signal)
}

#[cfg(not(any(unix, target_os = "windows")))]
pub(crate) fn signal_process_group(_pid: i32, _signal: ProcessSignal) -> Result<()> {
    Err(anyhow!(
        "process-group signaling is unsupported on this host"
    ))
}

#[cfg(unix)]
fn signal_process(pid: i32, signal: ProcessSignal) -> Result<()> {
    let signal = match signal {
        ProcessSignal::Terminate => Signal::SIGTERM,
        ProcessSignal::Kill => Signal::SIGKILL,
    };
    match kill(Pid::from_raw(pid), signal) {
        Ok(_) | Err(Errno::ESRCH) => Ok(()),
        Err(error) => Err(anyhow!("failed to send {signal:?} to PID {pid}: {error}")),
    }
}

#[cfg(target_os = "windows")]
fn signal_process(pid: i32, _signal: ProcessSignal) -> Result<()> {
    use sysinfo::{Pid as SysPid, System};

    let system = System::new_all();
    let Some(process) = system.process(SysPid::from_u32(pid as u32)) else {
        return Ok(());
    };
    if process.kill() {
        Ok(())
    } else {
        Err(anyhow!("failed to terminate Windows PID {pid}"))
    }
}

#[cfg(not(any(unix, target_os = "windows")))]
fn signal_process(_pid: i32, _signal: ProcessSignal) -> Result<()> {
    Err(anyhow!("process signaling is unsupported on this host"))
}

#[cfg(target_os = "linux")]
fn system_process_identity(pid: i32) -> Result<Option<ProcessIdentity>> {
    let Some(stat) = read_linux_proc_stat(pid) else {
        return Ok(None);
    };
    Ok(Some(ProcessIdentity {
        pid,
        birth: ProcessBirthIdentity::LinuxProcStartTicks {
            ticks: stat.start_ticks,
        },
    }))
}

#[cfg(target_os = "linux")]
fn system_live_cwd(pid: i32) -> Option<String> {
    let target_pgrp = read_linux_proc_stat(pid)?.pgrp;
    let mut best: Option<(i32, PathBuf)> = None;
    let proc_entries = std::fs::read_dir("/proc").ok()?;
    for entry in proc_entries.flatten() {
        let raw = entry.file_name();
        let raw = raw.to_string_lossy();
        if !raw.bytes().all(|byte| byte.is_ascii_digit()) {
            continue;
        }
        let Ok(proc_pid) = raw.parse::<i32>() else {
            continue;
        };
        if read_linux_proc_stat(proc_pid).map(|stat| stat.pgrp) != Some(target_pgrp) {
            continue;
        }
        let Ok(cwd) = std::fs::read_link(format!("/proc/{proc_pid}/cwd")) else {
            continue;
        };
        if best
            .as_ref()
            .is_none_or(|(best_pid, _)| proc_pid > *best_pid)
        {
            best = Some((proc_pid, cwd));
        }
    }
    best.map(|(_, cwd)| cwd.display().to_string()).or_else(|| {
        std::fs::read_link(format!("/proc/{pid}/cwd"))
            .ok()
            .map(|cwd| cwd.display().to_string())
    })
}

#[cfg(target_os = "linux")]
struct LinuxProcStat {
    ppid: i32,
    pgrp: i32,
    start_ticks: u64,
}

#[cfg(target_os = "linux")]
fn read_linux_proc_stat(pid: i32) -> Option<LinuxProcStat> {
    let raw = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let (_, after_comm) = raw.rsplit_once(") ")?;
    let fields = after_comm.split_whitespace().collect::<Vec<_>>();
    Some(LinuxProcStat {
        ppid: fields.get(1)?.parse().ok()?,
        pgrp: fields.get(2)?.parse().ok()?,
        start_ticks: fields.get(19)?.parse().ok()?,
    })
}

#[cfg(target_os = "linux")]
fn system_descendant_identities(pid: i32, limit: usize) -> Result<Vec<ProcessIdentity>> {
    if limit == 0 {
        return Ok(Vec::new());
    }

    // Bound both result size and process-table scanning. This is a best-effort
    // cleanup seam, not an unbounded inventory walk on a busy developer host.
    let scan_limit = limit.saturating_mul(16).clamp(256, 8192);
    let mut children_by_parent = HashMap::<i32, Vec<i32>>::new();
    let entries = match std::fs::read_dir("/proc") {
        Ok(entries) => entries,
        Err(_) => return Ok(Vec::new()),
    };
    for entry in entries.flatten().take(scan_limit) {
        let raw = entry.file_name();
        let raw = raw.to_string_lossy();
        if !raw.bytes().all(|byte| byte.is_ascii_digit()) {
            continue;
        }
        let Ok(child_pid) = raw.parse::<i32>() else {
            continue;
        };
        let Some(stat) = read_linux_proc_stat(child_pid) else {
            continue;
        };
        children_by_parent
            .entry(stat.ppid)
            .or_default()
            .push(child_pid);
    }

    let mut queue = VecDeque::from([pid]);
    let mut result = Vec::new();
    while let Some(parent) = queue.pop_front() {
        let Some(children) = children_by_parent.get(&parent) else {
            continue;
        };
        for child_pid in children {
            if result.len() >= limit {
                return Ok(result);
            }
            if let Some(identity) = system_process_identity(*child_pid)? {
                result.push(identity);
                queue.push_back(*child_pid);
            }
        }
    }
    Ok(result)
}

#[cfg(target_os = "linux")]
fn system_process_group_members(pgid: i32, limit: usize) -> Result<Vec<ProcessIdentity>> {
    if limit == 0 {
        return Ok(Vec::new());
    }

    let scan_limit = limit.saturating_mul(16).clamp(256, 8192);
    let entries = match std::fs::read_dir("/proc") {
        Ok(entries) => entries,
        Err(_) => return Ok(Vec::new()),
    };
    let mut result = Vec::new();
    for entry in entries.flatten().take(scan_limit) {
        let raw = entry.file_name();
        let raw = raw.to_string_lossy();
        if !raw.bytes().all(|byte| byte.is_ascii_digit()) {
            continue;
        }
        let Ok(pid) = raw.parse::<i32>() else {
            continue;
        };
        let Some(stat) = read_linux_proc_stat(pid) else {
            continue;
        };
        if stat.pgrp != pgid {
            continue;
        }
        if let Some(identity) = system_process_identity(pid)? {
            result.push(identity);
            if result.len() >= limit {
                break;
            }
        }
    }
    Ok(result)
}

#[cfg(target_os = "macos")]
fn system_process_identity(pid: i32) -> Result<Option<ProcessIdentity>> {
    use std::mem::{MaybeUninit, size_of};

    use nix::libc;

    let mut info = MaybeUninit::<libc::proc_bsdinfo>::zeroed();
    let expected = size_of::<libc::proc_bsdinfo>();
    // SAFETY: `info` points at an appropriately sized writable proc_bsdinfo
    // buffer and proc_pidinfo initializes it on a full successful return.
    let read = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            expected as i32,
        )
    };
    if read <= 0 {
        return Ok(None);
    }
    if read as usize != expected {
        return Err(anyhow!(
            "macOS proc_pidinfo returned {read} bytes for PID {pid}; expected {expected}"
        ));
    }
    // SAFETY: the full-size return above establishes initialization.
    let info = unsafe { info.assume_init() };
    Ok(Some(ProcessIdentity {
        pid,
        birth: ProcessBirthIdentity::MacOsStartTime {
            seconds: info.pbi_start_tvsec,
            microseconds: info.pbi_start_tvusec,
        },
    }))
}

#[cfg(target_os = "macos")]
fn system_live_cwd(pid: i32) -> Option<String> {
    use std::ffi::CStr;
    use std::mem::{MaybeUninit, size_of};

    use nix::libc;

    let mut info = MaybeUninit::<libc::proc_vnodepathinfo>::zeroed();
    let expected = size_of::<libc::proc_vnodepathinfo>();
    // SAFETY: `info` is a correctly sized writable proc_vnodepathinfo buffer.
    let read = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDVNODEPATHINFO,
            0,
            info.as_mut_ptr().cast(),
            expected as i32,
        )
    };
    if read as usize != expected {
        return None;
    }
    // SAFETY: the full-size return above establishes initialization. vip_path
    // is Apple's fixed MAXPATHLEN NUL-terminated char storage; libc represents
    // it as nested arrays only for Rust compatibility, so the first element is
    // still the beginning of one contiguous C string buffer.
    let info = unsafe { info.assume_init() };
    let path_ptr = info.pvi_cdir.vip_path.as_ptr().cast::<libc::c_char>();
    let path = unsafe { CStr::from_ptr(path_ptr) };
    let text = path.to_string_lossy();
    (!text.is_empty()).then(|| text.into_owned())
}

#[cfg(target_os = "macos")]
fn system_descendant_identities(pid: i32, limit: usize) -> Result<Vec<ProcessIdentity>> {
    use std::mem::size_of;

    use nix::libc;

    if limit == 0 {
        return Ok(Vec::new());
    }

    // proc_listchildpids returns a count of pid_t values (the libproc wrapper
    // converts proc_listpids' byte count). Fixed-size batches keep the sweep
    // bounded even if the host has a pathological process tree.
    const CHILD_BATCH: usize = 256;
    let mut queue = VecDeque::from([pid]);
    let mut result = Vec::new();
    while let Some(parent) = queue.pop_front() {
        let mut children = [0 as libc::pid_t; CHILD_BATCH];
        let count = unsafe {
            libc::proc_listchildpids(
                parent,
                children.as_mut_ptr().cast(),
                size_of::<[libc::pid_t; CHILD_BATCH]>() as i32,
            )
        };
        if count <= 0 {
            continue;
        }
        for child_pid in children.into_iter().take(count as usize) {
            if child_pid <= 0 {
                continue;
            }
            if result.len() >= limit {
                return Ok(result);
            }
            if let Some(identity) = system_process_identity(child_pid)? {
                result.push(identity);
                queue.push_back(child_pid);
            }
        }
    }
    Ok(result)
}

#[cfg(target_os = "macos")]
fn system_process_group_members(pgid: i32, limit: usize) -> Result<Vec<ProcessIdentity>> {
    use std::mem::{MaybeUninit, size_of};

    use nix::libc;

    if limit == 0 {
        return Ok(Vec::new());
    }

    let scan_limit = limit.saturating_mul(16).clamp(256, 8192);
    let mut pids = vec![0 as libc::pid_t; scan_limit];
    let count = unsafe {
        libc::proc_listallpids(
            pids.as_mut_ptr().cast(),
            (pids.len() * size_of::<libc::pid_t>()) as i32,
        )
    };
    if count <= 0 {
        return Ok(Vec::new());
    }

    let mut result = Vec::new();
    for pid in pids.into_iter().take(count as usize) {
        if pid <= 0 {
            continue;
        }
        let mut info = MaybeUninit::<libc::proc_bsdinfo>::zeroed();
        let expected = size_of::<libc::proc_bsdinfo>();
        let read = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDTBSDINFO,
                0,
                info.as_mut_ptr().cast(),
                expected as i32,
            )
        };
        if read as usize != expected {
            continue;
        }
        let info = unsafe { info.assume_init() };
        if info.pbi_pgid as i32 != pgid {
            continue;
        }
        result.push(ProcessIdentity {
            pid,
            birth: ProcessBirthIdentity::MacOsStartTime {
                seconds: info.pbi_start_tvsec,
                microseconds: info.pbi_start_tvusec,
            },
        });
        if result.len() >= limit {
            break;
        }
    }
    Ok(result)
}

#[cfg(target_os = "windows")]
fn system_process_identity(pid: i32) -> Result<Option<ProcessIdentity>> {
    use std::mem::MaybeUninit;

    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME};
    use windows_sys::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    if pid <= 0 {
        return Ok(None);
    }
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid as u32) };
    if handle.is_null() {
        return Ok(None);
    }
    let mut creation = MaybeUninit::<FILETIME>::zeroed();
    let mut exit = MaybeUninit::<FILETIME>::zeroed();
    let mut kernel = MaybeUninit::<FILETIME>::zeroed();
    let mut user = MaybeUninit::<FILETIME>::zeroed();
    let ok = unsafe {
        GetProcessTimes(
            handle,
            creation.as_mut_ptr(),
            exit.as_mut_ptr(),
            kernel.as_mut_ptr(),
            user.as_mut_ptr(),
        )
    };
    unsafe { CloseHandle(handle) };
    if ok == 0 {
        return Ok(None);
    }
    let creation = unsafe { creation.assume_init() };
    let ticks_100ns = ((creation.dwHighDateTime as u64) << 32) | creation.dwLowDateTime as u64;
    Ok(Some(ProcessIdentity {
        pid,
        birth: ProcessBirthIdentity::WindowsCreationTime { ticks_100ns },
    }))
}

#[cfg(target_os = "windows")]
fn windows_system() -> sysinfo::System {
    sysinfo::System::new_all()
}

#[cfg(target_os = "windows")]
fn system_live_cwd(pid: i32) -> Option<String> {
    use sysinfo::Pid as SysPid;

    windows_system()
        .process(SysPid::from_u32(pid as u32))
        .and_then(|process| process.cwd())
        .map(|cwd| cwd.display().to_string())
}

#[cfg(target_os = "windows")]
fn system_descendant_identities(pid: i32, limit: usize) -> Result<Vec<ProcessIdentity>> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let system = windows_system();
    let mut children_by_parent = HashMap::<u32, Vec<u32>>::new();
    for (process_pid, process) in system.processes() {
        let Some(parent) = process.parent() else {
            continue;
        };
        children_by_parent
            .entry(parent.as_u32())
            .or_default()
            .push(process_pid.as_u32());
    }
    let mut queue = VecDeque::from([pid as u32]);
    let mut result = Vec::new();
    while let Some(parent) = queue.pop_front() {
        let Some(children) = children_by_parent.get(&parent) else {
            continue;
        };
        for child in children {
            if result.len() >= limit {
                return Ok(result);
            }
            if let Some(identity) = system_process_identity(*child as i32)? {
                result.push(identity);
                queue.push_back(*child);
            }
        }
    }
    Ok(result)
}

#[cfg(target_os = "windows")]
fn system_process_group_members(pid: i32, limit: usize) -> Result<Vec<ProcessIdentity>> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let mut result = Vec::new();
    if let Some(leader) = system_process_identity(pid)? {
        result.push(leader);
    }
    if result.len() < limit {
        result.extend(system_descendant_identities(pid, limit - result.len())?);
    }
    Ok(result)
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn system_process_identity(_pid: i32) -> Result<Option<ProcessIdentity>> {
    Ok(None)
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn system_live_cwd(_pid: i32) -> Option<String> {
    None
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn system_descendant_identities(_pid: i32, _limit: usize) -> Result<Vec<ProcessIdentity>> {
    Ok(Vec::new())
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn system_process_group_members(_pgid: i32, _limit: usize) -> Result<Vec<ProcessIdentity>> {
    Ok(Vec::new())
}

#[cfg(test)]
mod tests {
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    use super::ProcessBirthIdentity;
    use super::{ProcessInspector, SystemProcessInspector};

    #[test]
    fn current_process_has_stable_identity_and_cwd_on_supported_host() {
        let inspector = SystemProcessInspector;
        let pid = std::process::id() as i32;
        let first = inspector.identity(pid).unwrap().expect("process identity");
        let second = inspector.identity(pid).unwrap().expect("process identity");
        assert_eq!(first, second);
        assert_eq!(first.pid, pid);
        #[cfg(target_os = "linux")]
        assert!(matches!(
            first.birth,
            ProcessBirthIdentity::LinuxProcStartTicks { ticks } if ticks > 0
        ));
        assert!(inspector.live_cwd(pid).is_some());
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_current_process_uses_creation_time_identity() {
        let inspector = SystemProcessInspector;
        let pid = std::process::id() as i32;
        let identity = inspector.identity(pid).unwrap().expect("process identity");
        assert!(matches!(
            identity.birth,
            ProcessBirthIdentity::WindowsCreationTime { ticks_100ns } if ticks_100ns > 0
        ));
        assert!(
            inspector
                .process_group_members(pid, 64)
                .unwrap()
                .contains(&identity)
        );
    }
}
