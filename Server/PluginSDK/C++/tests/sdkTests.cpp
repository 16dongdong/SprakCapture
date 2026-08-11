#include "trafficMod/sdk.hpp"

#include <atomic>
#include <chrono>
#include <condition_variable>
#include <cstddef>
#include <cstdint>
#include <cstdlib>
#include <functional>
#include <iostream>
#include <limits>
#include <mutex>
#include <string>
#include <thread>

using namespace trafficMod;

namespace {

struct ByteSlice final {
  const std::uint8_t *pointer;
  std::size_t length;
};

struct InitRequest final {
  std::uint32_t apiVersion;
  ByteSlice manifest;
  ByteSlice configuration;
};

struct OutputBuffer final {
  const std::uint8_t *pointer;
  std::size_t length;
  void *releaseContext;
  void (*release)(void *, const std::uint8_t *, std::size_t);
};

struct Exports final {
  std::uint32_t apiVersion;
  void *pluginContext;
  int (*invoke)(void *, ByteSlice, OutputBuffer *);
  void (*stop)(void *);
  void (*destroy)(void *);
};

std::atomic_int stopCount = 0;
std::atomic_int unloadCount = 0;
std::mutex blockingMutex;
std::condition_variable blockingCondition;
bool blockingHandlerStarted = false;
bool allowBlockingHandlerFinish = false;

/// 注册测试回调；测试通过完整 ABI 入口验证作者不需要接触指针和 JSON 输出。
void configure(Plugin &plugin) {
  plugin.onTcp([](const Event &event) {
    if (event.host() != "example.test" || event.port() != 443 ||
        event.direction() != Direction::ClientToServer) {
      return Decision::close();
    }
    if (!event.bytes().empty() && event.bytes().front() == 7) {
      std::unique_lock lock(blockingMutex);
      blockingHandlerStarted = true;
      blockingCondition.notify_all();
      blockingCondition.wait(lock, [] { return allowBlockingHandlerFinish; });
    }
    std::vector<std::uint8_t> bytes(event.bytes().begin(), event.bytes().end());
    bytes.push_back(9);
    return Decision::modify(event, std::move(bytes));
  });
  plugin.onUdp([](const Event &) { return Decision::drop(); });
  plugin.on(Stage::ProtocolClassified, [](const Event &) {
    return Decision::modifyPayload(Json::Object{{"protocol", "custom-binary"}});
  });
  plugin.on(Stage::TargetResolving, [](const Event &) {
    return Decision::modifyPayload(Json::Array{1, "two"});
  });
  plugin.on(Stage::CertificateSelecting,
            [](const Event &) { return Decision::modifyPayload(7); });
  plugin.on(Stage::ResponseHeaders,
            [](const Event &) { return Decision::modifyPayload(nullptr); });
  plugin.on(Stage::BeforeConnect, [](const Event &) {
    return Decision::redirect("upstream.example", 8443);
  });
  plugin.on(Stage::RequestHeaders,
            [](const Event &) { return Decision::reject("请求被插件拒绝"); });
  plugin.on(Stage::RequestComplete, [](const Event &) {
    return Decision::respond(Json::Object{{"statusCode", 200}, {"body", "ok"}});
  });
  plugin.onStop([] { ++stopCount; });
  plugin.onUnload([] { ++unloadCount; });
}

/// 把字符串借用为 ABI 切片；调用者保证字符串在调用返回前保持有效。
[[nodiscard]] ByteSlice slice(const std::string &value) {
  return {reinterpret_cast<const std::uint8_t *>(value.data()), value.size()};
}

/// 校验条件并输出中文错误；失败立即结束独立测试进程。
void require(bool condition, const char *message) {
  if (!condition) {
    std::cerr << message << '\n';
    std::exit(1);
  }
}

/// 返回 modify 动作根替换值；同时验证 SDK 输出的是跨语言统一 JSON Patch，
/// 而不是无法区分缺省与 null 的 output 字段。
[[nodiscard]] const Json &rootReplacement(const Json &action) {
  const auto &patch = action.find("patch")->asArray();
  require(patch.size() == 1 &&
              patch.front().find("op")->asString() == "replace" &&
              patch.front().find("path")->asString().empty(),
          "modify 动作没有生成根替换 JSON Patch");
  const Json *value = patch.front().find("value");
  require(value != nullptr, "modify 根替换缺少 value");
  return *value;
}

/// 校验参数错误会被 SDK 当场拒绝；没有抛出异常表示构造器接受了含糊动作。
void requireThrows(const std::function<void()> &operation,
                   const char *message) {
  try {
    operation();
  } catch (const std::invalid_argument &) {
    return;
  }
  require(false, message);
}

/// 构造最小但完整的 Native 调用帧；测试使用 JSON
/// 树避免自身依赖字符串拼接正确性。
[[nodiscard]] std::string makeInvocation(std::string eventId, std::string stage,
                                         Json context, Json payload) {
  return Json(Json::Object{
                  {"pluginId", "test.mod"},
                  {"moduleId", "transform"},
                  {"moduleKind", "streamTransformer"},
                  {"envelope", Json::Object{{"eventId", std::move(eventId)},
                                            {"stage", std::move(stage)},
                                            {"connectionId", "connection-1"},
                                            {"context", std::move(context)},
                                            {"payload", std::move(payload)}}},
              })
      .dump();
}

/// 通过真实 Native ABI 调用插件并复制动作 JSON；返回前按宿主约定释放 SDK 缓冲。
[[nodiscard]] Json invoke(Exports &exports, const std::string &request) {
  OutputBuffer output{};
  require(exports.invoke(exports.pluginContext, slice(request), &output) == 0,
          "插件调用失败");
  require(output.pointer != nullptr && output.length > 0 &&
              output.release != nullptr,
          "插件输出所有权无效");
  const std::string text(reinterpret_cast<const char *>(output.pointer),
                         output.length);
  output.release(output.releaseContext, output.pointer, output.length);
  return Json::parse(text);
}

} // namespace

