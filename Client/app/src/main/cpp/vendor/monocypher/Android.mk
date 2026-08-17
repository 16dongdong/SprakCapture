LOCAL_PATH := $(call my-dir)

# Monocypher 固定归档来自官方 4.0.3 源码；只导出稳定头文件和静态符号。
include $(CLEAR_VARS)
LOCAL_MODULE := monocypher-static
LOCAL_SRC_FILES := prebuilt/$(TARGET_ARCH_ABI)/libmonocypher.a
LOCAL_EXPORT_C_INCLUDES := $(LOCAL_PATH)
include $(PREBUILT_STATIC_LIBRARY)
