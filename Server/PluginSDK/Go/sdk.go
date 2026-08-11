// Package pluginsdk 封装 Sprak Capture 插件 ABI v2 的事件、动作、生命周期和运行时入口。
package pluginsdk

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"strings"
	"sync"
	"sync/atomic"
)

const nativeAPIVersion uint32 = 2

// Stage 标识宿主发布的稳定处理阶段；字符串值与 ABI v2 JSON 完全一致。
type Stage string

const (
	StageServiceStarting        Stage = "serviceStarting"
	StageServiceStarted         Stage = "serviceStarted"
	StageConfigurationChanged   Stage = "configurationChanged"
	StageServiceStopping        Stage = "serviceStopping"
	StageConnectionAccepted     Stage = "connectionAccepted"
	StageSocks5Authentication   Stage = "socks5Authentication"
	StageProtocolClassified     Stage = "protocolClassified"
	StageTargetResolving        Stage = "targetResolving"
	StageBeforeConnect          Stage = "beforeConnect"
	StageConnected              Stage = "connected"
	StageConnectionClosing      Stage = "connectionClosing"
	StageClientHelloObserved    Stage = "clientHelloObserved"
	StageCertificateSelecting   Stage = "certificateSelecting"
	StageTLSEstablished         Stage = "tlsEstablished"
	StageTLSFailed              Stage = "tlsFailed"
	StageRequestHeaders         Stage = "requestHeaders"
	StageRequestBodyChunk       Stage = "requestBodyChunk"
	StageRequestComplete        Stage = "requestComplete"
	StageBeforeUpstream         Stage = "beforeUpstream"
	StageResponseHeaders        Stage = "responseHeaders"
	StageResponseBodyChunk      Stage = "responseBodyChunk"
	StageResponseComplete       Stage = "responseComplete"
	StageWebSocketOpening       Stage = "webSocketOpening"
	StageWebSocketFrame         Stage = "webSocketFrame"
	StageWebSocketClosing       Stage = "webSocketClosing"
	StageTCPChunk               Stage = "tcpChunk"
	StageUDPDatagram            Stage = "udpDatagram"
	StageDNSMessage             Stage = "dnsMessage"
	StageBeforeRecord           Stage = "beforeRecord"
	StageTransactionUpdated     Stage = "transactionUpdated"
	StageTransactionCompleted   Stage = "transactionCompleted"
	StageRecordingCleared       Stage = "recordingCleared"
	StageInspectorDataRequested Stage = "inspectorDataRequested"
	StageCommandInvoked         Stage = "commandInvoked"
	StageContextActionInvoked   Stage = "contextActionInvoked"
)

// StageContext 保存连接、进程、协议和拦截模式上下文；不适用字段保持零值。
type StageContext struct {
	Entry            string   `json:"entry,omitempty"`
	ProcessID        uint32   `json:"processId,omitempty"`
	ProcessName      string   `json:"processName,omitempty"`
	ProcessPath      string   `json:"processPath,omitempty"`
	Transport        string   `json:"transport,omitempty"`
	Protocol         string   `json:"protocol,omitempty"`
	Direction        string   `json:"direction,omitempty"`
	Scheme           string   `json:"scheme,omitempty"`
	Host             string   `json:"host,omitempty"`
	Address          string   `json:"address,omitempty"`
	Port             uint16   `json:"port,omitempty"`
	Method           string   `json:"method,omitempty"`
	Path             string   `json:"path,omitempty"`
	StatusCode       uint16   `json:"statusCode,omitempty"`
	MIMEType         string   `json:"mimeType,omitempty"`
	Labels           []string `json:"labels"`
	InterceptionMode string   `json:"interceptionMode"`
}

// EventEnvelope 保存阶段事件身份、上下文与未解释负载；EventID 必须原样回填动作。
type EventEnvelope struct {
	APIVersion          string          `json:"apiVersion"`
	EventID             string          `json:"eventId"`
	Stage               Stage           `json:"stage"`
	ServiceGeneration   uint64          `json:"serviceGeneration"`
	RecordingGeneration uint64          `json:"recordingGeneration"`
	PluginInstanceID    string          `json:"pluginInstanceId"`
	ConnectionID        *string         `json:"connectionId"`
	TransactionID       *string         `json:"transactionId"`
	DeadlineUnixMS      uint64          `json:"deadlineUnixMs"`
	Context             StageContext    `json:"context"`
	Payload             json.RawMessage `json:"payload"`
}

// Invocation 保存 Native/JSONL 宿主传入的模块身份和完整阶段事件。
type Invocation struct {
	PluginID   string        `json:"pluginId"`
	ModuleID   string        `json:"moduleId"`
	ModuleKind string        `json:"moduleKind"`
	Envelope   EventEnvelope `json:"envelope"`
}

