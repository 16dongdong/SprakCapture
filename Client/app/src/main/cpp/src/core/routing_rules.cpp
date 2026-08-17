#include "core/routing_rules.h"

#include <arpa/inet.h>

#include <algorithm>
#include <charconv>
#include <cstdint>
#include <limits>
#include <sstream>
#include <string>
#include <unordered_set>
#include <utility>
#include <vector>

namespace routesocks::core {

namespace {

/** 判断规则文本允许的 ASCII 空白；不接受 locale 扩展字符，保证三层解析一致。 */
bool IsAsciiWhitespace(unsigned char character) {
  return character == ' ' || character == '\t' || character == '\r' ||
         character == '\n' || character == '\v' || character == '\f';
}

/** 去除规则字段两端 ASCII 空白；字段内部字节保持不变供后续严格校验。 */
std::string Trim(const std::string &input) {
  const auto begin =
      std::find_if(input.begin(), input.end(),
                   [](unsigned char c) { return !IsAsciiWhitespace(c); });
  if (begin == input.end()) {
    return {};
  }
  const auto end =
      std::find_if(input.rbegin(), input.rend(), [](unsigned char c) {
        return !IsAsciiWhitespace(c);
      }).base();
  return std::string(begin, end);
}

/** 移除行内注释和首行 UTF-8 BOM；空行返回空字符串。 */
std::string RemoveCommentAndTrim(const std::string &line) {
  const std::size_t hash = line.find('#');
  const std::string no_comment =
      (hash == std::string::npos) ? line : line.substr(0, hash);
  std::string out = Trim(no_comment);
  if (out.size() >= 3 && static_cast<unsigned char>(out[0]) == 0xEF &&
      static_cast<unsigned char>(out[1]) == 0xBB &&
      static_cast<unsigned char>(out[2]) == 0xBF) {
    out.erase(0, 3);
    out = Trim(out);
  }
  return out;
}

/** 仅转换 ASCII 小写字母；协议关键字不受设备 locale 影响。 */
std::string ToUpper(std::string input) {
  std::transform(
      input.begin(), input.end(), input.begin(),
      [](unsigned char c) {
        return static_cast<char>(c >= 'a' && c <= 'z' ? c - ('a' - 'A') : c);
      });
  return input;
}

/** 仅转换 ASCII 大写字母；域名高位字节保留并由语法校验处理。 */
std::string ToLower(std::string input) {
  std::transform(
      input.begin(), input.end(), input.begin(),
      [](unsigned char c) {
        return static_cast<char>(c >= 'A' && c <= 'Z' ? c + ('a' - 'A') : c);
      });
  return input;
}

/** 按逗号保留真实字段数量；FINAL 必须两列，其他规则必须三列，禁止补齐或截断。
 */
std::vector<std::string> SplitCsv3(const std::string &line) {
  std::vector<std::string> fields;
  fields.reserve(3);

  std::size_t start = 0;
  while (start <= line.size()) {
    const std::size_t comma = line.find(',', start);
    if (comma == std::string::npos) {
      fields.push_back(Trim(line.substr(start)));
      break;
    }
    fields.push_back(Trim(line.substr(start, comma - start)));
    start = comma + 1;
  }

  return fields;
}

/** 全量解析无符号十进制；尾随字符、符号和溢出均返回 false。 */
bool ParseUnsigned(const std::string &text, unsigned int *value) {
  if (text.empty() || value == nullptr)
    return false;
  const auto result =
      std::from_chars(text.data(), text.data() + text.size(), *value);
  return result.ec == std::errc() && result.ptr == text.data() + text.size();
}

/**
 * 严格校验 Android 包名；Native 虽不解析 UID，仍要拒绝与 Kotlin
 * 捕获范围不同义的正文。 允许每段以 ASCII
 * 小写 ASCII 字母开头，后续使用小写字母、数字或下划线，并要求至少一个点；
 * 该约束与 Android 打包标识及服务端规则合同一致，避免大小写变体产生作用域漂移。
 */
bool IsAndroidPackageName(const std::string &value) {
  bool has_dot = false;
  bool segment_start = true;
  for (unsigned char character : value) {
    if (character == '.') {
      if (segment_start)
        return false;
      has_dot = true;
      segment_start = true;
      continue;
    }
    if (segment_start) {
      if (character < 'a' || character > 'z')
        return false;
      segment_start = false;
      continue;
    }
    if ((character < 'a' || character > 'z') &&
        (character < '0' || character > '9') && character != '_')
      return false;
  }
  return has_dot && !segment_start;
}

/** 使用 libc 严格解析 IPv4 字面量并转主机序；内部空白、缩写和尾随字符全部失败。
 */
bool ParseIpv4ToHostOrder(const std::string &ip, uint32_t *out_host_order) {
  if (out_host_order == nullptr)
    return false;
  in_addr address{};
  if (inet_pton(AF_INET, ip.c_str(), &address) != 1)
    return false;
  *out_host_order = ntohl(address.s_addr);
  return true;
}

/** 解析单端口或闭区间；空值、反向范围和越界返回中文原因。 */
bool ParsePortRange(const std::string &raw, RoutingRules::PortRange *out_range,
                    std::string *out_error) {
  if (out_range == nullptr) {
    if (out_error != nullptr) {
      *out_error = "端口范围输出为空";
    }
    return false;
  }

  const std::string text = Trim(raw);
  if (text.empty()) {
    if (out_error != nullptr) {
      *out_error = "端口表达式为空";
    }
    return false;
  }

  const std::size_t sep = text.find('-');
  if (sep == std::string::npos) {
    unsigned int port = 0;
    if (!ParseUnsigned(text, &port)) {
      if (out_error != nullptr) {
        *out_error = "端口格式无效：" + text;
      }
      return false;
    }
    if (port == 0 || port > std::numeric_limits<uint16_t>::max()) {
      if (out_error != nullptr) {
        *out_error = "端口超出范围：" + text;
      }
      return false;
    }
    out_range->start = static_cast<uint16_t>(port);
    out_range->end = static_cast<uint16_t>(port);
    return true;
  }

  const std::string left = Trim(text.substr(0, sep));
  const std::string right = Trim(text.substr(sep + 1));
  unsigned int start = 0;
  unsigned int end = 0;
  if (text.find('-', sep + 1) != std::string::npos ||
      !ParseUnsigned(left, &start) || !ParseUnsigned(right, &end)) {
    if (out_error != nullptr) {
      *out_error = "端口范围格式无效：" + text;
    }
    return false;
  }

  if (start == 0 || end == 0 || start > end ||
      end > std::numeric_limits<uint16_t>::max()) {
    if (out_error != nullptr) {
      *out_error = "端口范围越界或顺序错误：" + text;
    }
    return false;
  }

  out_range->start = static_cast<uint16_t>(start);
  out_range->end = static_cast<uint16_t>(end);
  return true;
}

/** 解析 IPv4 CIDR 并预计算网络和掩码；格式或前缀错误返回中文原因。 */
bool ParseCidr(const std::string &raw, RoutingRules::Cidr *out_cidr,
               std::string *out_error) {
  if (out_cidr == nullptr) {
    if (out_error != nullptr) {
      *out_error = "CIDR 输出为空";
    }
    return false;
  }

  const std::string text = Trim(raw);
  if (text.empty()) {
    if (out_error != nullptr) {
      *out_error = "CIDR 表达式为空";
    }
    return false;
  }

  const std::size_t slash = text.find('/');
  const std::string ip =
      (slash == std::string::npos) ? text : text.substr(0, slash);
  const std::string prefix_str =
      (slash == std::string::npos) ? "32" : text.substr(slash + 1);

  uint32_t ip_host = 0;
  if (!ParseIpv4ToHostOrder(ip, &ip_host)) {
    if (out_error != nullptr) {
      *out_error = "CIDR 包含无效 IPv4：" + text;
    }
    return false;
  }

  unsigned int prefix = 0;
  if (text.find('/', slash == std::string::npos ? text.size() : slash + 1) !=
          std::string::npos ||
      !ParseUnsigned(prefix_str, &prefix)) {
    if (out_error != nullptr) {
      *out_error = "CIDR 前缀格式无效：" + text;
    }
    return false;
  }
  if (prefix > 32) {
    if (out_error != nullptr) {
      *out_error = "CIDR 前缀超出范围：" + text;
    }
    return false;
  }

  uint32_t mask = 0;
  if (prefix == 0) {
    mask = 0;
  } else {
    mask = (prefix == 32) ? 0xFFFFFFFFU : (0xFFFFFFFFU << (32 - prefix));
  }

  out_cidr->mask = mask;
  out_cidr->network = ip_host & mask;
  return true;
}

/** 将协议动作映射为内部枚举；未知动作返回 false，调用方负责附加行号。 */
bool ParseRouteAction(const std::string &action_text, RouteAction *out_action) {
  if (out_action == nullptr) {
    return false;
  }

  const std::string action_upper = ToUpper(Trim(action_text));
  if (action_upper == "PROXY") {
    *out_action = RouteAction::kProxy;
    return true;
  }
  if (action_upper == "DIRECT") {
    *out_action = RouteAction::kDirect;
    return true;
  }
  if (action_upper == "REJECT") {
    *out_action = RouteAction::kReject;
    return true;
  }
  return false;
}

/** 把枚举转换为诊断文本；只用于运行期命中原因，不参与协议序列化。 */
const char *RouteActionText(RouteAction action) {
  switch (action) {
  case RouteAction::kProxy:
    return "PROXY";
  case RouteAction::kDirect:
    return "DIRECT";
  case RouteAction::kReject:
    return "REJECT";
  default:
    return "UNKNOWN";
  }
}

} // namespace

/**
 * 把固定四段正文拆成小职责解析事务；对象只在全部语法和跨段不变量通过后移交，
 * 因而失败不会污染调用方正在使用的规则快照。
 */
class RoutingRuleParser {
 public:
  /** 绑定诊断输出；解析失败时只写入当前事务的精确中文原因。 */
  explicit RoutingRuleParser(std::string *error) : error_(error) {}

