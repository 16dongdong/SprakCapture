package main

import (
	"context"

	pluginsdk "sprakcapture/plugin-sdk-go"
)

//go:generate go run sprakcapture/plugin-sdk-go/cmd/sprak-plugin-gen -output .

// init 仅注册普通工厂与闭包；生成的 zzNativeBridge 文件负责所有 c-shared ABI 导出细节。
func init() {
	pluginsdk.Register(func(_ pluginsdk.InitContext) (pluginsdk.Plugin, error) {
		return pluginsdk.Plugin{Handle: rewriteBinary}, nil
	})
}

// rewriteBinary 同时处理 TCP chunk 与 UDP datagram，把 ASCII 小写字节改为大写。
func rewriteBinary(_ context.Context, invocation pluginsdk.Invocation) (pluginsdk.Action, error) {
	event, err := pluginsdk.ParseBinaryEvent(invocation)
	if err != nil {
		return pluginsdk.Action{}, err
	}
	for index, value := range event.Bytes {
		if value >= 'a' && value <= 'z' {
			event.Bytes[index] = value - ('a' - 'A')
		}
	}
	return pluginsdk.ModifyBytes(invocation, event.Bytes)
}

// main 仅满足 Go c-shared 构建要求；宿主通过 capture_extension_init 驱动插件。
func main() {}
