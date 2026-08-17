#pragma once

#include "engine/proxy_runtime.h"

namespace routesocks::runtime {

/** UDP 工作池中的一个数据报任务，目标与载荷在入队时转移所有权。 */
struct ProxyRuntime::UdpPacketTask {
  socks5::TargetAddress target;
  std::vector<uint8_t> payload;
};

/** 保存 UDP 响应回写端点；透明入口回写原始载荷，SOCKS 入口回写 RFC1928 帧。 */
struct ProxyRuntime::UdpResponseRoute {
  sockaddr_storage peer{};
  socklen_t peer_length = 0;
  bool transparent = false;
  int response_descriptor = -1;
};

/**
 * 一个 UDP peer 的完整转发状态，直连 socket、DNS 在途问题与上游 ASSOCIATE
 * 共同生灭。 operation_mutex 串行协议状态；udp_reactor_mutex_ 只在关闭会使 poll
 * 快照失效时参与。
 */
struct ProxyRuntime::UdpPeerSession {
  struct DirectChannel {
    int descriptor = -1;
    socks5::TargetAddress response_target;
    int64_t last_used_millis = 0;
  };

  struct PendingDns {
    std::vector<uint8_t> query;
    socks5::TargetAddress response_target;
    int64_t expiry_millis = 0;
    std::size_t server_index = 0;
  };

  UdpPeerSession(RuntimeConfig configuration,
                 net::SocketObserver *socket_observer, bool selected,
                 UdpResponseRoute response);

  RuntimeConfig config;
  net::SocketObserver *observer;
  bool selected_application;
  UdpResponseRoute response_route;
  std::unordered_map<std::string, DirectChannel> direct_channels;
  std::unordered_map<std::string,
                     std::unordered_map<uint16_t, std::deque<PendingDns>>>
      pending_dns;
  socks5::UpstreamUdpSession upstream;
  bool upstream_open = false;
  std::atomic<bool> closing{false};
  std::mutex operation_mutex;
  std::mutex pending_mutex;
  std::deque<UdpPacketTask> pending_packets;
  bool worker_scheduled = false;
  std::atomic<bool> active_counted{true};
  std::atomic<int64_t> last_used_millis;
};

/** 一条已认证 RFC1928 UDP ASSOCIATE 的所有权单元，控制与数据 fd 必须同生共死。
 */
struct ProxyRuntime::UdpAssociation {
  int control_descriptor = -1;
  int datagram_descriptor = -1;
  bool selected_application = false;
  socks5::TargetAddress requested_peer;
  std::string bound_peer;
  std::shared_ptr<UdpPeerSession> session;
};

} // namespace routesocks::runtime
