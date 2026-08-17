#include "net/socket_utils.h"

#include <arpa/inet.h>
#include <fcntl.h>
#include <netdb.h>
#include <netinet/in.h>
#include <poll.h>
#include <unistd.h>

#include <cerrno>
#include <cstring>
#include <string>

#ifndef IP_TRANSPARENT
#define IP_TRANSPARENT 19
#endif

namespace routesocks::net {
namespace {

thread_local int last_connect_error = 0;

/** 把 errno 转为带操作名的稳定中文错误，不输出地址或凭据。 */
std::string SystemError(const std::string &operation) {
  return operation + "失败：" + std::strerror(errno);
}

/** 为监听 socket 设置地址复用，避免正常重启被 TIME_WAIT 阻塞。 */
bool EnableAddressReuse(int descriptor, std::string *error) {
  const int enabled = 1;
  if (setsockopt(descriptor, SOL_SOCKET, SO_REUSEADDR, &enabled,
                 sizeof(enabled)) == 0)
    return true;
  *error = SystemError("设置地址复用");
  return false;
}

/**
 * 封装一次带时限的连接事务，避免将 socket 地址的多个组件作为松散参数传递。
 * address 只在 ConnectWithTimeout 返回前被读取，error
 * 在失败时接收精确的中文原因。
 */
struct TimedConnectRequest {
  int descriptor;
  const sockaddr *address;
  socklen_t address_length;
  int timeout_millis;
  std::string *error;
};

/** 以非阻塞 connect 实现确定超时，完成后恢复阻塞语义供协议层使用。 */
bool ConnectWithTimeout(const TimedConnectRequest &request) {
  const int original_flags = fcntl(request.descriptor, F_GETFL, 0);
  if (original_flags < 0 ||
      fcntl(request.descriptor, F_SETFL, original_flags | O_NONBLOCK) != 0) {
    *request.error = SystemError("设置连接超时");
    return false;
  }
  int result =
      connect(request.descriptor, request.address, request.address_length);
  if (result != 0 && errno != EINPROGRESS) {
    *request.error = SystemError("连接目标");
    return false;
  }
  if (result != 0) {
    pollfd event{request.descriptor, POLLOUT, 0};
    result = poll(&event, 1, request.timeout_millis);
    int socket_error = 0;
    socklen_t error_length = sizeof(socket_error);
    if (result <= 0 ||
        getsockopt(request.descriptor, SOL_SOCKET, SO_ERROR, &socket_error,
                   &error_length) != 0 ||
        socket_error != 0) {
      if (result == 0)
        errno = ETIMEDOUT;
      if (socket_error != 0)
        errno = socket_error;
      *request.error = SystemError("连接目标");
      return false;
    }
  }
  if (fcntl(request.descriptor, F_SETFL, original_flags) != 0) {
    *request.error = SystemError("恢复连接模式");
    return false;
  }
  return true;
}

/** 创建并绑定 IPv4 监听 socket，类型由调用方决定。 */
int CreateIpv4Listener(uint16_t port, int socket_type, std::string *error) {
  UniqueSocket listener(socket(AF_INET, socket_type, 0));
  if (listener.Get() < 0 || !EnableAddressReuse(listener.Get(), error)) {
    if (listener.Get() < 0)
      *error = SystemError("创建监听 socket");
    return -1;
  }
  sockaddr_in address{};
  address.sin_family = AF_INET;
  address.sin_port = htons(port);
  // Root 模式仅接收本机 OUTPUT
  // REDIRECT；透明属性用于开启原目标辅助数据，不代表应暴露到 LAN。
  // 统一绑定回环可阻止外部直连固定端口触发递归，同时保留
  // SO_ORIGINAL_DST/IP_RECVORIGDSTADDR。
  address.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
  if (bind(listener.Get(), reinterpret_cast<const sockaddr *>(&address),
           sizeof(address)) != 0) {
    *error = SystemError("绑定监听端口");
    return -1;
  }
  return listener.Release();
}

} // namespace

UniqueSocket::UniqueSocket(int descriptor) : descriptor_(descriptor) {}

UniqueSocket::~UniqueSocket() { Reset(); }

UniqueSocket::UniqueSocket(UniqueSocket &&other) noexcept
    : descriptor_(other.Release()) {}

UniqueSocket &UniqueSocket::operator=(UniqueSocket &&other) noexcept {
  if (this != &other)
    Reset(other.Release());
  return *this;
}

int UniqueSocket::Get() const { return descriptor_; }

int UniqueSocket::Release() {
  const int descriptor = descriptor_;
  descriptor_ = -1;
  return descriptor;
}

void UniqueSocket::Reset(int descriptor) {
  if (descriptor_ >= 0)
    close(descriptor_);
  descriptor_ = descriptor;
}

bool ResolveEndpoint(const runtime::Endpoint &endpoint,
                     sockaddr_storage *address, socklen_t *address_length,
                     std::string *error) {
  sockaddr_in ipv4{};
  ipv4.sin_family = AF_INET;
  ipv4.sin_port = htons(endpoint.port);
  if (inet_pton(AF_INET, endpoint.host.c_str(), &ipv4.sin_addr) == 1) {
    std::memcpy(address, &ipv4, sizeof(ipv4));
    *address_length = sizeof(ipv4);
    return true;
  }
  sockaddr_in6 ipv6{};
  ipv6.sin6_family = AF_INET6;
  ipv6.sin6_port = htons(endpoint.port);
  if (inet_pton(AF_INET6, endpoint.host.c_str(), &ipv6.sin6_addr) == 1) {
    std::memcpy(address, &ipv6, sizeof(ipv6));
    *address_length = sizeof(ipv6);
    return true;
  }
  *error = "网络端点必须先通过规则指定 DNS 解析为 IP 字面量";
  return false;
}

bool IsIpLiteral(const std::string &host) {
  in_addr ipv4{};
  in6_addr ipv6{};
  return inet_pton(AF_INET, host.c_str(), &ipv4) == 1 ||
         inet_pton(AF_INET6, host.c_str(), &ipv6) == 1;
}

int ConnectTcp(const runtime::Endpoint &endpoint, int timeout_ms,
               SocketObserver *observer, std::string *error) {
  sockaddr_storage address{};
  socklen_t address_length = 0;
  last_connect_error = 0;
  if (!ResolveEndpoint(endpoint, &address, &address_length, error)) {
    last_connect_error = EAFNOSUPPORT;
    return -1;
  }
  UniqueSocket candidate(socket(address.ss_family, SOCK_STREAM, 0));
  if (candidate.Get() < 0) {
    *error = SystemError("创建 TCP socket");
    return -1;
  }
  if (observer != nullptr && !observer->RegisterSocket(candidate.Get())) {
    *error = "TCP 连接因数据面切换而取消";
    return -1;
  }
  if (!ConnectWithTimeout({candidate.Get(),
                           reinterpret_cast<const sockaddr *>(&address),
                           address_length, timeout_ms, error})) {
    last_connect_error = errno;
    if (observer != nullptr)
      observer->UnregisterSocket(candidate.Get());
    return -1;
  }
  return candidate.Release();
}

int LastConnectError() { return last_connect_error; }

int CreateIpv4TcpListener(uint16_t port, std::string *error) {
  UniqueSocket listener(CreateIpv4Listener(port, SOCK_STREAM, error));
  if (listener.Get() < 0)
    return -1;
  if (listen(listener.Get(), 128) != 0) {
    *error = SystemError("启动 TCP 监听");
    return -1;
  }
  return listener.Release();
}

int CreateIpv4UdpListener(uint16_t port, bool transparent, std::string *error) {
  if (!transparent)
    return CreateIpv4Listener(port, SOCK_DGRAM, error);

  // TPROXY 不改写数据报目标，监听器必须开启 IP_TRANSPARENT 并绑定任意地址，
  // 否则 Android 会把回环代理端口作为 IP_ORIGDSTADDR 返回，UDP 随后递归投向自身。
  UniqueSocket listener(socket(AF_INET, SOCK_DGRAM, 0));
  if (listener.Get() < 0 || !EnableAddressReuse(listener.Get(), error)) {
    if (listener.Get() < 0)
      *error = SystemError("创建 UDP 透明监听 socket");
    return -1;
  }
  const int enabled = 1;
  if (setsockopt(listener.Get(), SOL_IP, IP_TRANSPARENT, &enabled,
                 sizeof(enabled)) != 0) {
    *error = SystemError("启用 UDP 透明监听");
    return -1;
  }
#ifdef IP_RECVORIGDSTADDR
  if (setsockopt(listener.Get(), SOL_IP, IP_RECVORIGDSTADDR, &enabled,
                 sizeof(enabled)) != 0) {
    *error = SystemError("启用 UDP 原目标读取");
    return -1;
  }
#endif
  sockaddr_in address{};
  address.sin_family = AF_INET;
  address.sin_port = htons(port);
  address.sin_addr.s_addr = htonl(INADDR_ANY);
  if (bind(listener.Get(), reinterpret_cast<const sockaddr *>(&address),
           sizeof(address)) != 0) {
    *error = SystemError("绑定 UDP 透明监听端口");
    return -1;
  }
  return listener.Release();
}

bool SendAll(int descriptor, const void *bytes, std::size_t length,
             std::string *error) {
  const auto *cursor = static_cast<const uint8_t *>(bytes);
  std::size_t remaining = length;
  while (remaining > 0) {
    const ssize_t sent = send(descriptor, cursor, remaining, MSG_NOSIGNAL);
    if (sent <= 0) {
      if (sent < 0 && errno == EINTR)
        continue;
      *error = SystemError("发送网络数据");
      return false;
    }
    cursor += sent;
    remaining -= static_cast<std::size_t>(sent);
  }
  return true;
}

bool ReceiveAll(int descriptor, void *bytes, std::size_t length,
                std::string *error) {
  auto *cursor = static_cast<uint8_t *>(bytes);
  std::size_t remaining = length;
  while (remaining > 0) {
    const ssize_t received = recv(descriptor, cursor, remaining, 0);
    if (received <= 0) {
      if (received < 0 && errno == EINTR)
        continue;
      *error = received == 0 ? "对端提前关闭连接" : SystemError("读取网络数据");
      return false;
    }
    cursor += received;
    remaining -= static_cast<std::size_t>(received);
  }
  return true;
}

bool WaitReadable(int descriptor, int timeout_ms) {
  pollfd event{descriptor, POLLIN, 0};
  int result;
  do {
    result = poll(&event, 1, timeout_ms);
  } while (result < 0 && errno == EINTR);
  return result > 0 && (event.revents & (POLLIN | POLLHUP)) != 0;
}

std::string AddressKey(const sockaddr_storage &address,
                       socklen_t address_length) {
  char host[NI_MAXHOST]{};
  char service[NI_MAXSERV]{};
  const int result = getnameinfo(
      reinterpret_cast<const sockaddr *>(&address), address_length, host,
      sizeof(host), service, sizeof(service), NI_NUMERICHOST | NI_NUMERICSERV);
  return result == 0 ? std::string(host) + ":" + service : std::string();
}

} // namespace routesocks::net
