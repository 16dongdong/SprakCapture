#pragma once

#include <cstddef>
#include <cstdint>
#include <string>
#include <vector>

namespace routesocks::net {

enum class SniffSource {
  kUnknown = 0,
  kTlsSni,
  kHttpHost,
};

struct DomainSniffResult {
  bool matched = false;
  std::string domain;
  SniffSource source = SniffSource::kUnknown;
  std::string detail;
};

class DomainSniffer {
 public:
  /** 按 TLS SNI、HTTP Host 顺序嗅探连续字节；未完整或未命中时返回中文诊断。 */
  static DomainSniffResult Sniff(const uint8_t* data, std::size_t len);
  /** vector 便捷入口；空输入返回未匹配，不抛异常。 */
  static DomainSniffResult Sniff(const std::vector<uint8_t>& data);

  /**
   * 解析可跨多个 TLS record 的完整 ClientHello；仅接受唯一且完整消费的
   * server_name/host_name，重复扩展、尾随内容或畸形边界返回未匹配。
   */
  static DomainSniffResult SniffTlsClientHelloSni(const uint8_t* data, std::size_t len);
  /**
   * 解析完整 HTTP/1 头并要求唯一 Host；absolute-form authority 必须与 Host
   * 一致，正文伪字段、折行和重复头不会参与规则匹配。
   */
  static DomainSniffResult SniffHttpHostHeader(const uint8_t* data, std::size_t len);
};

}  // namespace routesocks::net