// ActionKind 标识插件动作；宿主仍会按当前阶段复验其合法性。
type ActionKind string

const (
	ActionContinue ActionKind = "continue"
	ActionModify   ActionKind = "modify"
	ActionHold     ActionKind = "hold"
	ActionDrop     ActionKind = "drop"
	ActionReject   ActionKind = "reject"
	ActionRespond  ActionKind = "respond"
	ActionRedirect ActionKind = "redirect"
	ActionAnnotate ActionKind = "annotate"
	ActionClose    ActionKind = "close"
)

// Action 保存对当前事件的结构化决定；Output 中的 bytes 由宿主重封包到线上连接。
type Action struct {
	EventID     string            `json:"eventId"`
	Action      ActionKind        `json:"action"`
	Patch       []json.RawMessage `json:"patch"`
	Annotations []json.RawMessage `json:"annotations"`
	Output      any               `json:"output,omitempty"`
}

// Continue 返回不改变当前事件的动作，并自动绑定正确 EventID。
func Continue(invocation Invocation) Action {
	return NewAction(invocation, ActionContinue)
}

// NewAction 构造指定动作；Patch 与 Annotations 初始化为空数组以满足宿主稳定输出契约。
func NewAction(invocation Invocation, action ActionKind) Action {
	return Action{
		EventID:     invocation.Envelope.EventID,
		Action:      action,
		Patch:       []json.RawMessage{},
		Annotations: []json.RawMessage{},
	}
}

// ModifyPayload 原子替换完整事件 payload；无法编码的值会在进入宿主前明确返回错误。
func ModifyPayload(invocation Invocation, payload any) (Action, error) {
	encodedPayload, err := json.Marshal(payload)
	if err != nil {
		return Action{}, fmt.Errorf("编码修改后的 payload：%w", err)
	}
	operation, err := json.Marshal(map[string]any{
		"op":    "replace",
		"path":  "",
		"value": json.RawMessage(encodedPayload),
	})
	if err != nil {
		return Action{}, fmt.Errorf("编码 payload 替换操作：%w", err)
	}
	action := NewAction(invocation, ActionModify)
	action.Patch = []json.RawMessage{operation}
	return action, nil
}

// ModifyBytes 替换 TCP、UDP 或正文块的 bytes，同时保留 payload 其余字段；非对象 payload 明确失败。
func ModifyBytes(invocation Invocation, bytes []byte) (Action, error) {
	var payload map[string]json.RawMessage
	if err := json.Unmarshal(invocation.Envelope.Payload, &payload); err != nil || payload == nil {
		return Action{}, errors.New("二进制事件 payload 必须是对象")
	}
	encodedBytes, err := json.Marshal(append(ByteArray(nil), bytes...))
	if err != nil {
		return Action{}, fmt.Errorf("编码二进制负载：%w", err)
	}
	payload["bytes"] = encodedBytes
	return ModifyPayload(invocation, payload)
}

// Hold 暂存当前流式事件等待后续字节；动作能否用于当前阶段仍由宿主校验。
func Hold(invocation Invocation) Action {
	return NewAction(invocation, ActionHold)
}

// Drop 丢弃当前数据块或录制事务；最终作用域由宿主当前阶段确定。
func Drop(invocation Invocation) Action {
	return NewAction(invocation, ActionDrop)
}

// Reject 拒绝当前操作并携带非空原因；空白原因属于作者输入错误。
func Reject(invocation Invocation, reason string) (Action, error) {
	return reasonAction(invocation, ActionReject, reason)
}

// Close 请求立即关闭当前连接并携带非空原因；非连接阶段会由宿主拒绝。
func Close(invocation Invocation, reason string) (Action, error) {
	return reasonAction(invocation, ActionClose, reason)
}

// Annotate 添加结构化注释而不改变线上字节；任一无法编码的注释都会使整个动作失败。
func Annotate(invocation Invocation, annotations ...any) (Action, error) {
	action := NewAction(invocation, ActionAnnotate)
	action.Annotations = make([]json.RawMessage, 0, len(annotations))
	for index, annotation := range annotations {
		encodedAnnotation, err := json.Marshal(annotation)
		if err != nil {
			return Action{}, fmt.Errorf("编码第 %d 条注释：%w", index+1, err)
		}
		action.Annotations = append(action.Annotations, encodedAnnotation)
	}
	return action, nil
}