  /** 顺序扫描完整正文；行长、NUL、未知段和段外正文均在分配规则前拒绝。 */
  bool Parse(const std::string &text) {
    constexpr std::size_t kMaximumContentBytes = 1024 * 1024;
    constexpr std::size_t kMaximumLineBytes = 8192;
    if (text.empty() || text.size() > kMaximumContentBytes ||
        text.find('\0') != std::string::npos) {
      SetError("规则正文大小无效或包含 NUL");
      return false;
    }

    std::istringstream input(text);
    std::string line;
    int line_number = 0;
    while (std::getline(input, line)) {
      ++line_number;
      if (line.size() > kMaximumLineBytes) {
        return FailLine(line_number, "超过长度上限");
      }
      std::string row = RemoveCommentAndTrim(line);
      if (!row.empty() && row.back() == '\r') {
        row.pop_back();
        row = Trim(row);
      }
      if (!row.empty() && !ParseLine(row, line_number)) {
        return false;
      }
    }
    return ValidateDocument();
  }

  /** 移交已完全校验的快照；只能在 Parse 返回 true 后调用。 */
  RoutingRules TakeRules() { return std::move(parsed_); }

 private:
  enum class Section {
    kNone = 0,
    kDns,
    kRoutingRule,
    kGlobalRoutingRule,
    kProxyApp,
  };

