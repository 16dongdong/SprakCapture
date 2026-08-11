package tests

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	pluginsdk "sprakcapture/plugin-sdk-go"
	"sprakcapture/plugin-sdk-go/bridgegen"
)

var registerTestPluginOnce sync.Once
var stoppedTestRuntimes atomic.Int32
var destroyedTestRuntimes atomic.Int32

// registerTestPlugin 注册测试进程唯一插件；处理器按 payload.delayMs 延迟以验证并发乱序与 stop 屏障。
func registerTestPlugin() {
	registerTestPluginOnce.Do(func() {
		pluginsdk.Register(func(initContext pluginsdk.InitContext) (pluginsdk.Plugin, error) {
			var lifecycle struct {
				PanicStop bool `json:"panicStop"`
			}
			if err := json.Unmarshal(initContext.Configuration, &lifecycle); err != nil {
				return pluginsdk.Plugin{}, err
			}
			return pluginsdk.Plugin{Handle: func(ctx context.Context, invocation pluginsdk.Invocation) (pluginsdk.Action, error) {
				if err := ctx.Err(); err != nil {
					return pluginsdk.Action{}, err
				}
				var timing struct {
					DelayMS int `json:"delayMs"`
				}
				if err := json.Unmarshal(invocation.Envelope.Payload, &timing); err != nil {
					return pluginsdk.Action{}, err
				}
				if timing.DelayMS > 0 {
					timer := time.NewTimer(time.Duration(timing.DelayMS) * time.Millisecond)
					defer timer.Stop()
					select {
					case <-timer.C:
					case <-ctx.Done():
						return pluginsdk.Action{}, ctx.Err()
					}
				}
				event, err := pluginsdk.ParseBinaryEvent(invocation)
				if err != nil {
					return pluginsdk.Action{}, err
				}
				return pluginsdk.ModifyBytes(invocation, bytes.ToUpper(event.Bytes))
			}, OnStop: func() {
				stoppedTestRuntimes.Add(1)
				if lifecycle.PanicStop {
					panic("停止夹具 panic")
				}
			}, OnDestroy: func() {
				destroyedTestRuntimes.Add(1)
			}}, nil
		})
	})
}

// TestExpiredEnvelopeDeadlineIsInformational 验证过期 deadline 仅供参考，不会触发 SDK 强制取消。
func TestExpiredEnvelopeDeadlineIsInformational(t *testing.T) {
	registerTestPlugin()
	invocation := workerInvocation("event-expired", []byte("active"), 0)
	invocation["envelope"].(map[string]any)["deadlineUnixMs"] = uint64(1)
	input := encodeLines(t,
		map[string]any{"type": "initialize", "apiVersion": 2, "manifest": map[string]any{}, "configuration": map[string]any{}},
		map[string]any{"type": "invoke", "requestId": uint64(9), "invocation": invocation},
		map[string]any{"type": "stop"},
	)
	var output bytes.Buffer
	if err := pluginsdk.RunJSONL(context.Background(), strings.NewReader(input), &output); err != nil {
		t.Fatalf("过期参考 deadline 不应取消调用：%v", err)
	}
	if !strings.Contains(output.String(), `"type":"result"`) || !strings.Contains(output.String(), `"requestId":9`) {
		t.Fatalf("过期参考 deadline 未返回结果：%s", output.String())
	}
}

// TestWorkerRequestIDDomain 验证零值与最大安全整数无损回传，超出跨语言安全域的值明确失败。
func TestWorkerRequestIDDomain(t *testing.T) {
	registerTestPlugin()
	const maximumSafeRequestID uint64 = 9_007_199_254_740_991
	testCases := []struct {
		name      string
		requestID uint64
		rejected  bool
	}{
		{name: "零值", requestID: 0},
		{name: "最大安全整数", requestID: maximumSafeRequestID},
		{name: "超出安全整数", requestID: maximumSafeRequestID + 1, rejected: true},
	}
	for _, testCase := range testCases {
		t.Run(testCase.name, func(t *testing.T) {
			invocation := workerInvocation("event-request-id", []byte("id"), 0)
			input := encodeLines(t,
				map[string]any{"type": "initialize", "apiVersion": 2, "manifest": map[string]any{}, "configuration": map[string]any{}},
				map[string]any{"type": "invoke", "requestId": testCase.requestID, "invocation": invocation},
				map[string]any{"type": "stop"},
			)
			var output bytes.Buffer
			err := pluginsdk.RunJSONL(context.Background(), strings.NewReader(input), &output)
			if testCase.rejected {
				if err == nil || !strings.Contains(err.Error(), "安全整数") {
					t.Fatalf("超域 requestId 应明确失败：%v", err)
				}
				return
			}
			if err != nil {
				t.Fatalf("安全 requestId 不应失败：%v", err)
			}
			expected := fmt.Sprintf(`"requestId":%d`, testCase.requestID)
			if !strings.Contains(output.String(), expected) {
				t.Fatalf("requestId 未按 JSON 数字无损回传：%s", output.String())
			}
		})
	}
}

