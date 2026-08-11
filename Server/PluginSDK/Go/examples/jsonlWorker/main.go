package main

import (
	"context"
	"os"

	pluginsdk "sprakcapture/plugin-sdk-go"
)

// init 注册与 Native 示例相同的普通闭包；JSONL worker 复用同一事件与动作 API。
func init() {
	pluginsdk.Register(func(_ pluginsdk.InitContext) (pluginsdk.Plugin, error) {
		return pluginsdk.Plugin{Handle: func(_ context.Context, invocation pluginsdk.Invocation) (pluginsdk.Action, error) {
			return pluginsdk.Continue(invocation), nil
		}}, nil
	})
}

// main 通过标准输入输出运行完整 JSONL 生命周期；错误直接返回非零退出码。
func main() {
	if err := pluginsdk.RunJSONL(context.Background(), os.Stdin, os.Stdout); err != nil {
		_, _ = os.Stderr.WriteString(err.Error() + "\n")
		os.Exit(1)
	}
}
