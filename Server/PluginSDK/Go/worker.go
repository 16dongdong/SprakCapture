package pluginsdk

import (
	"context"
	"encoding/json"
	"errors"
	"io"
	"sync"
)

// processJSONSafeIntegerMax 与 Host 的跨语言 JSONL 数字域一致，避免 JavaScript 插件静默舍入。
const processJSONSafeIntegerMax uint64 = 9_007_199_254_740_991

// WorkerOptions 控制 JSONL worker 的调用调度；零值保持简单且确定的串行语义。
type WorkerOptions struct {
	// MaxConcurrentInvocations 大于 1 时允许多个 invoke 在途，并限制作者处理器的并发上限。
	MaxConcurrentInvocations int
}

// workerRequest 描述 Host→worker JSONL v2 消息；未使用字段保留为空。
type workerRequest struct {
	Type          string          `json:"type"`
	APIVersion    uint32          `json:"apiVersion,omitempty"`
	Manifest      json.RawMessage `json:"manifest,omitempty"`
	Configuration json.RawMessage `json:"configuration,omitempty"`
	RequestID     *uint64         `json:"requestId,omitempty"`
	Invocation    json.RawMessage `json:"invocation,omitempty"`
}

// workerResponse 描述 worker→Host JSONL v2 消息；每次写入一个完整换行对象。
type workerResponse struct {
	Type       string          `json:"type"`
	APIVersion uint32          `json:"apiVersion,omitempty"`
	RequestID  *uint64         `json:"requestId,omitempty"`
	Action     json.RawMessage `json:"action,omitempty"`
	Message    string          `json:"message,omitempty"`
}

// workerOutput 串行化并发调用的 JSONL 输出，并保留第一次写入失败供主循环返回。
type workerOutput struct {
	mutex      sync.Mutex
	encoder    *json.Encoder
	firstError error
}

// encode 原子写出单个响应；写入失败后保持首个错误，后续调用不会伪造成功。
func (output *workerOutput) encode(response workerResponse) error {
	output.mutex.Lock()
	defer output.mutex.Unlock()
	if output.firstError != nil {
		return output.firstError
	}
	output.firstError = output.encoder.Encode(response)
	return output.firstError
}

// err 返回第一次输出错误；调用方用它在读取下一条消息前尽快停止调度。
func (output *workerOutput) err() error {
	output.mutex.Lock()
	defer output.mutex.Unlock()
	return output.firstError
}

// invocationScheduler 管理可选并发、在途任务与 stop 屏障；响应写入由 workerOutput 互斥保护。
type invocationScheduler struct {
	runtimeID uint64
	limit     chan struct{}
	output    *workerOutput
	waitGroup sync.WaitGroup
}

// newInvocationScheduler 创建串行或有界并发调度器；上限小于 1 时返回明确配置错误。
func newInvocationScheduler(runtimeID uint64, output *workerOutput, maximum int) (*invocationScheduler, error) {
	if maximum < 1 {
		return nil, errors.New("worker 最大并发调用数必须大于零")
	}
	return &invocationScheduler{
		runtimeID: runtimeID,
		limit:     make(chan struct{}, maximum),
		output:    output,
	}, nil
}

// submit 按并发上限接收一次 invoke；上下文取消或输出失败时不再启动作者任务。
func (scheduler *invocationScheduler) submit(ctx context.Context, request workerRequest) error {
	if err := scheduler.output.err(); err != nil {
		return err
	}
	select {
	case scheduler.limit <- struct{}{}:
	case <-ctx.Done():
		return ctx.Err()
	}
	scheduler.waitGroup.Add(1)
	go func() {
		defer func() {
			<-scheduler.limit
			scheduler.waitGroup.Done()
		}()
		_ = scheduler.invoke(ctx, request)
	}()
	return nil
}

