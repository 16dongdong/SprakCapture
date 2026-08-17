# 统一 Native 库在 JNI_OnLoad 中按固定二进制类名和方法名注册 HEV，压缩阶段不得改写该 ABI。
-keep class hev.sockstun.TProxyService { *; }

# libroutesocks 使用静态 JNI 符号绑定该类和 native 方法；R8 改名会让发布包启动时报 UnsatisfiedLinkError。
-keep class app.proxy.client.runtime.NativeRuntime { *; }

# Root 伴随进程由 app_process 按固定类名进入 main；该类没有普通 Java 调用点，压缩器不得删除或改名。
-keep class app.proxy.client.runtime.RootCompanionMain { *; }

# HEV 通过固定类名回调五元组归属；混淆会让混合规则数据面无法启动。
-keep class app.proxy.client.runtime.VpnFlowClassifier { *; }
