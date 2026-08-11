//! 实现 Native ABI v2 的 C 布局、插件注册、生命周期与输出所有权转移。

use std::{
    ffi::c_void,
    panic::{AssertUnwindSafe, catch_unwind},
    ptr, slice,
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicBool, Ordering},
    },
};

use serde_json::Value;

use crate::{Action, Invocation};

const NATIVE_API_VERSION: u32 = 2;

/// 描述 ABI 调用期间有效的只读字节切片；宿主与插件均不得跨回调保存其指针。
#[repr(C)]
#[derive(Clone, Copy)]
pub struct NativeByteSlice {
    pub pointer: *const u8,
    pub length: usize,
}

/// 描述宿主传入的初始化请求；两个 JSON 切片仅在初始化回调期间有效。
#[repr(C)]
pub struct NativeExtensionInitRequest {
    pub apiVersion: u32,
    pub manifest: NativeByteSlice,
    pub configuration: NativeByteSlice,
}

/// 描述插件拥有的 JSON 输出；宿主复制后通过 release 恰好释放一次。
#[repr(C)]
pub struct NativeExtensionBuffer {
    pub pointer: *const u8,
    pub length: usize,
    pub releaseContext: *mut c_void,
    pub release: Option<unsafe extern "C" fn(*mut c_void, *const u8, usize)>,
}

/// 描述 Native ABI v2 导出表；字段顺序必须与宿主 `NativeExtensionExports` 保持一致。
#[repr(C)]
pub struct NativeExtensionExports {
    pub apiVersion: u32,
    pub pluginContext: *mut c_void,
    pub invoke: Option<
        unsafe extern "C" fn(*mut c_void, NativeByteSlice, *mut NativeExtensionBuffer) -> i32,
    >,
    pub stop: Option<unsafe extern "C" fn(*mut c_void)>,
    pub destroy: Option<unsafe extern "C" fn(*mut c_void)>,
}

/// 保存初始化阶段解析后的完整 manifest 与用户配置；工厂可将需要的数据复制进闭包。
#[derive(Clone, Debug)]
pub struct InitContext {
    pub manifest: Value,
    pub configuration: Value,
}

/// 描述插件工厂或事件处理失败；消息只用于当前 Native 调用诊断。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginError {
    pub message: String,
}