// invoke 执行一次作者调用并保留原 requestId 写回 result/error；业务错误不会终止 worker。
func (scheduler *invocationScheduler) invoke(ctx context.Context, request workerRequest) error {
	requestID := *request.RequestID
	action, err := invokeRuntimeWithContext(ctx, scheduler.runtimeID, request.Invocation)
	if err != nil {
		return scheduler.output.encode(workerResponse{
			Type: "error", RequestID: &requestID, Message: err.Error(),
		})
	}
	return scheduler.output.encode(workerResponse{
		Type: "result", RequestID: &requestID, Action: action,
	})
}

// wait 等待所有已接收 invoke 结束并返回输出错误；stop 只能在该屏障之后进入生命周期回调。
func (scheduler *invocationScheduler) wait() error {
	scheduler.waitGroup.Wait()
	return scheduler.output.err()
}

// RunJSONL 以默认串行模式运行 JSONL v2 worker，直到 stop、EOF 或上下文取消。
func RunJSONL(ctx context.Context, reader io.Reader, writer io.Writer) error {
	return RunJSONLWithOptions(ctx, reader, writer, WorkerOptions{})
}

// RunJSONLWithOptions 运行可选有界并发的完整 JSONL v2 worker。
//
// Decoder 流式读取 JSON 对象，不使用 Scanner，因而不会截断大正文。并发模式允许 result 按完成
// 顺序乱序返回，但 requestId 始终对应原调用；stop、EOF 与取消都会等待在途任务后执行 stop→destroy。
func RunJSONLWithOptions(
	ctx context.Context,
	reader io.Reader,
	writer io.Writer,
	options WorkerOptions,
) error {
	maximum := options.MaxConcurrentInvocations
	if maximum == 0 {
		maximum = 1
	}
	if maximum < 1 {
		return errors.New("worker 最大并发调用数必须大于零")
	}
	decoder := json.NewDecoder(reader)
	output := &workerOutput{encoder: json.NewEncoder(writer)}
	var runtimeID uint64
	var scheduler *invocationScheduler
	defer func() {
		if scheduler != nil {
			_ = scheduler.wait()
		}
		if runtimeID != 0 {
			destroyRuntime(runtimeID)
		}
	}()
	for {
		if err := ctx.Err(); err != nil {
			if scheduler != nil {
				_ = scheduler.wait()
			}
			return err
		}
		if err := output.err(); err != nil {
			if scheduler != nil {
				_ = scheduler.wait()
			}
			return err
		}
		var request workerRequest
		if err := decoder.Decode(&request); err != nil {
			if errors.Is(err, io.EOF) {
				if scheduler != nil {
					return scheduler.wait()
				}
				return nil
			}
			return err
		}
		switch request.Type {
		case "initialize":
			if runtimeID != 0 || request.APIVersion != nativeAPIVersion {
				return errors.New("重复初始化或 API 版本不匹配")
			}
			id, err := startRegistered(request.Manifest, request.Configuration)
			if err != nil {
				return err
			}
			runtimeID = id
			scheduler, err = newInvocationScheduler(id, output, maximum)
			if err != nil {
				return err
			}
			if err := output.encode(workerResponse{Type: "ready", APIVersion: nativeAPIVersion}); err != nil {
				return err
			}
		case "invoke":
			if request.RequestID == nil || *request.RequestID > processJSONSafeIntegerMax {
				return errors.New("requestId 必须是 JSONL 安全整数")
			}
			if scheduler == nil {
				requestID := *request.RequestID
				if err := output.encode(workerResponse{Type: "error", RequestID: &requestID, Message: "worker 尚未初始化"}); err != nil {
					return err
				}
				continue
			}
			if maximum == 1 {
				if err := scheduler.invoke(ctx, request); err != nil {
					return err
				}
				continue
			}
			if err := scheduler.submit(ctx, request); err != nil {
				return err
			}
		case "stop":
			if scheduler != nil {
				if err := scheduler.wait(); err != nil {
					return err
				}
				if err := stopRuntime(runtimeID); err != nil {
					return err
				}
			}
			return nil
		default:
			return errors.New("未知 JSONL v2 消息类型")
		}
	}
}
