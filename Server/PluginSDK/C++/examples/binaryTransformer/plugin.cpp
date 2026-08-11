#include "trafficMod/sdk.hpp"

#include <cstdint>
#include <map>
#include <memory>
#include <mutex>
#include <string>
#include <utility>
#include <vector>

using namespace trafficMod;

namespace {

/// 按连接和方向隔离半包，避免双向 TCP 数据共享一个解析游标而破坏协议帧边界。
class FrameTransformer final {
public:
  /// 接收任意 TCP 分块并输出零到多帧；半包保留在插件状态中并返回 hold。
  Decision process(const Event &event) {
    std::lock_guard lock(mutex_);
    auto &pending = buffers_[{event.connectionId(), event.direction()}];
    pending.insert(pending.end(), event.bytes().begin(), event.bytes().end());
    std::vector<std::uint8_t> output;
    std::size_t consumed = 0;
    while (pending.size() - consumed >= headerSize) {
      const std::size_t payloadSize =
          (static_cast<std::size_t>(pending[consumed]) << 8) |
          pending[consumed + 1];
      if (pending.size() - consumed < headerSize + payloadSize)
        break;
      transformFrame(pending, consumed, payloadSize, output);
      consumed += headerSize + payloadSize;
    }
    pending.erase(pending.begin(),
                  pending.begin() + static_cast<std::ptrdiff_t>(consumed));
    return output.empty() ? Decision::hold()
                          : Decision::modify(event, std::move(output));
  }

  /// 删除连接关闭后的双向缓冲，防止长时间运行时保留已经失效的连接状态。
  Decision close(const Event &event) {
    std::lock_guard lock(mutex_);
    buffers_.erase({event.connectionId(), Direction::ClientToServer});
    buffers_.erase({event.connectionId(), Direction::ServerToClient});
    return Decision::continueFlow();
  }

private:
  static constexpr std::size_t headerSize = 2;
  static constexpr std::uint8_t encryptionKey = 0xAA;

  /// 解密一帧、修改业务字符串并重新加密封包；长度字段根据修改后正文重新计算。
  static void transformFrame(const std::vector<std::uint8_t> &pending,
                             std::size_t frameOffset, std::size_t payloadSize,
                             std::vector<std::uint8_t> &output) {
    std::string plainText;
    plainText.reserve(payloadSize);
    for (std::size_t index = 0; index < payloadSize; ++index) {
      plainText.push_back(static_cast<char>(
          pending[frameOffset + headerSize + index] ^ encryptionKey));
    }
    constexpr std::string_view source = "blocked";
    constexpr std::string_view replacement = "allowed";
    if (const std::size_t offset = plainText.find(source);
        offset != std::string::npos) {
      plainText.replace(offset, source.size(), replacement);
    }
    const std::size_t newSize = plainText.size();
    output.push_back(static_cast<std::uint8_t>((newSize >> 8) & 0xFF));
    output.push_back(static_cast<std::uint8_t>(newSize & 0xFF));
    for (const unsigned char character : plainText)
      output.push_back(character ^ encryptionKey);
  }

  std::mutex mutex_;
  std::map<std::pair<std::string, Direction>, std::vector<std::uint8_t>>
      buffers_;
};

/// 以普通函数注册回调；SDK
/// 自动完成初始化、JSON、动作序列化、内存释放和固定符号导出。
void configure(Plugin &plugin) {
  auto transformer = std::make_shared<FrameTransformer>();
  plugin.onTcp([transformer](const Event &event) {
    return transformer->process(event);
  });
  plugin.on(Stage::ConnectionClosing, [transformer](const Event &event) {
    return transformer->close(event);
  });
  plugin.onUdp([](const Event &event) {
    if (event.bytes().empty())
      return Decision::continueFlow();
    std::vector<std::uint8_t> bytes(event.bytes().begin(), event.bytes().end());
    bytes.front() ^= 0x01;
    return Decision::modify(event, std::move(bytes));
  });
}

} // namespace

TRAFFIC_MOD_PLUGIN(configure)
