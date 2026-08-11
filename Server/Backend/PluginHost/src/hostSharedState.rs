//! 保存 Native 插件共享连接状态、控制面变化通知与宿主 ABI 回调。
//!
//! 该模块位于同步数据面热路径：锁只保护单次内存表操作，变化通知使用 `watch` 覆盖旧版本，
//! 不把控制面消费速度反向施加到代理连接。所有指针仅在插件同步回调期间有效。

use std::{
    collections::{HashMap, HashSet},
    ffi::c_void,
    ptr,
    sync::Arc,
};

use parking_lot::Mutex;
use tokio::sync::watch;

use super::{MAXIMUM_SESSION_VALUE_BYTES, MAXIMUM_SESSION_VALUES_PER_CONNECTION};

/// C ABI 的不可变字节视图；仅允许回调期间读取，插件不得保存指针。
#[repr(C)]
pub struct ByteSlice {
    pub pointer: *const u8,
    pub length: usize,
}

/// 保存单个连接内由字段名索引的插件会话值；值受宿主容量限制且不会进入公开快照。
type ConnectionSessionValues = HashMap<String, Vec<u8>>;

/// 保存一个插件全部活动连接的会话值；连接关闭时按 connectionId 整体释放。
type PluginSessionValues = HashMap<u64, ConnectionSessionValues>;

/// 保存所有已启用插件的隔离会话值；最外层插件标识防止不同插件读写彼此状态。
type SessionValues = HashMap<String, PluginSessionValues>;

/// 保存跨插件回调共享的 session bag、关闭请求与变化通知；热路径不在此处分配连接状态。
pub(super) struct HostSharedState {
    sessionValues: Mutex<SessionValues>,
    pluginConnections: Mutex<HashMap<String, HashSet<u64>>>,
    closeRequests: Mutex<HashSet<u64>>,
    changeSender: watch::Sender<u64>,
}

impl HostSharedState {
    /// 创建插件生命周期与连接热路径共享的状态容器；初始化只分配固定索引和覆盖式通知通道。
    pub(super) fn new() -> Self {
        let (changeSender, _) = watch::channel(0);
        Self {
            sessionValues: Mutex::new(HashMap::new()),
            pluginConnections: Mutex::new(HashMap::new()),
            closeRequests: Mutex::new(HashSet::new()),
            changeSender,
        }
    }

    /// 订阅插件公开状态变化；通知仅承担唤醒语义，调用方必须重新读取权威快照。
    pub(super) fn subscribeChanges(&self) -> watch::Receiver<u64> {
        self.changeSender.subscribe()
    }

    /// 通知控制面重新读取插件快照；`watch` 合并落后期间的连接抖动且不阻塞数据面。
    pub(super) fn notifyChanged(&self) {
        self.changeSender
            .send_modify(|revision| *revision = revision.wrapping_add(1));
    }

    /// 在连接打开后登记插件归属；仅首次插入发布变化，重复回调保持幂等。
    pub(super) fn registerConnection(&self, pluginId: &str, connectionId: u64) {
        let inserted = self
            .pluginConnections
            .lock()
            .entry(pluginId.to_owned())
            .or_default()
            .insert(connectionId);
        if inserted {
            self.notifyChanged();
        }
    }

    /// 在连接关闭后释放插件私有状态；只有公开活动连接数变化时才唤醒控制面。
    pub(super) fn unregisterConnection(&self, pluginId: &str, connectionId: u64) {
        let mut connections = self.pluginConnections.lock();
        let mut removed = false;
        if let Some(pluginConnections) = connections.get_mut(pluginId) {
            removed = pluginConnections.remove(&connectionId);
            if pluginConnections.is_empty() {
                connections.remove(pluginId);
            }
        }
        drop(connections);
        let mut values = self.sessionValues.lock();
        if let Some(pluginValues) = values.get_mut(pluginId) {
            pluginValues.remove(&connectionId);
            if pluginValues.is_empty() {
                values.remove(pluginId);
            }
        }
        drop(values);
        if removed {
            self.notifyChanged();
        }
    }

    /// 请求关闭一个连接；数据面在本次回调结束后读取标记，避免 FFI 回调重入网络对象。
    pub(super) fn requestClose(&self, connectionId: u64) {
        self.closeRequests.lock().insert(connectionId);
    }

    /// 原子消费连接关闭请求；同一请求只影响一次数据面调度决定。
    pub(super) fn takeCloseRequest(&self, connectionId: u64) -> bool {
        self.closeRequests.lock().remove(&connectionId)
    }

    /// 在插件禁用时标记其全部活动连接关闭，防止持有 TCP 半包的插件被静默移除。
    pub(super) fn requestPluginConnectionClose(&self, pluginId: &str) {
        let connectionIds = self
            .pluginConnections
            .lock()
            .get(pluginId)
            .cloned()
            .unwrap_or_default();
        self.closeRequests.lock().extend(connectionIds);
    }

    /// 返回指定插件的活动连接数；卸载必须等待归零，避免 DLL 句柄仍占用插件目录。
    pub(super) fn pluginConnectionCount(&self, pluginId: &str) -> usize {
        self.pluginConnections
            .lock()
            .get(pluginId)
            .map_or(0, HashSet::len)
    }
}

