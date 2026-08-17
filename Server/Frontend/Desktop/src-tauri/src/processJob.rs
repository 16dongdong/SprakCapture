use std::{io, process::Child};

#[cfg(target_os = "windows")]
use std::{mem::size_of, os::windows::io::AsRawHandle, ptr};

#[cfg(target_os = "windows")]
use windows_sys::Win32::{
    Foundation::{CloseHandle, HANDLE},
    System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject, TerminateJobObject,
    },
};

/// 管理桌面客户端拥有的完整代理进程树。
///
/// Windows 作业对象由内核维护，桌面客户端正常退出、崩溃或被任务管理器终止时都会关闭最后一个
/// 作业句柄；`KILL_ON_JOB_CLOSE` 随即终止代理服务及其插件子进程，避免父进程消失后留下孤立监听器。
#[cfg(target_os = "windows")]
pub struct ProcessJob {
    jobHandle: HANDLE,
}

#[cfg(target_os = "windows")]
// Windows 内核句柄支持跨线程并发使用；所有权仍由 `ProcessJob::drop` 唯一释放。
unsafe impl Send for ProcessJob {}

#[cfg(target_os = "windows")]
// `assign` 与 `terminate` 只调用线程安全的内核接口，不修改 Rust 侧可变状态。
unsafe impl Sync for ProcessJob {}

#[cfg(target_os = "windows")]
impl ProcessJob {
    /// 创建具有“关闭即终止”语义的匿名作业对象；创建或配置内核对象失败时返回系统错误。
    pub fn create() -> io::Result<Self> {
        let informationBytes = u32::try_from(size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>())
            .map_err(|_| io::Error::other("作业限制结构长度超过 Windows API 参数范围"))?;
        // 匿名作业避免多个桌面实例共享句柄；单实例约束仍由 Tauri 插件负责。
        let jobHandle = unsafe { CreateJobObjectW(ptr::null(), ptr::null()) };
        if jobHandle.is_null() {
            return Err(io::Error::last_os_error());
        }

        let mut limitInformation = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limitInformation.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let configured = unsafe {
            SetInformationJobObject(
                jobHandle,
                JobObjectExtendedLimitInformation,
                (&raw const limitInformation).cast(),
                informationBytes,
            )
        };
        if configured == 0 {
            let source = io::Error::last_os_error();
            unsafe {
                CloseHandle(jobHandle);
            }
            return Err(source);
        }

        Ok(Self { jobHandle })
    }

    /// 将刚创建的代理进程加入作业；加入失败时调用方必须立即回收该进程，禁止降级为无监管运行。
    pub fn assign(&self, childProcess: &Child) -> io::Result<()> {
        let assigned = unsafe {
            AssignProcessToJobObject(self.jobHandle, childProcess.as_raw_handle().cast())
        };
        if assigned == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// 立即终止作业内仍存活的全部进程；用于托盘显式退出后清理代理派生的插件与辅助进程。
    pub fn terminate(&self) -> io::Result<()> {
        let terminated = unsafe { TerminateJobObject(self.jobHandle, 0) };
        if terminated == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

#[cfg(target_os = "windows")]
impl Drop for ProcessJob {
    /// 关闭唯一作业句柄；即使退出流程未运行，内核仍会根据限制终止全部所属进程。
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.jobHandle);
        }
    }
}

/// 在非 Windows 构建中保持相同生命周期接口；进程本身仍由守护线程直接终止。
#[cfg(not(target_os = "windows"))]
pub struct ProcessJob;

#[cfg(not(target_os = "windows"))]
impl ProcessJob {
    /// 非 Windows 平台无需创建 Windows 作业对象，因此返回无状态管理器。
    pub const fn create() -> io::Result<Self> {
        Ok(Self)
    }

    /// 非 Windows 平台不执行作业绑定；直接子进程仍由统一停止流程回收。
    pub const fn assign(&self, _childProcess: &Child) -> io::Result<()> {
        Ok(())
    }

    /// 非 Windows 平台没有额外进程树句柄需要终止。
    pub const fn terminate(&self) -> io::Result<()> {
        Ok(())
    }
}
