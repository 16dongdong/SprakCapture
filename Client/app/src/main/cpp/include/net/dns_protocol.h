#pragma once

#include <cstdint>
#include <string>
#include <vector>

namespace routesocks::net {

/** 保存已校验 DNS 回答中可用于域名缓存的地址、问题域名和最小 TTL。 */
struct DnsAddressResult {
  std::vector<std::string> addresses;
  std::string domain;
  uint32_t minimum_ttl = 0;
};

/** 构造单问题 A/AAAA 查询；非法域名、类型或超长报文返回 false。 */
bool BuildDnsQuery(const std::string& domain, uint16_t type, std::vector<uint8_t>* query);

/** 比较事务 ID 与完整 question 指纹；损坏、非响应或错配回答返回 false。 */
bool DnsTransactionMatches(const std::vector<uint8_t>& query,
                           const std::vector<uint8_t>& response);

/** 校验 DNS 回答并提取 A/AAAA；任何协议不一致返回空结果。 */
DnsAddressResult ParseDnsAddresses(const std::vector<uint8_t>& query,
                                   const std::vector<uint8_t>& response);

}  // namespace routesocks::net
