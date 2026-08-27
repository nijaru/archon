//! Linux process creation inside a cgroup v2 lease.
//!
//! This module owns the raw `clone3`/`execvp` boundary. The runtime keeps
//! lifecycle and lease state; this module only creates and reaps one child.

use std::ffi::CString;
use std::io;
use std::mem::size_of;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::process::ExitStatusExt;
use std::process::ExitStatus;

use crate::cgroup::CgroupGroup;

const CLONE_INTO_CGROUP: u64 = 1u64 << 33;

#[repr(C)]
struct CloneArgs {
    flags: u64,
    pidfd: u64,
    child_tid: u64,
    parent_tid: u64,
    exit_signal: u64,
    stack: u64,
    stack_size: u64,
    tls: u64,
    set_tid: u64,
    set_tid_size: u64,
    cgroup: u64,
}

pub(crate) struct LinuxChild {
    pid: libc::pid_t,
    status: Option<i32>,
}

impl LinuxChild {
    pub(crate) fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        if let Some(status) = self.status {
            return Ok(Some(ExitStatus::from_raw(status)));
        }
        let mut status = 0;
        loop {
            // SAFETY: `self.pid` is a child returned by clone3 and `status`
            // is a valid writable wait-status pointer owned by this process.
            let result = unsafe { libc::waitpid(self.pid, &mut status, libc::WNOHANG) };
            if result == 0 {
                return Ok(None);
            }
            if result == -1 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error);
            }
            self.status = Some(status);
            return Ok(Some(ExitStatus::from_raw(status)));
        }
    }

    pub(crate) fn wait(&mut self) -> io::Result<ExitStatus> {
        if let Some(status) = self.status {
            return Ok(ExitStatus::from_raw(status));
        }
        let mut status = 0;
        loop {
            // SAFETY: `self.pid` is a child returned by clone3 and `status`
            // is a valid writable wait-status pointer owned by this process.
            let result = unsafe { libc::waitpid(self.pid, &mut status, 0) };
            if result == self.pid {
                self.status = Some(status);
                return Ok(ExitStatus::from_raw(status));
            }
            if result == -1 && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                continue;
            }
            if result == -1 {
                return Err(io::Error::last_os_error());
            }
            return Err(io::Error::other(format!(
                "waitpid returned unexpected pid {result}"
            )));
        }
    }

    pub(crate) fn kill(&mut self) -> io::Result<()> {
        if self.status.is_some() {
            return Ok(());
        }
        // SAFETY: `self.pid` is the tracked child process identifier.
        let result = unsafe { libc::kill(self.pid, libc::SIGKILL) };
        if result == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
}

