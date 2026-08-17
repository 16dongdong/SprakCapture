LOCAL_PATH := $(call my-dir)
ROUTESOCKS_PATH := $(LOCAL_PATH)

# HEV 及其 lwIP、协程和 YAML 依赖先构建为静态归档，再整体并入唯一业务 SO。
# 这样 APK 不再依赖第二个共享库，也不会因加载顺序丢失 JNI 注册或分类回调。
include $(LOCAL_PATH)/vendor/hev-socks5-tunnel/Android.mk
include $(ROUTESOCKS_PATH)/vendor/monocypher/Android.mk

LOCAL_PATH := $(ROUTESOCKS_PATH)
include $(CLEAR_VARS)

LOCAL_MODULE := routesocks
LOCAL_CPP_FEATURES := exceptions rtti
LOCAL_CPPFLAGS := -std=c++17 -Wall -Wextra -Werror -fvisibility=hidden
LOCAL_CFLAGS := -std=c11 -Wall -Wextra -Werror -fvisibility=hidden
LOCAL_LDLIBS := -llog
LOCAL_C_INCLUDES := \
    $(LOCAL_PATH)/include \
    $(LOCAL_PATH)/vendor/monocypher \
    $(LOCAL_PATH)/vendor/hev-socks5-tunnel/include
LOCAL_WHOLE_STATIC_LIBRARIES := hev-socks5-tunnel-static
LOCAL_STATIC_LIBRARIES := monocypher-static

LOCAL_SRC_FILES := \
    src/engine/runtime_config.cpp \
    src/engine/profile_crypto.cpp \
    src/engine/boundedTaskPool.cpp \
    src/engine/proxy_runtime.cpp \
    src/engine/proxy_runtime_tcp.cpp \
    src/engine/proxy_runtime_udp.cpp \
    src/engine/jni_bridge.cpp \
    src/core/routing_rules.cpp \
    src/net/domain_sniffer.cpp \
    src/net/dns_protocol.cpp \
    src/net/netfilter_queue.cpp \
    src/net/socket_utils.cpp \
    src/socks5/socks_protocol.cpp

include $(BUILD_SHARED_LIBRARY)
