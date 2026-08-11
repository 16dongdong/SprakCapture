#pragma once

#include <atomic>
#include <cstddef>
#include <cstdint>
#include <functional>
#include <map>
#include <optional>
#include <shared_mutex>
#include <span>
#include <string>
#include <string_view>
#include <variant>
#include <vector>

namespace trafficMod {

namespace detail {
struct RuntimeAccess;
}

/// 保存协议 JSON 的强类型树，作者可直接读写对象和数组而不需要拼接 JSON 文本。
class Json final {
public:
  using Array = std::vector<Json>;
  using Object = std::map<std::string, Json, std::less<>>;
  using Value = std::variant<std::nullptr_t, bool, std::int64_t, std::uint64_t,
                             double, std::string, Array, Object>;

  /// 构造空值或对应标量、数组、对象；所有重载都保留输入值的精确 JSON 类型。
  Json() noexcept;
  Json(std::nullptr_t) noexcept;
  Json(bool value) noexcept;
  Json(std::int32_t value) noexcept;
  Json(std::uint32_t value) noexcept;
  Json(std::int64_t value) noexcept;
  Json(std::uint64_t value) noexcept;
  Json(double value) noexcept;
  Json(const char *value);
  Json(std::string value);
  Json(Array value);
  Json(Object value);

  /// 解析 UTF-8 JSON；输入不完整、类型非法或尾部存在额外字符时抛出
  /// std::runtime_error。
  [[nodiscard]] static Json parse(std::string_view text);

  /// 序列化为稳定 UTF-8 JSON；对象键按字典序输出，非法浮点值会抛出
  /// std::runtime_error。
  [[nodiscard]] std::string dump() const;

  /// 返回当前节点是否为空值；该查询不改变节点。
  [[nodiscard]] bool isNull() const noexcept;
  /// 返回当前节点是否为对象；该查询不改变节点。
  [[nodiscard]] bool isObject() const noexcept;
  /// 返回当前节点是否为数组；该查询不改变节点。
  [[nodiscard]] bool isArray() const noexcept;
  /// 返回对象字段；字段不存在或当前节点不是对象时返回空指针。
  [[nodiscard]] const Json *find(std::string_view key) const noexcept;
  /// 返回对象字段并在缺失时创建空值；当前节点不是对象时抛出
  /// std::runtime_error。
  Json &operator[](std::string key);
  /// 返回数组元素；索引越界或当前节点不是数组时抛出 std::runtime_error。
  [[nodiscard]] const Json &at(std::size_t index) const;
  /// 返回字符串值；类型不匹配时抛出 std::runtime_error。
  [[nodiscard]] const std::string &asString() const;
  /// 返回布尔值；类型不匹配时抛出 std::runtime_error。
  [[nodiscard]] bool asBool() const;
  /// 返回有符号整数值；无符号节点超出 int64 时抛出 std::runtime_error。
  [[nodiscard]] std::int64_t asInteger() const;
  /// 返回无符号整数值；有符号节点为负数或类型不匹配时抛出 std::runtime_error。
  [[nodiscard]] std::uint64_t asUnsignedInteger() const;
  /// 返回数组值；类型不匹配时抛出 std::runtime_error。
  [[nodiscard]] const Array &asArray() const;
  /// 返回对象值；类型不匹配时抛出 std::runtime_error。
  [[nodiscard]] const Object &asObject() const;
  /// 返回底层值供高级作者访问；引用仅在当前 Json 未被修改期间有效。
  [[nodiscard]] const Value &value() const noexcept;

private:
  Value value_;
};

/// 列出 Native ABI v2 当前公开的全部阶段；未知阶段仍可通过 Event::stageName
/// 读取。
enum class Stage {
  Unknown,
  ServiceStarting,
  ServiceStarted,
  ConfigurationChanged,
  ServiceStopping,
  ConnectionAccepted,
  Socks5Authentication,
  ProtocolClassified,
  TargetResolving,
  BeforeConnect,
  Connected,
  ConnectionClosing,
  ClientHelloObserved,
  CertificateSelecting,
  TlsEstablished,
  TlsFailed,
  RequestHeaders,
  RequestBodyChunk,
  RequestComplete,
  BeforeUpstream,
  ResponseHeaders,
  ResponseBodyChunk,
  ResponseComplete,
  WebSocketOpening,
  WebSocketFrame,
  WebSocketClosing,
  TcpChunk,
  UdpDatagram,
  DnsMessage,
  BeforeRecord,
  TransactionUpdated,
  TransactionCompleted,
  RecordingCleared,
  InspectorDataRequested,
  CommandInvoked,
  ContextActionInvoked,
};

/// 描述当前线块相对代理的传输方向；非字节阶段使用 Unknown。
enum class Direction { Unknown, ClientToServer, ServerToClient };

/// 提供一次宿主回调的稳定只读视图；SDK 已完成
/// JSON、字节数组和常用上下文字段解析。
class Event final {
public:
  /// 从 Native ABI 调用 JSON 构造事件；字段缺失或格式错误时抛出
  /// std::runtime_error。
  [[nodiscard]] static Event parse(std::string_view request);

