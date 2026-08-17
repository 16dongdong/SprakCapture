#include "net/dns_protocol.h"

#include <arpa/inet.h>

#include <algorithm>
#include <atomic>
#include <climits>
#include <cstddef>
#include <unordered_map>
#include <unordered_set>
#include <utility>

namespace routesocks::net {
namespace {

constexpr uint32_t kMaximumDomainCacheSeconds = 300;
constexpr uint16_t kMaximumAnswerRecords = 256;
std::atomic<uint16_t> dns_transaction_counter{0x4000};

struct DnsAnswerRecord {
  std::string owner;
  std::string cname;
  std::string address;
  uint16_t type = 0;
  uint16_t record_class = 0;
  uint32_t ttl = 0;
};

/** 按网络序读取 16 位字段；越界返回 false，不访问不完整报文。 */
bool ReadUint16(const std::vector<uint8_t>& packet, std::size_t offset, uint16_t* value) {
  if (packet.size() - std::min(packet.size(), offset) < 2) return false;
  *value = static_cast<uint16_t>((packet[offset] << 8U) | packet[offset + 1]);
  return true;
}

/** 按网络序读取 32 位字段；越界返回 false，避免损坏 TTL 进入缓存。 */
bool ReadUint32(const std::vector<uint8_t>& packet, std::size_t offset, uint32_t* value) {
  if (packet.size() - std::min(packet.size(), offset) < 4) return false;
  *value = (static_cast<uint32_t>(packet[offset]) << 24U) |
           (static_cast<uint32_t>(packet[offset + 1]) << 16U) |
           (static_cast<uint32_t>(packet[offset + 2]) << 8U) | packet[offset + 3];
  return true;
}

/**
 * 读取普通或压缩 DNS 名称并推进原始游标；压缩跳转最多16次，循环、越界和保留标签均拒绝。
 * 原始游标只消费线上字段本身，跳转后的标签用于 owner/CNAME 语义校验而不破坏后续 RR 解析。
 */
bool ReadName(const std::vector<uint8_t>& packet,
              std::size_t* offset,
              std::string* domain) {
  std::size_t position = *offset;
  bool jumped = false;
  std::size_t jumps = 0;
  std::string parsed;
  while (position < packet.size()) {
    const uint8_t length = packet[position++];
    if (length == 0) {
      if (!jumped) *offset = position;
      *domain = std::move(parsed);
      return !domain->empty();
    }
    if ((length & 0xC0U) == 0xC0U) {
      if (position >= packet.size() || ++jumps > 16) return false;
      const std::size_t pointer = static_cast<std::size_t>((length & 0x3FU) << 8U) |
                                  packet[position++];
      if (pointer >= packet.size()) return false;
      if (!jumped) *offset = position;
      jumped = true;
      position = pointer;
      continue;
    }
    if ((length & 0xC0U) != 0 || length > 63 || packet.size() - position < length) return false;
    if (!parsed.empty()) parsed.push_back('.');
    parsed.append(reinterpret_cast<const char*>(packet.data() + position), length);
    position += length;
  }
  return false;
}

/**
 * 校验并规范化 DNS 主机名；只接受 ASCII 字母、数字、连字符和点，IDN 必须使用
 * punycode。空标签、标签首尾连字符和长度越界返回 false，保证缓存键跨层一致。
 */
bool NormalizeDnsDomain(std::string* domain) {
  if (domain == nullptr || domain->empty() || domain->size() > 253) return false;
  std::size_t label_length = 0;
  bool label_start = true;
  char previous = '\0';
  for (char& raw : *domain) {
    const unsigned char character = static_cast<unsigned char>(raw);
    if (raw == '.') {
      if (label_start || label_length > 63 || previous == '-') return false;
      label_start = true;
      label_length = 0;
      previous = raw;
      continue;
    }
    const bool letter = (character >= 'a' && character <= 'z') ||
                        (character >= 'A' && character <= 'Z');
    const bool digit = character >= '0' && character <= '9';
    if ((!letter && !digit && raw != '-') || (label_start && raw == '-')) {
      return false;
    }
    if (character >= 'A' && character <= 'Z') raw += 'a' - 'A';
    label_start = false;
    previous = raw;
    if (++label_length > 63) return false;
  }
  return !label_start && previous != '-';
}

/**
 * 先把全部 Answer RR 解析成有界记录集，不依赖服务器返回顺序。
 * CNAME 的压缩名称必须精确消费 RDATA；任一记录截断、歧义或地址长度错误均返回
 * false，调用方不会把部分回答写入域名缓存。
 */
bool ParseAnswerRecords(const std::vector<uint8_t>& response,
                        std::size_t offset,
                        uint16_t answer_count,
                        std::vector<DnsAnswerRecord>* records) {
  if (records == nullptr || answer_count > kMaximumAnswerRecords) return false;
  records->clear();
  records->reserve(answer_count);
  for (uint16_t index = 0; index < answer_count; ++index) {
    DnsAnswerRecord record;
    if (!ReadName(response, &offset, &record.owner) ||
        response.size() - std::min(response.size(), offset) < 10) {
      return false;
    }
    uint16_t record_length = 0;
    if (!ReadUint16(response, offset, &record.type) ||
        !ReadUint16(response, offset + 2, &record.record_class) ||
        !ReadUint32(response, offset + 4, &record.ttl) ||
        !ReadUint16(response, offset + 8, &record_length)) {
      return false;
    }
    offset += 10;
    if (response.size() - std::min(response.size(), offset) < record_length) {
      return false;
    }
    const std::size_t record_end = offset + record_length;
    if (!NormalizeDnsDomain(&record.owner)) return false;
    if (record.record_class == 1 && record.type == 5) {
      std::size_t cname_offset = offset;
      if (!ReadName(response, &cname_offset, &record.cname) ||
          cname_offset != record_end) {
        return false;
      }
      if (!NormalizeDnsDomain(&record.cname)) return false;
    } else if (record.record_class == 1 &&
               ((record.type == 1 && record_length == 4) ||
                (record.type == 28 && record_length == 16))) {
      char address_text[INET6_ADDRSTRLEN]{};
      const int family = record.type == 1 ? AF_INET : AF_INET6;
      if (inet_ntop(family, response.data() + offset, address_text,
                    sizeof(address_text)) == nullptr) {
        return false;
      }
      record.address = address_text;
    } else if (record.record_class == 1 &&
               ((record.type == 1 && record_length != 4) ||
                (record.type == 28 && record_length != 16))) {
      return false;
    }
    records->push_back(std::move(record));
    offset = record_end;
  }
  return true;
}

/**
 * 从问题 owner 沿唯一 CNAME 链求最终 canonical owner，并汇总链路与地址的最小
 * TTL。多目标 CNAME、可达环、别名 owner 同时携带地址或超过 Answer 数量的链均
 * 视为畸形；只有链尾地址可进入缓存，避免中间别名和无关 owner 污染规则决策。
 */
bool CollectReachableAddresses(const std::vector<DnsAnswerRecord>& records,
                               uint16_t query_type,
                               DnsAddressResult* result) {
  if (result == nullptr) return false;
  std::unordered_map<std::string, const DnsAnswerRecord*> aliases;
  for (const DnsAnswerRecord& record : records) {
    if (record.record_class != 1 || record.type != 5) continue;
    if (!aliases.emplace(record.owner, &record).second) return false;
  }

  std::unordered_set<std::string> reachable;
  std::string current = result->domain;
  uint32_t minimum_ttl = UINT32_MAX;
  for (std::size_t depth = 0; depth <= records.size(); ++depth) {
    if (!reachable.insert(current).second) return false;
    const auto alias = aliases.find(current);
    if (alias == aliases.end()) break;
    minimum_ttl = std::min(minimum_ttl, alias->second->ttl);
    current = alias->second->cname;
    if (depth == records.size()) return false;
  }

  for (const DnsAnswerRecord& record : records) {
    if (record.record_class != 1 || record.type != query_type ||
        record.address.empty()) {
      continue;
    }
    if (reachable.count(record.owner) != 0 && record.owner != current) {
      return false;
    }
    if (record.owner != current) continue;
    result->addresses.push_back(record.address);
    minimum_ttl = std::min(minimum_ttl, record.ttl);
  }
  if (!result->addresses.empty()) {
    result->minimum_ttl =
        std::min(minimum_ttl, kMaximumDomainCacheSeconds);
  }
  return true;
}

}  // namespace

bool BuildDnsQuery(const std::string& domain, uint16_t type, std::vector<uint8_t>* query) {
  std::string normalized_domain = domain;
  if (query == nullptr || (type != 1 && type != 28) ||
      !NormalizeDnsDomain(&normalized_domain)) return false;
  const uint16_t transaction = dns_transaction_counter.fetch_add(1);
  query->assign({static_cast<uint8_t>(transaction >> 8U), static_cast<uint8_t>(transaction),
                 0x01, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00});
  std::size_t start = 0;
  while (start < normalized_domain.size()) {
    const std::size_t dot = normalized_domain.find('.', start);
    const std::size_t end = dot == std::string::npos ? normalized_domain.size() : dot;
    const std::size_t length = end - start;
    if (length == 0 || length > 63) return false;
    query->push_back(static_cast<uint8_t>(length));
    query->insert(query->end(),
                  normalized_domain.begin() + static_cast<std::ptrdiff_t>(start),
                  normalized_domain.begin() + static_cast<std::ptrdiff_t>(end));
    start = end + 1;
  }
  query->insert(query->end(), {0x00, static_cast<uint8_t>(type >> 8U),
                               static_cast<uint8_t>(type), 0x00, 0x01});
  return query->size() <= 512;
}

bool DnsTransactionMatches(const std::vector<uint8_t>& query,
                           const std::vector<uint8_t>& response) {
  if (query.size() < 16 || response.size() < 16 || query[0] != response[0] || query[1] != response[1] ||
      (response[2] & 0x80U) == 0) return false;
  uint16_t query_questions = 0;
  uint16_t response_questions = 0;
  if (!ReadUint16(query, 4, &query_questions) || !ReadUint16(response, 4, &response_questions) ||
      query_questions != 1 || response_questions != 1) return false;
  std::size_t query_offset = 12;
  std::size_t response_offset = 12;
  std::string query_domain;
  std::string response_domain;
  if (!ReadName(query, &query_offset, &query_domain) ||
      !ReadName(response, &response_offset, &response_domain) ||
      query.size() - query_offset < 4 || response.size() - response_offset < 4) return false;
  if (!NormalizeDnsDomain(&query_domain) || !NormalizeDnsDomain(&response_domain)) {
    return false;
  }
  return query_domain == response_domain &&
         std::equal(query.begin() + static_cast<std::ptrdiff_t>(query_offset),
                    query.begin() + static_cast<std::ptrdiff_t>(query_offset + 4),
                    response.begin() + static_cast<std::ptrdiff_t>(response_offset));
}

DnsAddressResult ParseDnsAddresses(const std::vector<uint8_t>& query,
                                   const std::vector<uint8_t>& response) {
  DnsAddressResult result;
  if (!DnsTransactionMatches(query, response)) return result;
  std::size_t query_offset = 12;
  if (!ReadName(query, &query_offset, &result.domain) ||
      query.size() - query_offset < 4 || !NormalizeDnsDomain(&result.domain)) {
    return result;
  }
  uint16_t query_type = 0;
  uint16_t query_class = 0;
  if (!ReadUint16(query, query_offset, &query_type) ||
      !ReadUint16(query, query_offset + 2, &query_class)) return result;
  if ((query_type != 1 && query_type != 28) || query_class != 1 ||
      response.size() < 4 || (response[2] & 0x02U) != 0) return result;
  uint16_t answer_count = 0;
  if (!ReadUint16(response, 6, &answer_count) || (response[3] & 0x0FU) != 0) return result;
  std::size_t offset = 12;
  std::string response_domain;
  if (!ReadName(response, &offset, &response_domain) ||
      response.size() - offset < 4) return result;
  uint16_t response_type = 0;
  uint16_t response_class = 0;
  if (!ReadUint16(response, offset, &response_type) ||
      !ReadUint16(response, offset + 2, &response_class) ||
      !NormalizeDnsDomain(&response_domain) || result.domain != response_domain ||
      query_type != response_type || query_class != response_class) return result;
  offset += 4;
  std::vector<DnsAnswerRecord> records;
  if (!ParseAnswerRecords(response, offset, answer_count, &records) ||
      !CollectReachableAddresses(records, query_type, &result)) {
    return {};
  }
  return result;
}

}  // namespace routesocks::net