pub(crate) fn spawn(
    group: &CgroupGroup,
    program: &str,
    args: &[String],
    log_file: &std::fs::File,
) -> io::Result<LinuxChild> {
    let program = CString::new(program)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "program contains a NUL byte"))?;
    let arguments: Vec<CString> = args
        .iter()
        .map(|arg| {
            CString::new(arg.as_str()).map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidInput, "argument contains a NUL byte")
            })
        })
        .collect::<io::Result<_>>()?;
    let mut command = Vec::with_capacity(arguments.len() + 1);
    command.push(program.clone());
    command.extend(arguments);
    let mut argv: Vec<*const libc::c_char> = command.iter().map(|value| value.as_ptr()).collect();
    argv.push(std::ptr::null());

    let stdin = std::fs::File::open("/dev/null")?;
    let stdout_log = log_file.try_clone()?;
    let stderr_log = log_file.try_clone()?;
    let cgroup_fd = group
        .open_fd()
        .map_err(|err| io::Error::other(format!("open cgroup: {err}")))?;
    let mut pipe = [0; 2];
    // SAFETY: `pipe` points to two writable file-descriptor slots and the
    // flags are valid for Linux pipe2.
    if unsafe { libc::pipe2(pipe.as_mut_ptr(), libc::O_CLOEXEC) } == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: pipe2 initialized both descriptors on success, and this
    // process now owns them.
    let error_read = unsafe { OwnedFd::from_raw_fd(pipe[0]) };
    // SAFETY: pipe2 initialized both descriptors on success, and this
    // process now owns them.
    let error_write = unsafe { OwnedFd::from_raw_fd(pipe[1]) };
    let parent_pid = unsafe { libc::getpid() };
    let mut clone_args = CloneArgs {
        flags: CLONE_INTO_CGROUP,
        pidfd: 0,
        child_tid: 0,
        parent_tid: 0,
        exit_signal: libc::SIGCHLD as u64,
        stack: 0,
        stack_size: 0,
        tls: 0,
        set_tid: 0,
        set_tid_size: 0,
        cgroup: cgroup_fd.as_raw_fd() as u64,
    };

    // SAFETY: `clone_args` is a fully initialized Linux clone3 ABI
    // structure. The child has a separate address space and exits through
    // `_exit` or `execvp` without running Rust destructors.
    let result = unsafe {
        libc::syscall(
            libc::SYS_clone3,
            &mut clone_args as *mut CloneArgs,
            size_of::<CloneArgs>(),
        )
    };
    if result == -1 {
        let error = io::Error::last_os_error();
        return Err(io::Error::new(
            error.kind(),
            format!("clone3(CLONE_INTO_CGROUP): {error}"),
        ));
    }
    if result == 0 {
        // SAFETY: the child owns a private copy of these descriptors. The
        // parent-death setup and close/dup2/exec operations are the only work
        // done before exec.
        unsafe {
            if libc::getppid() != parent_pid {
                report_child_error(error_write.as_raw_fd(), libc::ESRCH);
            }
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) == -1 {
                report_child_error(
                    error_write.as_raw_fd(),
                    io::Error::last_os_error()
                        .raw_os_error()
                        .unwrap_or(libc::EIO),
                );
            }
            libc::close(error_read.as_raw_fd());
            redirect_fd_or_exit(
                stdin.as_raw_fd(),
                libc::STDIN_FILENO,
                error_write.as_raw_fd(),
            );
            redirect_fd_or_exit(
                stdout_log.as_raw_fd(),
                libc::STDOUT_FILENO,
                error_write.as_raw_fd(),
            );
            redirect_fd_or_exit(
                stderr_log.as_raw_fd(),
                libc::STDERR_FILENO,
                error_write.as_raw_fd(),
            );
            libc::execvp(program.as_ptr(), argv.as_ptr());
            let errno = io::Error::last_os_error()
                .raw_os_error()
                .unwrap_or(libc::EIO);
            report_child_error(error_write.as_raw_fd(), errno);
        }
    }

    drop(error_write);
    let pid = result as libc::pid_t;
    let mut child = LinuxChild { pid, status: None };
    match read_child_error(error_read.as_raw_fd(), &mut child)? {
        None => Ok(child),
        Some(errno) => {
            let _ = child.wait();
            Err(io::Error::from_raw_os_error(errno))
        }
    }
}

fn redirect_fd_or_exit(source: RawFd, target: RawFd, error_fd: RawFd) {
    if source != target {
        // SAFETY: the descriptors are inherited from the parent and the
        // targets are the standard streams in the freshly cloned child.
        if unsafe { libc::dup2(source, target) } == -1 {
            let errno = io::Error::last_os_error()
                .raw_os_error()
                .unwrap_or(libc::EIO);
            report_child_error(error_fd, errno);
        }
        // SAFETY: `source` is an inherited descriptor no longer needed after
        // it has been duplicated onto the standard stream.
        unsafe {
            libc::close(source);
        }
    }
}

fn report_child_error(error_fd: RawFd, errno: i32) -> ! {
    let bytes = errno.to_ne_bytes();
    let mut written = 0;
    while written < bytes.len() {
        // SAFETY: `bytes` is a valid readable buffer and `error_fd` is the
        // inherited write end of the close-on-exec error pipe.
        let result = unsafe {
            libc::write(
                error_fd,
                bytes[written..].as_ptr().cast(),
                bytes.len() - written,
            )
        };
        if result > 0 {
            written += result as usize;
        } else {
            break;
        }
    }
    // SAFETY: this is the child of clone3 and must not run parent-side Rust
    // destructors after reporting a pre-exec failure.
    unsafe { libc::_exit(127) }
}

fn read_child_error(error_fd: RawFd, child: &mut LinuxChild) -> io::Result<Option<i32>> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        let timeout_ms = remaining.as_millis().min(i32::MAX as u128) as i32;
        let mut pollfd = libc::pollfd {
            fd: error_fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: `pollfd` is a valid writable descriptor record and the
        // timeout is bounded.
        let result = unsafe { libc::poll(&mut pollfd, 1, timeout_ms) };
        if result == 0 {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "timed out waiting for clone3 child exec",
            ));
        }
        if result < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }

        let mut bytes = [0u8; size_of::<i32>()];
        let mut read = 0;
        while read < bytes.len() {
            // SAFETY: `bytes` is a valid writable buffer and `error_fd` is
            // the parent-owned read end of the error pipe.
            let result = unsafe {
                libc::read(
                    error_fd,
                    bytes[read..].as_mut_ptr().cast(),
                    bytes.len() - read,
                )
            };
            if result == 0 {
                break;
            }
            if result < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
            read += result as usize;
        }
        return match read {
            0 => Ok(None),
            n if n == bytes.len() => Ok(Some(i32::from_ne_bytes(bytes))),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "short clone3 exec-error report",
                ))
            }
        };
    }
}
