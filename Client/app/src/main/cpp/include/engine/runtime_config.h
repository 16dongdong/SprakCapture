#pragma once

#include <cstdint>
#include <string>
#include <vector>

namespace routesocks::runtime {

/** 描述一个可解析为 IPv4 或 IPv6 的网络端点。 */
struct Endpoint {
  std::string host;
  uint16_t port = 0;
};

/** 保存 Native 数据面唯一配置，敏感字段只驻留内存且不会进入日志。 */
struct RuntimeConfig {
  Endpoint upstream;
  std::string username;
  std::string password;
  std::string local_username;
  std::string local_password;
  uint16_t local_socks_port = 12580;
  uint16_t selected_socks_port = 12581;
  uint16_t transparent_tcp_port = 12345;
  uint16_t transparent_udp_port = 12346;
  uint16_t selected_transparent_tcp_port = 12347;
  uint16_t selected_transparent_udp_port = 12348;
  uint16_t global_udp_queue_number = 6100;
  uint16_t selected_udp_queue_number = 6101;
  std::vector<Endpoint> dns_servers;
};

/**
 * 解析 Kotlin 传入的逐行 `key=value` 配置和规则内 `[DNS]` 段。
 * 未知配置键、重复键、非法端口或缺少 DNS 都会返回 false 并给出中文错误。
 */
bool ParseRuntimeConfig(const std::string& configuration_text,
                        const std::string& routing_text,
                        RuntimeConfig* output,
                        std::string* error);

/** 仅解析规则中的 DNS 主备列表，供热更新在不接触秘密配置时复用。 */
bool ParseDnsServers(const std::string& routing_text,
                     std::vector<Endpoint>* servers,
                     std::string* error);

}  // namespace routesocks::runtime
