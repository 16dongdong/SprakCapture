# HEV SOCKS5 Tunnel 固定来源

- 上游：`https://github.com/heiher/hev-socks5-tunnel`
- 版本：`2.9.3`
- 提交：`b9b9b7b9b0febe32bb5d8cdb9ffa414d94242b75`
- 子模块提交、补丁与双 ABI 归档哈希：`../sourceLock.json`
- 许可证：MIT，完整文本见 `License`

## 仓库边界

本目录只跟踪对外头文件、本地补丁和经过锁定工具链验证的 Android 静态归档。上游 lwIP、YAML 与任务系统包含超过项目 2000 行上限的官方源码，因此不把大体积第三方源码复制进业务仓库。`../rebuildPrebuilt.ps1` 会在 D 盘任务临时目录中检出固定提交、核验所有子模块和补丁哈希、物化 Windows 符号链接、执行双 ABI 构建并在结束时清理源码。

## 本地补丁边界

1. lwIP TCP/UDP 会话在原始五元组仍可见时调用 `HevFlowClassifier`，selected/global 入口使用主机序端口。
2. Android JNI 注册并入 `libroutesocks.so` 的唯一 `JNI_OnLoad`；启动同步、异常状态和停止回收具有明确失败语义。
3. 本地 SOCKS 凭据不写日志；HEV 仅作为最终业务 SO 内部静态对象，不向 APK 暴露第二个共享库。
4. 流分类 ABI 固定 38 字节，分类负值拒绝会话，0/1 分别选择全局和应用入口。

升级来源、补丁或 NDK 时必须重建两个归档、更新 `sourceLock.json` 哈希，并重新验证唯一 SO、JNI 注册、TCP/UDP 五元组与启动停止生命周期。
