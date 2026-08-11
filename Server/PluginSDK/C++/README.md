# C++ Native 插件 SDK

该 SDK 封装 Native ABI v2。插件作者不需要接触 C 指针、JSON 文本拼接、输出释放函数或固定导出表。

## 最小插件

```cpp
#include "trafficMod/sdk.hpp"
using namespace trafficMod;

void configure(Plugin& plugin) {
    plugin.onTcp([](const Event& event) {
        auto bytes = std::vector<std::uint8_t>(event.bytes().begin(), event.bytes().end());
        // 修改 bytes
        return Decision::modify(event, std::move(bytes));
    });
}

TRAFFIC_MOD_PLUGIN(configure)
```

## 常用回调

```cpp
plugin.onTcp(handler);                         // TCP 线块
plugin.onUdp(handler);                         // 完整 UDP 数据报
plugin.on(Stage::ConnectionClosing, handler); // 任意阶段
plugin.onModule("decoder", Stage::TcpChunk, handler); // 模块专属阶段
plugin.onStop(cleanup);                        // 服务停止
plugin.onUnload(releaseResources);             // 实例销毁
```

回调接收 `const Event&`，常用字段已强类型化：

- `event.bytes()`：当前线块或数据报，无额外复制视图；
- `event.direction()`：上行或下行；
- `event.connectionId()`：连接标识；
- `event.host()` / `event.port()`：目标；
- `event.context()` / `event.payload()`：完整 JSON 树；
- `event.moduleId()` / `event.stage()`：模块和阶段。

回调直接返回：

```cpp
Decision::continueFlow();
Decision::modify(event, newBytes); // 只替换 bytes，保留正文的其他字段
Decision::modifyPayload(Json::Object{{"protocol", "custom"}});
Decision::hold();
Decision::drop();
Decision::reject("报文校验失败");
Decision::redirect("upstream.example", 8443);
Decision::respond(Json::Object{{"statusCode", 200}, {"body", "ok"}});
Decision::close();
Decision::annotate("协议", "自定义协议");
```

SDK 自动把决定绑定到当前 `eventId`，生成动作 JSON，并在宿主复制完成后释放输出内存。
`modifyPayload` 使用根 JSON Patch 表达替换，因此 `null`、标量、数组和对象都能与其他语言 SDK
一致地传给 Host；`modify(event, bytes)` 在同一机制上只改写 `bytes` 并保留其他正文元数据。
`Json` 对整数分别保留精确 `int64_t` 与 `uint64_t`，Native ABI 的完整 u64 代际、配置和
私有协议字段不会回退为 `double`；使用 `asInteger()` 或 `asUnsignedInteger()` 按字段语义读取。

## 构建

```powershell
cmake -S . -B BUILD_DIR -DTRAFFIC_MOD_BUILD_TESTS=ON
cmake --build BUILD_DIR --config Release
ctest --test-dir BUILD_DIR -C Release --output-on-failure
```

SDK 要求 C++20。Windows 导出 `.dll`，Linux 导出 `.so`，macOS 导出 `.dylib`。插件清单的
`runtime.kind` 使用 `native`，`runtime.protocolVersion` 使用 `2.0`，`runtime.entry` 指向生成的动态库。
示例构建会把 `plugin.json` 自动复制到动态库所在目录，因此该目录可以直接交给 Host，不需要手工拼装
清单与入口。
MinGW 默认静态链接 C++ 运行库，以便只分发插件 DLL；需要共享运行库时可设置
`-DTRAFFIC_MOD_STATIC_MINGW_RUNTIME=OFF`。MSVC 的 `/MD` 或 `/MT` 继续由插件工程选择。

## 并发与生命周期

宿主可以从多个连接线程并发调用同一个插件实例。SDK 不限制插件行为，也不串行化数据回调；插件作者维护的
连接状态需要自行加锁或分片。`Event::bytes()` 仅在当前回调期间有效，保存数据时应复制。`onStop` 用于终止线程
和 I/O；SDK 会先等待所有在途事件回调返回，再执行一次 `onStop`。`onUnload` 用于释放最终资源。

完整的 TCP 分包、异或解密、业务修改、重封包和 UDP 修改示例位于
`examples/binaryTransformer/`。
