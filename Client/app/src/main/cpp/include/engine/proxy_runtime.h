#pragma once

#include <sys/socket.h>

#include <array>
#include <atomic>
#include <chrono>
#include <condition_variable>
#include <cstdint>
#include <deque>
#include <functional>
#include <memory>
#include <mutex>
#include <optional>
#include <set>
#include <string>
#include <thread>
#include <unordered_map>
#include <utility>
#include <vector>

#include "core/routing_rules.h"
#include "engine/boundedTaskPool.h"
#include "engine/runtime_config.h"
#include "net/netfilter_queue.h"
#include "net/socket_utils.h"
#include "socks5/socks_protocol.h"

namespace routesocks::runtime {

/** Native 对 Kotlin 暴露的固定顺序统计快照。 */
struct RuntimeStats {
  uint64_t upload_bytes = 0;
  uint64_t download_bytes = 0;
  uint64_t active_connections = 0;
  uint64_t accepted_connections = 0;
  uint64_t failed_connections = 0;
};

/**
 * 统一承载 VPN 本地 SOCKS 和 Root 透明入口的数据面。
 * 所有入口共用同一不可变规则快照；更新规则会关闭既有连接，防止会话跨版本。
 */
class ProxyRuntime : public std::enable_shared_from_this<ProxyRuntime>,
                     public net::SocketObserver {
public:
  /** 保存不可变上游配置、初始规则和入口模式，构造阶段不打开 socket。 */
  ProxyRuntime(RuntimeConfig config, core::RoutingRules rules, bool root_mode);
  /** 执行幂等停止，保证对象销毁后没有监听线程引用成员。 */
  ~ProxyRuntime();
  ProxyRuntime(const ProxyRuntime &) = delete;
  ProxyRuntime &operator=(const ProxyRuntime &) = delete;

  /** 创建 VPN SOCKS 与 Root IPv4
   * 透明监听线程；任一端口失败会回滚已经创建的资源。 */
  bool Start(std::string *error);

  /** 幂等关闭监听、连接和 UDP 会话，并等待固定监听线程结束。 */
  void Stop();

  /** 原子替换规则并断开当前会话；解析由 JNI 层完成，失败不改变旧规则。 */
  void UpdateRules(core::RoutingRules rules, std::vector<Endpoint> dns_servers);

  /** 返回无锁原子计数快照，顺序由 JNI 契约固定。 */
  RuntimeStats Stats() const;

  /** 返回固定监听线程是否仍完整运行；false 要求 Kotlin 立即停止数据面。 */
  bool Healthy() const noexcept;

  /** 将新建或已连接 socket 纳入规则切换的统一 shutdown 集合。 */
  bool RegisterSocket(int descriptor) override;

  /** 从统一中断集合移除即将关闭的 socket。 */
  void UnregisterSocket(int descriptor) override;

private:
  struct UdpPeerSession;
  struct UdpPacketTask;
  struct UdpResponseRoute;
  struct UdpAssociation;

  /** 标识固定监听线程的职责，使所有 std::thread 共用同一个 noexcept 异常边界。
   */
  enum class ListenerRole : uint8_t {
    kLocalTcp,
    kUdpControls,
    kUdpReceivers,
    kTransparentTcp,
    kTransparentUdp,
  };

  /** 在 noexcept 线程边界调度监听角色；未捕获异常会停止整个 runtime 而不会
   * terminate 进程。 */
  void RunListenerBoundary(ListenerRole role, int listener,
                           bool selected_application) noexcept;

  /** 由失败的监听线程发起无 join 停止；只 shutdown 唤醒同伴，最终 close/join 由
   * Stop 完成。 */
  void FailRuntimeFromListener() noexcept;

  /** 接受本地 SOCKS5 TCP 控制或 CONNECT 请求。 */
  void RunLocalTcp(int listener, bool selected_application);

  /** 接受 Root iptables REDIRECT 后的透明 TCP。 */
  void RunTransparentTcp(int listener, bool selected_application);

  /** 接收 Root 透明 UDP 并从控制消息恢复原始目标。 */
  void RunTransparentUdp(int listener, bool selected_application);

  /**
   * 消费一个 Root NFQUEUE；先保存原目标再放行数据报，队列异常会使整个 Native
   * 数据面进入失败态。
   */
  void RunRootUdpQueue(net::NetfilterQueue *queue, bool selected_application);

  /**
   * 在 NFQUEUE verdict 前登记一条原目标；容量耗尽返回 false，使内核直接 DROP
   * 而不是把无目标数据交给代理。
   */
  bool StoreQueuedUdpTarget(const net::QueuedUdpPacket &packet,
                            bool selected_application);

  /**
   * 取出 REDIRECT
   * 后同一源端点最早的原目标；映射缺失或超时返回空值，调用方必须丢弃该包。
   */
  std::optional<socks5::TargetAddress>
  TakeQueuedUdpTarget(const sockaddr_storage &peer, bool selected_application);

  /** 处理单条 TCP 会话，目标和握手模式由入口层明确传入。 */
  void HandleTcp(int client_descriptor, socks5::TargetAddress target,
                 bool reply_socks_success, bool selected_application);

  /**
   * 把已登记的 TCP 客户端移交给长连接执行池；提交失败时发送可用的 SOCKS
   * 失败响应并完整回收。
   */
  bool ScheduleTcpConnection(int client_descriptor,
                             socks5::TargetAddress target,
                             bool reply_socks_success,
                             bool selected_application);

  /** 逐帧处理 TCP/53，主服务器超时或响应损坏时仅向后切换一次备用服务器。 */
  void HandleTcpDns(int client_descriptor, bool reply_socks_success,
                    bool selected_application);

  /** 从指定下标开始连接首个可用 DNS，并为逐帧读取设置确定超时。 */
  int ConnectDnsServer(const std::vector<Endpoint> &servers,
                       std::size_t start_index, std::size_t *connected_index,
                       std::string *error);

  /** 按已决策动作建立 TCP；DNS 始终忽略动作并依次直连规则主备服务器。 */
  socks5::TcpConnectResult ConnectTcpRoute(const socks5::TargetAddress &target,
                                           core::RouteAction action,
                                           bool selected_application,
                                           std::string *error);

  /** 使用规则 PRIMARY/SECONDARY 把域名目标解析为 IP；字面地址原样返回。 */
  bool ResolveTarget(const socks5::TargetAddress &target,
                     bool selected_application, socks5::TargetAddress *resolved,
                     std::string *error);

  /** 在指定 DNS 端点执行单帧 TCP 查询；用于 UDP 截断响应的标准回退。 */
  bool QueryDnsTcp(const Endpoint &server, const std::vector<uint8_t> &query,
                   std::vector<uint8_t> *response, std::string *error);

  /** 在单一 poll 线程维护全部 UDP ASSOCIATE
   * 控制连接，避免长寿命会话占满数据工作池。 */
  void RunUdpControls();

  /**
   * 在回复 REP=0 前原子预留 UDP ASSOCIATE 容量与所有权。
   * 达到上限或规则切换中返回空指针，调用方必须发送标准 REP=1 且不得伪报成功。
   */
  std::shared_ptr<UdpAssociation>
  ReserveUdpControl(int control_descriptor, int datagram_descriptor,
                    bool selected_application,
                    socks5::TargetAddress requested_peer);

  /** 按对象身份移除并关闭一条 association，fd 已复用时绝不误关新会话。 */
  void CloseUdpControl(const std::shared_ptr<UdpAssociation> &association);

  /** 处理一条已认证 association 独占端点上的 SOCKS5 UDP 帧。 */
  void ReceiveAssociatedUdp(const std::shared_ptr<UdpAssociation> &association);

  /** 按规则异步发送 UDP；端口 53 固定走 DNS 直连，函数从不等待响应。 */
  bool SendUdp(UdpPeerSession *session, const socks5::TargetAddress &target,
               const std::vector<uint8_t> &payload);

  /** 向目标独占的 connected socket 发送 DIRECT 数据报，不执行同步接收。 */
  bool SendDirectUdp(UdpPeerSession *session,
                     const socks5::TargetAddress &target,
                     const std::vector<uint8_t> &payload);

  /**
   * 在 session.operation_mutex 内为当前包淘汰一个无在途 DNS 的 LRU 通道。
   * 被淘汰 fd 只退出会话索引并交给 reactor 延迟关闭，全部忙碌或退役队列满时返回
   * false。
   */
  bool EnsureDirectUdpCapacity(UdpPeerSession *session);

  /** 把已从会话索引移除的 fd 交给接收 reactor 延迟关闭，避免 poll
   * 快照命中复用描述符。 */
  bool RetireUdpDescriptor(int descriptor);

  /** 在 reactor 关闭边界内注销并关闭全部退役 fd；调用后队列为空。 */
  void DrainRetiredUdpDescriptors();

  /** 向指定主备序号发送 DNS UDP，并登记事务映射供异步响应和超时切换使用。 */
  bool SendDnsUdp(UdpPeerSession *session,
                  const socks5::TargetAddress &original_target,
                  const std::vector<uint8_t> &payload,
                  std::size_t server_index);

  /** 在单一 poll 线程轮询全部 peer 的直连和上游
   * socket，避免每个源地址创建线程。 */
  void RunUdpReceivers();

  /** 按本地 SOCKS 或透明入口格式回写一条真实 UDP 响应。 */
  void SendUdpResponse(UdpPeerSession *session,
                       const socks5::TargetAddress &target,
                       const std::vector<uint8_t> &payload);

  /**
   * 关闭一个 UDP 会话及其全部网络资源。
   * 函数先取得会话操作权，再短暂进入 reactor fd
   * 生命周期边界；等待网络握手时不会 阻塞其他会话的 DNS/UDP
   * 回包。重复关闭安全返回，资源注销失败不伪造成功状态。
   */
  void CloseUdpSession(const std::shared_ptr<UdpPeerSession> &session);

  /** 关闭活动 TCP 和 UDP 会话，使规则更新和停止拥有确定边界。 */
  void CloseActiveSessions();

  /** 注册活动描述符；停止已开始时立即拒绝并关闭。 */
  bool RegisterConnection(int descriptor);

  /** 从活动集合移除描述符并更新计数，调用方仍负责 close。 */
  void UnregisterConnection(int descriptor);

  /** 返回不可变规则快照；高频 UDP 每包只复制 shared_ptr，不复制整份规则。 */
  std::shared_ptr<const core::RoutingRules> RulesSnapshot() const;

  /** 返回 DNS 主备副本，热更新不会与正在创建的流量发生容器数据竞争。 */
  std::vector<Endpoint> DnsSnapshot() const;

  /** 解析直连 DNS 问答并按规则作用域缓存地址到域名映射。 */
  void ObserveDns(const std::vector<uint8_t> &query,
                  const std::vector<uint8_t> &response,
                  bool selected_application);

  /** 查找同一规则作用域内最近 DNS 回答对应的全部域名；读取时删除过期项。 */
  std::vector<std::string> LookupDomains(const std::string &address,
                                         bool selected_application);

  /** 在 domain_cache_mutex_
   * 内为新键腾出有界容量；优先清过期项，否则淘汰最早到期键。 */
  void ReserveDomainCacheSlot(const std::string &cache_key,
                              std::chrono::steady_clock::time_point now);

  /** 用显式域名或 DNS 候选执行保守决策，多个候选按 REJECT、PROXY、DIRECT 优先。
   */
  core::RouteMatchResult EvaluateTarget(const core::RoutingRules &rules,
                                        const socks5::TargetAddress &target,
                                        const std::string &observed_domain,
                                        bool selected_application);

  /** 清理超过固定空闲时间的 UDP peer，限制恶意源地址造成的内存增长。 */
  void PruneUdpSessions(
      std::unordered_map<std::string, std::shared_ptr<UdpPeerSession>>
          *sessions);

  /** 等待控制、TCP 和 UDP 三类任务全部退出，形成规则切换屏障。 */
  void WaitWorkersIdle();

  /** 把同一 UDP peer 的数据报放入其有界串行队列，避免等待线程占满全局工作池。
   */
  bool EnqueueUdpPacket(const std::shared_ptr<UdpPeerSession> &session,
                        UdpPacketTask task);

  /** 在一个工作项内排空同一 peer 的待发数据报，其他 peer 可由不同工作线程并发。
   */
  void RunUdpPeer(const std::shared_ptr<UdpPeerSession> &session);

  RuntimeConfig config_;
  bool root_mode_;
  std::atomic<bool> running_{false};
  std::atomic<bool> accepting_sessions_{false};
  std::atomic<bool> listener_healthy_{true};
  // 多次热更新必须串行；生命周期锁只保护短临界区，禁止持锁等待 reactor。
  std::mutex update_mutex_;
  // 会话发布与规则热更共用此锁，保证 accepting=false 后没有旧配置 UDP
  // 会话漏入。
  std::mutex session_lifecycle_mutex_;
  mutable std::mutex rules_mutex_;
  std::shared_ptr<const core::RoutingRules> rules_;
  mutable std::mutex dns_mutex_;
  mutable std::mutex domain_cache_mutex_;
  struct CachedDomain {
    std::string domain;
    std::chrono::steady_clock::time_point expiry;
    uint64_t observation_sequence = 0;
  };
  std::unordered_map<std::string, std::vector<CachedDomain>> domain_cache_;
  uint64_t domain_observation_sequence_ = 0;
  mutable std::mutex connections_mutex_;
  std::set<int> active_descriptors_;
  std::set<int> interrupt_descriptors_;
  mutable std::mutex udp_mutex_;
  std::vector<std::shared_ptr<UdpPeerSession>> active_udp_sessions_;
  // reactor 持锁期间 fd 不会被其他线程关闭，防止 poll
  // 整数快照命中已复用描述符。
  std::mutex udp_reactor_mutex_;
  std::mutex retired_udp_mutex_;
  std::vector<int> retired_udp_descriptors_;
  int local_tcp_listener_ = -1;
  int selected_tcp_listener_ = -1;
  int transparent_tcp_listener_ = -1;
  int transparent_udp_listener_ = -1;
  int selected_transparent_tcp_listener_ = -1;
  int selected_transparent_udp_listener_ = -1;
  std::unique_ptr<net::NetfilterQueue> global_udp_queue_;
  std::unique_ptr<net::NetfilterQueue> selected_udp_queue_;
  struct QueuedUdpTarget {
    socks5::TargetAddress target;
    std::chrono::steady_clock::time_point expiry;
  };
  std::mutex queued_udp_targets_mutex_;
  std::unordered_map<std::string, std::deque<QueuedUdpTarget>>
      queued_udp_targets_;
  std::size_t queued_udp_target_count_ = 0;
  std::vector<std::thread> listener_threads_;
  std::mutex udp_control_mutex_;
  std::condition_variable udp_control_condition_;
  std::unordered_map<int, std::shared_ptr<UdpAssociation>> udp_associations_;
  // 长连接不能与 DNS/UDP 或 SOCKS
  // 控制握手共用线程；三个独立有界池保证任一协议拥塞
  // 都不会饿死另外两类基础流量。
  BoundedTaskPool controlWorkers_;
  BoundedTaskPool connectionWorkers_;
  BoundedTaskPool datagramWorkers_;
  std::atomic<uint64_t> upload_bytes_{0};
  std::atomic<uint64_t> download_bytes_{0};
  std::atomic<uint64_t> active_connections_{0};
  std::atomic<uint64_t> accepted_connections_{0};
  std::atomic<uint64_t> failed_connections_{0};
};

} // namespace routesocks::runtime
