//! 实现开放可信 Mod 的进程内 Native 阶段运行时。
//!
//! Native 动态库与宿主共享进程权限、地址空间和故障域。宿主只固定初始化、调用、停止和
//! 释放的 C ABI，并验证返回 JSON 属于当前事件；插件作者可在回调内自由使用操作系统能力。

use std::{
    ffi::c_void,
    future::Future,
    path::Path,
    pin::Pin,
    ptr, slice,
    sync::atomic::{AtomicBool, Ordering},
};

use libloading::Library;
use serde_json::Value as JsonValue;

use crate::{
    ByteSlice, ExtensionAction, ExtensionManifest, ExtensionRuntime, PluginHostError,
    RuntimeInvocation,
};

const EXTENSION_NATIVE_API_VERSION: u32 = 2;
const EXTENSION_NATIVE_INIT_SYMBOL: &[u8] = b"capture_extension_init\0";

/// 描述 Native 初始化所需的稳定只读输入；两个 JSON 切片仅在初始化回调期间有效。
#[repr(C)]
pub struct NativeExtensionInitRequest {
    pub apiVersion: u32,
    pub manifest: ByteSlice,
    pub configuration: ByteSlice,
}

/// 描述插件返回的拥有型 JSON 缓冲；宿主复制完成后恰好调用一次 `release`。
#[repr(C)]
pub struct NativeExtensionBuffer {
    pub pointer: *const u8,
    pub length: usize,
    pub releaseContext: *mut c_void,
    pub release: Option<unsafe extern "C" fn(*mut c_void, *const u8, usize)>,
}

/// 描述完整 Native 扩展导出；插件自行保证 `invoke` 可被多个连接并发调用。
#[repr(C)]
pub struct NativeExtensionExports {
    pub apiVersion: u32,
    pub pluginContext: *mut c_void,
    pub invoke:
        Option<unsafe extern "C" fn(*mut c_void, ByteSlice, *mut NativeExtensionBuffer) -> i32>,
    pub stop: Option<unsafe extern "C" fn(*mut c_void)>,
    pub destroy: Option<unsafe extern "C" fn(*mut c_void)>,
}

/// 定义 Native 扩展固定初始化符号；非零返回值表示插件拒绝本次实例创建。
pub type NativeExtensionInit =
    unsafe extern "C" fn(*const NativeExtensionInitRequest, *mut NativeExtensionExports) -> i32;

/// 持有进程内动态库和回调表；字段顺序保证回调结束后才卸载库代码。
pub struct NativeExtensionRuntime {
    exports: NativeExtensionExports,
    library: Library,
    stopped: AtomicBool,
}

/// Native ABI 约定插件自行同步并发回调；选择该运行时即接受与宿主共享线程和故障域。
unsafe impl Send for NativeExtensionRuntime {}

/// Native ABI 约定插件自行同步并发回调；宿主不为开放可信 Mod 添加串行化限制。
unsafe impl Sync for NativeExtensionRuntime {}

impl NativeExtensionRuntime {
    /// 从插件版本目录加载进程内 Native 扩展。
    ///
    /// 运行上下文：`manifest.runtime.entry` 已通过相对路径校验；`configuration` 是当前权威配置。
    /// 失败语义：入口缺失、动态库/符号加载、初始化或导出表不合法时返回稳定宿主错误，绝不发布半实例。
    pub fn load(
        manifest: &ExtensionManifest,
        directory: &Path,
        configuration: &JsonValue,
    ) -> Result<Self, PluginHostError> {
        let entryPath = directory.join(&manifest.runtime.entry);
        if !entryPath.is_file() || !entryPath.starts_with(directory) {
            return Err(PluginHostError::MissingEntry);
        }
        let manifestBytes = serde_json::to_vec(manifest).map_err(PluginHostError::StateFormat)?;
        let configurationBytes =
            serde_json::to_vec(configuration).map_err(PluginHostError::StateFormat)?;
        let library = unsafe { Library::new(entryPath) }.map_err(PluginHostError::Load)?;
        let initialize = unsafe {
            *library
                .get::<NativeExtensionInit>(EXTENSION_NATIVE_INIT_SYMBOL)
                .map_err(PluginHostError::Load)?
        };
        let request = NativeExtensionInitRequest {
            apiVersion: EXTENSION_NATIVE_API_VERSION,
            manifest: byteSlice(&manifestBytes),
            configuration: byteSlice(&configurationBytes),
        };
        let mut exports = NativeExtensionExports {
            apiVersion: 0,
            pluginContext: ptr::null_mut(),
            invoke: None,
            stop: None,
            destroy: None,
        };
        let status = unsafe { initialize(&request, &mut exports) };
        if status != 0 {
            return Err(PluginHostError::Initialization);
        }
        if exports.apiVersion != EXTENSION_NATIVE_API_VERSION || exports.invoke.is_none() {
            if let Some(destroy) = exports.destroy {
                unsafe { destroy(exports.pluginContext) };
            }
            return Err(PluginHostError::InvalidExports);
        }
        Ok(Self {
            exports,
            library,
            stopped: AtomicBool::new(false),
        })
    }

