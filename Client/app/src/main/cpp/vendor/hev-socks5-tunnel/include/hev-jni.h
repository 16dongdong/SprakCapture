/*
 ============================================================================
 Name        : hev-jni.h
 Author      : hev <r@hev.cc>
 Copyright   : Copyright (c) 2019 - 2023 hev
 Description : Java Native Interface
 ============================================================================
 */

#ifndef __HEV_JNI_H__
#define __HEV_JNI_H__

#ifdef ANDROID

#include <jni.h>

#ifdef __cplusplus
extern "C" {
#endif

/*
 * 把 HEV 的原生方法注册并入宿主 SO 的唯一 JNI_OnLoad。
 * 该函数仅在 VM 加载 libroutesocks.so 时调用一次；类查找、方法注册或同步原语初始化失败返回 JNI_ERR。
 */
jint hev_jni_initialize (JavaVM *vm);

#ifdef __cplusplus
}
#endif

#endif

#endif /* __HEV_JNI_H__ */