impl PluginError {
    /// 从作者提供的稳定消息构造错误；消息不会跨 ABI 作为动作返回。
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for PluginError {
    /// 输出作者诊断消息，不包含事件正文。
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for PluginError {}

type EventHandler = dyn Fn(&Invocation) -> Result<Action, PluginError> + Send + Sync;
type LifecycleHandler = dyn Fn() + Send + Sync;

/// 保存普通事件闭包与生命周期闭包；回调可能在不同连接线程并发执行。
pub struct Plugin {
    handler: Arc<EventHandler>,
    onStop: Option<Arc<LifecycleHandler>>,
    onDestroy: Option<Arc<LifecycleHandler>>,
}

impl Plugin {
    /// 直接用普通函数或闭包创建插件；处理器必须可并发调用且不得返回其他事件的 ID。
    pub fn new<H>(handler: H) -> Self
    where
        H: Fn(&Invocation) -> Result<Action, PluginError> + Send + Sync + 'static,
    {
        Self {
            handler: Arc::new(handler),
            onStop: None,
            onDestroy: None,
        }
    }

    /// 返回链式构造器，用于同时注册处理器和生命周期闭包。
    pub fn builder<H>(handler: H) -> PluginBuilder
    where
        H: Fn(&Invocation) -> Result<Action, PluginError> + Send + Sync + 'static,
    {
        PluginBuilder {
            plugin: Self::new(handler),
        }
    }
}

/// 提供普通闭包注册接口；构建完成后生命周期由 ABI 上下文独占管理。
pub struct PluginBuilder {
    plugin: Plugin,
}

impl PluginBuilder {
    /// 注册热停止通知；宿主重复 stop 或随后 destroy 时最多调用一次。
    pub fn onStop<H>(mut self, handler: H) -> Self
    where
        H: Fn() + Send + Sync + 'static,
    {
        self.plugin.onStop = Some(Arc::new(handler));
        self
    }

    /// 注册最终销毁通知；动态库卸载前恰好调用一次。
    pub fn onDestroy<H>(mut self, handler: H) -> Self
    where
        H: Fn() + Send + Sync + 'static,
    {
        self.plugin.onDestroy = Some(Arc::new(handler));
        self
    }

    /// 完成不可变插件实例；之后事件处理器可被宿主并发调用。
    pub fn build(self) -> Plugin {
        self.plugin
    }
}

/// 保存 ABI 实例状态；停止标志先于回调执行读取，销毁路径固定先停止再释放。
struct RuntimeContext {
    plugin: Plugin,
    invocationGate: RwLock<()>,
    stopped: AtomicBool,
    destroyed: AtomicBool,
    lifecycleLock: Mutex<()>,
}

impl RuntimeContext {
    /// 执行至多一次停止回调；互斥锁保证并发 stop/destroy 的生命周期顺序。
    fn stop(&self) {
        let _invocations = self
            .invocationGate
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _guard = self
            .lifecycleLock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !self.stopped.swap(true, Ordering::AcqRel)
            && let Some(handler) = &self.plugin.onStop
            && catch_unwind(AssertUnwindSafe(|| handler())).is_err()
        {
            eprintln!("插件停止生命周期回调发生 panic");
        }
    }
}

/// 将 ABI 切片复制为 Rust 所有字节；空切片合法，非零长度空指针失败。
unsafe fn copySlice(bytes: NativeByteSlice) -> Result<Vec<u8>, PluginError> {
    if bytes.length == 0 {
        return Ok(Vec::new());
    }
    if bytes.pointer.is_null() {
        return Err(PluginError::new("Native ABI 字节指针为空"));
    }
    Ok(unsafe { slice::from_raw_parts(bytes.pointer, bytes.length) }.to_vec())
}

/// 初始化插件导出表；任何指针、版本、JSON、工厂 panic 或工厂错误都返回非零状态。
///
/// # Safety
/// `request` 与 `exports` 必须指向宿主在本次调用内提供的有效 C 布局对象。
pub unsafe fn initializePlugin<F>(
    factory: F,
    request: *const NativeExtensionInitRequest,
    exports: *mut NativeExtensionExports,
) -> i32
where
    F: FnOnce(InitContext) -> Result<Plugin, PluginError>,
{
    if request.is_null() || exports.is_null() {
        return -1;
    }
    let result = catch_unwind(AssertUnwindSafe(|| {
        let request = unsafe { &*request };
        if request.apiVersion != NATIVE_API_VERSION {
            return Err(PluginError::new("Native ABI 版本不匹配"));
        }
        let manifest = serde_json::from_slice(&unsafe { copySlice(request.manifest)? })
            .map_err(|_| PluginError::new("manifest 不是有效 JSON"))?;
        let configuration = serde_json::from_slice(&unsafe { copySlice(request.configuration)? })
            .map_err(|_| PluginError::new("configuration 不是有效 JSON"))?;
        factory(InitContext {
            manifest,
            configuration,
        })
    }));
    let plugin = match result {
        Ok(Ok(plugin)) => plugin,
        _ => return -2,
    };
    let context = Box::new(RuntimeContext {
        plugin,
        invocationGate: RwLock::new(()),
        stopped: AtomicBool::new(false),
        destroyed: AtomicBool::new(false),
        lifecycleLock: Mutex::new(()),
    });
    unsafe {
        *exports = NativeExtensionExports {
            apiVersion: NATIVE_API_VERSION,
            pluginContext: Box::into_raw(context).cast(),
            invoke: Some(invokePlugin),
            stop: Some(stopPlugin),
            destroy: Some(destroyPlugin),
        };
    }
    0
}

/// 解析一次调用并序列化动作到独占缓冲；panic、停止态、错误事件 ID 或 JSON 失败均返回非零。
unsafe extern "C" fn invokePlugin(
    context: *mut c_void,
    request: NativeByteSlice,
    output: *mut NativeExtensionBuffer,
) -> i32 {
    if context.is_null() || output.is_null() {
        return -1;
    }
    let runtime = unsafe { &*context.cast::<RuntimeContext>() };
    // 读锁覆盖完整作者调用，确保 stop/destroy 在所有在途处理结束后才执行清理回调。
    let _invocation = runtime
        .invocationGate
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if runtime.stopped.load(Ordering::Acquire) {
        return -2;
    }
    let result = catch_unwind(AssertUnwindSafe(|| -> Result<Vec<u8>, PluginError> {
        let invocation = serde_json::from_slice::<Invocation>(&unsafe { copySlice(request)? })
            .map_err(|_| PluginError::new("invocation 不是有效 ABI v2 JSON"))?;
        let action = (runtime.plugin.handler)(&invocation)?;
        if action.eventId != invocation.envelope.eventId {
            return Err(PluginError::new("动作 eventId 与当前事件不一致"));
        }
        serde_json::to_vec(&action).map_err(|_| PluginError::new("动作无法序列化"))
    }));
    let bytes = match result {
        Ok(Ok(bytes)) if !bytes.is_empty() => bytes.into_boxed_slice(),
        _ => return -3,
    };
    let length = bytes.len();
    let pointer = Box::into_raw(bytes).cast::<u8>();
    unsafe {
        *output = NativeExtensionBuffer {
            pointer,
            length,
            releaseContext: ptr::null_mut(),
            release: Some(releaseOutput),
        };
    }
    0
}

/// 原子停止插件；空上下文和生命周期 panic 均不跨 ABI 传播。
unsafe extern "C" fn stopPlugin(context: *mut c_void) {
    if !context.is_null() {
        unsafe { &*context.cast::<RuntimeContext>() }.stop();
    }
}

/// 按 stop→destroy 固定顺序销毁上下文；宿主必须只调用一次 destroy。
unsafe extern "C" fn destroyPlugin(context: *mut c_void) {
    if context.is_null() {
        return;
    }
    let runtime = unsafe { Box::from_raw(context.cast::<RuntimeContext>()) };
    runtime.stop();
    if !runtime.destroyed.swap(true, Ordering::AcqRel)
        && let Some(handler) = &runtime.plugin.onDestroy
        && catch_unwind(AssertUnwindSafe(|| handler())).is_err()
    {
        eprintln!("插件销毁生命周期回调发生 panic");
    }
}

/// 释放一次序列化输出；指针与长度必须保持宿主收到的原值。
unsafe extern "C" fn releaseOutput(_: *mut c_void, pointer: *const u8, length: usize) {
    if !pointer.is_null() {
        let bytes = ptr::slice_from_raw_parts_mut(pointer.cast_mut(), length);
        drop(unsafe { Box::from_raw(bytes) });
    }
}