  /** 解析单个有效行并按当前段分发；DNS 正文由同一输入上的 DNS 解析器负责。 */
  bool ParseLine(const std::string &row, int line_number) {
    if (row.size() >= 2 && row.front() == '[' && row.back() == ']') {
      return EnterSection(row, line_number);
    }
    if (section_ == Section::kNone) {
      return FailLine(line_number, "位于已知段之外");
    }
    if (section_ == Section::kDns) {
      return true;
    }
    if (section_ == Section::kProxyApp) {
      return ParsePackage(row, line_number);
    }
    return ParseRoute(row, line_number);
  }

  /** 进入唯一已知段；重复声明和拼写错误会被精确拒绝，禁止静默吞掉正文。 */
  bool EnterSection(const std::string &row, int line_number) {
    const std::string section_name = ToUpper(row);
    if (section_name == "[DNS]") {
      return SelectSection(Section::kDns, &dns_found_, "[DNS]", line_number);
    }
    if (section_name == "[ROUTINGRULE]") {
      return SelectSection(Section::kRoutingRule, &routing_found_,
                           "[RoutingRule]", line_number);
    }
    if (section_name == "[GROUTINGRULE]") {
      return SelectSection(Section::kGlobalRoutingRule, &global_found_,
                           "[GRoutingRule]", line_number);
    }
    if (section_name == "[PROXY_APP]") {
      return SelectSection(Section::kProxyApp, &proxy_app_found_,
                           "[proxy_app]", line_number);
    }
    return FailLine(line_number, "包含未知段：" + row);
  }

