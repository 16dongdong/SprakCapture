# Go JSONL Worker 示例

该示例只注册普通工厂与处理闭包，`RunJSONL` 负责 ABI v2 的 initialize、invoke、stop、
数字 `requestId` 回传和生命周期顺序。

```powershell
New-Item -ItemType Directory -Force dist | Out-Null
go build -o dist/goJsonlWorker.exe .
```

上述命令生成与 `plugin.json` 中 `runtime.entry` 完全一致的 `dist/goJsonlWorker.exe`，整个示例目录
可直接交给 Host。生产环境若需要并行处理多个连接，可改用
`RunJSONLWithOptions` 并设置有界的 `MaxConcurrentInvocations`。
