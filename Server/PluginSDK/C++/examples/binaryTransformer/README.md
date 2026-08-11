# 二进制协议转换插件

作者只实现普通函数并注册回调：

```cpp
void configure(Plugin& plugin) {
    plugin.onTcp([](const Event& event) {
        return Decision::modify(event, transform(event.bytes()));
    });
}

TRAFFIC_MOD_PLUGIN(configure)
```

示例协议使用两字节大端正文长度，正文按 `0xAA` 异或。实现演示：

- 为每个连接的上下行分别保存半包；
- 收到半包时返回 `Decision::hold()`；
- 解密完整帧，将 `blocked` 改为 `allowed`；
- 重算长度并重新加密；
- 一次回调可合并输出多帧；
- 连接关闭时清理双向状态；
- UDP 报文保持报文边界并直接修改。

编译后的动态库名称在非 Windows 平台可能为 `.so` 或 `.dylib`，打包前应同步修改
`plugin.json` 的 `runtime.entry`。
