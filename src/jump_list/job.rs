use crate::visual::wide;
use anyhow::{Result, bail, ensure};
use std::{
    mem::{size_of, zeroed},
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::*,
    System::{JobObjects::*, Threading::*},
};
struct Handle(HANDLE);
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}
pub(super) fn quote(value: &str) -> String {
    let mut result = String::from("\"");
    let mut slashes = 0;
    for c in value.chars() {
        if c == '\\' {
            slashes += 1;
            continue;
        }
        result.extend(std::iter::repeat_n(
            '\\',
            if c == '"' { slashes * 2 + 1 } else { slashes },
        ));
        result.push(c);
        slashes = 0;
    }
    result.extend(std::iter::repeat_n('\\', slashes * 2));
    result.push('"');
    result
}
pub fn run(request: &Path, generation: u64, current: &AtomicU64) -> Result<()> {
    unsafe {
        let raw_job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        ensure!(!raw_job.is_null(), "Cannot create Jump List query job");
        let mut job = Some(Handle(raw_job));
        let mut allowed = 0;
        let mut system = 0;
        ensure!(
            GetProcessAffinityMask(GetCurrentProcess(), &mut allowed, &mut system) != 0,
            "Cannot query affinity"
        );
        let affinity = allowed & allowed.wrapping_neg();
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = zeroed();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
            | JOB_OBJECT_LIMIT_PRIORITY_CLASS
            | JOB_OBJECT_LIMIT_AFFINITY;
        limits.BasicLimitInformation.PriorityClass = BELOW_NORMAL_PRIORITY_CLASS;
        limits.BasicLimitInformation.Affinity = affinity;
        ensure!(
            SetInformationJobObject(
                raw_job,
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32
            ) != 0,
            "Cannot constrain query process"
        );
        let cpu = JOBOBJECT_CPU_RATE_CONTROL_INFORMATION {
            ControlFlags: JOB_OBJECT_CPU_RATE_CONTROL_ENABLE | JOB_OBJECT_CPU_RATE_CONTROL_HARD_CAP,
            Anonymous: JOBOBJECT_CPU_RATE_CONTROL_INFORMATION_0 { CpuRate: 2000 },
        };
        ensure!(
            SetInformationJobObject(
                raw_job,
                JobObjectCpuRateControlInformation,
                (&cpu as *const JOBOBJECT_CPU_RATE_CONTROL_INFORMATION).cast(),
                size_of::<JOBOBJECT_CPU_RATE_CONTROL_INFORMATION>() as u32
            ) != 0,
            "Cannot constrain query CPU"
        );
        let exe = std::env::current_exe()?;
        let command = format!(
            "{} --jump-list-helper {}",
            quote(&exe.to_string_lossy()),
            quote(&request.to_string_lossy())
        );
        let mut command = wide(&command);
        let mut startup: STARTUPINFOW = zeroed();
        startup.cb = size_of::<STARTUPINFOW>() as u32;
        let mut info: PROCESS_INFORMATION = zeroed();
        ensure!(
            CreateProcessW(
                wide(&exe.to_string_lossy()).as_ptr(),
                command.as_mut_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                0,
                CREATE_SUSPENDED | CREATE_NO_WINDOW | BELOW_NORMAL_PRIORITY_CLASS,
                std::ptr::null(),
                std::ptr::null(),
                &startup,
                &mut info
            ) != 0,
            "Cannot launch query helper ({})",
            GetLastError()
        );
        let process = Handle(info.hProcess);
        let thread = Handle(info.hThread);
        if AssignProcessToJobObject(raw_job, process.0) == 0 {
            TerminateProcess(process.0, 1);
            WaitForSingleObject(process.0, 500);
            bail!("Cannot isolate query helper ({})", GetLastError())
        }
        ensure!(
            ResumeThread(thread.0) != u32::MAX,
            "Cannot resume query helper"
        );
        let deadline = Instant::now() + Duration::from_millis(2500);
        loop {
            if WaitForSingleObject(process.0, 25) == WAIT_OBJECT_0 {
                let mut code = 1;
                GetExitCodeProcess(process.0, &mut code);
                ensure!(code == 0, "Jump List helper exited with {code:#x}");
                return Ok(());
            }
            if current.load(Ordering::Relaxed) != generation || Instant::now() >= deadline {
                drop(job.take());
                WaitForSingleObject(process.0, 500);
                bail!("Jump List query cancelled or timed out")
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn command_arguments_preserve_quotes_and_trailing_backslashes() {
        assert_eq!(quote("C:\\With space\\"), "\"C:\\With space\\\\\"");
        assert_eq!(quote("a\"b"), "\"a\\\"b\"");
    }
}