// Redirect 改写最终上游目标；主机为空或端口不在 1..65535 时明确失败。
func Redirect(invocation Invocation, host string, port int) (Action, error) {
	if strings.TrimSpace(host) == "" || port < 1 || port > 65535 {
		return Action{}, errors.New("重定向目标必须包含有效主机和端口")
	}
	action := NewAction(invocation, ActionRedirect)
	action.Output = map[string]any{"host": host, "port": port}
	return action, nil
}

// Respond 生成 HTTP、DNS 或命令阶段的完整合成响应；无法编码的输出会在进入宿主前失败。
func Respond(invocation Invocation, output any) (Action, error) {
	encodedOutput, err := json.Marshal(output)
	if err != nil {
		return Action{}, fmt.Errorf("编码合成响应：%w", err)
	}
	action := NewAction(invocation, ActionRespond)
	action.Output = json.RawMessage(encodedOutput)
	return action, nil
}

// reasonAction 统一 reject/close 的原因校验与输出结构，避免终止动作字段发生漂移。
func reasonAction(invocation Invocation, actionKind ActionKind, reason string) (Action, error) {
	if strings.TrimSpace(reason) == "" {
		return Action{}, errors.New("终止原因不能为空")
	}
	action := NewAction(invocation, actionKind)
	action.Output = map[string]any{"reason": reason}
	return action, nil
}

// Handler 是作者注册的普通函数或闭包；同一插件可能被多个连接并发调用。
type Handler func(context.Context, Invocation) (Action, error)

// InitContext 保存初始化阶段的原始 manifest 与配置；切片由 SDK 独占复制。
type InitContext struct {
	Manifest      json.RawMessage
	Configuration json.RawMessage
}

// Plugin 保存普通事件与生命周期闭包；OnStop 和 OnDestroy 均由 SDK 至多调用一次。
type Plugin struct {
	Handle    Handler
	OnStop    func()
	OnDestroy func()
}

// Factory 根据 manifest 和配置创建插件实例；每个启用实例独立调用一次。
type Factory func(InitContext) (Plugin, error)

var registration struct {
	sync.RWMutex
	factory Factory
}

// Register 注册当前动态库或 worker 的唯一工厂；重复注册会直接 panic 暴露包结构错误。
func Register(factory Factory) {
	if factory == nil {
		panic("插件工厂不能为空")
	}
	registration.Lock()
	defer registration.Unlock()
	if registration.factory != nil {
		panic("同一插件进程只能注册一个工厂")
	}
	registration.factory = factory
}

// runtimeInstance 保存单个插件实例状态；事件处理器本身负责其业务状态的并发同步。
type runtimeInstance struct {
	plugin         Plugin
	invocationGate sync.RWMutex
	stopped        atomic.Bool
	stopOnce       sync.Once
	destroyOnce    sync.Once
}

var runtimes struct {
	sync.RWMutex
	nextID    atomic.Uint64
	instances map[uint64]*runtimeInstance
}

// init 初始化进程内运行时表；这里只建立空容器，不执行作者代码或外部 I/O。
func init() {
	runtimes.instances = make(map[uint64]*runtimeInstance)
}

// startRegistered 创建已注册工厂的独立实例；JSON 无效或处理器为空时不发布半实例。
func startRegistered(manifest, configuration []byte) (uint64, error) {
	registration.RLock()
	factory := registration.factory
	registration.RUnlock()
	if factory == nil {
		return 0, errors.New("尚未注册插件工厂")
	}
	if !json.Valid(manifest) || !json.Valid(configuration) {
		return 0, errors.New("manifest 或 configuration 不是有效 JSON")
	}
	plugin, err := callFactory(factory, InitContext{
		Manifest:      append(json.RawMessage(nil), manifest...),
		Configuration: append(json.RawMessage(nil), configuration...),
	})
	if err != nil {
		return 0, err
	}
	if plugin.Handle == nil {
		return 0, errors.New("插件处理器不能为空")
	}
	id := runtimes.nextID.Add(1)
	runtimes.Lock()
	runtimes.instances[id] = &runtimeInstance{plugin: plugin}
	runtimes.Unlock()
	return id, nil
}

// callFactory 截获作者工厂 panic 并转换为初始化错误，避免异常越过 Native/worker 运行时边界。
func callFactory(factory Factory, initContext InitContext) (plugin Plugin, err error) {
	defer func() {
		if recovered := recover(); recovered != nil {
			err = fmt.Errorf("插件工厂 panic：%v", recovered)
		}
	}()
	return factory(initContext)
}

// loadRuntime 返回仍存在的实例；销毁后的标识明确失败，不回退到其他实例。
func loadRuntime(id uint64) (*runtimeInstance, error) {
	runtimes.RLock()
	runtime := runtimes.instances[id]
	runtimes.RUnlock()
	if runtime == nil {
		return nil, errors.New("插件实例不存在")
	}
	return runtime, nil
}

