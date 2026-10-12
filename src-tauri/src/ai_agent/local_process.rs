use crate::app_error::AppError;
use tokio::process::{Child, Command};

// Own the whole command tree independently of the shell's lifetime. A shell
// can exit while a descendant still owns stdout/stderr and keeps exec waiting.
pub(super) struct LocalCommandTree {
    #[cfg(windows)]
    job: std::os::windows::io::OwnedHandle,
    #[cfg(unix)]
    process_group: u32,
    released: bool,
}

fn tree_error(error: impl std::fmt::Display) -> AppError {
    AppError::new(
        "ai_command_cancel_failed",
        "管理本地命令进程树失败。",
        error,
        true,
    )
}

impl LocalCommandTree {
    pub(super) fn configure(command: &mut Command) {
        // Attach the suspended shell before it can launch children. This avoids
        // the race between spawn and job assignment; no visible helper window.
        #[cfg(windows)]
        command.creation_flags(0x08000004); // CREATE_NO_WINDOW | CREATE_SUSPENDED
        #[cfg(unix)]
        command.process_group(0);
    }

    pub(super) fn attach(child: &Child) -> Result<Self, AppError> {
        #[cfg(windows)]
        {
            use std::os::windows::io::{FromRawHandle, OwnedHandle};
            use windows::Win32::Foundation::HANDLE;
            use windows::Win32::System::JobObjects::{AssignProcessToJobObject, CreateJobObjectW};
            let raw = unsafe { CreateJobObjectW(None, windows_core::PCWSTR::null()) }
                .map_err(tree_error)?;
            // Each successful Win32 acquisition has one OwnedHandle owner.
            let job = unsafe { OwnedHandle::from_raw_handle(raw.0) };
            let tree = Self {
                job,
                released: false,
            };
            tree.set_kill_on_close(true)?;
            let process_handle = child
                .raw_handle()
                .ok_or_else(|| tree_error("missing shell handle"))?;
            unsafe { AssignProcessToJobObject(raw, HANDLE(process_handle)) }.map_err(tree_error)?;
            tree.resume_shell(child.id().ok_or_else(|| tree_error("missing shell PID"))?)?;
            Ok(tree)
        }
        #[cfg(unix)]
        {
            Ok(Self {
                process_group: child.id().ok_or_else(|| tree_error("missing shell PID"))?,
                released: false,
            })
        }
    }

    pub(super) fn terminate(&self) -> Result<(), AppError> {
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;
            use windows::Win32::{Foundation::HANDLE, System::JobObjects::TerminateJobObject};
            unsafe { TerminateJobObject(HANDLE(self.job.as_raw_handle()), 1) }.map_err(tree_error)
        }
        #[cfg(unix)]
        {
            if unsafe { libc::kill(-(self.process_group as i32), libc::SIGKILL) } == 0 {
                return Ok(());
            }
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ESRCH) {
                Ok(())
            } else {
                Err(tree_error(error))
            }
        }
    }

    pub(super) fn release(&mut self) -> Result<(), AppError> {
        // A naturally finished command may deliberately leave detached work.
        // Cancellation and dropped futures always retain tree cleanup ownership.
        #[cfg(windows)]
        self.set_kill_on_close(false)?;
        self.released = true;
        Ok(())
    }

    #[cfg(windows)]
    fn set_kill_on_close(&self, enabled: bool) -> Result<(), AppError> {
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::Foundation::HANDLE;
        use windows::Win32::System::JobObjects::{
            JobObjectExtendedLimitInformation, SetInformationJobObject,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        };
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        if enabled {
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        }
        unsafe {
            SetInformationJobObject(
                HANDLE(self.job.as_raw_handle()),
                JobObjectExtendedLimitInformation,
                &limits as *const _ as *const _,
                std::mem::size_of_val(&limits) as u32,
            )
        }
        .map_err(tree_error)
    }

    #[cfg(windows)]
    fn resume_shell(&self, pid: u32) -> Result<(), AppError> {
        use std::os::windows::io::{FromRawHandle, OwnedHandle};
        use windows::Win32::System::Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
        };
        use windows::Win32::System::Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME};
        let snapshot =
            unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) }.map_err(tree_error)?;
        let _snapshot_owner = unsafe { OwnedHandle::from_raw_handle(snapshot.0) };
        let mut entry = THREADENTRY32 {
            dwSize: std::mem::size_of::<THREADENTRY32>() as u32,
            ..Default::default()
        };
        unsafe { Thread32First(snapshot, &mut entry) }.map_err(tree_error)?;
        loop {
            if entry.th32OwnerProcessID == pid {
                let thread =
                    unsafe { OpenThread(THREAD_SUSPEND_RESUME, false, entry.th32ThreadID) }
                        .map_err(tree_error)?;
                let _thread_owner = unsafe { OwnedHandle::from_raw_handle(thread.0) };
                if unsafe { ResumeThread(thread) } == u32::MAX {
                    return Err(tree_error(std::io::Error::last_os_error()));
                }
                return Ok(());
            }
            if unsafe { Thread32Next(snapshot, &mut entry) }.is_err() {
                return Err(tree_error("suspended shell thread missing"));
            }
        }
    }
}

impl Drop for LocalCommandTree {
    fn drop(&mut self) {
        if self.released {
            return;
        }
        #[cfg(unix)]
        if let Err(error) = self.terminate() {
            eprintln!(
                "Dropped local command cleanup failed: {}",
                error.raw_message
            );
        }
        // Windows job closure automatically terminates all owned descendants.
    }
}