  /// 返回调用所针对的插件模块标识。
  [[nodiscard]] const std::string &moduleId() const noexcept;
  /// 返回事件唯一标识；SDK 生成动作时自动回填该值。
  [[nodiscard]] const std::string &eventId() const noexcept;
  /// 返回已识别阶段枚举；新宿主阶段在旧 SDK 中为 Unknown。
  [[nodiscard]] Stage stage() const noexcept;
  /// 返回宿主原始阶段名，便于插件前向兼容新增阶段。
  [[nodiscard]] const std::string &stageName() const noexcept;
  /// 返回客户端到服务端或服务端到客户端方向。
  [[nodiscard]] Direction direction() const noexcept;
  /// 返回当前 TCP 块或 UDP 报文；非字节事件返回空视图。
  [[nodiscard]] std::span<const std::uint8_t> bytes() const noexcept;
  /// 返回连接标识；不属于连接的阶段返回空字符串。
  [[nodiscard]] const std::string &connectionId() const noexcept;
  /// 返回目标主机；字段不可用时返回空字符串。
  [[nodiscard]] const std::string &host() const noexcept;
  /// 返回目标端口；字段不可用时返回 0。
  [[nodiscard]] std::uint16_t port() const noexcept;
  /// 返回完整上下文对象，供通用插件读取未来新增字段。
  [[nodiscard]] const Json &context() const noexcept;
  /// 返回完整阶段正文，供协议解码器读取自定义字段。
  [[nodiscard]] const Json &payload() const noexcept;

private:
  std::string moduleId_;
  std::string eventId_;
  std::string stageName_;
  Stage stage_ = Stage::Unknown;
  Direction direction_ = Direction::Unknown;
  std::vector<std::uint8_t> bytes_;
  std::string connectionId_;
  std::string host_;
  std::uint16_t port_ = 0;
  Json context_;
  Json payload_;
};

/// 表示插件作者对当前事件的决定；公开构造器隐藏宿主动作 JSON 和内存所有权协议。
class Decision final {
public:
  /// 原样继续处理当前事件。
  [[nodiscard]] static Decision continueFlow();
  /// 用新字节替换当前 TCP 块或 UDP 报文，并保留事件正文中的其他字段。
  [[nodiscard]] static Decision modify(const Event &event,
                                       std::vector<std::uint8_t> bytes);
  /// 用任意 JSON 原子替换当前阶段正文；null、标量、数组和对象均保持原类型。
  [[nodiscard]] static Decision modifyPayload(Json payload);
  /// 暂存当前流块并等待后续数据；只应在宿主允许 hold 的流阶段使用。
  [[nodiscard]] static Decision hold();
  /// 丢弃当前块、报文或录制项。
  [[nodiscard]] static Decision drop();
  /// 拒绝当前操作并携带非空原因；空原因会抛出 std::invalid_argument。
  [[nodiscard]] static Decision reject(std::string reason);
  /// 把连接重定向到非空主机和非零端口；参数无效时抛出 std::invalid_argument。
  [[nodiscard]] static Decision redirect(std::string host, std::uint16_t port);
  /// 返回结构化合成响应；空响应会抛出 std::invalid_argument。
  [[nodiscard]] static Decision respond(Json response);
  /// 关闭当前连接或流。
  [[nodiscard]] static Decision close();
  /// 添加结构化标注且不改变线上字节。
  [[nodiscard]] static Decision annotate(std::string name,
                                         Json value = Json(true));

private:
  friend class Plugin;
  std::string action_ = "continue";
  std::vector<Json> patch_;
  std::optional<Json> output_;
  std::vector<Json> annotations_;
};

/// 封装清单与用户配置；它们在插件实例整个生命周期内保持有效。
struct InitContext final {
  Json manifest;
  Json configuration;
};

/// 注册普通 C++ 回调并负责所有 Native ABI v2
/// 适配；同一实例可能被多个连接线程并发调用。
class Plugin final {
public:
  using Handler = std::function<Decision(const Event &)>;
  using LifecycleHandler = std::function<void()>;

