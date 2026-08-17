#pragma once

#include <sys/socket.h>

#include <cstddef>
#include <cstdint>
#include <string>
#include <vector>

#include "engine/runtime_config.h"

namespace routesocks::net {
class SocketObserver;
}

namespace routesocks::socks5 {

/** 保存 SOCKS5 请求目标；host 可以是域名、IPv4 或 IPv6。 */
struct TargetAddress {
  std::string host;
  uint16_t port = 0;
};

/** 服务端握手结果，UDP ASSOCIATE 与 CONNECT 共用同一解析入口。 */
struct ServerRequest {
  uint8_t command = 0;
  TargetAddress target;
};

/** 上游 CONNECT 的完整结果；失败也携带规范化 REP，成功保留服务端返回的真实 BND。 */
struct TcpConnectResult {
  int descriptor = -1;
  TargetAddress bound{"0.0.0.0", 0};
  uint8_t reply_status = 1;
};

/** 使用每次启动的内部 RFC1929 凭据读取本地 SOCKS5 请求；超时、错误凭据或协议错误返回 false。 */
bool ReadServerRequest(int descriptor,
                       const runtime::RuntimeConfig& config,
                       ServerRequest* request,
                       std::string* error);

/** 发送 SOCKS5 服务端结果，成功时携带实际绑定端点。 */
bool SendServerReply(int descriptor, uint8_t status, const runtime::Endpoint& bound);

/** 经带可选 RFC1929 认证的上游 SOCKS5 建立 TCP CONNECT。 */
TcpConnectResult ConnectUpstreamTcp(const runtime::RuntimeConfig& config,
                                    const TargetAddress& target,
                                    net::SocketObserver* observer,
                                    std::string* error);

/** 维持一个上游 UDP ASSOCIATE 控制连接及其 UDP 中继端点。 */
class UpstreamUdpSession {
 public:
  /** 创建尚未连接的会话，实际网络资源由 Open 分配。 */
  UpstreamUdpSession();
  /** 关闭控制连接和 UDP socket，保证异常路径不泄漏。 */
  ~UpstreamUdpSession();
  UpstreamUdpSession(const UpstreamUdpSession&) = delete;
  UpstreamUdpSession& operator=(const UpstreamUdpSession&) = delete;

  /** 建立控制连接和 UDP socket；任一步失败都会完整回滚。 */
  bool Open(const runtime::RuntimeConfig& config,
            net::SocketObserver* observer,
            std::string* error);

  /** 复用既有 ASSOCIATE 只发送一帧；响应由独立接收循环异步读取。 */
  bool Send(const TargetAddress& target,
            const std::vector<uint8_t>& payload,
            std::string* error);

  /** 从已可读中继 socket 读取一帧并校验真实中继来源；损坏帧返回 false。 */
  bool Receive(TargetAddress* target, std::vector<uint8_t>* payload, std::string* error);

  /** 返回供 poll 使用的 UDP 中继描述符；未建立时返回 -1。 */
  int Descriptor() const;

  /** 返回供 poll 监测 ASSOCIATE 生命周期的 TCP 控制描述符；未建立时返回 -1。 */
  int ControlDescriptor() const;

  /** 非阻塞检查控制连接仍无数据且未关闭；关闭或协议外数据都返回 false。 */
  bool ControlConnectionAlive() const;

  /** 关闭控制与 UDP socket，使阻塞收发立即结束。 */
  void Close();

 private:
  int control_descriptor_ = -1;
  int udp_descriptor_ = -1;
  sockaddr_storage relay_address_{};
  socklen_t relay_address_length_ = 0;
  net::SocketObserver* observer_ = nullptr;
};

/** 解析 SOCKS5 UDP 帧并保留原目标，分片帧被精确拒绝。 */
bool ParseUdpFrame(const uint8_t* bytes,
                   std::size_t length,
                   TargetAddress* target,
                   std::vector<uint8_t>* payload);

/** 生成 SOCKS5 UDP 响应帧，地址类型按 host 自动选择。 */
bool BuildUdpFrame(const TargetAddress& target,
                   const std::vector<uint8_t>& payload,
                   std::vector<uint8_t>* frame);

}  // namespace routesocks::socks5
