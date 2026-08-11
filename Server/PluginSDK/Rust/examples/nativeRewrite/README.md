# Rust Native 改写示例

`src/lib.rs` 只注册一个普通函数和事件闭包。`exportPlugin!` 生成固定
`capture_extension_init`，SDK 负责 ABI v2、JSON、释放和生命周期。

```powershell
cargo build --release -p rust-native-rewrite-example
```

将生成的动态库复制到插件包 `dist/`，文件名与 `plugin.json` 的 `runtime.entry` 一致。