// TestWorkerStopPanicReturnsError 验证 worker stop 回调异常被转换为错误而非越过运行时边界。
func TestWorkerStopPanicReturnsError(t *testing.T) {
	registerTestPlugin()
	input := encodeLines(t,
		map[string]any{"type": "initialize", "apiVersion": 2, "manifest": map[string]any{}, "configuration": map[string]any{"panicStop": true}},
		map[string]any{"type": "stop"},
	)
	var output bytes.Buffer
	err := pluginsdk.RunJSONL(context.Background(), strings.NewReader(input), &output)
	if err == nil || !strings.Contains(err.Error(), "OnStop") {
		t.Fatalf("stop panic 应返回明确错误：%v", err)
	}
}

// TestPacketHelpers 验证分包、合包与跨 chunk 长度前缀解析保持全部字节和顺序。
func TestPacketHelpers(t *testing.T) {
	packets, err := pluginsdk.SplitPackets([]byte("abcdefgh"), 3)
	if err != nil {
		t.Fatalf("切分负载：%v", err)
	}
	joined, err := pluginsdk.JoinPackets(packets)
	if err != nil || string(joined) != "abcdefgh" {
		t.Fatalf("合并负载错误：%q, %v", joined, err)
	}
	framer, err := pluginsdk.NewLengthPrefixedFrames(32)
	if err != nil {
		t.Fatalf("创建分帧器：%v", err)
	}
	encoded, err := framer.Encode([]byte("hello"))
	if err != nil {
		t.Fatalf("重封包：%v", err)
	}
	first, err := framer.Push(encoded[:2])
	if err != nil || len(first) != 0 {
		t.Fatalf("半前缀不得产生帧：%v, %v", first, err)
	}
	second, err := framer.Push(encoded[2:])
	if err != nil || len(second) != 1 || string(second[0]) != "hello" {
		t.Fatalf("跨 chunk 解析错误：%v, %v", second, err)
	}
}

// TestJSONLWorker 验证 initialize→ready、invoke→result、stop 的完整生产协议与字节改写。
func TestJSONLWorker(t *testing.T) {
	registerTestPlugin()
	invocation := map[string]any{
		"pluginId":   "example.go-worker",
		"moduleId":   "rewrite",
		"moduleKind": "streamTransformer",
		"envelope": map[string]any{
			"apiVersion": "2.0.0", "eventId": "event-go-1", "stage": "udpDatagram",
			"serviceGeneration": 1, "recordingGeneration": 1,
			"pluginInstanceId": "example.go-worker@1.0.0#1", "deadlineUnixMs": uint64(4102444800000),
			"context": map[string]any{"transport": "udp", "interceptionMode": "intercept", "labels": []string{}},
			"payload": map[string]any{"bytes": pluginsdk.ByteArray("hello"), "endOfStream": false},
		},
	}
	input := encodeLines(t,
		map[string]any{"type": "initialize", "apiVersion": 2, "manifest": map[string]any{}, "configuration": map[string]any{}},
		map[string]any{"type": "invoke", "requestId": uint64(1), "invocation": invocation},
		map[string]any{"type": "stop"},
	)
	var output bytes.Buffer
	if err := pluginsdk.RunJSONL(context.Background(), strings.NewReader(input), &output); err != nil {
		t.Fatalf("运行 JSONL worker：%v", err)
	}
	decoder := json.NewDecoder(&output)
	var ready map[string]any
	var result map[string]any
	if err := decoder.Decode(&ready); err != nil || ready["type"] != "ready" || ready["apiVersion"] != float64(2) {
		t.Fatalf("ready 响应错误：%v, %v", ready, err)
	}
	if err := decoder.Decode(&result); err != nil || result["type"] != "result" || result["requestId"] != float64(1) {
		t.Fatalf("result 响应错误：%v, %v", result, err)
	}
	action := result["action"].(map[string]any)
	patch := action["patch"].([]any)
	bytesValue := patch[0].(map[string]any)["value"].(map[string]any)["bytes"].([]any)
	if len(bytesValue) != 5 || bytesValue[0] != float64('H') || bytesValue[4] != float64('O') {
		t.Fatalf("改写动作字节错误：%v", bytesValue)
	}
}

