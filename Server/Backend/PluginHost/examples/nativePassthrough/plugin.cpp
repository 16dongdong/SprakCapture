#include "streamPlugin.h"

#include <new>

namespace {

/// 保存初始化后稳定的宿主函数表；本示例不保留任何连接级可变状态，因此可被多个连接并发调用。
struct PluginContext {
    const StreamHostFunctions *host;
};

/// 释放插件初始化时创建的上下文；宿主保证在卸载动态库之前调用该函数。
void destroyPlugin(void *context) {
    delete static_cast<PluginContext *>(context);
}

/// 记录连接打开事件，示例仅证明生命周期回调可用，不在高频路径执行日志操作。
int32_t onConnectionOpen(void *context, const StreamConnectionOpenEvent *event) {
    const auto *plugin = static_cast<PluginContext *>(context);
    if (plugin == nullptr || plugin->host == nullptr || event == nullptr) {
        return -1;
    }
    static constexpr uint8_t message[] = "nativePassthrough connected";
    plugin->host->log(
        plugin->host->hostContext,
        1,
        StreamByteSlice{message, sizeof(message) - 1}
    );
    return 0;
}

/// 原样转发当前字节段；真实插件可原地改写 event->data 并将 *event->length 缩短到 capacity 以内。
int32_t onStreamData(void *context, StreamDataEvent *event) {
    if (context == nullptr || event == nullptr || event->length == nullptr) {
        return -1;
    }
    if (*event->length > event->capacity) {
        return -2;
    }
    return STREAM_ACTION_FORWARD;
}

/// 连接关闭时示例没有待释放状态；真实协议解析器应在此删除 connectionId 对应的私有缓冲。
void onConnectionClose(void *context, const StreamConnectionCloseEvent *event) {
    (void)context;
    (void)event;
}

} // namespace

/// 填充 旧版 ABI 导出表；所有函数均使用 C 链接，禁止异常离开该边界。
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