  /** 记录段出现状态；同一段只能出现一次，防止段切换改变有序规则语义。 */
  bool SelectSection(Section section, bool *found, const char *name,
                     int line_number) {
    if (*found) {
      return FailLine(line_number, std::string("重复声明 ") + name);
    }
    *found = true;
    section_ = section;
    return true;
  }

  /** 校验小写 Android 包名并拒绝重复项，保持 UID 作用域与 Kotlin 完全一致。 */
  bool ParsePackage(const std::string &row, int line_number) {
    if (row.find(',') != std::string::npos || !IsAndroidPackageName(row)) {
      return FailLine(line_number, "应用包名格式无效");
    }
    if (!proxy_packages_.insert(row).second) {
      return FailLine(line_number, "重复声明应用包名：" + row);
    }
    has_proxy_packages_ = true;
    return true;
  }

  /** 解析有序路由行；FINAL 一经出现，本段后续任何有效行都立即失败。 */
  bool ParseRoute(const std::string &row, int line_number) {
    const std::vector<std::string> fields = SplitCsv3(row);
    if (fields.empty()) {
      return FailLine(line_number, "字段数量无效");
    }
    const std::string type = ToUpper(fields.front());
    bool &final_seen = section_ == Section::kGlobalRoutingRule
                           ? global_final_seen_
                           : routing_final_seen_;
    if (final_seen) {
      return FailLine(line_number, "位于 FINAL 之后");
    }
    if (type == "FINAL") {
      return ParseFinal(fields, line_number, &final_seen);
    }
    if (fields.size() != 3 || fields[1].empty() || fields[2].empty()) {
      return FailLine(line_number, "必须恰好包含三个非空字段");
    }

    RouteAction action = RouteAction::kDirect;
    if (!ParseRouteAction(fields[2], &action)) {
      return FailLine(line_number, "动作无效：" + fields[2]);
    }
    RoutingRules::Rule rule;
    rule.action = action;
    if (!ParseTypedRule(type, fields[1], line_number, &rule)) {
      return false;
    }
    Destination().push_back(std::move(rule));
    MarkCurrentRulesPresent();
    return true;
  }

  /** 解析本段终止动作；继续扫描而非提前结束，以便发现 FINAL 后的死规则。 */
  bool ParseFinal(const std::vector<std::string> &fields, int line_number,
                  bool *final_seen) {
    if (fields.size() != 2) {
      return FailLine(line_number, "FINAL 必须恰好包含两列");
    }
    RouteAction action = RouteAction::kDirect;
    if (!ParseRouteAction(fields[1], &action)) {
      return FailLine(line_number, "FINAL 动作无效：" + fields[1]);
    }
    Destination().push_back(
        {RoutingRules::RuleType::kFinal, action, {}, {}, {}});
    MarkCurrentRulesPresent();
    *final_seen = true;
    return true;
  }

  /** 解析非终止规则的类型专属值；失败时在底层原因前附加稳定行号。 */
  bool ParseTypedRule(const std::string &type, const std::string &value,
                      int line_number, RoutingRules::Rule *rule) {
    std::string field_error;
    if (type == "PORT") {
      RoutingRules::PortRange range;
      if (!ParsePortRange(value, &range, &field_error)) {
        return FailLine(line_number, field_error);
      }
      rule->type = RoutingRules::RuleType::kPort;
      rule->port = range;
      return true;
    }
    if (type == "IP-CIDR") {
      RoutingRules::Cidr cidr;
      if (!ParseCidr(value, &cidr, &field_error)) {
        return FailLine(line_number, field_error);
      }
      rule->type = RoutingRules::RuleType::kIpv4Cidr;
      rule->cidr = cidr;
      return true;
    }
    if (type == "DOMAIN" || type == "DOMAIN-KEYWORD") {
      rule->type = type == "DOMAIN" ? RoutingRules::RuleType::kDomain
                                     : RoutingRules::RuleType::kDomainKeyword;
      rule->text = ToLower(value);
      return true;
    }
    return FailLine(line_number, "类型不受支持：" + type);
  }

