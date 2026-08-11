use std::{env, fs, path::Path, process::Command};

use plugin_host::{
    ConnectionMetadata, HookActionResult, PluginHost, StreamDirection, TransportKind,
};
use tempfile::tempdir;

const NATIVE_PLUGIN_SOURCE: &str = r#"
use std::{
    ffi::c_void,
    slice,
    sync::atomic::{AtomicBool, Ordering},
};

static HELD_ONCE: AtomicBool = AtomicBool::new(false);

#[repr(C)]
struct ConnectionOpenEvent {
    connection_id: u64,
    transport: u8,
    reserved: [u8; 7],
    client_address: ByteSlice,
    target_host: ByteSlice,
    target_port: u16,
}

#[repr(C)]
struct ByteSlice {
    pointer: *const u8,
    length: usize,
}

#[repr(C)]
struct HostFunctions {
    api_version: u32,
    host_context: *mut c_void,
    log: Option<unsafe extern "C" fn(*mut c_void, u32, ByteSlice)>,
    get_config: Option<unsafe extern "C" fn(*mut c_void, *mut u8, usize) -> usize>,
    set_session_value: Option<
        unsafe extern "C" fn(*mut c_void, u64, ByteSlice, ByteSlice) -> i32,
    >,
    get_session_value: Option<
        unsafe extern "C" fn(*mut c_void, u64, ByteSlice, *mut u8, usize) -> usize,
    >,
    close_connection: Option<unsafe extern "C" fn(*mut c_void, u64)>,
}

#[repr(C)]
struct StreamDataEvent {
    connection_id: u64,
    direction: u8,
    reserved: [u8; 7],
    data: *mut u8,
    length: *mut usize,
    capacity: usize,
}

#[repr(C)]
struct ConnectionCloseEvent {
    connection_id: u64,
}

#[repr(C)]
struct PluginExports {
    api_version: u32,
    plugin_context: *mut c_void,
    destroy: Option<unsafe extern "C" fn(*mut c_void)>,
    on_connection_open: Option<unsafe extern "C" fn(*mut c_void, *const ConnectionOpenEvent) -> i32>,
    on_stream_data: Option<unsafe extern "C" fn(*mut c_void, *mut StreamDataEvent) -> i32>,
    on_connection_close: Option<unsafe extern "C" fn(*mut c_void, *const ConnectionCloseEvent)>,
}

struct PluginContext {
    host: *const HostFunctions,
}

/// 释放夹具初始化时创建的上下文；空指针代表初始化未转移所有权，不执行回收。
unsafe extern "C" fn destroy_plugin(context: *mut c_void) {
    if !context.is_null() {
        drop(unsafe { Box::from_raw(context.cast::<PluginContext>()) });
    }
}

/// 在连接打开后经持久化的宿主函数表读取配置；地址失效、函数缺失或内容错误均返回失败码。
unsafe extern "C" fn on_connection_open(
    context: *mut c_void,
    _: *const ConnectionOpenEvent,
) -> i32 {
    if context.is_null() {
        return -1;
    }
    let plugin = unsafe { &*context.cast::<PluginContext>() };
    if plugin.host.is_null() {
        return -2;
    }
    let host = unsafe { &*plugin.host };
    let Some(get_config) = host.get_config else {
        return -3;
    };
    let required = unsafe { get_config(host.host_context, std::ptr::null_mut(), 0) };
    if required != 2 {
        return -4;
    }
    let mut configuration = [0_u8; 2];
    let copied = unsafe {
        get_config(
            host.host_context,
            configuration.as_mut_ptr(),
            configuration.len(),
        )
    };
    if copied == configuration.len() && configuration == *b"{}" {
        0
    } else {
        -5
    }
}

unsafe extern "C" fn on_stream_data(_: *mut c_void, event: *mut StreamDataEvent) -> i32 {
    if event.is_null() {
        return -1;
    }
    let event = unsafe { &mut *event };
    if event.data.is_null() || event.length.is_null() {
        return -2;
    }
    let length = unsafe { *event.length };
    if length > event.capacity {
        return -3;
    }
    let bytes = unsafe { slice::from_raw_parts_mut(event.data, length) };
    for byte in bytes.iter_mut() {
        if *byte == b'a' {
            *byte = b'z';
        }
    }
    if bytes.first() == Some(&b'h') && !HELD_ONCE.swap(true, Ordering::AcqRel) {
        1
    } else {
        0
    }
}