// TestConcurrentJSONLWorker 验证显式并发允许按完成顺序返回，并在 stop 前等待慢调用完整写出。
func TestConcurrentJSONLWorker(t *testing.T) {
	registerTestPlugin()
	stoppedBefore := stoppedTestRuntimes.Load()
	destroyedBefore := destroyedTestRuntimes.Load()
	slow := workerInvocation("event-slow", []byte("slow"), 80)
	fast := workerInvocation("event-fast", []byte("fast"), 0)
	input := encodeLines(t,
		map[string]any{"type": "initialize", "apiVersion": 2, "manifest": map[string]any{}, "configuration": map[string]any{}},
		map[string]any{"type": "invoke", "requestId": uint64(1), "invocation": slow},
		map[string]any{"type": "invoke", "requestId": uint64(2), "invocation": fast},
		map[string]any{"type": "stop"},
	)
	var output bytes.Buffer
	if err := pluginsdk.RunJSONLWithOptions(
		context.Background(),
		strings.NewReader(input),
		&output,
		pluginsdk.WorkerOptions{MaxConcurrentInvocations: 2},
	); err != nil {
		t.Fatalf("运行并发 JSONL worker：%v", err)
	}
	decoder := json.NewDecoder(&output)
	responses := make([]map[string]any, 0, 3)
	for len(responses) < 3 {
		var response map[string]any
		if err := decoder.Decode(&response); err != nil {
			t.Fatalf("解析并发响应：%v", err)
		}
		responses = append(responses, response)
	}
	var unexpected map[string]any
	if err := decoder.Decode(&unexpected); !errors.Is(err, io.EOF) {
		t.Fatalf("并发 worker 输出了额外响应：%v, %v", unexpected, err)
	}
	if len(responses) != 3 || responses[0]["type"] != "ready" {
		t.Fatalf("并发响应数量或 ready 错误：%v", responses)
	}
	if responses[1]["requestId"] != float64(2) || responses[2]["requestId"] != float64(1) {
		t.Fatalf("并发完成顺序或 requestId 错误：%v", responses)
	}
	if stoppedTestRuntimes.Load() != stoppedBefore+1 || destroyedTestRuntimes.Load() != destroyedBefore+1 {
		t.Fatalf("stop/destroy 生命周期没有各执行一次：stop=%d, destroy=%d", stoppedTestRuntimes.Load()-stoppedBefore, destroyedTestRuntimes.Load()-destroyedBefore)
	}
}