  explicit Plugin(InitContext context);
  Plugin(const Plugin &) = delete;
  Plugin &operator=(const Plugin &) = delete;
  Plugin(Plugin &&) = delete;
  Plugin &operator=(Plugin &&) = delete;
  ~Plugin();

  /// 注册任意阶段回调；同一阶段后注册的回调替换先前回调。
  void on(Stage stage, Handler handler);
  /// 注册特定模块的阶段回调；它优先于通用阶段回调。
  void onModule(std::string moduleId, Stage stage, Handler handler);
  /// 注册 TCP 块回调，等价于 on(Stage::TcpChunk, handler)。
  void onTcp(Handler handler);
  /// 注册 UDP 报文回调，等价于 on(Stage::UdpDatagram, handler)。
  void onUdp(Handler handler);
  /// 注册宿主停止通知；SDK
  /// 等待在途事件回调结束后执行，作者应在其中结束自建线程和 I/O。
  void onStop(LifecycleHandler handler);
  /// 注册实例销毁通知；回调在动态库卸载前执行一次。
  void onUnload(LifecycleHandler handler);
  /// 返回初始化清单与配置；引用在 Plugin 生命周期内有效。
  [[nodiscard]] const InitContext &init() const noexcept;

private:
  friend struct detail::RuntimeAccess;

  /// 调度已解析事件并序列化决定；回调异常向 ABI 返回失败而不跨越 C 边界。
  [[nodiscard]] std::string invoke(std::string_view request);
  /// 等待全部在途调用结束后发送停止通知，并保证最多执行一次。
  void stop() noexcept;

  InitContext context_;
  std::map<Stage, Handler> handlers_;
  std::map<std::pair<std::string, Stage>, Handler> moduleHandlers_;
  LifecycleHandler stopHandler_;
  LifecycleHandler unloadHandler_;
  std::shared_mutex invocationGate_;
  std::atomic_bool stopped_ = false;
};

namespace detail {

/// Native ABI v2 的内部初始化桥；公开是为了让导出宏生成单一平台无关入口。
int initialize(const void *request, void *exports,
               void (*configure)(Plugin &)) noexcept;

} // namespace detail

} // namespace trafficMod

#if defined(_WIN32)
#define TRAFFIC_MOD_EXPORT extern "C" __declspec(dllexport)
#else
#define TRAFFIC_MOD_EXPORT extern "C" __attribute__((visibility("default")))
#endif

/// 导出 Native ABI v2 固定入口；插件作者只需实现一个接收 Plugin&
/// 的普通配置函数。
#define TRAFFIC_MOD_PLUGIN(configureFunction)                                  \
  TRAFFIC_MOD_EXPORT int capture_extension_init(const void *request,           \
                                                void *exports) {               \
    return ::trafficMod::detail::initialize(request, exports,                  \
                                            configureFunction);                \
  }
