#include "proxy_runtime_udp_internal.h"

#include <arpa/inet.h>
#include <netinet/in.h>
#include <poll.h>
#include <unistd.h>

#include <algorithm>
#include <array>
#include <chrono>
#include <cstring>
#include <utility>

#include "net/dns_protocol.h"
#include "net/socket_utils.h"

namespace routesocks::runtime {
namespace {

constexpr int kUdpTimeoutSeconds = 2;
constexpr auto kUdpIdleLifetime = std::chrono::seconds(90);
constexpr std::size_t kMaximumUdpPeers = 1024;
constexpr std::size_t kMaximumQueuedPeerDatagrams = 128;
constexpr std::size_t kMaximumDirectUdpTargetsPerPeer = 64;
constexpr std::size_t kMaximumUdpControls = 256;
constexpr std::size_t kMaximumRetiredUdpDescriptors = 4096;
// 发送与 reactor 共用 fd 关闭边界；10ms 上限避免 LRU 换槽对首包引入可感延迟。
constexpr auto kUdpPollInterval = std::chrono::milliseconds(10);

/** 返回进程内单调毫秒，供控制、发送和接收线程比较 UDP 活跃时间。 */
int64_t NowMonotonicMillis() {
  return std::chrono::duration_cast<std::chrono::milliseconds>(
             std::chrono::steady_clock::now().time_since_epoch())
      .count();
}

/** 创建受运行时统一登记的直连 UDP socket；登记或超时配置失败时关闭并返回 -1。
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

} // namespace

/**
 * 构造一个 peer 独占的 UDP 转发状态；socket 直到首包到达才按需建立。
 * 配置和响应路由在构造时形成快照，分配异常直接交由调用方撤销关联。
 */
ProxyRuntime::UdpPeerSession::UdpPeerSession(
    RuntimeConfig configuration, net::SocketObserver *socket_observer,
    bool selected, UdpResponseRoute response)
    : config(std::move(configuration)), observer(socket_observer),
      selected_application(selected), response_route(std::move(response)),
      last_used_millis(NowMonotonicMillis()) {}

/**
 * 幂等关闭 peer 持有的全部 socket 与待处理报文。
 * operation_mutex 必须先于 reactor 锁取得，避免慢速上游握手把全局 UDP
 * 接收循环卡住； 关闭开始后所有发送路径都会观察 closing，失败语义是立即停止该
 * peer 且不再重建通道。
 */
void ProxyRuntime::CloseUdpSession(
    const std::shared_ptr<UdpPeerSession> &session) {
  if (session->closing.exchange(true))
    return;
  {
    std::lock_guard<std::mutex> pending_lock(session->pending_mutex);
    session->pending_packets.clear();
  }
  std::unique_lock<std::mutex> operation_lock(session->operation_mutex);
  std::lock_guard<std::mutex> reactor_lock(udp_reactor_mutex_);
  for (const auto &entry : session->direct_channels) {
    if (session->observer != nullptr)
      session->observer->UnregisterSocket(entry.second.descriptor);
    shutdown(entry.second.descriptor, SHUT_RDWR);
    close(entry.second.descriptor);
  }
  session->direct_channels.clear();
  session->pending_dns.clear();
  session->upstream.Close();
  session->upstream_open = false;
  DrainRetiredUdpDescriptors();
}

/** 将报文放入 peer 有界队列；停止、关闭或容量耗尽时返回 false 且不保留任务。 */
bool ProxyRuntime::EnqueueUdpPacket(
    const std::shared_ptr<UdpPeerSession> &session, UdpPacketTask task) {
  {
    std::lock_guard<std::mutex> lock(session->pending_mutex);
    if (!accepting_sessions_.load() || session->closing.load() ||
        session->pending_packets.size() >= kMaximumQueuedPeerDatagrams)
      return false;
    session->pending_packets.push_back(std::move(task));
    if (session->worker_scheduled)
      return true;
    session->worker_scheduled = true;
  }
  auto self = shared_from_this();
  bool submitted = false;
  try {
    submitted = accepting_sessions_.load() &&
                datagramWorkers_.Submit(
                    [self, session]() { self->RunUdpPeer(session); });
  } catch (...) {
    // UDP 报文的任务对象或队列扩容失败只丢弃当前 peer 队列；监听和接收 reactor
    // 必须继续运行，避免把瞬时内存压力扩大成全局 DNS 中断。
    submitted = false;
  }
  if (submitted)
    return true;
  std::lock_guard<std::mutex> lock(session->pending_mutex);
  session->pending_packets.clear();
  session->worker_scheduled = false;
  return false;
}

/** 在工作线程串行排空单个 peer；任一异常都会清队列并解除调度标记。 */
void ProxyRuntime::RunUdpPeer(const std::shared_ptr<UdpPeerSession> &session) {
  while (true) {
    UdpPacketTask task;
    {
      std::lock_guard<std::mutex> lock(session->pending_mutex);
      if (session->closing.load() || session->pending_packets.empty()) {
        session->pending_packets.clear();
        session->worker_scheduled = false;
        return;
      }
      task = std::move(session->pending_packets.front());
      session->pending_packets.pop_front();
    }
    bool sent = false;
    try {
      sent = SendUdp(session.get(), task.target, task.payload);
    } catch (...) {
      // 当前 peer
      // 的工作标记必须在异常路径清除，否则后续数据报只会入队而永远不再调度。
      std::lock_guard<std::mutex> lock(session->pending_mutex);
      session->pending_packets.clear();
      session->worker_scheduled = false;
      failed_connections_.fetch_add(1);
      return;
    }
    if (!sent) {
      failed_connections_.fetch_add(1);
      continue;
    }
    upload_bytes_.fetch_add(task.payload.size());
  }
}

/** 在回复 REP=0 前原子预留关联；停机、热更、容量满或分配失败均返回空指针。 */
std::shared_ptr<ProxyRuntime::UdpAssociation>
ProxyRuntime::ReserveUdpControl(int control_descriptor, int datagram_descriptor,
                                bool selected_application,
                                socks5::TargetAddress requested_peer) {
  std::lock_guard<std::mutex> lock(udp_control_mutex_);
  if (!running_.load() || !accepting_sessions_.load() ||
      udp_associations_.size() >= kMaximumUdpControls) {
    return nullptr;
  }
  try {
    auto association = std::make_shared<UdpAssociation>();
    association->control_descriptor = control_descriptor;
    association->datagram_descriptor = datagram_descriptor;
    association->selected_application = selected_application;
    association->requested_peer = std::move(requested_peer);
    if (!udp_associations_.emplace(control_descriptor, association).second)
      return nullptr;
    return association;
  } catch (const std::bad_alloc &) {
    // 容量对象分配失败与达到硬上限共用同一个“未预留”语义，调用方可安全回复
    // REP=1。
    return nullptr;
  }
}

/** 按关联身份移除并关闭控制、数据与 peer 资源；描述符复用不会误关新会话。 */
void ProxyRuntime::CloseUdpControl(
    const std::shared_ptr<UdpAssociation> &association) {
  {
    std::lock_guard<std::mutex> lock(udp_control_mutex_);
    const auto iterator =
        udp_associations_.find(association->control_descriptor);
    if (iterator == udp_associations_.end() || iterator->second != association)
      return;
    udp_associations_.erase(iterator);
  }
  // reactor 快照携带 association 身份而非裸 fd；即使 fd
  // 数值被内核复用，也不会误删后继会话。 数据 reactor 可能正在用 datagram
  // 回写响应，必须先取得关闭边界锁再释放 fd 数值。
  if (association->session != nullptr) {
    CloseUdpSession(association->session);
    {
      std::lock_guard<std::mutex> lock(udp_mutex_);
      active_udp_sessions_.erase(std::remove(active_udp_sessions_.begin(),
                                             active_udp_sessions_.end(),
                                             association->session),
                                 active_udp_sessions_.end());
    }
    if (association->session->active_counted.exchange(false))
      active_connections_.fetch_sub(1);
    association->session.reset();
  }
  {
    std::lock_guard<std::mutex> reactor_lock(udp_reactor_mutex_);
    UnregisterConnection(association->control_descriptor);
    shutdown(association->control_descriptor, SHUT_RDWR);
    close(association->control_descriptor);
    UnregisterSocket(association->datagram_descriptor);
    close(association->datagram_descriptor);
    DrainRetiredUdpDescriptors();
  }
  udp_control_condition_.notify_all();
}

/** 接收已认证关联的数据报；首个合法源原子锁定 peer，其他来源直接丢弃。 */
void ProxyRuntime::ReceiveAssociatedUdp(
    const std::shared_ptr<UdpAssociation> &association) {
  std::array<uint8_t, 65535> buffer{};
  sockaddr_storage peer{};
  socklen_t peer_length = sizeof(peer);
  const ssize_t received =
      recvfrom(association->datagram_descriptor, buffer.data(), buffer.size(),
               MSG_DONTWAIT, reinterpret_cast<sockaddr *>(&peer), &peer_length);
  if (received <= 0)
    return;
  socks5::TargetAddress target;
  std::vector<uint8_t> payload;
  if (!socks5::ParseUdpFrame(buffer.data(), static_cast<std::size_t>(received),
                             &target, &payload)) {
    failed_connections_.fetch_add(1);
    return;
  }
  const std::string key = net::AddressKey(peer, peer_length);
  if (key.empty() || peer.ss_family != AF_INET) {
    failed_connections_.fetch_add(1);
    return;
  }
  const auto *ipv4_peer = reinterpret_cast<const sockaddr_in *>(&peer);
  char peer_host[INET_ADDRSTRLEN]{};
  if (inet_ntop(AF_INET, &ipv4_peer->sin_addr, peer_host, sizeof(peer_host)) ==
      nullptr)
    return;
  const uint16_t peer_port = ntohs(ipv4_peer->sin_port);
  const bool requested_host_matches =
      association->requested_peer.host.empty() ||
      association->requested_peer.host == "0.0.0.0" ||
      association->requested_peer.host == peer_host;
  const bool requested_port_matches =
      association->requested_peer.port == 0 ||
      association->requested_peer.port == peer_port;
  // RFC1928
  // 的零端点由首个合法数据报原子锁定；后续不同来源即使知道临时端口也不能借用关联。
  if (!requested_host_matches || !requested_port_matches ||
      (!association->bound_peer.empty() && association->bound_peer != key)) {
    failed_connections_.fetch_add(1);
    return;
  }
  if (association->bound_peer.empty())
    association->bound_peer = key;
  if (association->session == nullptr) {
    std::lock_guard<std::mutex> lifecycle_lock(session_lifecycle_mutex_);
    if (!accepting_sessions_.load()) {
      failed_connections_.fetch_add(1);
      return;
    }
    UdpResponseRoute response{peer, peer_length, false,
                              association->datagram_descriptor};
    association->session = std::make_shared<UdpPeerSession>(
        config_, this, association->selected_application, std::move(response));
    {
      std::lock_guard<std::mutex> lock(udp_mutex_);
      active_udp_sessions_.push_back(association->session);
    }
    active_connections_.fetch_add(1);
    accepted_connections_.fetch_add(1);
  }
  if (!EnqueueUdpPacket(association->session,
                        {std::move(target), std::move(payload)})) {
    failed_connections_.fetch_add(1);
  }
}

/** 单线程轮询全部控制连接与关联端口；事件处理前重新验证关联身份。 */
void ProxyRuntime::RunUdpControls() {
  std::array<uint8_t, 64> discard{};
  while (running_.load()) {
    std::vector<std::shared_ptr<UdpAssociation>> associations;
    {
      std::lock_guard<std::mutex> lock(udp_control_mutex_);
      associations.reserve(udp_associations_.size());
      for (const auto &entry : udp_associations_)
        associations.push_back(entry.second);
    }
    std::vector<pollfd> events;
    events.reserve(associations.size() * 2);
    for (const auto &association : associations) {
      events.push_back({association->control_descriptor,
                        static_cast<short>(POLLIN | POLLERR | POLLHUP), 0});
      events.push_back({association->datagram_descriptor,
                        static_cast<short>(POLLIN | POLLERR | POLLHUP), 0});
    }
    if (events.empty()) {
      std::this_thread::sleep_for(kUdpPollInterval);
      continue;
    }
    if (poll(events.data(), events.size(),
             static_cast<int>(kUdpPollInterval.count())) <= 0)
      continue;
    for (std::size_t index = 0; index < associations.size(); ++index) {
      const short control_events = events[index * 2].revents;
      const short datagram_events = events[index * 2 + 1].revents;
      bool close_association = false;
      {
        // poll 快照只保存整数 fd；实际 recv/recvfrom 前必须在所有权锁下重验
        // association 身份。 CloseUdpControl 先从同一 map 移除对象再关
        // fd，因此此临界区内整数绝不会被复用。
        std::lock_guard<std::mutex> ownership_lock(udp_control_mutex_);
        const auto current =
            udp_associations_.find(associations[index]->control_descriptor);
        if (current == udp_associations_.end() ||
            current->second != associations[index]) {
          continue;
        }
        if (control_events != 0) {
          const ssize_t received =
              recv(associations[index]->control_descriptor, discard.data(),
                   discard.size(), MSG_DONTWAIT);
          close_association =
              received <= 0 || (control_events & (POLLERR | POLLHUP)) != 0;
        }
        if (!close_association && (datagram_events & POLLIN) != 0)
          ReceiveAssociatedUdp(associations[index]);
        if ((datagram_events & (POLLERR | POLLHUP)) != 0)
          close_association = true;
      }
      if (close_association)
        CloseUdpControl(associations[index]);
    }
  }
  std::vector<std::shared_ptr<UdpAssociation>> remaining;
  {
    std::lock_guard<std::mutex> lock(udp_control_mutex_);
    for (const auto &entry : udp_associations_)
      remaining.push_back(entry.second);
  }
  for (const auto &association : remaining)
    CloseUdpControl(association);
}

/** 移除已关闭或超时 peer；清理同时精确归还活动连接计数。 */
void ProxyRuntime::PruneUdpSessions(
    std::unordered_map<std::string, std::shared_ptr<UdpPeerSession>>
        *sessions) {
  const int64_t now_millis = NowMonotonicMillis();
  for (auto iterator = sessions->begin(); iterator != sessions->end();) {
    bool busy = false;
    {
      std::lock_guard<std::mutex> pending_lock(iterator->second->pending_mutex);
      busy = iterator->second->worker_scheduled ||
             !iterator->second->pending_packets.empty();
    }
    const int64_t idle_millis =
        now_millis - iterator->second->last_used_millis.load();
    if (!iterator->second->closing.load() &&
        (busy ||
         idle_millis <= std::chrono::duration_cast<std::chrono::milliseconds>(
                            kUdpIdleLifetime)
                            .count())) {
      ++iterator;
      continue;
    }
    const std::shared_ptr<UdpPeerSession> expired_session = iterator->second;
    {
      std::lock_guard<std::mutex> lock(udp_mutex_);
      active_udp_sessions_.erase(std::remove(active_udp_sessions_.begin(),
                                             active_udp_sessions_.end(),
                                             expired_session),
                                 active_udp_sessions_.end());
    }
    CloseUdpSession(expired_session);
    iterator = sessions->erase(iterator);
    if (expired_session->active_counted.exchange(false))
      active_connections_.fetch_sub(1);
  }
}

/** 为新直连目标淘汰无在途 DNS 的最久未用通道；全部忙碌时返回 false。 */
bool ProxyRuntime::EnsureDirectUdpCapacity(UdpPeerSession *session) {
  if (session->direct_channels.size() < kMaximumDirectUdpTargetsPerPeer)
    return true;
  auto victim = session->direct_channels.end();
  for (auto candidate = session->direct_channels.begin();
       candidate != session->direct_channels.end(); ++candidate) {
    const auto pending = session->pending_dns.find(candidate->first);
    if (pending != session->pending_dns.end() && !pending->second.empty())
      continue;
    if (victim == session->direct_channels.end() ||
        candidate->second.last_used_millis < victim->second.last_used_millis) {
      victim = candidate;
    }
  }
  if (victim == session->direct_channels.end())
    return false;
  if (!RetireUdpDescriptor(victim->second.descriptor))
    return false;
  session->direct_channels.erase(victim);
  return true;
}

/** 把失去会话索引的 fd 放入有界生命周期队列；实际 close 只由 reactor 边界执行。
 */
bool ProxyRuntime::RetireUdpDescriptor(int descriptor) {
  std::lock_guard<std::mutex> lock(retired_udp_mutex_);
  if (retired_udp_descriptors_.size() >= kMaximumRetiredUdpDescriptors)
    return false;
  retired_udp_descriptors_.push_back(descriptor);
  return true;
}

/** 注销并关闭已退役 fd；调用方持有 udp_reactor_mutex_，因此没有在途 poll 快照。
 */
void ProxyRuntime::DrainRetiredUdpDescriptors() {
  std::vector<int> descriptors;
  {
    std::lock_guard<std::mutex> lock(retired_udp_mutex_);
    descriptors.swap(retired_udp_descriptors_);
  }
  for (int descriptor : descriptors) {
    UnregisterSocket(descriptor);
    close(descriptor);
  }
}

/** 将传统 DNS 固定发往指定服务器并登记完整问题；事务槽不足或发送失败返回
 * false。 */
bool ProxyRuntime::SendDnsUdp(UdpPeerSession *session,
                              const socks5::TargetAddress &original_target,
                              const std::vector<uint8_t> &payload,
                              std::size_t server_index) {
  if (payload.size() < 2)
    return false;
  const std::vector<Endpoint> servers = DnsSnapshot();
  if (server_index >= servers.size())
    return false;
  sockaddr_storage address{};
  socklen_t address_length = 0;
  std::string error;
  if (!net::ResolveEndpoint(servers[server_index], &address, &address_length,
                            &error))
    return false;
  const std::string endpoint_key = net::AddressKey(address, address_length);
  std::unique_lock<std::mutex> operation_lock(session->operation_mutex);
  if (session->closing.load())
    return false;
  auto channel = session->direct_channels.find(endpoint_key);
  if (channel == session->direct_channels.end()) {
    if (!EnsureDirectUdpCapacity(session))
      return false;
    const int descriptor = CreateDirectUdpSocket(address.ss_family, this);
    if (descriptor < 0)
      return false;
    if (connect(descriptor, reinterpret_cast<const sockaddr *>(&address),
                address_length) != 0) {
      UnregisterSocket(descriptor);
      close(descriptor);
      return false;
    }
    socks5::TargetAddress dns_target{servers[server_index].host, 53};
    channel = session->direct_channels
                  .emplace(endpoint_key,
                           UdpPeerSession::DirectChannel{descriptor,
                                                         std::move(dns_target),
                                                         NowMonotonicMillis()})
                  .first;
  }
  const uint16_t transaction =
      static_cast<uint16_t>((payload[0] << 8U) | payload[1]);
  auto &transactions = session->pending_dns[endpoint_key];
  std::size_t pending_count = 0;
  for (const auto &pending : transactions)
    pending_count += pending.second.size();
  if (pending_count >= kMaximumQueuedPeerDatagrams)
    return false;
  auto &transaction_queue = transactions[transaction];
  transaction_queue.push_back(
      {payload, original_target,
       NowMonotonicMillis() + kUdpTimeoutSeconds * 1000LL, server_index});
  if (send(channel->second.descriptor, payload.data(), payload.size(), 0) ==
      static_cast<ssize_t>(payload.size())) {
    channel->second.last_used_millis = NowMonotonicMillis();
    return true;
  }
  transaction_queue.pop_back();
  if (transaction_queue.empty())
    transactions.erase(transaction);
  return false;
}

/** 复用或创建目标独占直连通道；满载慢路径与 reactor 串行关闭旧 fd。 */
bool ProxyRuntime::SendDirectUdp(UdpPeerSession *session,
                                 const socks5::TargetAddress &target,
                                 const std::vector<uint8_t> &payload) {
  sockaddr_storage address{};
  socklen_t address_length = 0;
  std::string error;
  if (!net::ResolveEndpoint({target.host, target.port}, &address,
                            &address_length, &error))
    return false;
  const std::string endpoint_key = net::AddressKey(address, address_length);
  std::unique_lock<std::mutex> operation_lock(session->operation_mutex);
  if (session->closing.load())
    return false;
  auto channel = session->direct_channels.find(endpoint_key);
  if (channel == session->direct_channels.end()) {
    if (!EnsureDirectUdpCapacity(session))
      return false;
    const int descriptor = CreateDirectUdpSocket(address.ss_family, this);
    if (descriptor < 0)
      return false;
    // 每个远端独占 connected socket，长期接收循环可把迟到包按真实目标异步回写。
    if (connect(descriptor, reinterpret_cast<const sockaddr *>(&address),
                address_length) != 0) {
      UnregisterSocket(descriptor);
      close(descriptor);
      return false;
    }
    channel = session->direct_channels
                  .emplace(endpoint_key,
                           UdpPeerSession::DirectChannel{descriptor, target,
                                                         NowMonotonicMillis()})
                  .first;
  }
  const bool sent =
      send(channel->second.descriptor, payload.data(), payload.size(), 0) ==
      static_cast<ssize_t>(payload.size());
  if (sent)
    channel->second.last_used_millis = NowMonotonicMillis();
  return sent;
}

/** 按统一规则异步发送 UDP；53 端口优先走指定 DNS，关闭后禁止重开通道。 */
bool ProxyRuntime::SendUdp(UdpPeerSession *session,
                           const socks5::TargetAddress &target,
                           const std::vector<uint8_t> &payload) {
  if (session->closing.load())
    return false;
  session->last_used_millis.store(NowMonotonicMillis());
  if (target.port == 853)
    return false;
  // 传统 DNS 是数据面基础设施，始终直连规则指定主备；FINAL/REJECT
  // 仅约束业务流量，不能截断解析。
  if (target.port == 53)
    return SendDnsUdp(session, target, payload, 0);
  const std::shared_ptr<const core::RoutingRules> rules = RulesSnapshot();
  const std::vector<std::string> cached_domains =
      net::IsIpLiteral(target.host)
          ? LookupDomains(target.host, session->selected_application)
          : std::vector<std::string>{target.host};
  const std::string observed_domain =
      cached_domains.empty() ? std::string() : cached_domains.front();
  const core::RouteMatchResult route = EvaluateTarget(
      *rules, target, observed_domain, session->selected_application);
  if (route.action == core::RouteAction::kReject)
    return false;
  socks5::TargetAddress routed_target = target;
  if (route.action == core::RouteAction::kProxy && !observed_domain.empty()) {
    routed_target.host = observed_domain;
  }
  std::string error;
  if (route.action == core::RouteAction::kDirect) {
    socks5::TargetAddress resolved;
    if (!ResolveTarget(target, session->selected_application, &resolved,
                       &error))
      return false;
    return SendDirectUdp(session, resolved, payload);
  }
  std::unique_lock<std::mutex> operation_lock(session->operation_mutex);
  if (session->closing.load())
    return false;
  if (session->upstream_open && session->upstream.ControlConnectionAlive())
    return session->upstream.Send(routed_target, payload, &error);
  // 旧 fd 的关闭必须进入 reactor
  // 边界，但新连接的网络握手只持当前会话锁；否则一个 SOCKS UDP ASSOCIATE
  // 慢握手会冻结所有会话已经到达的 DNS 响应。
  if (session->upstream_open && !session->upstream.ControlConnectionAlive()) {
    std::lock_guard<std::mutex> reactor_lock(udp_reactor_mutex_);
    session->upstream.Close();
    session->upstream_open = false;
  }
  if (!session->upstream_open) {
    session->upstream_open = session->upstream.Open(config_, this, &error);
    if (!session->upstream_open)
      return false;
  }
  return session->upstream.Send(routed_target, payload, &error);
}

/**
 * 按入口协议回写一个 UDP 响应。
 * session 提供原客户端端点和生命周期锁，target 是远端真实来源，payload
 * 是原始响应正文； SOCKS 入口封装 RFC1928 帧；Root REDIRECT
 * 入口必须从原监听端口回写，让 conntrack 反向恢复 target
 * 源地址。发送失败时不伪造回包，只累计不到下载流量并等待后续数据报重试。
 */
void ProxyRuntime::SendUdpResponse(UdpPeerSession *session,
                                   const socks5::TargetAddress &target,
                                   const std::vector<uint8_t> &payload) {
  const UdpResponseRoute &route = session->response_route;
  ssize_t sent = -1;
  if (route.transparent) {
    sent = sendto(route.response_descriptor, payload.data(), payload.size(), 0,
                  reinterpret_cast<const sockaddr *>(&route.peer),
                  route.peer_length);
  } else {
    std::vector<uint8_t> frame;
    if (!socks5::BuildUdpFrame(target, payload, &frame))
      return;
    sent = sendto(route.response_descriptor, frame.data(), frame.size(), 0,
                  reinterpret_cast<const sockaddr *>(&route.peer),
                  route.peer_length);
  }
  if (sent >= 0) {
    session->last_used_millis.store(NowMonotonicMillis());
    download_bytes_.fetch_add(payload.size());
  }
}

/** 共享 reactor 轮询全部直连和上游通道；关闭与 poll 快照由同一锁隔离。 */
void ProxyRuntime::RunUdpReceivers() {
  struct PollChannel {
    int descriptor;
    std::string key;
    socks5::TargetAddress target;
    bool upstream;
    bool upstream_control;
    std::shared_ptr<UdpPeerSession> session;
  };
  struct DnsRetry {
    socks5::TargetAddress target;
    std::vector<uint8_t> query;
    std::size_t server_index;
    std::shared_ptr<UdpPeerSession> session;
  };
  while (running_.load()) {
    // reactor 在构造整数 fd 快照到处理 revents 期间独占关闭边界，杜绝 fd
    // 被复用后误投响应。
    std::unique_lock<std::mutex> reactor_lock(udp_reactor_mutex_);
    std::vector<std::shared_ptr<UdpPeerSession>> sessions;
    {
      std::lock_guard<std::mutex> lock(udp_mutex_);
      sessions = active_udp_sessions_;
    }
    const std::size_t dns_server_count = DnsSnapshot().size();
    std::vector<DnsRetry> retries;
    std::vector<PollChannel> channels;
    for (const auto &session : sessions) {
      if (session->closing.load())
        continue;
      std::unique_lock<std::mutex> lock(session->operation_mutex,
                                        std::try_to_lock);
      if (!lock.owns_lock())
        continue;
      const int64_t now = NowMonotonicMillis();
      for (auto &endpoint : session->pending_dns) {
        for (auto transaction = endpoint.second.begin();
             transaction != endpoint.second.end();) {
          auto &transaction_queue = transaction->second;
          for (auto pending = transaction_queue.begin();
               pending != transaction_queue.end();) {
            if (pending->expiry_millis > now) {
              ++pending;
              continue;
            }
            const std::size_t next_server = pending->server_index + 1;
            if (next_server < dns_server_count) {
              retries.push_back({pending->response_target, pending->query,
                                 next_server, session});
            }
            pending = transaction_queue.erase(pending);
          }
          if (transaction_queue.empty())
            transaction = endpoint.second.erase(transaction);
          else
            ++transaction;
        }
      }
      for (const auto &entry : session->direct_channels) {
        channels.push_back({entry.second.descriptor, entry.first,
                            entry.second.response_target, false, false,
                            session});
      }
      if (session->upstream_open && session->upstream.Descriptor() >= 0) {
        channels.push_back(
            {session->upstream.Descriptor(), {}, {}, true, false, session});
        channels.push_back({session->upstream.ControlDescriptor(),
                            {},
                            {},
                            false,
                            true,
                            session});
      }
    }
    std::vector<pollfd> events;
    events.reserve(channels.size());
    for (const PollChannel &channel : channels) {
      events.push_back({channel.descriptor,
                        static_cast<short>(POLLIN | POLLERR | POLLHUP), 0});
    }
    const int ready = poll(events.data(), events.size(),
                           static_cast<int>(kUdpPollInterval.count()));
    if (ready > 0) {
      for (std::size_t index = 0; index < events.size(); ++index) {
        if (events[index].revents == 0)
          continue;
        const std::shared_ptr<UdpPeerSession> &session =
            channels[index].session;
        if (session->closing.load())
          continue;
        if (channels[index].upstream_control) {
          std::unique_lock<std::mutex> lock(session->operation_mutex,
                                            std::try_to_lock);
          if (!lock.owns_lock())
            continue;
          if (!session->upstream.ControlConnectionAlive()) {
            session->upstream.Close();
            session->upstream_open = false;
          }
          continue;
        }
        if ((events[index].revents & POLLIN) == 0)
          continue;
        socks5::TargetAddress response_target = channels[index].target;
        std::vector<uint8_t> response;
        std::vector<uint8_t> dns_query;
        {
          std::unique_lock<std::mutex> lock(session->operation_mutex,
                                            std::try_to_lock);
          if (!lock.owns_lock())
            continue;
          if (channels[index].upstream) {
            std::string error;
            if (!session->upstream.Receive(&response_target, &response, &error))
              continue;
          } else {
            std::array<uint8_t, 65535> buffer{};
            const ssize_t received = recv(channels[index].descriptor,
                                          buffer.data(), buffer.size(), 0);
            if (received <= 0)
              continue;
            response.assign(buffer.begin(), buffer.begin() + received);
            if (response_target.port == 53) {
              if (response.size() < 2)
                continue;
              const uint16_t transaction =
                  static_cast<uint16_t>((response[0] << 8U) | response[1]);
              auto endpoint = session->pending_dns.find(channels[index].key);
              if (endpoint == session->pending_dns.end())
                continue;
              auto transaction_queue = endpoint->second.find(transaction);
              if (transaction_queue == endpoint->second.end())
                continue;
              auto pending = std::find_if(
                  transaction_queue->second.begin(),
                  transaction_queue->second.end(),
                  [&](const UdpPeerSession::PendingDns &candidate) {
                    return net::DnsTransactionMatches(candidate.query,
                                                      response);
                  });
              if (pending == transaction_queue->second.end())
                continue;
              dns_query = pending->query;
              response_target = pending->response_target;
              transaction_queue->second.erase(pending);
              if (transaction_queue->second.empty())
                endpoint->second.erase(transaction_queue);
            }
          }
        }
        if (!dns_query.empty())
          ObserveDns(dns_query, response, session->selected_application);
        SendUdpResponse(session.get(), response_target, response);
      }
    }
    // 重试可能创建或退役通道；离开 poll 快照边界后执行，避免扩大 reactor
    // 临界区。
    DrainRetiredUdpDescriptors();
    reactor_lock.unlock();
    for (const DnsRetry &retry : retries) {
      SendDnsUdp(retry.session.get(), retry.target, retry.query,
                 retry.server_index);
    }
  }
  std::lock_guard<std::mutex> reactor_lock(udp_reactor_mutex_);
  DrainRetiredUdpDescriptors();
}

/**
 * 接收 NAT REDIRECT 交付的 Root UDP，并消费 NFQUEUE 在 ACCEPT
 * 前保存的真实目标。
 * 目标映射缺失说明队列与回环交付已经失去顺序一致性，该包必须丢弃而不能误发到本地端口。
 */
void ProxyRuntime::RunTransparentUdp(int listener, bool selected_application) {
  std::unordered_map<std::string, std::shared_ptr<UdpPeerSession>> sessions;
  std::array<uint8_t, 65535> buffer{};
  while (running_.load()) {
    sockaddr_storage peer{};
    socklen_t peer_length = sizeof(peer);
    const ssize_t received =
        recvfrom(listener, buffer.data(), buffer.size(), 0,
                 reinterpret_cast<sockaddr *>(&peer), &peer_length);
    if (received <= 0)
      continue;
    std::optional<socks5::TargetAddress> target =
        TakeQueuedUdpTarget(peer, selected_application);
    if (!target.has_value()) {
      failed_connections_.fetch_add(1);
      continue;
    }
    const std::string key = net::AddressKey(peer, peer_length);
    PruneUdpSessions(&sessions);
    if (key.empty() || (sessions.find(key) == sessions.end() &&
                        sessions.size() >= kMaximumUdpPeers)) {
      failed_connections_.fetch_add(1);
      continue;
    }
    auto [iterator, inserted] = sessions.try_emplace(key, nullptr);
    if (inserted) {
      std::lock_guard<std::mutex> lifecycle_lock(session_lifecycle_mutex_);
      if (!accepting_sessions_.load()) {
        sessions.erase(iterator);
        failed_connections_.fetch_add(1);
        continue;
      }
      UdpResponseRoute response{peer, peer_length, true, listener};
      iterator->second = std::make_shared<UdpPeerSession>(
          config_, this, selected_application, std::move(response));
      const std::shared_ptr<UdpPeerSession> created = iterator->second;
      std::lock_guard<std::mutex> lock(udp_mutex_);
      active_udp_sessions_.push_back(created);
      active_connections_.fetch_add(1);
      accepted_connections_.fetch_add(1);
    }
    std::vector<uint8_t> payload(buffer.begin(), buffer.begin() + received);
    const std::shared_ptr<UdpPeerSession> session = iterator->second;
    UdpPacketTask task{std::move(*target), std::move(payload)};
    if (!EnqueueUdpPacket(session, std::move(task))) {
      failed_connections_.fetch_add(1);
    }
  }
  for (auto &entry : sessions) {
    CloseUdpSession(entry.second);
    std::lock_guard<std::mutex> lock(udp_mutex_);
    active_udp_sessions_.erase(std::remove(active_udp_sessions_.begin(),
                                           active_udp_sessions_.end(),
                                           entry.second),
                               active_udp_sessions_.end());
    if (entry.second->active_counted.exchange(false))
      active_connections_.fetch_sub(1);
  }
}

} // namespace routesocks::runtime
