#pragma once

#include <sys/socket.h>

#include <cstddef>
#include <cstdint>
#include <string>

#include "engine/runtime_config.h"

namespace routesocks::net {

/** 让连接建立过程把尚未完成的 socket 纳入运行时统一中断边界。 */
class SocketObserver {
 public:
  /** 虚析构保证实现可以经接口安全释放。 */
  virtual ~SocketObserver() = default;
  /** 注册 socket；运行时正在停止或切换规则时返回 false。 */
  virtual bool RegisterSocket(int descriptor) = 0;
  /** 移除 socket；调用方随后关闭描述符。 */
  virtual void UnregisterSocket(int descriptor) = 0;
};

/** RAII 文件描述符，保证所有错误分支都关闭 socket。 */
class UniqueSocket {
 public:
  /** 接管 descriptor；负值表示当前未持有资源。 */
  explicit UniqueSocket(int descriptor = -1);
  /** 关闭仍被持有的描述符，析构过程不抛异常。 */
  ~UniqueSocket();
  /** 转移描述符所有权，源对象恢复为空。 */
  UniqueSocket(UniqueSocket&& other) noexcept;
  /** 先释放当前资源再接管源对象，支持容器中的安全移动。 */
  UniqueSocket& operator=(UniqueSocket&& other) noexcept;
  UniqueSocket(const UniqueSocket&) = delete;
  UniqueSocket& operator=(const UniqueSocket&) = delete;
  /** 返回当前描述符但不转移所有权。 */
  int Get() const;
  /** 返回当前描述符并放弃所有权，供成功路径交给上层。 */
  int Release();
  /** 关闭旧描述符并接管新值；默认参数仅执行关闭。 */
  void Reset(int descriptor = -1);

 private:
  int descriptor_;
};

/** 把端点解析并连接为 TCP，超时或解析失败返回 -1 和精确错误。 */
int ConnectTcp(const runtime::Endpoint& endpoint,
               int timeout_ms,
               SocketObserver* observer,
               std::string* error);

/** 返回当前工作线程最近一次 TCP 建链失败的 errno；仅供 SOCKS REP 精确映射。 */
int LastConnectError();

/** 创建仅供本机 SOCKS 或 OUTPUT REDIRECT 使用的回环 TCP 服务。 */
int CreateIpv4TcpListener(uint16_t port, std::string* error);

/** 创建 IPv4 UDP 服务并按需开启原目标辅助数据。 */
int CreateIpv4UdpListener(uint16_t port, bool transparent, std::string* error);

/** 完整发送缓冲区；连接中断时返回 false，避免短写破坏协议帧。 */
bool SendAll(int descriptor, const void* bytes, std::size_t length, std::string* error);

/** 完整读取固定长度；对端提前关闭时返回 false。 */
bool ReceiveAll(int descriptor, void* bytes, std::size_t length, std::string* error);

/** 在给定毫秒内等待可读；超时和系统错误均返回 false。 */
bool WaitReadable(int descriptor, int timeout_ms);

/** 只解析 IP 字面量为 sockaddr；域名必须先经过规则指定 DNS，禁止隐式系统解析。 */
bool ResolveEndpoint(const runtime::Endpoint& endpoint,
                     sockaddr_storage* address,
                     socklen_t* address_length,
                     std::string* error);

/** 判断文本是否为可解析的 IPv4/IPv6 字面量；域名及损坏地址返回 false。 */
bool IsIpLiteral(const std::string& host);

/** 把 sockaddr 转成稳定的 host:port 文本，只用于会话键而不记录秘密。 */
std::string AddressKey(const sockaddr_storage& address, socklen_t address_length);

}  // namespace routesocks::net
