#include "engine/proxy_runtime.h"

#include <arpa/inet.h>
#include <linux/netfilter_ipv4.h>
#include <netinet/in.h>
#include <poll.h>
#include <unistd.h>

#include <algorithm>
#include <array>
#include <chrono>
#include <cstring>
#include <utility>

#include "net/dns_protocol.h"
#include "net/domain_sniffer.h"
#include "net/socket_utils.h"

namespace routesocks::runtime {
namespace {

constexpr int kConnectTimeoutMs = 10000;
// HEV 收到乐观成功应答后还要经过 lwIP 协程调度，再把应用首包送入本地 SOCKS
// 入口。 真机高负载下 250ms 会在 Chrome 发出 Host/SNI
// 前提前结束，事务因此退化成 IP； 1 秒仍是严格有界等待，同时覆盖 Android
// 调度抖动且不会让 server-first 协议无限阻塞。
constexpr int kSniffTimeoutMs = 1000;
constexpr int kUdpTimeoutSeconds = 2;
constexpr std::size_t kMaximumSniffBytes = 16 * 1024;

uint8_t SocksReplyForErrno(int error_number) {
  switch (error_number) {
  case EACCES:
    return 2;
  case ENETUNREACH:
    return 3;
  case EHOSTUNREACH:
    return 4;
  case ECONNREFUSED:
    return 5;
  case ETIMEDOUT:
    return 6;
  case EAFNOSUPPORT:
    return 8;
  default:
    return 1;
  }
}

/**
 * 在 1 秒总预算内增量读取 HTTP 头或 TLS ClientHello，支持 TCP 分段和多 TLS
 * record。 读取到完整域名、达到 16KiB
 * 或超时即返回；客户端关闭时保留已读前缀供调用方转发。
 */
void ReadSniffPrefix(int descriptor, std::vector<uint8_t> *prefix) {
  const auto deadline = std::chrono::steady_clock::now() +
                        std::chrono::milliseconds(kSniffTimeoutMs);
  while (prefix->size() < kMaximumSniffBytes) {
    const auto remaining =
        std::chrono::duration_cast<std::chrono::milliseconds>(
            deadline - std::chrono::steady_clock::now())
            .count();
    if (remaining <= 0 ||
        !net::WaitReadable(descriptor, static_cast<int>(remaining)))
      return;
    const std::size_t old_size = prefix->size();
    prefix->resize(std::min(kMaximumSniffBytes, old_size + 4096));
    const ssize_t received = recv(descriptor, prefix->data() + old_size,
                                  prefix->size() - old_size, 0);
    if (received <= 0) {
      prefix->resize(old_size);
      return;
    }
    prefix->resize(old_size + static_cast<std::size_t>(received));
    if (net::DomainSniffer::Sniff(*prefix).matched)
      return;
  }
}

/** 从 IPv4 REDIRECT socket 读取原目标；非 IPv4 或零端口异常均拒绝。 */
bool ReadOriginalTcpTarget(int descriptor, socks5::TargetAddress *target) {
  sockaddr_storage local{};
  socklen_t local_length = sizeof(local);
  if (getsockname(descriptor, reinterpret_cast<sockaddr *>(&local),
                  &local_length) != 0)
    return false;
  if (local.ss_family != AF_INET)
    return false;
  sockaddr_in original{};
  socklen_t length = sizeof(original);
  if (getsockopt(descriptor, SOL_IP, SO_ORIGINAL_DST, &original, &length) != 0)
    return false;
  char address[INET_ADDRSTRLEN]{};
  if (inet_ntop(AF_INET, &original.sin_addr, address, sizeof(address)) ==
      nullptr)
    return false;
  target->host = address;
  target->port = ntohs(original.sin_port);
  return target->port != 0;
}

/** 建立带确定接收超时的直连 UDP socket；用于 TCP 域名解析，注册失败时完整关闭。
 */
int CreateDirectUdpSocket(int address_family, net::SocketObserver *observer) {
  const int descriptor = socket(address_family, SOCK_DGRAM, 0);
  if (descriptor < 0)
    return -1;
  if (observer != nullptr && !observer->RegisterSocket(descriptor)) {
    close(descriptor);
    return -1;
  }
  timeval timeout{kUdpTimeoutSeconds, 0};
  if (setsockopt(descriptor, SOL_SOCKET, SO_RCVTIMEO, &timeout,
                 sizeof(timeout)) != 0) {
    if (observer != nullptr)
      observer->UnregisterSocket(descriptor);
    close(descriptor);
    return -1;
  }
  return descriptor;
}

/**
 * 为一条已认证 UDP ASSOCIATE 创建回环独占端点并返回真实端口。
 * 端口由内核分配，失败时返回
 * -1；端点绝不绑定固定业务端口，避免未认证进程直接投递 SOCKS 帧。
 */
int CreateAssociationDatagram(uint16_t *bound_port) {
  net::UniqueSocket descriptor(socket(AF_INET, SOCK_DGRAM, 0));
  if (descriptor.Get() < 0 || bound_port == nullptr)
    return -1;
  sockaddr_in address{};
  address.sin_family = AF_INET;
  address.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
  if (bind(descriptor.Get(), reinterpret_cast<const sockaddr *>(&address),
           sizeof(address)) != 0) {
    return -1;
  }
  socklen_t address_length = sizeof(address);
  if (getsockname(descriptor.Get(), reinterpret_cast<sockaddr *>(&address),
                  &address_length) != 0) {
    return -1;
  }
  *bound_port = ntohs(address.sin_port);
  return descriptor.Release();
}

/** 读取已连接 socket 的真实本地 BND；读取失败返回 RFC1928 规定的全零端点。 */
socks5::TargetAddress ReadBoundEndpoint(int descriptor) {
  sockaddr_storage storage{};
  socklen_t length = sizeof(storage);
  if (getsockname(descriptor, reinterpret_cast<sockaddr *>(&storage),
                  &length) != 0)
    return {};
  char host[INET6_ADDRSTRLEN]{};
  uint16_t port = 0;
  const void *address = nullptr;
  int family = storage.ss_family;
  if (family == AF_INET) {
    const auto *ipv4 = reinterpret_cast<const sockaddr_in *>(&storage);
    address = &ipv4->sin_addr;
    port = ntohs(ipv4->sin_port);
  } else if (family == AF_INET6) {
    const auto *ipv6 = reinterpret_cast<const sockaddr_in6 *>(&storage);
    address = &ipv6->sin6_addr;
    port = ntohs(ipv6->sin6_port);
  }
  if (address == nullptr ||
      inet_ntop(family, address, host, sizeof(host)) == nullptr)
    return {};
  return {host, port};
}

struct TcpRelayContext {
  int client = -1;
  int remote = -1;
  std::atomic<bool> *running = nullptr;
  std::atomic<uint64_t> *upload = nullptr;
  std::atomic<uint64_t> *download = nullptr;
};

/** 将一条完整 TCP 上下文双向搬运并使用 poll
 * 避免为每个方向再创建线程；任一端关闭即返回。 */
void RelayTcp(const TcpRelayContext &context) {
  std::array<uint8_t, 32768> buffer{};
  while (context.running->load()) {
    std::array<pollfd, 2> events{
        {{context.client, POLLIN, 0}, {context.remote, POLLIN, 0}}};
    const int ready = poll(events.data(), events.size(), 1000);
    if (ready < 0) {
      if (errno == EINTR)
        continue;
      break;
    }
    if (ready == 0)
      continue;
    for (std::size_t index = 0; index < events.size(); ++index) {
      if ((events[index].revents & (POLLIN | POLLHUP | POLLERR)) == 0)
        continue;
      const int source = index == 0 ? context.client : context.remote;
      const int destination = index == 0 ? context.remote : context.client;
      const ssize_t received = recv(source, buffer.data(), buffer.size(), 0);
      if (received <= 0)
        return;
      std::string error;
      if (!net::SendAll(destination, buffer.data(),
                        static_cast<std::size_t>(received), &error))
        return;
      (index == 0 ? context.upload : context.download)
          ->fetch_add(static_cast<uint64_t>(received));
    }
  }
}

} // namespace

void ProxyRuntime::RunLocalTcp(int listener, bool selected_application) {
  while (running_.load()) {
    const int client = accept(listener, nullptr, nullptr);
    if (client < 0) {
      if (running_.load() && errno != EINTR)
        failed_connections_.fetch_add(1);
      continue;
    }
    if (!RegisterConnection(client)) {
      close(client);
      continue;
    }
    auto self = shared_from_this();
    bool submitted = false;
    try {
      submitted =
          accepting_sessions_.load() &&
          controlWorkers_.Submit([self, client, selected_application]() {
            net::UniqueSocket client_socket(client);
            bool control_transferred = false;
            std::shared_ptr<UdpAssociation> association;
            try {
              socks5::ServerRequest request;
              std::string error;
              if (!socks5::ReadServerRequest(client, self->config_, &request,
                                             &error)) {
                self->failed_connections_.fetch_add(1);
              } else if (request.command == 1) {
                // CONNECT
                // 后的双向转发可能持续数小时，必须移交长连接池；控制池只承担有界握手，
                // 确保新 DNS 的 UDP ASSOCIATE 永远不会排在业务长连接之后。
                client_socket.Release();
                control_transferred = true;
                self->ScheduleTcpConnection(client, std::move(request.target),
                                            true, selected_application);
              } else if (request.command == 3) {
                uint16_t port = 0;
                net::UniqueSocket datagram(CreateAssociationDatagram(&port));
                if (datagram.Get() >= 0 &&
                    self->RegisterSocket(datagram.Get())) {
                  association = self->ReserveUdpControl(
                      client, datagram.Get(), selected_application,
                      std::move(request.target));
                }
                if (association == nullptr) {
                  // 容量、热更或 socket 失败都必须回复全零 BND 的
                  // REP=1，不让客户端进入伪成功状态。
                  if (datagram.Get() >= 0)
                    self->UnregisterSocket(datagram.Get());
                  socks5::SendServerReply(client, 1, {"0.0.0.0", 0});
                  self->failed_connections_.fetch_add(1);
                } else {
                  // 预留成功后 reactor 已是两个 fd 的唯一所有者；先释放
                  // RAII，避免发回失败与 fd 复用造成二次关闭。
                  datagram.Release();
                  client_socket.Release();
                  control_transferred = true;
                  if (!socks5::SendServerReply(client, 0,
                                               {"127.0.0.1", port})) {
                    self->CloseUdpControl(association);
                    self->failed_connections_.fetch_add(1);
                  }
                }
              } else {
                socks5::SendServerReply(client, 7, {"0.0.0.0", 0});
                self->failed_connections_.fetch_add(1);
              }
            } catch (...) {
              // 分配失败也必须撤销已预留的 association，使容量、fd
              // 和连接统计保持一致。
              if (association != nullptr)
                self->CloseUdpControl(association);
              self->failed_connections_.fetch_add(1);
            }
            if (!control_transferred)
              self->UnregisterConnection(client);
          });
    } catch (...) {
      // 控制任务对象或队列扩容失败仍只影响当前连接；监听器必须继续工作，不能让一次
      // 内存压力把整个 DNS/SOCKS 控制入口误判为永久故障。
      submitted = false;
    }
    if (!submitted) {
      UnregisterConnection(client);
      close(client);
      failed_connections_.fetch_add(1);
    }
  }
}

bool ProxyRuntime::ScheduleTcpConnection(int client_descriptor,
                                         socks5::TargetAddress target,
                                         bool reply_socks_success,
                                         bool selected_application) {
  auto self = shared_from_this();
  bool submitted = false;
  try {
    submitted =
        accepting_sessions_.load() &&
        connectionWorkers_.Submit(
            [self, client_descriptor, target = std::move(target),
             reply_socks_success, selected_application]() mutable {
              net::UniqueSocket client_socket(client_descriptor);
              try {
                self->HandleTcp(client_descriptor, std::move(target),
                                reply_socks_success, selected_application);
              } catch (...) {
                // 连接级异常仍由同一尾声注销 fd
                // 与统计，不能遗留规则热更无法中断的会话。
                self->failed_connections_.fetch_add(1);
              }
              self->UnregisterConnection(client_descriptor);
            });
  } catch (...) {
    // 队列扩容失败发生在控制池已经移交 fd
    // 之后；这里必须恢复唯一所有权并走同一失败尾声。
    submitted = false;
  }
  if (submitted) {
    return true;
  }
  if (reply_socks_success)
    socks5::SendServerReply(client_descriptor, 1, {"0.0.0.0", 0});
  UnregisterConnection(client_descriptor);
  close(client_descriptor);
  failed_connections_.fetch_add(1);
  return false;
}

void ProxyRuntime::RunTransparentTcp(int listener, bool selected_application) {
  while (running_.load()) {
    const int client = accept(listener, nullptr, nullptr);
    if (client < 0) {
      if (running_.load() && errno != EINTR)
        failed_connections_.fetch_add(1);
      continue;
    }
    socks5::TargetAddress target;
    if (!ReadOriginalTcpTarget(client, &target) ||
        !RegisterConnection(client)) {
      close(client);
      failed_connections_.fetch_add(1);
      continue;
    }
    ScheduleTcpConnection(client, std::move(target), false,
                          selected_application);
  }
}

int ProxyRuntime::ConnectDnsServer(const std::vector<Endpoint> &servers,
                                   std::size_t start_index,
                                   std::size_t *connected_index,
                                   std::string *error) {
  for (std::size_t index = start_index; index < servers.size(); ++index) {
    const int descriptor =
        net::ConnectTcp(servers[index], kConnectTimeoutMs, this, error);
    if (descriptor < 0)
      continue;
    timeval timeout{kUdpTimeoutSeconds, 0};
    if (setsockopt(descriptor, SOL_SOCKET, SO_RCVTIMEO, &timeout,
                   sizeof(timeout)) != 0) {
      UnregisterSocket(descriptor);
      close(descriptor);
      *error = "设置 DNS TCP 响应超时失败";
      continue;
    }
    *connected_index = index;
    return descriptor;
  }
  return -1;
}

void ProxyRuntime::HandleTcpDns(int client_descriptor, bool reply_socks_success,
                                bool selected_application) {
  const std::vector<Endpoint> servers = DnsSnapshot();
  std::string error;
  std::size_t server_index = 0;
  net::UniqueSocket remote(ConnectDnsServer(servers, 0, &server_index, &error));
  if (remote.Get() < 0) {
    if (reply_socks_success)
      socks5::SendServerReply(client_descriptor, 5, {"0.0.0.0", 0});
    failed_connections_.fetch_add(1);
    return;
  }
  const socks5::TargetAddress dns_bound = ReadBoundEndpoint(remote.Get());
  if (reply_socks_success &&
      !socks5::SendServerReply(client_descriptor, 0,
                               {dns_bound.host, dns_bound.port})) {
    UnregisterSocket(remote.Get());
    failed_connections_.fetch_add(1);
    return;
  }

  while (running_.load()) {
    std::array<uint8_t, 2> query_header{};
    if (!net::ReceiveAll(client_descriptor, query_header.data(),
                         query_header.size(), &error))
      break;
    const std::size_t query_length =
        static_cast<std::size_t>((query_header[0] << 8U) | query_header[1]);
    if (query_length == 0) {
      failed_connections_.fetch_add(1);
      break;
    }
    std::vector<uint8_t> query(query_length);
    if (!net::ReceiveAll(client_descriptor, query.data(), query.size(), &error))
      break;
    std::vector<uint8_t> framed_query(query_header.begin(), query_header.end());
    framed_query.insert(framed_query.end(), query.begin(), query.end());

    std::array<uint8_t, 2> response_header{};
    std::vector<uint8_t> response;
    bool response_ready = false;
    while (remote.Get() >= 0) {
      if (net::SendAll(remote.Get(), framed_query.data(), framed_query.size(),
                       &error) &&
          net::ReceiveAll(remote.Get(), response_header.data(),
                          response_header.size(), &error)) {
        const std::size_t response_length = static_cast<std::size_t>(
            (response_header[0] << 8U) | response_header[1]);
        response.resize(response_length);
        if (response_length > 0 &&
            net::ReceiveAll(remote.Get(), response.data(), response.size(),
                            &error) &&
            net::DnsTransactionMatches(query, response)) {
          response_ready = true;
          break;
        }
      }
      UnregisterSocket(remote.Get());
      remote.Reset(
          ConnectDnsServer(servers, server_index + 1, &server_index, &error));
    }
    if (!response_ready) {
      failed_connections_.fetch_add(1);
      break;
    }
    std::vector<uint8_t> framed_response(response_header.begin(),
                                         response_header.end());
    framed_response.insert(framed_response.end(), response.begin(),
                           response.end());
    if (!net::SendAll(client_descriptor, framed_response.data(),
                      framed_response.size(), &error))
      break;
    ObserveDns(query, response, selected_application);
    upload_bytes_.fetch_add(framed_query.size());
    download_bytes_.fetch_add(framed_response.size());
  }
  if (remote.Get() >= 0)
    UnregisterSocket(remote.Get());
}

bool ProxyRuntime::ResolveTarget(const socks5::TargetAddress &target,
                                 bool selected_application,
                                 socks5::TargetAddress *resolved,
                                 std::string *error) {
  if (net::IsIpLiteral(target.host)) {
    *resolved = target;
    return true;
  }
  for (uint16_t query_type :
       {static_cast<uint16_t>(1), static_cast<uint16_t>(28)}) {
    std::vector<uint8_t> query;
    if (!net::BuildDnsQuery(target.host, query_type, &query)) {
      *error = "域名格式无法编码为 DNS 查询";
      return false;
    }
    for (const Endpoint &server : DnsSnapshot()) {
      sockaddr_storage dns_address{};
      socklen_t dns_length = 0;
      if (!net::ResolveEndpoint(server, &dns_address, &dns_length, error))
        continue;
      const int descriptor = CreateDirectUdpSocket(dns_address.ss_family, this);
      if (descriptor < 0)
        continue;
      net::UniqueSocket dns_socket(descriptor);
      if (connect(descriptor, reinterpret_cast<const sockaddr *>(&dns_address),
                  dns_length) != 0 ||
          send(descriptor, query.data(), query.size(), 0) !=
              static_cast<ssize_t>(query.size())) {
        UnregisterSocket(descriptor);
        continue;
      }
      std::array<uint8_t, 65535> buffer{};
      const ssize_t received =
          recv(descriptor, buffer.data(), buffer.size(), 0);
      UnregisterSocket(descriptor);
      if (received <= 0)
        continue;
      std::vector<uint8_t> response(buffer.begin(), buffer.begin() + received);
      // UDP DNS 的 TC 位表示回答被截断；必须向同一指定服务器走
      // TCP，不能改用系统解析器。
      if (response.size() >= 3 && (response[2] & 0x02U) != 0 &&
          !QueryDnsTcp(server, query, &response, error)) {
        continue;
      }
      const net::DnsAddressResult parsed =
          net::ParseDnsAddresses(query, response);
      if (parsed.addresses.empty())
        continue;
      ObserveDns(query, response, selected_application);
      resolved->host = parsed.addresses.front();
      resolved->port = target.port;
      return true;
    }
  }
  *error = "规则指定 DNS 未能解析目标域名";
  return false;
}

bool ProxyRuntime::QueryDnsTcp(const Endpoint &server,
                               const std::vector<uint8_t> &query,
                               std::vector<uint8_t> *response,
                               std::string *error) {
  net::UniqueSocket connection(
      net::ConnectTcp(server, kConnectTimeoutMs, this, error));
  if (connection.Get() < 0)
    return false;
  timeval timeout{kUdpTimeoutSeconds, 0};
  if (setsockopt(connection.Get(), SOL_SOCKET, SO_RCVTIMEO, &timeout,
                 sizeof(timeout)) != 0) {
    *error = "设置 DNS TCP 响应超时失败";
    UnregisterSocket(connection.Get());
    return false;
  }
  const uint16_t length = static_cast<uint16_t>(query.size());
  const std::array<uint8_t, 2> prefix{static_cast<uint8_t>(length >> 8U),
                                      static_cast<uint8_t>(length)};
  if (!net::SendAll(connection.Get(), prefix.data(), prefix.size(), error) ||
      !net::SendAll(connection.Get(), query.data(), query.size(), error)) {
    UnregisterSocket(connection.Get());
    return false;
  }
  std::array<uint8_t, 2> response_prefix{};
  if (!net::ReceiveAll(connection.Get(), response_prefix.data(),
                       response_prefix.size(), error)) {
    UnregisterSocket(connection.Get());
    return false;
  }
  const std::size_t response_length =
      (static_cast<std::size_t>(response_prefix[0]) << 8U) | response_prefix[1];
  if (response_length < 12 || response_length > 65535) {
    *error = "DNS TCP 响应长度无效";
    UnregisterSocket(connection.Get());
    return false;
  }
  response->resize(response_length);
  const bool received = net::ReceiveAll(connection.Get(), response->data(),
                                        response->size(), error);
  UnregisterSocket(connection.Get());
  return received;
}

socks5::TcpConnectResult
ProxyRuntime::ConnectTcpRoute(const socks5::TargetAddress &target,
                              core::RouteAction action,
                              bool selected_application, std::string *error) {
  socks5::TcpConnectResult result;
  // 代理出口必须把已恢复的域名原样交给上游 SOCKS5。统一预解析会让 Host/SNI
  // 再次退化为 IP，服务端事务因此失去应用原始域名；只有 DIRECT 才在本地解析。
  if (action == core::RouteAction::kProxy) {
    return socks5::ConnectUpstreamTcp(config_, target, this, error);
  }
  socks5::TargetAddress resolved;
  if (!ResolveTarget(target, selected_application, &resolved, error))
    return result;
  result.descriptor = net::ConnectTcp({resolved.host, resolved.port},
                                      kConnectTimeoutMs, this, error);
  if (result.descriptor < 0) {
    result.reply_status = SocksReplyForErrno(net::LastConnectError());
    return result;
  }
  result.bound = ReadBoundEndpoint(result.descriptor);
  result.reply_status = 0;
  return result;
}

void ProxyRuntime::HandleTcp(int client_descriptor,
                             socks5::TargetAddress target,
                             bool reply_socks_success,
                             bool selected_application) {
  if (target.port == 53) {
    HandleTcpDns(client_descriptor, reply_socks_success, selected_application);
    return;
  }
  std::vector<uint8_t> prefix;
  const std::vector<std::string> cached_domains =
      net::IsIpLiteral(target.host)
          ? LookupDomains(target.host, selected_application)
          : std::vector<std::string>{target.host};
  // 共享 IP 取同作用域最近一次 DNS 观察作为候选，再允许严格 Host/SNI 覆盖；
  // 嗅探超时也不会把已经观察到的域名重新降级成 IP。
  std::string domain =
      cached_domains.empty() ? std::string() : cached_domains.front();
  const std::shared_ptr<const core::RoutingRules> rules = RulesSnapshot();
  // DoT 端口承载加密 DNS，目的地址改写会破坏证书语义；统一拒绝可防止其绕入
  // Sprak 上游。
  if (target.port == 853) {
    if (reply_socks_success)
      socks5::SendServerReply(client_descriptor, 2, {"0.0.0.0", 0});
    failed_connections_.fetch_add(1);
    return;
  }
  // 域名身份不仅服务于 DOMAIN
  // 规则，也属于事务观测模型的一部分。旧实现仅在当前规则含域名条件时 读取 HTTP
  // Host/TLS SNI，FINAL,PROXY 等常见规则会永久只上报 IP；当 DNS 被浏览器缓存或
  // DoH 绕过传统 53 端口时更无法恢复。只要数值目标没有唯一可信 DNS
  // 映射就执行有界首包嗅探， 使规则计算与上游 SOCKS5 DOMAIN
  // 同时使用同一份严格验证结果。
  const bool requires_domain_sniff = target.port != 53 &&
                                     cached_domains.size() != 1 &&
                                     net::IsIpLiteral(target.host);
  const bool optimistic_sniff = reply_socks_success && requires_domain_sniff;
  bool success_replied = false;
  // 标准客户端在 REP=0 前不会发送首包。只有数值目标且缓存无法唯一归属时使用
  // 受限乐观分支；该分支后续连接失败只能以 EOF 表达。其余请求均先真实
  // 连接再 REP。共享 IP 命中多个缓存域名时必须重新信任首包 SNI/Host，不能退化为
  // IP。
  if (optimistic_sniff) {
    success_replied =
        socks5::SendServerReply(client_descriptor, 0, {"0.0.0.0", 0});
    if (!success_replied) {
      failed_connections_.fetch_add(1);
      return;
    }
  }
  // 透明入口与 HEV
  // 内部入口都需要恢复域名身份；读取仍受固定时限约束，server-first
  // 协议不会无限等待。
  if ((!reply_socks_success || optimistic_sniff) && requires_domain_sniff) {
    ReadSniffPrefix(client_descriptor, &prefix);
  }
  if (!prefix.empty()) {
    const net::DomainSniffResult sniffed = net::DomainSniffer::Sniff(prefix);
    if (sniffed.matched)
      domain = sniffed.domain;
  }
  const core::RouteMatchResult route =
      EvaluateTarget(*rules, target, domain, selected_application);
  if (route.action == core::RouteAction::kReject) {
    if (reply_socks_success && !success_replied) {
      socks5::SendServerReply(client_descriptor, 2, {"0.0.0.0", 0});
    }
    failed_connections_.fetch_add(1);
    return;
  }

  socks5::TargetAddress upstream_target = target;
  // HEV 交给本地入口的是数值地址，旧实现虽然用 DNS 缓存或首包嗅探恢复了域名，
  // 却只把域名用于规则匹配，向 Sprak 服务建立隧道时仍发送原始 IP，导致事务列表
  // 永远丢失 Host/SNI 身份。仅代理出口改用已严格验证的域名，直连出口继续固定原
  // IP； 这样既保留 DIRECT 的原目标语义，也让服务端按 SOCKS5 DOMAIN
  // 记录和解析域名。
  if (route.action == core::RouteAction::kProxy && !domain.empty()) {
    upstream_target.host = domain;
  }

  std::string error;
  const socks5::TcpConnectResult connection = ConnectTcpRoute(
      upstream_target, route.action, selected_application, &error);
  net::UniqueSocket remote(connection.descriptor);
  if (remote.Get() < 0) {
    if (reply_socks_success && !success_replied) {
      socks5::SendServerReply(client_descriptor, connection.reply_status,
                              {"0.0.0.0", 0});
    }
    failed_connections_.fetch_add(1);
    return;
  }
  if (reply_socks_success && !success_replied &&
      !socks5::SendServerReply(
          client_descriptor, 0,
          {connection.bound.host, connection.bound.port})) {
    UnregisterSocket(remote.Get());
    failed_connections_.fetch_add(1);
    return;
  }
  if (!prefix.empty() &&
      !net::SendAll(remote.Get(), prefix.data(), prefix.size(), &error)) {
    UnregisterSocket(remote.Get());
    failed_connections_.fetch_add(1);
    return;
  }
  upload_bytes_.fetch_add(prefix.size());
  RelayTcp({client_descriptor, remote.Get(), &running_, &upload_bytes_,
            &download_bytes_});
  UnregisterSocket(remote.Get());
}

} // namespace routesocks::runtime
