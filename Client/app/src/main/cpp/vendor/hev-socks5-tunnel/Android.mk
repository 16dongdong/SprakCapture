LOCAL_PATH := $(call my-dir)

# 固定归档由来源锁定脚本从 HEV 2.9.3、指定子模块和本地补丁重建。
# 归档只作为最终业务 SO 的内部对象集合，APK 不会产生第二个共享库。
include $(CLEAR_VARS)
LOCAL_MODULE := hev-socks5-tunnel-static
LOCAL_SRC_FILES := prebuilt/$(TARGET_ARCH_ABI)/libhev-socks5-tunnel-static.a
LOCAL_EXPORT_C_INCLUDES := $(LOCAL_PATH)/include
include $(PREBUILT_STATIC_LIBRARY)
