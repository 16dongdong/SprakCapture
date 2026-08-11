# 旧版 Native 本地流改写兼容示例

该插件使用 `legacyNative` ABI v1 在宿主进程内改写 `127.0.0.1:19081` 的 TCP 和 UDP 流量：客户端到服务端将 `a` 改为 `A`，服务端到客户端将 `s` 改为 `S`。它仅用于迁移与回归，不代表完整插件与模块平台的推荐隔离模型。

## 构建

```powershell
cmake -S . -B $env:TEMP/capture-nativeStreamRewrite-build
cmake --build $env:TEMP/capture-nativeStreamRewrite-build --config Release
```

将生成的 `nativeStreamRewrite.dll` 放入如下目录：

```text
{pluginPackage}/dist/nativeStreamRewrite.dll
```

再将本目录的 `plugin.json` 放入 `{pluginPackage}/plugin.json`，把两者压缩为 `.tplugin.zip` 后在插件管理页安装并启用。

## 验证语义

向 SOCKS5 代理发送 `alpha\n` 并连接本地 `127.0.0.1:19081` 时，上游收到 `AlphA\n`；若上游返回 `server:AlphA\n`，客户端收到 `Server:AlphA\n`。这同时验证连接打开、上行字节改写、下行字节改写与连接关闭回调。
