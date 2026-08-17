#pragma once

#include <sys/socket.h>

#include <atomic>
#include <cstdint>
#include <functional>
#include <string>

#include "socks5/socks_protocol.h"

namespace routesocks::net {

/**
 * 描述一条仍保留原始五元组的 IPv4 UDP 数据报。
 * source 用于和 NAT REDIRECT 后的回环数据报配对，target 是规则执行所需的真实目标。
 */
struct QueuedUdpPacket {
  sockaddr_storage source{};
  socklen_t source_length = 0;
  socks5::TargetAddress target;
};

/**
 * 直接使用 NETLINK_NETFILTER 管理一个 NFQUEUE。
 * Root 进程在 iptables 规则发布前绑定队列；回调必须先保存原目标，再由本类发送 ACCEPT，
 * 从而保证回环监听器不可能早于目标映射收到数据报。
 */
class NetfilterQueue {
public:
  using PacketCallback = std::function<bool(const QueuedUdpPacket &)>;

  /** 创建尚未占用内核队列的对象；queue_number 必须与 Root iptables 事务一致。 */
  explicit NetfilterQueue(uint16_t queue_number);
  /** 关闭 netlink 描述符并释放内核队列；析构不会抛出异常。 */
  ~NetfilterQueue();
  NetfilterQueue(const NetfilterQueue &) = delete;
  NetfilterQueue &operator=(const NetfilterQueue &) = delete;

  /**
   * 绑定队列并启用完整 IPv4 包复制；队列已占用或协议配置失败时返回 false 和中文错误。
   */
  bool Open(std::string *error);

  /**
   * 阻塞处理队列直到 Close；回调成功返回 ACCEPT，容量或解析失败返回 DROP，绝不放行无目标映射的数据报。
   */
  void Run(const PacketCallback &callback);

  /** 幂等关闭队列并唤醒 Run；可与监听线程并发调用。 */
  void Close() noexcept;

private:
  /** 发送带 ACK 的 NFQUEUE 配置消息；内核拒绝时返回 false。 */
  bool Configure(uint8_t command, std::string *error);
  /** 设置包复制范围和队列长度；任一 ACK 失败时返回 false。 */
  bool ConfigureCopy(std::string *error);
  /** 解析单条内核消息并提交 verdict；非队列包消息保持忽略。 */
  void ProcessMessage(const void *bytes, std::size_t length,
                      const PacketCallback &callback);
  /** 对 packet_id 发送 ACCEPT 或 DROP；发送失败会结束当前 Run 循环。 */
  bool SendVerdict(uint32_t packet_id, bool accept);

  uint16_t queue_number_;
  std::atomic<int> descriptor_{-1};
  std::atomic<uint32_t> sequence_{1};
};

} // namespace routesocks::net