/// 保存传给 Native 插件的宿主上下文；地址在插件生命周期内稳定，只能经 HostFunctions 使用。
pub(super) struct NativeHostContext {
    pub(super) pluginId: String,
    pub(super) configuration: Arc<Vec<u8>>,
    pub(super) shared: Arc<HostSharedState>,
}

/// 将 Rust 字节切片转换为 ABI 视图；调用方必须保证底层字节至少存活到外部函数返回。
pub(super) fn byteSlice(bytes: &[u8]) -> ByteSlice {
    ByteSlice {
        pointer: bytes.as_ptr(),
        length: bytes.len(),
    }
}

/// 从宿主上下文指针恢复只读引用；空指针返回空，其他指针的有效性由插件 ABI 契约保证。
unsafe fn hostContext<'a>(context: *mut c_void) -> Option<&'a NativeHostContext> {
    (!context.is_null()).then(|| unsafe { &*(context as *const NativeHostContext) })
}

/// 从 ABI 字节视图读取有限长度 UTF-8 键；会话键不接受空、二进制或超长字符串。
unsafe fn sessionKey(slice: ByteSlice) -> Option<String> {
    if slice.pointer.is_null() || slice.length == 0 || slice.length > 256 {
        return None;
    }
    let bytes = unsafe { std::slice::from_raw_parts(slice.pointer, slice.length) };
    std::str::from_utf8(bytes).ok().map(str::to_owned)
}

/// 写入插件日志；热路径日志由插件自行节流，宿主不将日志存入事务或控制快照。
pub(super) unsafe extern "C" fn hostLog(context: *mut c_void, level: u32, message: ByteSlice) {
    let Some(host) = (unsafe { hostContext(context) }) else {
        return;
    };
    if message.pointer.is_null() || message.length > 4096 {
        return;
    }
    let bytes = unsafe { std::slice::from_raw_parts(message.pointer, message.length) };
    let Ok(message) = std::str::from_utf8(bytes) else {
        return;
    };
    if level >= 3 {
        tracing::warn!(pluginId = %host.pluginId, "{message}");
    } else {
        tracing::info!(pluginId = %host.pluginId, "{message}");
    }
}

/// 将当前插件配置复制到调用方缓冲区；返回完整长度，调用方可先传空缓冲查询容量。
pub(super) unsafe extern "C" fn hostGetConfig(
    context: *mut c_void,
    output: *mut u8,
    capacity: usize,
) -> usize {
    let Some(host) = (unsafe { hostContext(context) }) else {
        return 0;
    };
    let length = host.configuration.len();
    if !output.is_null() && capacity > 0 {
        let copyLength = length.min(capacity);
        unsafe { ptr::copy_nonoverlapping(host.configuration.as_ptr(), output, copyLength) };
    }
    length
}

/// 保存单连接插件状态；上限防止协议解析器把无界重组缓冲塞入宿主通用状态表。
pub(super) unsafe extern "C" fn hostSetSessionValue(
    context: *mut c_void,
    connectionId: u64,
    key: ByteSlice,
    value: ByteSlice,
) -> i32 {
    let Some(host) = (unsafe { hostContext(context) }) else {
        return -1;
    };
    let Some(key) = (unsafe { sessionKey(key) }) else {
        return -2;
    };
    if value.pointer.is_null() || value.length > MAXIMUM_SESSION_VALUE_BYTES {
        return -3;
    }
    let value = unsafe { std::slice::from_raw_parts(value.pointer, value.length) }.to_vec();
    let mut values = host.shared.sessionValues.lock();
    let pluginValues = values.entry(host.pluginId.clone()).or_default();
    let connectionValues = pluginValues.entry(connectionId).or_default();
    if !connectionValues.contains_key(&key)
        && connectionValues.len() >= MAXIMUM_SESSION_VALUES_PER_CONNECTION
    {
        return -4;
    }
    connectionValues.insert(key, value);
    0
}

/// 读取单连接插件状态；返回完整长度，缺失键返回零且不分配临时字节。
pub(super) unsafe extern "C" fn hostGetSessionValue(
    context: *mut c_void,
    connectionId: u64,
    key: ByteSlice,
    output: *mut u8,
    capacity: usize,
) -> usize {
    let Some(host) = (unsafe { hostContext(context) }) else {
        return 0;
    };
    let Some(key) = (unsafe { sessionKey(key) }) else {
        return 0;
    };
    let values = host.shared.sessionValues.lock();
    let Some(value) = values
        .get(&host.pluginId)
        .and_then(|pluginValues| pluginValues.get(&connectionId))
        .and_then(|connectionValues| connectionValues.get(&key))
    else {
        return 0;
    };
    if !output.is_null() && capacity > 0 {
        let copyLength = value.len().min(capacity);
        unsafe { ptr::copy_nonoverlapping(value.as_ptr(), output, copyLength) };
    }
    value.len()
}

/// 请求数据面在当前回调返回后关闭指定连接，避免 Native 插件获得网络对象所有权。
pub(super) unsafe extern "C" fn hostCloseConnection(context: *mut c_void, connectionId: u64) {
    if let Some(host) = unsafe { hostContext(context) } {
        host.shared.requestClose(connectionId);
    }
}
