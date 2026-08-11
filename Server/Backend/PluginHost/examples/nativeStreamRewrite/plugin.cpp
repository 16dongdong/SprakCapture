#include "streamPlugin.h"

#include <new>

namespace {

/// 保存宿主函数表的稳定引用；插件只在连接回调期间使用宿主上下文，不保留任何连接级指针。
struct PluginContext {
    const StreamHostFunctions *host;
};

/// 释放初始化期间分配的插件上下文；宿主会在卸载动态库前调用，空指针由 delete 安全处理。
void destroyPlugin(void *context) {
    delete static_cast<PluginContext *>(context);
}

/// 校验连接打开回调参数，证明目标匹配后的连接已进入 Native 生命周期；失败返回负值让宿主熔断本插件。
int32_t onConnectionOpen(void *context, const StreamConnectionOpenEvent *event) {
    const auto *plugin = static_cast<PluginContext *>(context);
    if (plugin == nullptr || plugin->host == nullptr || event == nullptr) {
        return -1;
    }
    return 0;
}

/// 在热路径中原地改写固定 ASCII 标记，分别验证客户端上行与服务端下行均会穿过插件；异常长度会被宿主按 ABI 错误处理。
int32_t onStreamData(void *context, StreamDataEvent *event) {
    if (context == nullptr || event == nullptr || event->data == nullptr || event->length == nullptr) {
        return -1;
    }
    if (*event->length > event->capacity) {
        return -2;
    }

    uint8_t sourceByte = 0;
    uint8_t replacementByte = 0;
    if (event->direction == STREAM_DIRECTION_CLIENT_TO_SERVER) {
        sourceByte = static_cast<uint8_t>('a');
        replacementByte = static_cast<uint8_t>('A');
    } else if (event->direction == STREAM_DIRECTION_SERVER_TO_CLIENT) {
        sourceByte = static_cast<uint8_t>('s');
        replacementByte = static_cast<uint8_t>('S');
    } else {
        return -3;
    }

    for (size_t index = 0; index < *event->length; ++index) {
        if (event->data[index] == sourceByte) {
            event->data[index] = replacementByte;
        }
    }
    return STREAM_ACTION_FORWARD;
}

/// 连接关闭时不保留会话资源；示例刻意不在这里执行 I/O 或锁操作，确保关闭路径可预测。
void onConnectionClose(void *context, const StreamConnectionCloseEvent *event) {
    (void)context;
    (void)event;
}

} // namespace

/// 初始化 ABI v1 导出表；仅在宿主和请求版本完全匹配时创建上下文，避免不兼容调用进入流量路径。
extern "C" STREAM_PLUGIN_EXPORT int32_t stream_plugin_init(
    const StreamHostFunctions *host,
    const StreamPluginInitRequest *request,
    StreamPluginExports *exports
) {
    if (host == nullptr || request == nullptr || exports == nullptr) {
        return -1;
    }
    if (host->apiVersion != STREAM_PLUGIN_API_VERSION || request->apiVersion != STREAM_PLUGIN_API_VERSION) {
        return -2;
    }

    auto *context = new (std::nothrow) PluginContext{host};
    if (context == nullptr) {
        return -3;
    }
    *exports = StreamPluginExports{
        STREAM_PLUGIN_API_VERSION,
        context,
        destroyPlugin,
        onConnectionOpen,
        onStreamData,
        onConnectionClose,
    };
    return 0;
}
