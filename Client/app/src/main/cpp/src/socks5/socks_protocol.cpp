#include "socks5/socks_protocol.h"

#include <arpa/inet.h>
#include <unistd.h>

#include <algorithm>
#include <array>
#include <cerrno>
#include <cstring>
#include <utility>

#include "net/socket_utils.h"

namespace routesocks::socks5 {
namespace {

constexpr uint8_t kVersion = 5;
constexpr uint8_t kAddressIpv4 = 1;
constexpr uint8_t kAddressDomain = 3;
constexpr uint8_t kAddressIpv6 = 4;
constexpr int kHandshakeTimeoutMilliseconds = 10000;

/** 为握手阶段设置有界读写等待；任一 setsockopt 失败返回 false，零恢复阻塞模式。
 */
bool SetIoTimeout(int descriptor, int timeout_ms) {
  timeval timeout{timeout_ms / 1000, (timeout_ms % 1000) * 1000};
  return setsockopt(descriptor, SOL_SOCKET, SO_RCVTIMEO, &timeout,
                    sizeof(timeout)) == 0 &&
         setsockopt(descriptor, SOL_SOCKET, SO_SNDTIMEO, &timeout,
                    sizeof(timeout)) == 0;
}

/** 读取 SOCKS 地址字段；长度或类型错误会中止握手，避免错位读取后续字段。 */
bool ReadAddress(int descriptor, TargetAddress *target, std::string *error) {
  uint8_t type = 0;
  if (!net::ReceiveAll(descriptor, &type, 1, error))
    return false;
  std::array<uint8_t, 16> binary{};
  if (type == kAddressIpv4 || type == kAddressIpv6) {
    const std::size_t length = type == kAddressIpv4 ? 4 : 16;
    if (!net::ReceiveAll(descriptor, binary.data(), length, error))
      return false;
    char text[INET6_ADDRSTRLEN]{};
    if (inet_ntop(type == kAddressIpv4 ? AF_INET : AF_INET6, binary.data(),
                  text, sizeof(text)) == nullptr) {
      *error = "SOCKS5 地址转换失败";
      return false;
    }
    target->host = text;
  } else if (type == kAddressDomain) {
    uint8_t length = 0;
    if (!net::ReceiveAll(descriptor, &length, 1, error) || length == 0) {
      *error = "SOCKS5 域名长度无效";
      return false;
    }
    target->host.resize(length);
    if (!net::ReceiveAll(descriptor, target->host.data(), length, error))
      return false;
  } else {
    *error = "SOCKS5 地址类型不受支持";
    return false;
  }
  std::array<uint8_t, 2> port{};
  if (!net::ReceiveAll(descriptor, port.data(), port.size(), error))
    return false;
  target->port = static_cast<uint16_t>((port[0] << 8U) | port[1]);
  return true;
}

/** 把目标编码为 SOCKS 地址字段，域名过长会精确失败。 */
bool AppendAddress(const TargetAddress &target, std::vector<uint8_t> *bytes) {
  in_addr ipv4{};
  in6_addr ipv6{};
  if (inet_pton(AF_INET, target.host.c_str(), &ipv4) == 1) {
    bytes->push_back(kAddressIpv4);
    const auto *begin = reinterpret_cast<const uint8_t *>(&ipv4);
    bytes->insert(bytes->end(), begin, begin + sizeof(ipv4));
  } else if (inet_pton(AF_INET6, target.host.c_str(), &ipv6) == 1) {
    bytes->push_back(kAddressIpv6);
    const auto *begin = reinterpret_cast<const uint8_t *>(&ipv6);
    bytes->insert(bytes->end(), begin, begin + sizeof(ipv6));
  } else {
    if (target.host.empty() || target.host.size() > 255)
      return false;
    bytes->push_back(kAddressDomain);
    bytes->push_back(static_cast<uint8_t>(target.host.size()));
    bytes->insert(bytes->end(), target.host.begin(), target.host.end());
  }
  bytes->push_back(static_cast<uint8_t>(target.port >> 8U));
  bytes->push_back(static_cast<uint8_t>(target.port & 0xFFU));
  return true;
}

/**
 * 使用配置层已验证的非空账号密码完成上游 RFC1929 认证。
 * 稳定边界是只协商 method=2；服务端选择无认证或其他方法均精确失败。
 */
bool AuthenticateUpstream(int descriptor, const runtime::RuntimeConfig &config,
                          std::string *error) {
  const std::array<uint8_t, 3> greeting{kVersion, 1, 2};
  std::array<uint8_t, 2> selection{};
  if (!net::SendAll(descriptor, greeting.data(), greeting.size(), error) ||
      !net::ReceiveAll(descriptor, selection.data(), selection.size(), error))
    return false;
  if (selection[0] != kVersion || selection[1] != 2) {
    *error = "上游 SOCKS5 拒绝认证方法";
    return false;
  }
  if (config.username.empty() || config.password.empty() ||
      config.username.size() > 255 || config.password.size() > 255) {
    *error = "上游 SOCKS5 认证协商不兼容";
    return false;
  }
  std::vector<uint8_t> request{1, static_cast<uint8_t>(config.username.size())};
  request.insert(request.end(), config.username.begin(), config.username.end());
  request.push_back(static_cast<uint8_t>(config.password.size()));
  request.insert(request.end(), config.password.begin(), config.password.end());
  std::array<uint8_t, 2> reply{};
  if (!net::SendAll(descriptor, request.data(), request.size(), error) ||
      !net::ReceiveAll(descriptor, reply.data(), reply.size(), error))
    return false;
  if (reply[0] != 1 || reply[1] != 0) {
    *error = "上游 SOCKS5 账号认证失败";
    return false;
  }
  return true;
}

struct CommandResult {
  TargetAddress bound{"0.0.0.0", 0};
  uint8_t reply_status = 1;
  bool succeeded = false;
};

/** 发送 SOCKS 命令并解析回复绑定地址；网络错误返回通用 REP=1，协议 REP
 * 始终规范到 0..8。 */
CommandResult SendCommand(int descriptor, uint8_t command,
                          const TargetAddress &target, std::string *error) {
  CommandResult result;
  std::vector<uint8_t> request{kVersion, command, 0};
  if (!AppendAddress(target, &request) ||
      !net::SendAll(descriptor, request.data(), request.size(), error)) {
    if (error->empty())
      *error = "SOCKS5 目标地址无效";
    return result;
  }
  std::array<uint8_t, 3> reply{};
  if (!net::ReceiveAll(descriptor, reply.data(), reply.size(), error))
    return result;
  const bool header_valid = reply[0] == kVersion && reply[2] == 0;
  TargetAddress bound;
  // RFC1928 的成功和失败回复都具有完整 BND
  // 字段；先消费并校验整个帧，禁止把截断失败包伪装成标准 REP。
  const bool address_valid = ReadAddress(descriptor, &bound, error);
  if (!header_valid) {
    *error = "上游 SOCKS5 回复头无效";
    return result;
  }
  if (!address_valid) {
    result.reply_status = 1;
    return result;
  }
  // RFC1928 仅定义 REP
  // 0..8；非法上游值统一映射为通用失败，绝不向本地下游传播未定义状态。
  result.reply_status = reply[1] <= 8 ? reply[1] : 1;
  if (reply[1] != 0) {
    *error =
        "上游 SOCKS5 命令失败，状态=" + std::to_string(result.reply_status);
    return result;
  }
  result.bound = std::move(bound);
  result.succeeded = true;
  return result;
}

/** 从内存中的 SOCKS UDP 地址字段读取目标并推进游标。 */
bool ParseMemoryAddress(const uint8_t *bytes, std::size_t length,
                        std::size_t *cursor, TargetAddress *target) {
  if (*cursor >= length)
    return false;
  const uint8_t type = bytes[(*cursor)++];
  char text[INET6_ADDRSTRLEN]{};
  if (type == kAddressIpv4 || type == kAddressIpv6) {
    const std::size_t size = type == kAddressIpv4 ? 4 : 16;
    if (length - *cursor < size ||
        inet_ntop(type == kAddressIpv4 ? AF_INET : AF_INET6, bytes + *cursor,
                  text, sizeof(text)) == nullptr) {
      return false;
    }
    target->host = text;
    *cursor += size;
  } else if (type == kAddressDomain) {
    if (*cursor >= length)
      return false;
    const std::size_t size = bytes[(*cursor)++];
    if (size == 0 || length - *cursor < size)
      return false;
    target->host.assign(reinterpret_cast<const char *>(bytes + *cursor), size);
    *cursor += size;
  } else {
    return false;
  }
  if (length - *cursor < 2)
    return false;
  target->port =
      static_cast<uint16_t>((bytes[*cursor] << 8U) | bytes[*cursor + 1]);
  *cursor += 2;
  return target->port != 0;
}

/** 恒定时间比较内部凭据；长度差也并入差异值，禁止短路暴露逐字节匹配位置。 */
bool ConstantTimeEquals(const std::string &expected,
                        const std::vector<uint8_t> &supplied) {
  const std::size_t comparison_length =
      std::max(expected.size(), supplied.size());
  std::size_t difference = expected.size() ^ supplied.size();
  for (std::size_t index = 0; index < comparison_length; ++index) {
    const uint8_t expected_byte =
        index < expected.size() ? static_cast<uint8_t>(expected[index]) : 0;
    const uint8_t supplied_byte = index < supplied.size() ? supplied[index] : 0;
    difference |= expected_byte ^ supplied_byte;
  }
  return difference == 0;
}

} // namespace

bool ReadServerRequest(int descriptor, const runtime::RuntimeConfig &config,
                       ServerRequest *request, std::string *error) {
  if (!SetIoTimeout(descriptor, kHandshakeTimeoutMilliseconds)) {
    *error = "设置本地 SOCKS5 握手超时失败";
    return false;
  }
  std::array<uint8_t, 2> header{};
  if (!net::ReceiveAll(descriptor, header.data(), header.size(), error) ||
      header[0] != kVersion || header[1] == 0) {
    *error = "本地 SOCKS5 方法协商无效";
    return false;
  }
  std::vector<uint8_t> methods(header[1]);
  if (!net::ReceiveAll(descriptor, methods.data(), methods.size(), error))
    return false;
  const bool supports_password =
      std::find(methods.begin(), methods.end(), 2) != methods.end();
  const std::array<uint8_t, 2> method_reply{
      kVersion, static_cast<uint8_t>(supports_password ? 2 : 0xFF)};
  if (!net::SendAll(descriptor, method_reply.data(), method_reply.size(),
                    error) ||
      !supports_password) {
    *error = "本地 SOCKS5 客户端未提供内部凭据认证";
    return false;
  }
  std::array<uint8_t, 2> auth_header{};
  if (!net::ReceiveAll(descriptor, auth_header.data(), auth_header.size(),
                       error) ||
      auth_header[0] != 1 || auth_header[1] == 0) {
    *error = "本地 SOCKS5 认证头无效";
    return false;
  }
  std::vector<uint8_t> username(auth_header[1]);
  uint8_t password_length = 0;
  if (!net::ReceiveAll(descriptor, username.data(), username.size(), error) ||
      !net::ReceiveAll(descriptor, &password_length, 1, error) ||
      password_length == 0) {
    *error = "本地 SOCKS5 认证字段无效";
    return false;
  }
  std::vector<uint8_t> password(password_length);
  if (!net::ReceiveAll(descriptor, password.data(), password.size(), error))
    return false;
  const bool username_matches =
      ConstantTimeEquals(config.local_username, username);
  const bool password_matches =
      ConstantTimeEquals(config.local_password, password);
  const bool authenticated = username_matches && password_matches;
  const std::array<uint8_t, 2> auth_reply{
      1, static_cast<uint8_t>(authenticated ? 0 : 1)};
  if (!net::SendAll(descriptor, auth_reply.data(), auth_reply.size(), error) ||
      !authenticated) {
    *error = "本地 SOCKS5 内部凭据校验失败";
    return false;
  }
  std::array<uint8_t, 3> command{};
  if (!net::ReceiveAll(descriptor, command.data(), command.size(), error) ||
      command[0] != kVersion || command[2] != 0) {
    *error = "本地 SOCKS5 命令头无效";
    return false;
  }
  request->command = command[1];
  if (!ReadAddress(descriptor, &request->target, error))
    return false;
  if (request->command == 1 && request->target.port == 0) {
    *error = "SOCKS5 CONNECT 目标端口不能为零";
    return false;
  }
  if (!SetIoTimeout(descriptor, 0)) {
    *error = "恢复本地 SOCKS5 阻塞模式失败";
    return false;
  }
  return true;
}

bool SendServerReply(int descriptor, uint8_t status,
                     const runtime::Endpoint &bound) {
  std::vector<uint8_t> reply{kVersion, status, 0};
  TargetAddress address{bound.host.empty() ? "0.0.0.0" : bound.host,
                        bound.port};
  if (!AppendAddress(address, &reply))
    return false;
  std::string error;
  return net::SendAll(descriptor, reply.data(), reply.size(), &error);
}

TcpConnectResult ConnectUpstreamTcp(const runtime::RuntimeConfig &config,
                                    const TargetAddress &target,
                                    net::SocketObserver *observer,
                                    std::string *error) {
  TcpConnectResult result;
  net::UniqueSocket connection(
      net::ConnectTcp(config.upstream, 10000, observer, error));
  if (connection.Get() < 0)
    return result;
  if (!SetIoTimeout(connection.Get(), 10000)) {
    if (observer != nullptr)
      observer->UnregisterSocket(connection.Get());
    *error = "设置上游 TCP 握手超时失败";
    return result;
  }
  if (!AuthenticateUpstream(connection.Get(), config, error)) {
    if (observer != nullptr)
      observer->UnregisterSocket(connection.Get());
    return result;
  }
  const CommandResult command = SendCommand(connection.Get(), 1, target, error);
  result.bound = command.bound;
  result.reply_status = command.reply_status;
  if (!command.succeeded) {
    if (observer != nullptr)
      observer->UnregisterSocket(connection.Get());
    return result;
  }
  if (!SetIoTimeout(connection.Get(), 0)) {
    if (observer != nullptr)
      observer->UnregisterSocket(connection.Get());
    *error = "恢复上游 TCP 阻塞模式失败";
    return result;
  }
  result.descriptor = connection.Release();
  result.reply_status = 0;
  return result;
}

UpstreamUdpSession::UpstreamUdpSession() = default;

UpstreamUdpSession::~UpstreamUdpSession() { Close(); }

bool UpstreamUdpSession::Open(const runtime::RuntimeConfig &config,
                              net::SocketObserver *observer,
                              std::string *error) {
  Close();
  net::UniqueSocket control(
      net::ConnectTcp(config.upstream, 10000, observer, error));
  if (control.Get() < 0)
    return false;
  if (!SetIoTimeout(control.Get(), 10000)) {
    if (observer != nullptr)
      observer->UnregisterSocket(control.Get());
    *error = "设置 UDP ASSOCIATE 握手超时失败";
    return false;
  }
  observer_ = observer;
  if (!AuthenticateUpstream(control.Get(), config, error)) {
    if (observer_ != nullptr)
      observer_->UnregisterSocket(control.Get());
    observer_ = nullptr;
    return false;
  }
  const CommandResult command =
      SendCommand(control.Get(), 3, {"0.0.0.0", 0}, error);
  TargetAddress bound = command.bound;
  if (!command.succeeded) {
    if (observer_ != nullptr)
      observer_->UnregisterSocket(control.Get());
    observer_ = nullptr;
    return false;
  }
  if (bound.port == 0) {
    if (observer_ != nullptr)
      observer_->UnregisterSocket(control.Get());
    observer_ = nullptr;
    *error = "上游 SOCKS5 返回了无效 UDP 中继端口";
    return false;
  }
  if (bound.host == "0.0.0.0" || bound.host == "::")
    bound.host = config.upstream.host;
  runtime::Endpoint relay{bound.host, bound.port};
  if (!net::ResolveEndpoint(relay, &relay_address_, &relay_address_length_,
                            error)) {
    if (observer_ != nullptr)
      observer_->UnregisterSocket(control.Get());
    observer_ = nullptr;
    return false;
  }
  net::UniqueSocket udp(socket(relay_address_.ss_family, SOCK_DGRAM, 0));
  if (udp.Get() < 0) {
    *error = "创建上游 UDP 中继 socket 失败";
    if (observer_ != nullptr)
      observer_->UnregisterSocket(control.Get());
    observer_ = nullptr;
    return false;
  }
  if (observer_ != nullptr && !observer_->RegisterSocket(udp.Get())) {
    observer_->UnregisterSocket(control.Get());
    observer_ = nullptr;
    *error = "上游 UDP 会话因数据面切换而取消";
    return false;
  }
  control_descriptor_ = control.Release();
  udp_descriptor_ = udp.Release();
  if (!SetIoTimeout(control_descriptor_, 0)) {
    *error = "恢复 UDP ASSOCIATE 控制连接阻塞模式失败";
    Close();
    return false;
  }
  return true;
}

bool UpstreamUdpSession::Send(const TargetAddress &target,
                              const std::vector<uint8_t> &payload,
                              std::string *error) {
  std::vector<uint8_t> frame;
  if (udp_descriptor_ < 0 || !BuildUdpFrame(target, payload, &frame)) {
    *error = "上游 UDP 会话未建立或目标无效";
    return false;
  }
  const ssize_t sent =
      sendto(udp_descriptor_, frame.data(), frame.size(), 0,
             reinterpret_cast<const sockaddr *>(&relay_address_),
             relay_address_length_);
  if (sent != static_cast<ssize_t>(frame.size())) {
    *error = "发送上游 SOCKS5 UDP 帧失败";
    return false;
  }
  return true;
}

bool UpstreamUdpSession::Receive(TargetAddress *target,
                                 std::vector<uint8_t> *payload,
                                 std::string *error) {
  std::array<uint8_t, 65535> buffer{};
  sockaddr_storage source{};
  socklen_t source_length = sizeof(source);
  const ssize_t received =
      recvfrom(udp_descriptor_, buffer.data(), buffer.size(), 0,
               reinterpret_cast<sockaddr *>(&source), &source_length);
  if (received <= 0) {
    *error = "读取上游 SOCKS5 UDP 响应失败";
    return false;
  }
  if (net::AddressKey(source, source_length) !=
      net::AddressKey(relay_address_, relay_address_length_)) {
    *error = "上游 SOCKS5 UDP 响应来源不匹配";
    return false;
  }
  if (!ParseUdpFrame(buffer.data(), static_cast<std::size_t>(received), target,
                     payload)) {
    *error = "上游 SOCKS5 UDP 响应帧损坏";
    return false;
  }
  return true;
}

int UpstreamUdpSession::Descriptor() const { return udp_descriptor_; }

int UpstreamUdpSession::ControlDescriptor() const {
  return control_descriptor_;
}

bool UpstreamUdpSession::ControlConnectionAlive() const {
  if (control_descriptor_ < 0)
    return false;
  uint8_t unexpected = 0;
  const ssize_t received =
      recv(control_descriptor_, &unexpected, 1, MSG_PEEK | MSG_DONTWAIT);
  return received < 0 && (errno == EAGAIN || errno == EWOULDBLOCK);
}

void UpstreamUdpSession::Close() {
  if (observer_ != nullptr && control_descriptor_ >= 0)
    observer_->UnregisterSocket(control_descriptor_);
  if (observer_ != nullptr && udp_descriptor_ >= 0)
    observer_->UnregisterSocket(udp_descriptor_);
  if (control_descriptor_ >= 0)
    close(control_descriptor_);
  if (udp_descriptor_ >= 0)
    close(udp_descriptor_);
  control_descriptor_ = -1;
  udp_descriptor_ = -1;
  observer_ = nullptr;
}

bool ParseUdpFrame(const uint8_t *bytes, std::size_t length,
                   TargetAddress *target, std::vector<uint8_t> *payload) {
  if (length < 4 || bytes[0] != 0 || bytes[1] != 0 || bytes[2] != 0)
    return false;
  std::size_t cursor = 3;
  if (!ParseMemoryAddress(bytes, length, &cursor, target))
    return false;
  payload->assign(bytes + cursor, bytes + length);
  return true;
}

bool BuildUdpFrame(const TargetAddress &target,
                   const std::vector<uint8_t> &payload,
                   std::vector<uint8_t> *frame) {
  frame->assign({0, 0, 0});
  if (!AppendAddress(target, frame))
    return false;
  frame->insert(frame->end(), payload.begin(), payload.end());
  return true;
}

} // namespace routesocks::socks5
