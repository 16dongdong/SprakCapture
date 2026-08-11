# 旧版 Native 直通兼容示例

本示例用于验证 `legacyNative` 旧版 ABI 的迁移兼容性，不代表完整插件与模块平台的推荐运行时或能力边界。新模块优先使用 Wasm、sidecar 或隔离的 native worker。

构建：

```powershell
cmake -S . -B $env:TEMP/capture-nativePassthrough-build
cmake --build $env:TEMP/capture-nativePassthrough-build --config Release
```

将生成的 `nativePassthrough.dll` 复制为：

```text
{dataDirectory}/plugins/example.nativePassthrough/dist/nativePassthrough.dll
```

再把本目录的 `plugin.json` 复制到 `{dataDirectory}/plugins/example.nativePassthrough/plugin.json`，重启 `proxyService` 后通过 `capture_plugin_list` 和 `capture_plugin_set_enabled` 管理。

该示例只验证旧 C ABI 与连接生命周期，流数据原样转发。完整 manifest、运行时、阶段、权限和兼容层语义见 `Docs/pluginHookApi.md`。
