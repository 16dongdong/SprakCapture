# Go Native 改写示例

作者代码只在 `main.go` 注册普通工厂和闭包。首次构建前生成固定 ABI 桥：

```powershell
go generate ./...
go build -buildmode=c-shared -o dist/goNativeRewrite.dll .
```

`zzNativeBridge*` 封装 `capture_extension_init`、invoke、stop/destroy 和 C 内存释放；不要手改。
