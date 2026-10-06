//! Utilities for operating on processes.

use std::path::PathBuf;

use rustix::io::Errno;
use rustix::process::{self, Pid};

/// Try to convert a [`u32`] PID into a typed [`Pid`].
///
/// Standard library functions like [`std::process::id`] use [`u32`], but functions in this module
/// expect [`Pid`].
pub fn pid_from_u32(pid: u32) -> Option<Pid> {
    i32::try_from(pid).ok().and_then(rustix::process::Pid::from_raw)
}

/// Check whether the given pid is alive.
#[must_use]
pub fn is_alive(pid: Pid) -> bool {
    !matches!(process::test_kill_process(pid), Err(Errno::SRCH))
}

/// Get when the process was started in seconds.
pub fn process_start_time(pid: Pid) -> Option<u64> {
    let pid = sysinfo::Pid::from_u32(pid.as_raw_nonzero().get().unsigned_abs());
    let mut system = sysinfo::System::new();
    if system.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[pid]), true) > 0 {
        system.process(pid).map(sysinfo::Process::start_time)
    } else {
        None
    }
}

/// Get a process's current working directory.
#[must_use]
pub fn cwd(pid: Pid) -> Option<PathBuf> {
    let pid = u32::try_from(pid.as_raw_pid()).ok()?;

    if cfg!(target_os = "linux") {
        std::fs::read_link(format!("/proc/{pid}/cwd")).ok()
    } else {
        cwd_generic(pid)
    }
}

/// Implementation of [`cwd`] that works on all Unix-like systems.
///
/// Linux has a specific, more efficient implementation.
fn cwd_generic(pid: u32) -> Option<PathBuf> {
    let pid = sysinfo::Pid::from_u32(pid);
    let mut system = sysinfo::System::new();
    if system.refresh_processes_specifics(
        sysinfo::ProcessesToUpdate::Some(&[pid]),
        true,
        sysinfo::ProcessRefreshKind::nothing().with_cwd(sysinfo::UpdateKind::Always),
    ) == 0
    {
        return None;
    }
    system.process(pid)?.cwd().map(Into::into)
}

#[cfg(test)]
mod tests {
    use std::process::{Child, Command};
    use std::time::{Duration, Instant};

    use rstest::rstest;

    use super::*;

    /// Spawn a process that sits still in `dir`.
    fn sleeper(dir: &std::path::Path) -> (Child, Pid) {
        let child = Command::new("sleep").arg("30").current_dir(dir).spawn().unwrap();
        let pid = Pid::from_raw(child.id().cast_signed()).unwrap();
        (child, pid)
    }

    /// Wait for `read` to return `expected`, then report what it last returned.
    ///
    /// A child chdirs somewhere between fork and exec, so it may not have arrived yet.
    fn settles_on(
        expected: &std::path::Path,
        mut read: impl FnMut() -> Option<PathBuf>,
    ) -> Option<PathBuf> {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let found = read();
            if found.as_deref() == Some(expected) || Instant::now() >= deadline {
                return found;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[rstest]
    fn reads_the_working_directory_of_another_process() {
        let dir = tempfile::tempdir().unwrap();
        // The temporary directory may sit behind a symlink (/tmp on macOS), and a working
        // directory never does.
        let expected = dir.path().canonicalize().unwrap();
        let (mut child, pid) = sleeper(dir.path());

        let found = settles_on(&expected, || cwd(pid));

        child.kill().unwrap();
        child.wait().unwrap();
        assert_eq!(found.as_deref(), Some(expected.as_path()));
    }

    #[rstest]
    fn the_generic_lookup_reads_the_working_directory_too() {
        // Linux takes the /proc route, so without this the portable route would go untested
        // until it reached a platform that has no choice but to use it.
        let dir = tempfile::tempdir().unwrap();
        let expected = dir.path().canonicalize().unwrap();
        let (mut child, _) = sleeper(dir.path());
        let pid = child.id();

        let found = settles_on(&expected, || cwd_generic(pid));

        child.kill().unwrap();
        child.wait().unwrap();
        assert_eq!(found.as_deref(), Some(expected.as_path()));
    }

    #[rstest]
    fn a_process_that_has_gone_has_no_working_directory() {
        let dir = tempfile::tempdir().unwrap();
        let (mut child, pid) = sleeper(dir.path());
        child.kill().unwrap();
        child.wait().unwrap();

        assert_eq!(cwd(pid), None);
    }

    #[rstest]
    #[case::zero(0, None)]
    #[case::one(1, Some(1))]
    #[case::max(i32::MAX.cast_unsigned(), Some(i32::MAX))]
    #[case::just_over_max(i32::MAX.cast_unsigned() + 1, None)]
    #[case::minus_one_if_wrapped(u32::MAX, None)]
    fn test_pid_from_u32(#[case] pid: u32, #[case] expected: Option<i32>) {
        assert_eq!(pid_from_u32(pid).map(Pid::as_raw_pid), expected);
    }
}
