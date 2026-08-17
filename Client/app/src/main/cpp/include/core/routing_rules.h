#pragma once

#include <cstdint>
#include <string>
#include <vector>

namespace routesocks::core {

enum class RouteAction : uint8_t {
  kProxy = 0,
  kDirect = 1,
  kReject = 2,
};

struct RouteMatchResult {
  RouteAction action = RouteAction::kDirect;
  std::string reason;
};

class RoutingRules {
 public:
  struct PortRange {
    uint16_t start = 0;
    uint16_t end = 0;
  };

  struct Cidr {
    uint32_t network = 0;
    uint32_t mask = 0;
  };

  static bool ParseFromText(const std::string& text, RoutingRules* out_rules, std::string* out_error);

  /** 按正文顺序评估互斥应用上下文；选中应用只执行普通规则，其他应用只执行全局规则。 */
  RouteMatchResult EvaluateIpv4ForContext(const std::string& dst_ip,
                                          uint16_t dst_port,
                                          const std::string& domain,
                                          bool selected_application) const;
  /** 仅检查给定应用作用域的域名规则，避免另一作用域触发不必要的乐观握手。 */
  bool HasDomainRulesForContext(bool selected_application) const;

 private:
  friend class RoutingRuleParser;

  enum class RuleType : uint8_t {
    kPort,
    kIpv4Cidr,
    kDomain,
    kDomainKeyword,
    kFinal,
  };

  /** 保存一条已校验规则；只填写与 type 对应的字段，避免运行期重复解析文本。 */
  struct Rule {
    RuleType type = RuleType::kFinal;
    RouteAction action = RouteAction::kDirect;
    PortRange port;
    Cidr cidr;
    std::string text;
  };

  std::vector<Rule> selected_rules_;
  std::vector<Rule> global_rules_;
};

}  // namespace routesocks::core
