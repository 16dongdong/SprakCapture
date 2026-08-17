#include "engine/proxy_runtime.h"

#include "proxy_runtime_udp_internal.h"

#include <netinet/in.h>
#include <unistd.h>

#include <algorithm>
#include <array>
#include <chrono>
#include <stdexcept>
#include <utility>

#include "engine/vpn_socket_protector.h"
#include "net/dns_protocol.h"
#include "net/socket_utils.h"

namespace routesocks::runtime {
namespace {

constexpr std::size_t kMaximumDomainCacheKeys = 4096;
constexpr std::size_t kMaximumQueuedConnections = 256;
constexpr std::size_t kControlWorkerCount = 8;
constexpr std::size_t kConnectionWorkerCount = 48;
constexpr std::size_t kDatagramWorkerCount = 8;
constexpr std::size_t kMaximumActiveConnections = 1024;
constexpr std::size_t kMaximumQueuedUdpTargets = 8192;
constexpr std::size_t kMaximumQueuedUdpTargetsPerPeer = 128;
constexpr auto kQueuedUdpTargetLifetime = std::chrono::seconds(5);

/** 生成包含 selected/global 作用域的 DNS 缓存键，避免两个规则空间互相污染。 */
std::string DomainCacheKey(const std::string &address,
                           bool selected_application) {
  return std::string(selected_application ? "S|" : "G|") + address;
}

} // namespace

ProxyRuntime::ProxyRuntime(RuntimeConfig config, core::RoutingRules rules,
                           bool root_mode)
    : config_(std::move(config)), root_mode_(root_mode),
      rules_(std::make_shared<const core::RoutingRules>(std::move(rules))),
      controlWorkers_(kControlWorkerCount, kMaximumQueuedConnections,
                      [this]() { failed_connections_.fetch_add(1); }),
      connectionWorkers_(kConnectionWorkerCount, kMaximumQueuedConnections,
                         [this]() { failed_connections_.fetch_add(1); }),
      datagramWorkers_(kDatagramWorkerCount, kMaximumQueuedConnections,
                       [this]() { failed_connections_.fetch_add(1); }) {}

ProxyRuntime::~ProxyRuntime() { Stop(); }

bool ProxyRuntime::Start(std::string *error) {
  if (running_.exchange(true)) {
    *error = "Native 数据面已经启动";
    return false;
  }
  listener_healthy_.store(true);
  try {
    local_tcp_listener_ =
        net::CreateIpv4TcpListener(config_.local_socks_port, error);
    if (local_tcp_listener_ >= 0) {
      selected_tcp_listener_ =
          net::CreateIpv4TcpListener(config_.selected_socks_port, error);
    }
    if (root_mode_ && selected_tcp_listener_ >= 0) {
      global_udp_queue_ = std::make_unique<net::NetfilterQueue>(
          config_.global_udp_queue_number);
      selected_udp_queue_ = std::make_unique<net::NetfilterQueue>(
          config_.selected_udp_queue_number);
      if (!global_udp_queue_->Open(error) ||
          !selected_udp_queue_->Open(error)) {
        Stop();
        return false;
      }
      transparent_tcp_listener_ =
          net::CreateIpv4TcpListener(config_.transparent_tcp_port, error);
      if (transparent_tcp_listener_ >= 0) {
        transparent_udp_listener_ = net::CreateIpv4UdpListener(
            config_.transparent_udp_port, false, error);
      }
      if (transparent_udp_listener_ >= 0) {
        selected_transparent_tcp_listener_ = net::CreateIpv4TcpListener(
            config_.selected_transparent_tcp_port, error);
      }
      if (selected_transparent_tcp_listener_ >= 0) {
        selected_transparent_udp_listener_ = net::CreateIpv4UdpListener(
            config_.selected_transparent_udp_port, false, error);
      }
    }
    if (local_tcp_listener_ < 0 || selected_tcp_listener_ < 0 ||
        (root_mode_ &&
         (transparent_tcp_listener_ < 0 || transparent_udp_listener_ < 0 ||
          selected_transparent_tcp_listener_ < 0 ||
          selected_transparent_udp_listener_ < 0))) {
      Stop();
      return false;
    }
    accepting_sessions_.store(true);
    // 控制握手、长连接与数据报必须在监听线程启动前分别就绪；任一线程池创建失败由
    // Start 的事务边界统一 Stop，不能留下只启动一部分协议的数据面。
    controlWorkers_.Start();
    connectionWorkers_.Start();
    datagramWorkers_.Start();
    listener_threads_.emplace_back([this]() {
      RunListenerBoundary(ListenerRole::kLocalTcp, local_tcp_listener_, false);
    });
    listener_threads_.emplace_back([this]() {
      RunListenerBoundary(ListenerRole::kUdpControls, -1, false);
    });
    listener_threads_.emplace_back([this]() {
      RunListenerBoundary(ListenerRole::kUdpReceivers, -1, false);
    });
    listener_threads_.emplace_back([this]() {
      RunListenerBoundary(ListenerRole::kLocalTcp, selected_tcp_listener_,
                          true);
    });
    if (root_mode_) {
      listener_threads_.emplace_back([this]() {
        try {
          RunRootUdpQueue(global_udp_queue_.get(), false);
        } catch (...) {
          FailRuntimeFromListener();
        }
      });
      listener_threads_.emplace_back([this]() {
        try {
          RunRootUdpQueue(selected_udp_queue_.get(), true);
        } catch (...) {
          FailRuntimeFromListener();
        }
      });
      listener_threads_.emplace_back([this]() {
        RunListenerBoundary(ListenerRole::kTransparentTcp,
                            transparent_tcp_listener_, false);
      });
      listener_threads_.emplace_back([this]() {
        RunListenerBoundary(ListenerRole::kTransparentUdp,
                            transparent_udp_listener_, false);
      });
      listener_threads_.emplace_back([this]() {
        RunListenerBoundary(ListenerRole::kTransparentTcp,
                            selected_transparent_tcp_listener_, true);
      });
      listener_threads_.emplace_back([this]() {
        RunListenerBoundary(ListenerRole::kTransparentUdp,
                            selected_transparent_udp_listener_, true);
      });
    }
    return true;
  } catch (...) {
    // 线程容器扩容或 std::thread
    // 创建可能抛出；启动必须作为一个事务回收已打开的端口和线程。
    // 清理完成后保留原异常类型，由 JNI 统一边界转换为中文失败结果。
    Stop();
    throw;
  }
}

void ProxyRuntime::RunListenerBoundary(ListenerRole role, int listener,
                                       bool selected_application) noexcept {
  try {
    switch (role) {
    case ListenerRole::kLocalTcp:
      RunLocalTcp(listener, selected_application);
      return;
    case ListenerRole::kUdpControls:
      RunUdpControls();
      return;
    case ListenerRole::kUdpReceivers:
      RunUdpReceivers();
      return;
    case ListenerRole::kTransparentTcp:
      RunTransparentTcp(listener, selected_application);
      return;
    case ListenerRole::kTransparentUdp:
      RunTransparentUdp(listener, selected_application);
      return;
    }
  } catch (...) {
    FailRuntimeFromListener();
  }
}

void ProxyRuntime::FailRuntimeFromListener() noexcept {
  listener_healthy_.store(false);
  failed_connections_.fetch_add(1);
  accepting_sessions_.store(false);
  if (!running_.exchange(false))
    return;
  const std::array<int, 6> listeners{
      {local_tcp_listener_, selected_tcp_listener_, transparent_tcp_listener_,
       transparent_udp_listener_, selected_transparent_tcp_listener_,
       selected_transparent_udp_listener_}};
  for (int descriptor : listeners) {
    if (descriptor >= 0)
      shutdown(descriptor, SHUT_RDWR);
  }
  if (global_udp_queue_ != nullptr)
    global_udp_queue_->Close();
  if (selected_udp_queue_ != nullptr)
    selected_udp_queue_->Close();
  try {
    std::lock_guard<std::mutex> lock(connections_mutex_);
    for (int descriptor : interrupt_descriptors_)
      shutdown(descriptor, SHUT_RDWR);
  } catch (...) {
    // 致命线程边界已处于资源异常中，此处只保证 noexcept；Stop 仍会做最终关闭。
  }
}

void ProxyRuntime::Stop() {
  accepting_sessions_.store(false);
  running_.store(false);
  if (global_udp_queue_ != nullptr)
    global_udp_queue_->Close();
  if (selected_udp_queue_ != nullptr)
    selected_udp_queue_->Close();
  const std::array<int *, 6> listeners{
      {&local_tcp_listener_, &selected_tcp_listener_,
       &transparent_tcp_listener_, &transparent_udp_listener_,
       &selected_transparent_tcp_listener_,
       &selected_transparent_udp_listener_}};
  for (int *listener : listeners) {
    if (*listener < 0)
      continue;
    shutdown(*listener, SHUT_RDWR);
    close(*listener);
    *listener = -1;
  }
  CloseActiveSessions();
  for (std::thread &thread : listener_threads_) {
    if (thread.joinable())
      thread.join();
  }
  listener_threads_.clear();
  global_udp_queue_.reset();
  selected_udp_queue_.reset();
  controlWorkers_.Stop();
  connectionWorkers_.Stop();
  datagramWorkers_.Stop();
}

void ProxyRuntime::UpdateRules(core::RoutingRules rules,
                               std::vector<Endpoint> dns_servers) {
  // 不可变快照的唯一可抛分配在切断会话前完成；资源耗尽时旧规则与旧连接保持完整。
  auto next_rules =
      std::make_shared<const core::RoutingRules>(std::move(rules));
  std::lock_guard<std::mutex> update_lock(update_mutex_);
  {
    std::lock_guard<std::mutex> lifecycle_lock(session_lifecycle_mutex_);
    accepting_sessions_.store(false);
  }
  // 先切断旧连接并等待全部工作项退出，再提交规则/DNS快照；旧会话不会观察到半新半旧配置。
  CloseActiveSessions();
  {
    std::unique_lock<std::mutex> control_lock(udp_control_mutex_);
    udp_control_condition_.wait(control_lock,
                                [this]() { return udp_associations_.empty(); });
  }
  WaitWorkersIdle();
  {
    std::lock_guard<std::mutex> lock(rules_mutex_);
    rules_ = std::move(next_rules);
  }
  {
    std::lock_guard<std::mutex> lock(dns_mutex_);
    config_.dns_servers = std::move(dns_servers);
  }
  {
    // 缓存域名参与规则匹配；规则版本切换时清空，保证新规则不会继承旧观察状态。
    std::lock_guard<std::mutex> lock(domain_cache_mutex_);
    domain_cache_.clear();
  }
  {
    // 等待阶段不持有生命周期锁，首包创建可观察 accepting=false 后退出，避免与
    // control reactor 循环等待。
    std::lock_guard<std::mutex> lifecycle_lock(session_lifecycle_mutex_);
    accepting_sessions_.store(running_.load());
  }
}

RuntimeStats ProxyRuntime::Stats() const {
  return {upload_bytes_.load(), download_bytes_.load(),
          active_connections_.load(), accepted_connections_.load(),
          failed_connections_.load()};
}

bool ProxyRuntime::Healthy() const noexcept { return listener_healthy_.load(); }

void ProxyRuntime::WaitWorkersIdle() {
  controlWorkers_.WaitIdle();
  connectionWorkers_.WaitIdle();
  datagramWorkers_.WaitIdle();
}

bool ProxyRuntime::RegisterConnection(int descriptor) {
  if (!RegisterSocket(descriptor))
    return false;
  {
    std::lock_guard<std::mutex> lock(connections_mutex_);
    if (active_descriptors_.size() >= kMaximumActiveConnections) {
      interrupt_descriptors_.erase(descriptor);
      return false;
    }
    active_descriptors_.insert(descriptor);
  }
  active_connections_.fetch_add(1);
  accepted_connections_.fetch_add(1);
  return true;
}

void ProxyRuntime::UnregisterConnection(int descriptor) {
  {
    std::lock_guard<std::mutex> lock(connections_mutex_);
    if (active_descriptors_.erase(descriptor) > 0)
      active_connections_.fetch_sub(1);
  }
  UnregisterSocket(descriptor);
}

bool ProxyRuntime::RegisterSocket(int descriptor) {
  // addDisallowedApplication 在部分厂商系统上不能可靠覆盖 Native/HEV 创建的
  // socket；connect 前逐 fd protect 才能保证上游响应不会重新进入 TUN 形成递归。
  if (!root_mode_ && !ProtectVpnSocket(descriptor))
    return false;
  std::lock_guard<std::mutex> lock(connections_mutex_);
  if (!running_.load() || !accepting_sessions_.load())
    return false;
  interrupt_descriptors_.insert(descriptor);
  return true;
}

void ProxyRuntime::UnregisterSocket(int descriptor) {
  std::lock_guard<std::mutex> lock(connections_mutex_);
  interrupt_descriptors_.erase(descriptor);
}

void ProxyRuntime::CloseActiveSessions() {
  {
    std::lock_guard<std::mutex> lock(connections_mutex_);
    for (int descriptor : interrupt_descriptors_)
      shutdown(descriptor, SHUT_RDWR);
  }
  std::vector<std::shared_ptr<UdpPeerSession>> sessions;
  {
    std::lock_guard<std::mutex> lock(udp_mutex_);
    sessions.swap(active_udp_sessions_);
  }
  for (const auto &session : sessions) {
    CloseUdpSession(session);
    if (session->active_counted.exchange(false))
      active_connections_.fetch_sub(1);
  }
  {
    std::lock_guard<std::mutex> lock(queued_udp_targets_mutex_);
    queued_udp_targets_.clear();
    queued_udp_target_count_ = 0;
  }
}

void ProxyRuntime::RunRootUdpQueue(net::NetfilterQueue *queue,
                                   bool selected_application) {
  if (queue == nullptr)
    throw std::logic_error("Root UDP 队列未初始化");
  queue->Run([this, selected_application](const net::QueuedUdpPacket &packet) {
    return StoreQueuedUdpTarget(packet, selected_application);
  });
  if (running_.load())
    throw std::runtime_error("Root UDP 队列意外退出");
}

bool ProxyRuntime::StoreQueuedUdpTarget(const net::QueuedUdpPacket &packet,
                                        bool selected_application) {
  const std::string address =
      net::AddressKey(packet.source, packet.source_length);
  if (address.empty())
    return false;
  const std::string key =
      std::string(selected_application ? "S|" : "G|") + address;
  const auto now = std::chrono::steady_clock::now();
  std::lock_guard<std::mutex> lock(queued_udp_targets_mutex_);
  if (!running_.load() || !accepting_sessions_.load())
    return false;
  if (queued_udp_target_count_ >= kMaximumQueuedUdpTargets) {
    for (auto iterator = queued_udp_targets_.begin();
         iterator != queued_udp_targets_.end();) {
      auto &targets = iterator->second;
      while (!targets.empty() && targets.front().expiry <= now) {
        targets.pop_front();
        --queued_udp_target_count_;
      }
      if (targets.empty())
        iterator = queued_udp_targets_.erase(iterator);
      else
        ++iterator;
    }
  }
  if (queued_udp_target_count_ >= kMaximumQueuedUdpTargets)
    return false;
  auto &targets = queued_udp_targets_[key];
  while (!targets.empty() && targets.front().expiry <= now) {
    targets.pop_front();
    --queued_udp_target_count_;
  }
  if (targets.size() >= kMaximumQueuedUdpTargetsPerPeer)
    return false;
  targets.push_back({packet.target, now + kQueuedUdpTargetLifetime});
  ++queued_udp_target_count_;
  return true;
}

std::optional<socks5::TargetAddress>
ProxyRuntime::TakeQueuedUdpTarget(const sockaddr_storage &peer,
                                  bool selected_application) {
  const socklen_t peer_length = peer.ss_family == AF_INET
                                    ? sizeof(sockaddr_in)
                                    : sizeof(sockaddr_storage);
  const std::string address = net::AddressKey(peer, peer_length);
  if (address.empty())
    return std::nullopt;
  const std::string key =
      std::string(selected_application ? "S|" : "G|") + address;
  const auto now = std::chrono::steady_clock::now();
  std::lock_guard<std::mutex> lock(queued_udp_targets_mutex_);
  const auto iterator = queued_udp_targets_.find(key);
  if (iterator == queued_udp_targets_.end())
    return std::nullopt;
  auto &targets = iterator->second;
  while (!targets.empty() && targets.front().expiry <= now) {
    targets.pop_front();
    --queued_udp_target_count_;
  }
  if (targets.empty()) {
    queued_udp_targets_.erase(iterator);
    return std::nullopt;
  }
  socks5::TargetAddress target = std::move(targets.front().target);
  targets.pop_front();
  --queued_udp_target_count_;
  if (targets.empty())
    queued_udp_targets_.erase(iterator);
  return target;
}

std::shared_ptr<const core::RoutingRules> ProxyRuntime::RulesSnapshot() const {
  std::lock_guard<std::mutex> lock(rules_mutex_);
  return rules_;
}

std::vector<Endpoint> ProxyRuntime::DnsSnapshot() const {
  std::lock_guard<std::mutex> lock(dns_mutex_);
  return config_.dns_servers;
}

void ProxyRuntime::ObserveDns(const std::vector<uint8_t> &query,
                              const std::vector<uint8_t> &response,
                              bool selected_application) {
  const net::DnsAddressResult parsed = net::ParseDnsAddresses(query, response);
  if (parsed.addresses.empty() || parsed.minimum_ttl == 0)
    return;
  const auto expiry = std::chrono::steady_clock::now() +
                      std::chrono::seconds(parsed.minimum_ttl);
  std::lock_guard<std::mutex> lock(domain_cache_mutex_);
  const uint64_t observation_sequence = ++domain_observation_sequence_;
  for (const std::string &address : parsed.addresses) {
    const std::string cache_key = DomainCacheKey(address, selected_application);
    ReserveDomainCacheSlot(cache_key, std::chrono::steady_clock::now());
    std::vector<CachedDomain> &candidates = domain_cache_[cache_key];
    const auto existing = std::find_if(candidates.begin(), candidates.end(),
                                       [&](const CachedDomain &entry) {
                                         return entry.domain == parsed.domain;
                                       });
    if (existing != candidates.end()) {
      existing->expiry = expiry;
      existing->observation_sequence = observation_sequence;
    } else {
      if (candidates.size() >= 8)
        candidates.erase(candidates.begin());
      candidates.push_back({parsed.domain, expiry, observation_sequence});
    }
  }
}

void ProxyRuntime::ReserveDomainCacheSlot(
    const std::string &cache_key, std::chrono::steady_clock::time_point now) {
  if (domain_cache_.find(cache_key) != domain_cache_.end() ||
      domain_cache_.size() < kMaximumDomainCacheKeys)
    return;
  for (auto iterator = domain_cache_.begin();
       iterator != domain_cache_.end();) {
    auto &candidates = iterator->second;
    candidates.erase(std::remove_if(candidates.begin(), candidates.end(),
                                    [&](const CachedDomain &entry) {
                                      return entry.expiry <= now;
                                    }),
                     candidates.end());
    if (candidates.empty())
      iterator = domain_cache_.erase(iterator);
    else
      ++iterator;
  }
  if (domain_cache_.size() < kMaximumDomainCacheKeys)
    return;
  // 缓存满且均未过期时淘汰最早到期键；键数保持硬上限，持续唯一 DNS
  // 回答不会放大内存。
  auto victim = std::min_element(
      domain_cache_.begin(), domain_cache_.end(),
      [](const auto &left, const auto &right) {
        const auto left_expiry =
            std::min_element(
                left.second.begin(), left.second.end(),
                [](const CachedDomain &first, const CachedDomain &second) {
                  return first.expiry < second.expiry;
                })
                ->expiry;
        const auto right_expiry =
            std::min_element(
                right.second.begin(), right.second.end(),
                [](const CachedDomain &first, const CachedDomain &second) {
                  return first.expiry < second.expiry;
                })
                ->expiry;
        return left_expiry < right_expiry;
      });
  if (victim != domain_cache_.end())
    domain_cache_.erase(victim);
}

std::vector<std::string>
ProxyRuntime::LookupDomains(const std::string &address,
                            bool selected_application) {
  std::lock_guard<std::mutex> lock(domain_cache_mutex_);
  const auto iterator =
      domain_cache_.find(DomainCacheKey(address, selected_application));
  if (iterator == domain_cache_.end())
    return {};
  const auto now = std::chrono::steady_clock::now();
  auto &candidates = iterator->second;
  candidates.erase(std::remove_if(candidates.begin(), candidates.end(),
                                  [&](const CachedDomain &entry) {
                                    return entry.expiry <= now;
                                  }),
                   candidates.end());
  if (candidates.empty()) {
    domain_cache_.erase(iterator);
    return {};
  }
  std::vector<std::string> domains;
  domains.reserve(candidates.size());
  // 同一 CDN 地址可由多个域名复用。连接无法携带 DNS 事务 ID
  // 时，最近一次同作用域 解析最接近应用当前意图；返回顺序固定为新到旧，首包
  // Host/SNI 仍可覆盖该候选。
  std::vector<const CachedDomain *> ordered;
  ordered.reserve(candidates.size());
  for (const CachedDomain &candidate : candidates)
    ordered.push_back(&candidate);
  std::sort(ordered.begin(), ordered.end(),
            [](const CachedDomain *left, const CachedDomain *right) {
              return left->observation_sequence > right->observation_sequence;
            });
  for (const CachedDomain *candidate : ordered)
    domains.push_back(candidate->domain);
  return domains;
}

core::RouteMatchResult ProxyRuntime::EvaluateTarget(
    const core::RoutingRules &rules, const socks5::TargetAddress &target,
    const std::string &observed_domain, bool selected_application) {
  if (!observed_domain.empty()) {
    return rules.EvaluateIpv4ForContext(target.host, target.port,
                                        observed_domain, selected_application);
  }
  const std::vector<std::string> domains =
      net::IsIpLiteral(target.host)
          ? LookupDomains(target.host, selected_application)
          : std::vector<std::string>{target.host};
  if (domains.empty()) {
    return rules.EvaluateIpv4ForContext(target.host, target.port, "",
                                        selected_application);
  }
  core::RouteMatchResult selected{core::RouteAction::kDirect,
                                  "DNS 候选域名默认直连"};
  for (const std::string &domain : domains) {
    const core::RouteMatchResult candidate = rules.EvaluateIpv4ForContext(
        target.host, target.port, domain, selected_application);
    if (candidate.action == core::RouteAction::kReject)
      return candidate;
    // 一个共享 IP 可能对应多个域名；保守优先级 REJECT > PROXY > DIRECT
    // 防止代理目标被直连候选覆盖。
    if (candidate.action == core::RouteAction::kProxy)
      selected = candidate;
  }
  return selected;
}

} // namespace routesocks::runtime