TRAFFIC_MOD_PLUGIN(configure)

/// 覆盖 JSON、事件解析、TCP/UDP 动作与生命周期；任一断言失败返回非零退出码。
int main() {
  const Json unicode =
      Json::parse(R"({"name":"\u4e2d\u6587","number":12,"array":[true,null]})");
  require(unicode.find("name")->asString() == "中文", "Unicode 解析失败");
  require(Json::parse(unicode.dump()).find("number")->asInteger() == 12,
          "JSON 往返失败");
  constexpr std::uint64_t firstUnsignedInteger =
      static_cast<std::uint64_t>(std::numeric_limits<std::int64_t>::max()) + 1;
  constexpr std::uint64_t maximumUnsignedInteger =
      std::numeric_limits<std::uint64_t>::max();
  const Json unsignedBoundaries = Json::parse(
      R"({"first":9223372036854775808,"maximum":18446744073709551615})");
  require(unsignedBoundaries.find("first")->asUnsignedInteger() ==
                  firstUnsignedInteger &&
              unsignedBoundaries.find("maximum")->asUnsignedInteger() ==
                  maximumUnsignedInteger,
          "u64 边界解析发生精度损失");
  require(unsignedBoundaries.dump() ==
              R"({"first":9223372036854775808,"maximum":18446744073709551615})",
          "u64 边界序列化发生精度损失");
  require(Json(maximumUnsignedInteger).asUnsignedInteger() ==
              maximumUnsignedInteger,
          "u64 构造器发生精度损失");

  const std::string manifest = R"({"id":"test.mod"})";
  const std::string configuration = R"({"enabled":true})";
  const InitRequest request{2, slice(manifest), slice(configuration)};
  Exports exports{};
  require(capture_extension_init(&request, &exports) == 0,
          "Native ABI 初始化失败");
  require(exports.apiVersion == 2 && exports.pluginContext != nullptr,
          "Native ABI 导出表无效");

  const std::string tcpRequest =
      R"({"pluginId":"test.mod","moduleId":"transform","moduleKind":"streamTransformer","envelope":{"apiVersion":"2.0.0","eventId":"event-1","stage":"tcpChunk","serviceGeneration":18446744073709551615,"recordingGeneration":18446744073709551615,"pluginInstanceId":"instance","connectionId":"connection-1","transactionId":null,"deadlineUnixMs":18446744073709551615,"context":{"direction":"up","host":"example.test","port":443,"interceptionMode":"intercept"},"payload":{"bytes":[1,2,3],"endOfStream":false,"sequence":18446744073709551615}}})";
  const Json tcpAction = invoke(exports, tcpRequest);
  require(tcpAction.find("eventId")->asString() == "event-1",
          "SDK 未自动回填事件标识");
  require(tcpAction.find("action")->asString() == "modify", "TCP 修改动作错误");
  const Json &tcpPayload = rootReplacement(tcpAction);
  const auto &bytes = tcpPayload.find("bytes")->asArray();
  require(bytes.size() == 4 && bytes.back().asInteger() == 9,
          "TCP 修改字节错误");
  require(!tcpPayload.find("endOfStream")->asBool(),
          "TCP 修改丢失正文其他字段");
  require(tcpPayload.find("sequence")->asUnsignedInteger() ==
              maximumUnsignedInteger,
          "真实 ABI 调用未无损保留 u64 正文字段");

  const std::string udpRequest =
      R"({"pluginId":"test.mod","moduleId":"transform","moduleKind":"streamTransformer","envelope":{"apiVersion":"2.0.0","eventId":"event-2","stage":"udpDatagram","serviceGeneration":1,"recordingGeneration":1,"pluginInstanceId":"instance","connectionId":"connection-1","transactionId":null,"deadlineUnixMs":1,"context":{"direction":"down","host":"example.test","port":53,"interceptionMode":"intercept"},"payload":{"bytes":[8],"endOfStream":false}}})";
  require(invoke(exports, udpRequest).find("action")->asString() == "drop",
          "UDP 丢弃动作错误");

  const Json commonContext = Json::Object{
      {"direction", "up"}, {"host", "example.test"}, {"port", 443}};
  const Json payload = Json::Object{};
  const Json payloadAction =
      invoke(exports, makeInvocation("event-3", "protocolClassified",
                                     commonContext, payload));
  require(payloadAction.find("action")->asString() == "modify" &&
              rootReplacement(payloadAction).find("protocol")->asString() ==
                  "custom-binary",
          "通用正文修改动作错误");

  const Json arrayAction =
      invoke(exports, makeInvocation("event-array", "targetResolving",
                                     commonContext, payload));
  const Json &arrayPayload = rootReplacement(arrayAction);
  require(arrayPayload.isArray() && arrayPayload.at(0).asInteger() == 1 &&
              arrayPayload.at(1).asString() == "two",
          "数组正文修改没有保留 JSON 类型");

  const Json scalarAction =
      invoke(exports, makeInvocation("event-scalar", "certificateSelecting",
                                     commonContext, payload));
  require(rootReplacement(scalarAction).asInteger() == 7,
          "标量正文修改没有保留 JSON 类型");

  const Json nullAction =
      invoke(exports, makeInvocation("event-null", "responseHeaders",
                                     commonContext, payload));
  require(rootReplacement(nullAction).isNull(),
          "null 正文修改被误解为缺省 output");

  const Json redirectAction =
      invoke(exports, makeInvocation("event-4", "beforeConnect", commonContext,
                                     payload));
  require(redirectAction.find("action")->asString() == "redirect" &&
              redirectAction.find("output")->find("host")->asString() ==
                  "upstream.example" &&
              redirectAction.find("output")->find("port")->asInteger() == 8443,
          "重定向动作错误");

  const Json rejectAction =
      invoke(exports, makeInvocation("event-5", "requestHeaders", commonContext,
                                     payload));
  require(rejectAction.find("action")->asString() == "reject" &&
              rejectAction.find("output")->find("reason")->asString() ==
                  "请求被插件拒绝",
          "拒绝动作错误");

  const Json responseAction =
      invoke(exports, makeInvocation("event-6", "requestComplete",
                                     commonContext, payload));
  require(responseAction.find("action")->asString() == "respond" &&
              responseAction.find("output")->find("statusCode")->asInteger() ==
                  200,
          "合成响应动作错误");

  requireThrows([] { (void)Decision::reject(" \t\r\n"); },
                "拒绝动作接受了空白原因");
  requireThrows([] { (void)Decision::redirect(" \t", 443); },
                "重定向动作接受了无效目标");
  requireThrows([] { (void)Decision::respond(nullptr); },
                "合成响应动作接受了空响应");

  std::string blockingRequest = tcpRequest;
  blockingRequest.replace(blockingRequest.find("[1,2,3]"), 7, "[7]");
  std::thread invocationThread([&] { (void)invoke(exports, blockingRequest); });
  {
    std::unique_lock lock(blockingMutex);
    blockingCondition.wait(lock, [] { return blockingHandlerStarted; });
  }
  std::thread stopThread([&] { exports.stop(exports.pluginContext); });
  std::this_thread::sleep_for(std::chrono::milliseconds(20));
  require(stopCount.load() == 0, "stop 未等待在途 invoke 完成");
  {
    std::lock_guard lock(blockingMutex);
    allowBlockingHandlerFinish = true;
  }
  blockingCondition.notify_all();
  invocationThread.join();
  stopThread.join();
  require(stopCount.load() == 1, "stop 未在 invoke 完成后执行");

  exports.stop(exports.pluginContext);
  require(stopCount.load() == 1, "停止回调不是幂等的");
  exports.destroy(exports.pluginContext);
  require(unloadCount.load() == 1, "卸载回调执行次数错误");

  std::cout << "C++ Native SDK 全部测试通过\n";
  return 0;
}
