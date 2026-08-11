#ifndef STREAM_PLUGIN_H
#define STREAM_PLUGIN_H

/*
 * Native 插件 ABI v1。所有结构体保持 C 布局；宿主函数表及其 hostContext 在插件运行周期内地址稳定，
 * 插件不得保存连接回调和宿主函数调用期间传入的临时字节指针。
 * 回调可能被不同连接的线程并发调用，插件 context 与回调实现必须自行保证线程安全。
 */

#include <stddef.h>
#include <stdint.h>

#if defined(_WIN32)
#define STREAM_PLUGIN_EXPORT __declspec(dllexport)
#else
#define STREAM_PLUGIN_EXPORT __attribute__((visibility("default")))
#endif

#ifdef __cplusplus
extern "C" {
#endif

enum {
    STREAM_PLUGIN_API_VERSION = 1,
    STREAM_TRANSPORT_TCP = 1,
    STREAM_TRANSPORT_UDP = 2,
    STREAM_DIRECTION_CLIENT_TO_SERVER = 1,
    STREAM_DIRECTION_SERVER_TO_CLIENT = 2,
    STREAM_ACTION_FORWARD = 0,
    STREAM_ACTION_HOLD = 1,
    STREAM_ACTION_DROP = 2,
    STREAM_ACTION_CLOSE = 3,
};

typedef struct {
    const uint8_t *pointer;
    size_t length;
} StreamByteSlice;

typedef struct {
    uint64_t connectionId;
    uint8_t transport;
    uint8_t reserved[7];
    StreamByteSlice clientAddress;
    StreamByteSlice targetHost;
    uint16_t targetPort;
} StreamConnectionOpenEvent;

typedef struct {
    uint64_t connectionId;
    uint8_t direction;
    uint8_t reserved[7];
    uint8_t *data;
    size_t *length;
    size_t capacity;
} StreamDataEvent;

typedef struct {
    uint64_t connectionId;
} StreamConnectionCloseEvent;

typedef struct {
    uint32_t apiVersion;
    void *hostContext;
    void (*log)(void *hostContext, uint32_t level, StreamByteSlice message);
    size_t (*getConfig)(void *hostContext, uint8_t *output, size_t capacity);
    int32_t (*setSessionValue)(void *hostContext, uint64_t connectionId, StreamByteSlice key, StreamByteSlice value);
    size_t (*getSessionValue)(void *hostContext, uint64_t connectionId, StreamByteSlice key, uint8_t *output, size_t capacity);
    void (*closeConnection)(void *hostContext, uint64_t connectionId);
} StreamHostFunctions;

typedef struct {
    uint32_t apiVersion;
    StreamByteSlice configuration;
} StreamPluginInitRequest;

typedef struct {
    uint32_t apiVersion;
    void *pluginContext;
    void (*destroy)(void *pluginContext);
    int32_t (*onConnectionOpen)(void *pluginContext, const StreamConnectionOpenEvent *event);
    int32_t (*onStreamData)(void *pluginContext, StreamDataEvent *event);
    void (*onConnectionClose)(void *pluginContext, const StreamConnectionCloseEvent *event);
} StreamPluginExports;

/*
 * 固定导出符号 stream_plugin_init。返回 0 表示成功；成功时必须填入 apiVersion 与 manifest 声明的回调。
 * onStreamData 仅可原地改写 data，并将 *length 设为不大于 capacity 的值；不得抛出 C++ 异常或跨 ABI 展开。
 */
STREAM_PLUGIN_EXPORT int32_t stream_plugin_init(
    const StreamHostFunctions *host,
    const StreamPluginInitRequest *request,
    StreamPluginExports *exports
);

#ifdef __cplusplus
}
#endif

#endif