  /** 校验固定段和跨段关系；应用规则与包范围必须成对出现。 */
  bool ValidateDocument() {
    if (!dns_found_ || !routing_found_ || !global_found_ ||
        !proxy_app_found_) {
      SetError("规则缺少 [DNS]、[RoutingRule]、[GRoutingRule] 或 "
               "[proxy_app] 必需段");
      return false;
    }
    if (!has_routing_rules_ && !has_global_rules_) {
      SetError("规则至少需要一条应用规则或全局规则");
      return false;
    }
    if (has_routing_rules_ != has_proxy_packages_) {
      SetError("[RoutingRule] 与 [proxy_app] 必须同时配置");
      return false;
    }
    return true;
  }

  /** 返回当前路由段的目标容器；调用方已保证当前段不是 DNS 或 proxy_app。 */
  std::vector<RoutingRules::Rule> &Destination() {
    return section_ == Section::kGlobalRoutingRule ? parsed_.global_rules_
                                                    : parsed_.selected_rules_;
  }

  /** 标记当前作用域至少包含一条规则，用于最终跨段不变量校验。 */
  void MarkCurrentRulesPresent() {
    if (section_ == Section::kGlobalRoutingRule) {
      has_global_rules_ = true;
    } else {
      has_routing_rules_ = true;
    }
  }

  /** 生成带行号诊断并返回 false，供所有分支保持统一失败语义。 */
  bool FailLine(int line_number, const std::string &reason) {
    std::ostringstream message;
    message << "规则第 " << line_number << " 行" << reason;
    SetError(message.str());
    return false;
  }

  /** 写入可选诊断对象；调用方不请求文本时仍保留相同布尔失败语义。 */
  void SetError(const std::string &message) {
    if (error_ != nullptr) {
      *error_ = message;
    }
  }

  std::string *error_ = nullptr;
  RoutingRules parsed_;
  Section section_ = Section::kNone;
  bool dns_found_ = false;
  bool routing_found_ = false;
  bool global_found_ = false;
  bool proxy_app_found_ = false;
  bool has_routing_rules_ = false;
  bool has_global_rules_ = false;
  bool has_proxy_packages_ = false;
  bool routing_final_seen_ = false;
  bool global_final_seen_ = false;
  std::unordered_set<std::string> proxy_packages_;
};

/**
 * 严格解析固定四段规则正文；仅在完整事务成功后替换输出，失败返回精确诊断。
 */
bool RoutingRules::ParseFromText(const std::string &text,
                                 RoutingRules *out_rules,
                                 std::string *out_error) {
  if (out_rules == nullptr) {
    if (out_error != nullptr) {
      *out_error = "规则输出对象为空";
    }
    return false;
  }
  RoutingRuleParser parser(out_error);
  if (!parser.Parse(text)) {
    return false;
  }
  *out_rules = parser.TakeRules();
  return true;
}

RouteMatchResult RoutingRules::EvaluateIpv4ForContext(
    const std::string &dst_ip, uint16_t dst_port, const std::string &domain,
    bool selected_application) const {
  uint32_t ip_host = 0;
  const bool ip_valid = ParseIpv4ToHostOrder(dst_ip, &ip_host);
  const std::string domain_lower = ToLower(Trim(domain));
  const std::vector<Rule> &rules =
      selected_application ? selected_rules_ : global_rules_;
  for (const Rule &rule : rules) {
    bool matched = false;
    switch (rule.type) {
    case RuleType::kPort:
      matched = dst_port >= rule.port.start && dst_port <= rule.port.end;
      break;
    case RuleType::kIpv4Cidr:
      matched = ip_valid && (ip_host & rule.cidr.mask) == rule.cidr.network;
      break;
    case RuleType::kDomain:
      matched = !domain_lower.empty() && domain_lower == rule.text;
      break;
    case RuleType::kDomainKeyword:
      matched = !domain_lower.empty() &&
                domain_lower.find(rule.text) != std::string::npos;
      break;
    case RuleType::kFinal:
      matched = true;
      break;
    }
    if (matched) {
      return {rule.action,
              std::string(selected_application ? "应用规则 " : "全局规则 ") +
                  RouteActionText(rule.action)};
    }
  }
  return {RouteAction::kDirect,
          selected_application ? "应用默认 DIRECT" : "全局默认 DIRECT"};
}

bool RoutingRules::HasDomainRulesForContext(bool selected_application) const {
  const std::vector<Rule> &rules =
      selected_application ? selected_rules_ : global_rules_;
  return std::any_of(rules.begin(), rules.end(), [](const Rule &rule) {
    return rule.type == RuleType::kDomain ||
           rule.type == RuleType::kDomainKeyword;
  });
}

} // namespace routesocks::core