    /// 同步调用 Native 阶段回调并复制插件拥有的 JSON 动作。
    ///
    /// 运行上下文：由 `ExtensionRuntime::invoke` 在当前数据面任务调用；插件可自行决定是否阻塞或创建线程。
    /// 失败语义：停止态、ABI 状态码、空指针、越界输出和无效 JSON 都只使当前调用失败。
    fn invokeNative(&self, invocation: &RuntimeInvocation) -> Result<ExtensionAction, String> {
        if self.stopped.load(Ordering::Acquire) {
            return Err("extensionNativeStopped".to_owned());
        }
        let request = serde_json::to_vec(invocation).map_err(|_| "extensionNativeRequest")?;
        let mut output = NativeExtensionBuffer {
            pointer: ptr::null(),
            length: 0,
            releaseContext: ptr::null_mut(),
            release: None,
        };
        let invoke = self.exports.invoke.ok_or("extensionNativeMissingInvoke")?;
        let status =
            unsafe { invoke(self.exports.pluginContext, byteSlice(&request), &mut output) };
        let releasedOutput = NativeOutputGuard::new(output);
        if status != 0 {
            return Err("extensionNativeInvokeFailed".to_owned());
        }
        let bytes = releasedOutput.bytes()?;
        serde_json::from_slice(bytes).map_err(|_| "extensionNativeActionInvalid".to_owned())
    }
}

impl ExtensionRuntime for NativeExtensionRuntime {
    /// 调用进程内回调；该模式故意不切换工作进程，插件作者对同步时延负全责。
    fn invoke<'a>(
        &'a self,
        invocation: RuntimeInvocation,
    ) -> Pin<Box<dyn Future<Output = Result<ExtensionAction, String>> + Send + 'a>> {
        Box::pin(async move { self.invokeNative(&invocation) })
    }

    /// 原子停止新调用并通知插件释放活动工作；停止回调必须自行终止其网络和线程。
    fn stop(&self) {
        if !self.stopped.swap(true, Ordering::AcqRel)
            && let Some(stop) = self.exports.stop
        {
            unsafe { stop(self.exports.pluginContext) };
        }
    }
}

impl Drop for NativeExtensionRuntime {
    /// 按 stop→destroy→卸载动态库的固定顺序回收实例，避免卸载后再执行插件代码。
    fn drop(&mut self) {
        self.stop();
        if let Some(destroy) = self.exports.destroy {
            unsafe { destroy(self.exports.pluginContext) };
        }
        let _ = &self.library;
    }
}

/// 在宿主复制完成前持有插件输出；所有返回分支都通过 Drop 恰好释放一次。
struct NativeOutputGuard {
    output: NativeExtensionBuffer,
}

impl NativeOutputGuard {
    /// 接管插件缓冲所有权；构造后调用方不得再直接释放原始字段。
    fn new(output: NativeExtensionBuffer) -> Self {
        Self { output }
    }

    /// 返回 ABI 声明的完整只读输出；开放可信 Native Mod 自行决定输出规模和内存策略。
    ///
    /// 失败语义：空指针或零长度不构成合法动作；宿主不再施加额外字节配额。
    fn bytes(&self) -> Result<&[u8], String> {
        if self.output.pointer.is_null() || self.output.length == 0 {
            return Err("extensionNativeOutputInvalid".to_owned());
        }
        Ok(unsafe { slice::from_raw_parts(self.output.pointer, self.output.length) })
    }
}

impl Drop for NativeOutputGuard {
    /// 调用插件提供的释放函数；没有释放函数的静态缓冲由插件自行持有到实例销毁。
    fn drop(&mut self) {
        if let Some(release) = self.output.release {
            unsafe {
                release(
                    self.output.releaseContext,
                    self.output.pointer,
                    self.output.length,
                )
            };
        }
    }
}

/// 把 Rust 切片映射为单次 ABI 调用使用的只读视图；空切片使用空指针。
fn byteSlice(bytes: &[u8]) -> ByteSlice {
    ByteSlice {
        pointer: if bytes.is_empty() {
            ptr::null()
        } else {
            bytes.as_ptr()
        },
        length: bytes.len(),
    }
}