// invokeRuntime 解析并执行一次 ABI v2 调用；停止态与错误 EventID 均拒绝序列化。
func invokeRuntime(id uint64, request []byte) ([]byte, error) {
	return invokeRuntimeWithContext(context.Background(), id, request)
}

// invokeRuntimeWithContext 执行带父上下文的调用；worker 取消会传给作者，Native 调用保持独立上下文。
func invokeRuntimeWithContext(parent context.Context, id uint64, request []byte) ([]byte, error) {
	runtime, err := loadRuntime(id)
	if err != nil {
		return nil, err
	}
	// 读锁覆盖完整作者调用，确保 stop/destroy 等待所有在途处理结束且不会与清理回调交错。
	runtime.invocationGate.RLock()
	defer runtime.invocationGate.RUnlock()
	if runtime.stopped.Load() {
		return nil, errors.New("插件实例已停止")
	}
	var invocation Invocation
	if err := json.Unmarshal(request, &invocation); err != nil {
		return nil, fmt.Errorf("解析 invocation: %w", err)
	}
	// deadlineUnixMs 是宿主提供给作者的调度参考值，SDK 不把它变成强制取消边界。
	action, err := callHandler(runtime.plugin.Handle, parent, invocation)
	if err != nil {
		return nil, err
	}
	if action.EventID != invocation.Envelope.EventID {
		return nil, errors.New("动作 eventId 与当前事件不一致")
	}
	return json.Marshal(action)
}

// callHandler 截获作者处理器 panic 并转换为当前调用错误，使其他连接与生命周期保持可控。
func callHandler(handler Handler, ctx context.Context, invocation Invocation) (action Action, err error) {
	defer func() {
		if recovered := recover(); recovered != nil {
			err = fmt.Errorf("插件处理器 panic：%v", recovered)
		}
	}()
	return handler(ctx, invocation)
}

// stopRuntime 原子停止实例并至多调用一次 OnStop；重复 stop 保持幂等。
func stopRuntime(id uint64) error {
	runtime, err := loadRuntime(id)
	if err != nil {
		return err
	}
	runtime.invocationGate.Lock()
	defer runtime.invocationGate.Unlock()
	var callbackError error
	runtime.stopOnce.Do(func() {
		runtime.stopped.Store(true)
		callbackError = callLifecycle("OnStop", runtime.plugin.OnStop)
	})
	return callbackError
}

// destroyRuntime 按 stop→destroy 顺序移除实例；删除映射后新调用无法取得旧上下文。
func destroyRuntime(id uint64) {
	runtime, err := loadRuntime(id)
	if err != nil {
		return
	}
	reportLifecycleError(stopRuntime(id))
	runtime.destroyOnce.Do(func() {
		reportLifecycleError(callLifecycle("OnDestroy", runtime.plugin.OnDestroy))
	})
	runtimes.Lock()
	delete(runtimes.instances, id)
	runtimes.Unlock()
}

// reportLifecycleError 把 void ABI 无法返回的生命周期异常写入 stderr，避免污染 JSONL stdout。
func reportLifecycleError(err error) {
	if err != nil {
		_, _ = fmt.Fprintf(os.Stderr, "插件生命周期异常：%v\n", err)
	}
}

// callLifecycle 隔离作者生命周期回调 panic；空回调视为成功，异常不会跨越 C ABI 边界。
func callLifecycle(name string, callback func()) (err error) {
	if callback == nil {
		return nil
	}
	defer func() {
		if recovered := recover(); recovered != nil {
			err = fmt.Errorf("插件 %s 回调 panic：%v", name, recovered)
		}
	}()
	callback()
	return nil
}

// NativeAPIVersion 返回 c-shared 生成桥应写入导出表的固定 ABI 版本。
func NativeAPIVersion() uint32 { return nativeAPIVersion }

// StartNative 供生成的 c-shared 桥创建实例；作者代码不直接调用。
func StartNative(manifest, configuration []byte) (uint64, error) {
	return startRegistered(manifest, configuration)
}

// InvokeNative 供生成的 c-shared 桥执行事件并取得插件拥有的 JSON；作者代码不直接调用。
func InvokeNative(id uint64, request []byte) ([]byte, error) { return invokeRuntime(id, request) }

// StopNative 供生成的 c-shared 桥热停止实例；作者代码不直接调用。
func StopNative(id uint64) { reportLifecycleError(stopRuntime(id)) }

// DestroyNative 供生成的 c-shared 桥执行最终回收；作者代码不直接调用。
func DestroyNative(id uint64) { destroyRuntime(id) }
