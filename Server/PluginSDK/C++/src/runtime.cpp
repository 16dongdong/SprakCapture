#include "trafficMod/sdk.hpp"

#include <atomic>
#include <cstdio>
#include <limits>
#include <memory>
#include <mutex>
#include <stdexcept>
#include <utility>

namespace trafficMod {
namespace {

constexpr std::uint32_t nativeApiVersion = 2;

/// 与宿主 Rust `ByteSlice` 保持字段顺序和机器字长一致；仅在单次 ABI
/// 回调期间借用内存。
struct ByteSlice final {
  const std::uint8_t *pointer;
  std::size_t length;
};

/// 与宿主 Native 初始化请求保持 C 布局；SDK 在配置函数运行前复制两个 JSON
/// 输入。
struct InitRequest final {
  std::uint32_t apiVersion;
  ByteSlice manifest;
  ByteSlice configuration;
};

/// 描述交给宿主的拥有型 JSON 缓冲；释放回调把所有权归还 SDK 分配器。
struct OutputBuffer final {
  const std::uint8_t *pointer;
  std::size_t length;
  void *releaseContext;
  void (*release)(void *, const std::uint8_t *, std::size_t);
};

/// 与宿主 Native 导出表保持 C 布局；任何字段顺序变化都会破坏 ABI v2。
struct Exports final {
  std::uint32_t apiVersion;
  void *pluginContext;
  int (*invoke)(void *, ByteSlice, OutputBuffer *);
  void (*stop)(void *);
  void (*destroy)(void *);
};

/// 把 ABI 借用切片复制成字符串；空指针和空 JSON 都表示损坏的调用帧。
[[nodiscard]] std::string copyText(ByteSlice slice) {
  if (slice.pointer == nullptr || slice.length == 0) {
    throw std::runtime_error("Native ABI JSON 输入为空");
  }
  return std::string(reinterpret_cast<const char *>(slice.pointer),
                     slice.length);
}

/// 向标准错误输出稳定生命周期诊断；该 noexcept
/// 路径不分配内存，适用于析构和停止回调异常。
void reportLifecycleFailure(const char *message) noexcept {
  std::fputs(message, stderr);
  std::fputc('\n', stderr);
}

/// 返回必需对象字段；字段缺失时拒绝事件而不是伪造默认身份。
[[nodiscard]] const Json &requireField(const Json &object,
                                       std::string_view key) {
  const Json *value = object.find(key);
  if (value == nullptr)
    throw std::runtime_error("Native ABI 事件缺少字段：" + std::string(key));
  return *value;
}

/// 读取可选字符串；缺失字段返回空字符串，类型错误由 Json 明确抛出。
[[nodiscard]] std::string optionalString(const Json &object,
                                         std::string_view key) {
  const Json *value = object.find(key);
  return value == nullptr || value->isNull() ? std::string()
                                             : value->asString();
}

/// 判断作者输入是否只含协议常见空白；校验不依赖进程区域设置，确保不同机器结果一致。
[[nodiscard]] bool isBlank(std::string_view value) noexcept {
  return value.empty() ||
         value.find_first_not_of(" \t\r\n\f\v") == std::string_view::npos;
}

/// 将稳定阶段线名映射为 C++ 枚举；未知值保留原名并映射为 Unknown
/// 以支持前向兼容。
[[nodiscard]] Stage parseStage(std::string_view value) noexcept {
  static const std::map<std::string_view, Stage, std::less<>> stages = {
      {"serviceStarting", Stage::ServiceStarting},
      {"serviceStarted", Stage::ServiceStarted},
      {"configurationChanged", Stage::ConfigurationChanged},
      {"serviceStopping", Stage::ServiceStopping},
      {"connectionAccepted", Stage::ConnectionAccepted},
      {"socks5Authentication", Stage::Socks5Authentication},
      {"protocolClassified", Stage::ProtocolClassified},
      {"targetResolving", Stage::TargetResolving},
      {"beforeConnect", Stage::BeforeConnect},
      {"connected", Stage::Connected},
      {"connectionClosing", Stage::ConnectionClosing},
      {"clientHelloObserved", Stage::ClientHelloObserved},
      {"certificateSelecting", Stage::CertificateSelecting},
      {"tlsEstablished", Stage::TlsEstablished},
      {"tlsFailed", Stage::TlsFailed},
      {"requestHeaders", Stage::RequestHeaders},
      {"requestBodyChunk", Stage::RequestBodyChunk},
      {"requestComplete", Stage::RequestComplete},
      {"beforeUpstream", Stage::BeforeUpstream},
      {"responseHeaders", Stage::ResponseHeaders},
      {"responseBodyChunk", Stage::ResponseBodyChunk},
      {"responseComplete", Stage::ResponseComplete},
      {"webSocketOpening", Stage::WebSocketOpening},
      {"webSocketFrame", Stage::WebSocketFrame},
      {"webSocketClosing", Stage::WebSocketClosing},
      {"tcpChunk", Stage::TcpChunk},
      {"udpDatagram", Stage::UdpDatagram},
      {"dnsMessage", Stage::DnsMessage},
      {"beforeRecord", Stage::BeforeRecord},
      {"transactionUpdated", Stage::TransactionUpdated},
      {"transactionCompleted", Stage::TransactionCompleted},
      {"recordingCleared", Stage::RecordingCleared},
      {"inspectorDataRequested", Stage::InspectorDataRequested},
      {"commandInvoked", Stage::CommandInvoked},
      {"contextActionInvoked", Stage::ContextActionInvoked},
  };
  const auto iterator = stages.find(value);
  return iterator == stages.end() ? Stage::Unknown : iterator->second;
}

/// 从阶段上下文读取传输方向；宿主使用 up/down，其他值按未知处理。
[[nodiscard]] Direction parseDirection(const Json &context) {
  const std::string direction = optionalString(context, "direction");
  if (direction == "up")
    return Direction::ClientToServer;
  if (direction == "down")
    return Direction::ServerToClient;
  return Direction::Unknown;
}

/// 把 JSON 字节数组转换为拥有型缓冲；越界值或非整数会拒绝整帧，绝不转发坏包。
[[nodiscard]] std::vector<std::uint8_t> parseBytes(const Json &payload) {
  const Json *bytes = payload.find("bytes");
  if (bytes == nullptr)
    return {};
  std::vector<std::uint8_t> output;
  output.reserve(bytes->asArray().size());
  for (const Json &value : bytes->asArray()) {
    const std::int64_t byte = value.asInteger();
    if (byte < 0 || byte > std::numeric_limits<std::uint8_t>::max()) {
      throw std::runtime_error("Native ABI 字节值越界");
    }
    output.push_back(static_cast<std::uint8_t>(byte));
  }
  return output;
}

/// 把字节缓冲转换为 JSON 数组；逐项整数编码与宿主当前 `payload.bytes`
/// 契约一致。
[[nodiscard]] Json bytesJson(std::span<const std::uint8_t> bytes) {
  Json::Array values;
  values.reserve(bytes.size());
  for (const std::uint8_t byte : bytes)
    values.emplace_back(static_cast<std::uint32_t>(byte));
  return values;
}

/// 释放一次 invoke 返回的字符串；releaseContext 是唯一所有者，指针参数仅供 ABI
/// 对称校验。
void releaseOutput(void *context, const std::uint8_t *, std::size_t) noexcept {
  delete static_cast<std::string *>(context);
}

/// 将序列化动作交给宿主；字符串对象持续持有 data 指针直到宿主调用 release。
void publishOutput(std::string output, OutputBuffer &buffer) {
  auto owner = std::make_unique<std::string>(std::move(output));
  buffer.pointer = reinterpret_cast<const std::uint8_t *>(owner->data());
  buffer.length = owner->size();
  buffer.releaseContext = owner.release();
  buffer.release = releaseOutput;
}

/// ABI invoke 桥接函数；所有 C++ 异常在此转换为非零状态，绝不穿过 Rust/C 边界。
int invokeBridge(void *context, ByteSlice request,
                 OutputBuffer *output) noexcept;
/// ABI stop 桥接函数；空上下文被忽略，用户回调异常由 Plugin::stop 吸收。
void stopBridge(void *context) noexcept;
/// ABI destroy 桥接函数；销毁 Plugin 会执行一次 unload 回调并释放全部函数对象。
void destroyBridge(void *context) noexcept;

} // namespace

namespace detail {

/// 允许 ABI 桥接访问 Plugin 私有调度，同时不把任何原始指针接口暴露给插件作者。
struct RuntimeAccess final {
  /// 调用 Plugin 的私有事件调度；异常由外层 ABI 桥统一转换。
  [[nodiscard]] static std::string invoke(Plugin &plugin,
                                          std::string_view request) {
    return plugin.invoke(request);
  }
  /// 向 Plugin 发送幂等停止通知。
  static void stop(Plugin &plugin) noexcept { plugin.stop(); }
};

/// 初始化插件实例并发布 ABI v2 导出表；配置函数只接触强类型 Plugin，不接触 ABI
/// 内存。
int initialize(const void *requestPointer, void *exportsPointer,
               void (*configure)(Plugin &)) noexcept {
  if (requestPointer == nullptr || exportsPointer == nullptr ||
      configure == nullptr)
    return -1;
  try {
    const auto &request = *static_cast<const InitRequest *>(requestPointer);
    if (request.apiVersion != nativeApiVersion)
      return -2;
    InitContext context{
        Json::parse(copyText(request.manifest)),
        Json::parse(copyText(request.configuration)),
    };
    auto plugin = std::make_unique<Plugin>(std::move(context));
    configure(*plugin);
    auto &exports = *static_cast<Exports *>(exportsPointer);
    exports = Exports{
        nativeApiVersion, plugin.get(), invokeBridge, stopBridge, destroyBridge,
    };
    plugin.release();
    return 0;
  } catch (...) {
    return -3;
  }
}

} // namespace detail

namespace {

/// ABI invoke
/// 桥接实现；失败时不发布半成品缓冲，宿主会按插件失败策略处理当前事件。
int invokeBridge(void *context, ByteSlice request,
                 OutputBuffer *output) noexcept {
  if (context == nullptr || output == nullptr || request.pointer == nullptr ||
      request.length == 0)
    return -1;
  try {
    const std::string_view requestText(
        reinterpret_cast<const char *>(request.pointer), request.length);
    publishOutput(detail::RuntimeAccess::invoke(*static_cast<Plugin *>(context),
                                                requestText),
                  *output);
    return 0;
  } catch (...) {
    return -2;
  }
}

/// ABI stop 桥接实现；停止可与连接回调并发发生，幂等控制位位于 Plugin 内部。
void stopBridge(void *context) noexcept {
  if (context != nullptr)
    detail::RuntimeAccess::stop(*static_cast<Plugin *>(context));
}

/// ABI destroy 桥接实现；宿主保证 destroy 后不再发起 invoke，随后才卸载动态库。
void destroyBridge(void *context) noexcept {
  delete static_cast<Plugin *>(context);
}

} // namespace

/// 解析完整运行时调用并提取常用字段；未知可选字段保留在 context/payload
/// 原始树中。
Event Event::parse(std::string_view request) {
  const Json root = Json::parse(request);
  const Json &envelope = requireField(root, "envelope");
  Event event;
  event.moduleId_ = requireField(root, "moduleId").asString();
  event.eventId_ = requireField(envelope, "eventId").asString();
  event.stageName_ = requireField(envelope, "stage").asString();
  event.stage_ = parseStage(event.stageName_);
  event.connectionId_ = optionalString(envelope, "connectionId");
  event.context_ = requireField(envelope, "context");
  event.payload_ = requireField(envelope, "payload");
  event.direction_ = parseDirection(event.context_);
  event.bytes_ = parseBytes(event.payload_);
  event.host_ = optionalString(event.context_, "host");
  if (const Json *port = event.context_.find("port");
      port != nullptr && !port->isNull()) {
    // 端口在线上协议中是无符号字段；兼容解析器产生的正 int64 与作者构造的
    // uint64， 但仍在缩窄前检查 u16 边界，避免截断后连接到错误目标。
    const std::uint64_t value = port->asUnsignedInteger();
    if (value > std::numeric_limits<std::uint16_t>::max())
      throw std::runtime_error("Native ABI 端口越界");
    event.port_ = static_cast<std::uint16_t>(value);
  }
  return event;
}

/// 返回事件所属模块标识。
const std::string &Event::moduleId() const noexcept { return moduleId_; }
/// 返回事件唯一标识。
const std::string &Event::eventId() const noexcept { return eventId_; }
/// 返回识别后的阶段枚举。
Stage Event::stage() const noexcept { return stage_; }
/// 返回宿主原始阶段线名。
const std::string &Event::stageName() const noexcept { return stageName_; }
/// 返回数据传输方向。
Direction Event::direction() const noexcept { return direction_; }
/// 返回不复制的字节视图；视图不得在回调返回后保存。
std::span<const std::uint8_t> Event::bytes() const noexcept { return bytes_; }
/// 返回连接标识或空字符串。
const std::string &Event::connectionId() const noexcept {
  return connectionId_;
}
/// 返回目标主机或空字符串。
const std::string &Event::host() const noexcept { return host_; }
/// 返回目标端口或 0。
std::uint16_t Event::port() const noexcept { return port_; }
/// 返回完整上下文树。
const Json &Event::context() const noexcept { return context_; }
/// 返回完整阶段正文树。
const Json &Event::payload() const noexcept { return payload_; }

/// 构造透明继续决定。
Decision Decision::continueFlow() { return {}; }

/// 构造字节替换决定；复制正文后只替换 bytes，避免丢失 endOfStream、opcode
/// 等阶段元数据。
Decision Decision::modify(const Event &event, std::vector<std::uint8_t> bytes) {
  Json payload = event.payload();
  if (!payload.isObject())
    throw std::invalid_argument("二进制事件正文必须是对象");
  payload["bytes"] = bytesJson(bytes);
  return modifyPayload(std::move(payload));
}

/// 构造通用正文替换决定；根 JSON Patch 能区分“没有 output”和“正文替换为
/// null”，并与其他语言 SDK 共享完全相同的任意 JSON 语义。
Decision Decision::modifyPayload(Json payload) {
  Decision decision;
  decision.action_ = "modify";
  decision.patch_.emplace_back(Json::Object{
      {"op", "replace"}, {"path", ""}, {"value", std::move(payload)}});
  return decision;
}

/// 构造暂存决定；缓冲策略和后续重封包由插件作者维护。
Decision Decision::hold() {
  Decision decision;
  decision.action_ = "hold";
  return decision;
}

/// 构造丢弃决定。
Decision Decision::drop() {
  Decision decision;
  decision.action_ = "drop";
  return decision;
}

/// 构造拒绝决定；原因进入结构化输出，空原因会使诊断失去意义，因此在 SDK
/// 边界拒绝。
Decision Decision::reject(std::string reason) {
  if (isBlank(reason))
    throw std::invalid_argument("拒绝原因不能为空");
  Decision decision;
  decision.action_ = "reject";
  decision.output_ = Json::Object{{"reason", std::move(reason)}};
  return decision;
}

/// 构造连接重定向决定；主机和端口作为独立字段编码，不允许作者拼接易歧义的地址字符串。
Decision Decision::redirect(std::string host, std::uint16_t port) {
  if (isBlank(host) || port == 0)
    throw std::invalid_argument("重定向主机不能为空且端口必须非零");
  Decision decision;
  decision.action_ = "redirect";
  decision.output_ = Json::Object{{"host", std::move(host)}, {"port", port}};
  return decision;
}

/// 构造合成响应决定；null
/// 无法表达可发送的响应，其他结构由具体协议模块自行定义。
Decision Decision::respond(Json response) {
  if (response.isNull())
    throw std::invalid_argument("合成响应不能为空");
  Decision decision;
  decision.action_ = "respond";
  decision.output_ = std::move(response);
  return decision;
}

/// 构造关闭连接决定。
Decision Decision::close() {
  Decision decision;
  decision.action_ = "close";
  return decision;
}

/// 构造结构化标注决定；名称和值被分别编码，不存在字符串拼接注入。
Decision Decision::annotate(std::string name, Json value) {
  Decision decision;
  decision.action_ = "annotate";
  decision.annotations_.emplace_back(
      Json::Object{{"name", std::move(name)}, {"value", std::move(value)}});
  return decision;
}

/// 保存初始化上下文；配置函数返回后清单和配置仍可由运行期回调读取。
Plugin::Plugin(InitContext context) : context_(std::move(context)) {}

/// 销毁实例并执行一次卸载通知；作者异常被吸收以保证动态库可以继续卸载。
Plugin::~Plugin() {
  stop();
  if (unloadHandler_) {
    try {
      unloadHandler_();
    } catch (...) {
      reportLifecycleFailure("C++ 插件卸载回调抛出异常");
    }
  }
}

/// 注册通用阶段回调；初始化完成后宿主只读回调表，因此调用方应仅在配置函数内注册。
void Plugin::on(Stage stage, Handler handler) {
  if (!handler)
    throw std::invalid_argument("阶段回调不能为空");
  handlers_.insert_or_assign(stage, std::move(handler));
}

/// 注册模块专属回调；同一插件含多个协议模块时可保持独立状态机。
void Plugin::onModule(std::string moduleId, Stage stage, Handler handler) {
  if (moduleId.empty() || !handler)
    throw std::invalid_argument("模块标识和回调不能为空");
  moduleHandlers_.insert_or_assign({std::move(moduleId), stage},
                                   std::move(handler));
}

/// 注册 TCP 块回调；该便捷函数不改变回调的同步和并发语义。
void Plugin::onTcp(Handler handler) { on(Stage::TcpChunk, std::move(handler)); }
/// 注册 UDP 报文回调；每次回调的 bytes 都对应一个完整数据报。
void Plugin::onUdp(Handler handler) {
  on(Stage::UdpDatagram, std::move(handler));
}

/// 注册停止通知；重复注册以最后一个为准，宿主停止只触发一次。
void Plugin::onStop(LifecycleHandler handler) {
  stopHandler_ = std::move(handler);
}
/// 注册卸载通知；回调在 stop 之后、插件对象释放前运行一次。
void Plugin::onUnload(LifecycleHandler handler) {
  unloadHandler_ = std::move(handler);
}
/// 返回实例初始化上下文，不复制清单和配置树。
const InitContext &Plugin::init() const noexcept { return context_; }

/// 解析事件、选择模块或阶段回调并生成宿主动作；没有处理器时保持透明继续。
std::string Plugin::invoke(std::string_view request) {
  // 读锁覆盖解析与完整作者回调，stop
  // 取得写锁后即可确定没有处理器继续使用作者资源。
  std::shared_lock invocationLock(invocationGate_);
  if (stopped_.load(std::memory_order_acquire))
    throw std::runtime_error("插件实例已停止");
  const Event event = Event::parse(request);
  Decision decision = Decision::continueFlow();
  const auto moduleIterator =
      moduleHandlers_.find({event.moduleId(), event.stage()});
  if (moduleIterator != moduleHandlers_.end()) {
    decision = moduleIterator->second(event);
  } else if (const auto iterator = handlers_.find(event.stage());
             iterator != handlers_.end()) {
    decision = iterator->second(event);
  }
  return Json(Json::Object{
                  {"eventId", event.eventId()},
                  {"action", decision.action_},
                  {"patch",
                   Json::Array(decision.patch_.begin(), decision.patch_.end())},
                  {"annotations", Json::Array(decision.annotations_.begin(),
                                              decision.annotations_.end())},
                  {"output", decision.output_.has_value() ? *decision.output_
                                                          : Json(nullptr)},
              })
      .dump();
}

/// 发送一次停止通知；原子交换保证宿主并发 stop/destroy
/// 不会重复执行作者清理逻辑。
void Plugin::stop() noexcept {
  // 写锁先等待全部在途 invoke 退出，再改变停止态和执行作者清理，语义与 Rust/Go
  // SDK 一致。
  std::unique_lock invocationLock(invocationGate_);
  if (stopped_.exchange(true, std::memory_order_acq_rel))
    return;
  if (stopHandler_) {
    try {
      stopHandler_();
    } catch (...) {
      reportLifecycleFailure("C++ 插件停止回调抛出异常");
    }
  }
}

} // namespace trafficMod