// TestActionConstructors 验证全部稳定宿主动作的字段、输入校验及二进制字段保留语义。
func TestActionConstructors(t *testing.T) {
	invocation := pluginsdk.Invocation{Envelope: pluginsdk.EventEnvelope{
		EventID: "event-actions",
		Payload: json.RawMessage(`{"bytes":[1,2],"endOfStream":true,"channel":"client"}`),
	}}
	if action := pluginsdk.Continue(invocation); action.Action != pluginsdk.ActionContinue {
		t.Fatalf("continue 动作错误：%v", action)
	}
	if action := pluginsdk.Hold(invocation); action.Action != pluginsdk.ActionHold {
		t.Fatalf("hold 动作错误：%v", action)
	}
	if action := pluginsdk.Drop(invocation); action.Action != pluginsdk.ActionDrop {
		t.Fatalf("drop 动作错误：%v", action)
	}

	modified, err := pluginsdk.ModifyBytes(invocation, []byte{3, 4})
	if err != nil {
		t.Fatalf("构造二进制修改动作：%v", err)
	}
	var operation map[string]json.RawMessage
	if err := json.Unmarshal(modified.Patch[0], &operation); err != nil {
		t.Fatalf("解析修改补丁：%v", err)
	}
	var payload map[string]json.RawMessage
	if err := json.Unmarshal(operation["value"], &payload); err != nil {
		t.Fatalf("解析修改后的 payload：%v", err)
	}
	if string(payload["bytes"]) != "[3,4]" || string(payload["endOfStream"]) != "true" || string(payload["channel"]) != `"client"` {
		t.Fatalf("二进制修改未保留 payload 字段：%s", operation["value"])
	}

	rejected, err := pluginsdk.Reject(invocation, "不允许")
	if err != nil || rejected.Action != pluginsdk.ActionReject {
		t.Fatalf("reject 动作错误：%v, %v", rejected, err)
	}
	closed, err := pluginsdk.Close(invocation, "连接结束")
	if err != nil || closed.Action != pluginsdk.ActionClose {
		t.Fatalf("close 动作错误：%v, %v", closed, err)
	}
	if _, err := pluginsdk.Reject(invocation, "  "); err == nil {
		t.Fatal("空白 reject 原因必须失败")
	}

	annotated, err := pluginsdk.Annotate(invocation, map[string]any{"tag": "检查"})
	if err != nil || annotated.Action != pluginsdk.ActionAnnotate || len(annotated.Annotations) != 1 {
		t.Fatalf("annotate 动作错误：%v, %v", annotated, err)
	}
	redirected, err := pluginsdk.Redirect(invocation, "upstream.local", 8443)
	if err != nil || redirected.Action != pluginsdk.ActionRedirect {
		t.Fatalf("redirect 动作错误：%v, %v", redirected, err)
	}
	if _, err := pluginsdk.Redirect(invocation, "", 0); err == nil {
		t.Fatal("无效 redirect 目标必须失败")
	}
	responded, err := pluginsdk.Respond(invocation, map[string]any{"statusCode": 204})
	if err != nil || responded.Action != pluginsdk.ActionRespond {
		t.Fatalf("respond 动作错误：%v, %v", responded, err)
	}
	modifiedPayload, err := pluginsdk.ModifyPayload(invocation, map[string]any{"value": 7})
	if err != nil || modifiedPayload.Action != pluginsdk.ActionModify || len(modifiedPayload.Patch) != 1 {
		t.Fatalf("通用 modify 动作错误：%v, %v", modifiedPayload, err)
	}
}

// workerInvocation 构造带可控延迟的二进制 worker 调用，供串并行契约测试复用。
func workerInvocation(eventID string, payload []byte, delayMS int) map[string]any {
	return map[string]any{
		"pluginId": "example.go-worker", "moduleId": "rewrite", "moduleKind": "streamTransformer",
		"envelope": map[string]any{
			"apiVersion": "2.0.0", "eventId": eventID, "stage": "tcpChunk",
			"serviceGeneration": 1, "recordingGeneration": 1,
			"pluginInstanceId": "example.go-worker@1.0.0#1", "deadlineUnixMs": uint64(4102444800000),
			"context": map[string]any{"transport": "tcp", "interceptionMode": "intercept", "labels": []string{}},
			"payload": map[string]any{"bytes": pluginsdk.ByteArray(payload), "endOfStream": false, "delayMs": delayMS},
		},
	}
}

// TestBridgeGenerator 验证生成器只创建固定 ABI 桥文件并包含宿主要求的导出符号。
func TestBridgeGenerator(t *testing.T) {
	directory := t.TempDir()
	if err := bridgegen.Generate(bridgegen.Options{
		OutputDirectory: directory,
		PackageName:     "main",
		SDKImportPath:   "sprakcapture/plugin-sdk-go",
	}); err != nil {
		t.Fatalf("生成 Native 桥：%v", err)
	}
	exportBytes, err := os.ReadFile(filepath.Join(directory, "zzNativeBridge.go"))
	if err != nil {
		t.Fatalf("读取生成桥：%v", err)
	}
	if !bytes.Contains(exportBytes, []byte("//export capture_extension_init")) ||
		!bytes.Contains(exportBytes, []byte("goCaptureInvoke")) {
		t.Fatalf("生成桥缺少 ABI v2 导出")
	}
}

// encodeLines 把测试消息编码为真实 JSONL；任一序列化失败都立即终止用例。
func encodeLines(t *testing.T, messages ...any) string {
	t.Helper()
	var output bytes.Buffer
	encoder := json.NewEncoder(&output)
	for _, message := range messages {
		if err := encoder.Encode(message); err != nil {
			t.Fatalf("编码 JSONL 消息：%v", err)
		}
	}
	return output.String()
}