unsafe extern "C" fn on_connection_close(_: *mut c_void, _: *const ConnectionCloseEvent) {}

#[unsafe(no_mangle)]
/// 保存宿主函数表并导出完整回调；参数为空时返回失败码且不创建插件上下文。
pub unsafe extern "C" fn stream_plugin_init(
    host: *const HostFunctions,
    _: *const c_void,
    exports: *mut PluginExports,
) -> i32 {
    if host.is_null() || exports.is_null() {
        return -1;
    }
    let plugin_context = Box::into_raw(Box::new(PluginContext { host })).cast::<c_void>();
    unsafe {
        *exports = PluginExports {
            api_version: 1,
            plugin_context,
            destroy: Some(destroy_plugin),
            on_connection_open: Some(on_connection_open),
            on_stream_data: Some(on_stream_data),
            on_connection_close: Some(on_connection_close),
        };
    }
    0
}
"#;

/// 编译只供本测试使用的动态库；输出目录位于任务临时目录，测试结束即随夹具删除。
fn build_native_fixture(output_path: &Path) {
    let source_path = output_path.with_extension("rs");
    fs::write(&source_path, NATIVE_PLUGIN_SOURCE).expect("写入原生插件夹具");
    let compilation = Command::new(env::var("RUSTC").unwrap_or_else(|_| "rustc".to_owned()))
        .args([
            "--edition",
            "2024",
            "--crate-type",
            "cdylib",
            source_path.to_str().expect("夹具源路径 UTF-8"),
            "-o",
            output_path.to_str().expect("夹具库路径 UTF-8"),
        ])
        .output()
        .expect("启动 Rust 编译器");
    assert!(
        compilation.status.success(),
        "原生插件夹具编译失败：{}",
        String::from_utf8_lossy(&compilation.stderr)
    );
}

/// 覆盖已经返回的初始化调用栈，避免失效的宿主函数表指针因旧字节暂未变化而在测试中偶然可用。
#[inline(never)]
fn overwrite_released_stack() -> usize {
    let stack_bytes = [0xA5_u8; 64 * 1024];
    std::hint::black_box(&stack_bytes);
    stack_bytes.iter().filter(|byte| **byte == 0xA5).count()
}

/// 验证宿主从动态库解析固定导出符号，并在连接打开、流改写和关闭生命周期内调用 ABI 回调。
#[test]
fn loads_and_runs_native_plugin() {
    let directory = tempdir().expect("创建临时目录");
    let plugin_directory = directory.path().join("nativeFixture");
    fs::create_dir_all(&plugin_directory).expect("创建插件目录");
    let library_name = format!("nativeFixture{}", env::consts::DLL_SUFFIX);
    build_native_fixture(&plugin_directory.join(&library_name));
    fs::write(
        plugin_directory.join("plugin.json"),
        format!(
            r#"{{"id":"native.fixture","name":"原生夹具","version":"1.0.0","apiVersion":1,"entry":"{library_name}","hooks":["on_connection_open","on_stream_data","on_connection_close"]}}"#
        ),
    )
    .expect("写入插件清单");

    let host = PluginHost::new(directory.path()).expect("创建插件宿主");
    host.enable("native.fixture").expect("启用原生插件");
    assert_eq!(overwrite_released_stack(), 64 * 1024);
    let connection = host.openConnection(ConnectionMetadata {
        transport: TransportKind::Tcp,
        clientAddress: "127.0.0.1:1".to_owned(),
        targetHost: "example.test".to_owned(),
        targetPort: 443,
    });
    let mut bytes = *b"data";
    assert_eq!(
        host.processStreamData(&connection, StreamDirection::ClientToServer, &mut bytes),
        HookActionResult::Forward { length: 4 }
    );
    assert_eq!(bytes, *b"dztz");
    let mut held_bytes = *b"ha";
    assert_eq!(
        host.processStreamData(
            &connection,
            StreamDirection::ClientToServer,
            &mut held_bytes
        ),
        HookActionResult::Hold
    );
    let mut resumed_bytes = *b"ta";
    assert_eq!(
        host.processStreamData(
            &connection,
            StreamDirection::ClientToServer,
            &mut resumed_bytes
        ),
        HookActionResult::ForwardOwned {
            bytes: b"hztz".to_vec()
        }
    );
    host.closeConnection(connection);
}
