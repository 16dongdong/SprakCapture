#include "engine/runtime_config.h"

#include <arpa/inet.h>

#include <algorithm>
#include <charconv>
#include <set>
#include <sstream>
#include <string>

namespace routesocks::runtime {
namespace {

/** 判断配置语法允许的 ASCII 空白；结果不随 Android 进程 locale 改变。 */
bool IsAsciiWhitespace(unsigned char character) {
  return character == ' ' || character == '\t' || character == '\r' ||
         character == '\n' || character == '\v' || character == '\f';
}

/** 去除配置行两端空白，值内部空白保持原样以免改变凭据。 */
std::string Trim(const std::string &value) {
  const auto first =
      std::find_if(value.begin(), value.end(), [](unsigned char character) {
        return !IsAsciiWhitespace(character);
      });
  if (first == value.end())
    return {};
  const auto last =
      std::find_if(value.rbegin(), value.rend(), [](unsigned char character) {
        return !IsAsciiWhitespace(character);
      }).base();
  return std::string(first, last);
}

/** 去除文本行开头的 UTF-8 BOM；管理页面或 Windows
 * 编辑器保存的规则必须与服务端同义。 */
std::string RemoveUtf8Bom(std::string value) {
  constexpr char bom[] = "\xEF\xBB\xBF";
  if (value.size() >= 3 && value.compare(0, 3, bom) == 0)
    value.erase(0, 3);
  return value;
}

/** 把协议标识规范为 ASCII 大写，段名和 DNS 键与服务端保持不区分大小写。 */
std::string ToUpper(std::string value) {
  std::transform(value.begin(), value.end(), value.begin(),
                 [](unsigned char character) {
                   return static_cast<char>(
                       character >= 'a' && character <= 'z'
                           ? character - ('a' - 'A')
                           : character);
                 });
  return value;
}

/** 严格解析 1..65535 端口，禁止截断、符号和尾随字符。 */
bool ParsePort(const std::string &value, uint16_t *output) {
  unsigned int parsed = 0;
  const auto result =
      std::from_chars(value.data(), value.data() + value.size(), parsed);
  if (result.ec != std::errc() || result.ptr != value.data() + value.size() ||
      parsed == 0 || parsed > 65535) {
    return false;
  }
  *output = static_cast<uint16_t>(parsed);
  return true;
}

/** 写入已知配置键；键集合固定可及时暴露拼写错误。 */
bool AssignConfiguration(const std::string &key, const std::string &value,
                         RuntimeConfig *output, std::string *error) {
  if (key == "upstreamHost") {
    output->upstream.host = value;
    return true;
  }
  if (key == "username") {
    output->username = value;
    return true;
  }
  if (key == "password") {
    output->password = value;
    return true;
  }
  if (key == "localUsername") {
    output->local_username = value;
    return true;
  }
  if (key == "localPassword") {
    output->local_password = value;
    return true;
  }
  uint16_t *port = nullptr;
  if (key == "upstreamPort")
    port = &output->upstream.port;
  if (key == "localSocksPort")
    port = &output->local_socks_port;
  if (key == "selectedSocksPort")
    port = &output->selected_socks_port;
  if (key == "transparentTcpPort")
    port = &output->transparent_tcp_port;
  if (key == "transparentUdpPort")
    port = &output->transparent_udp_port;
  if (key == "selectedTransparentTcpPort")
    port = &output->selected_transparent_tcp_port;
  if (key == "selectedTransparentUdpPort")
    port = &output->selected_transparent_udp_port;
  if (key == "globalUdpQueueNumber")
    port = &output->global_udp_queue_number;
  if (key == "selectedUdpQueueNumber")
    port = &output->selected_udp_queue_number;
  if (port == nullptr) {
    *error = "Native 配置包含未知字段：" + key;
    return false;
  }
  if (!ParsePort(value, port)) {
    *error = "Native 配置端口无效：" + key;
    return false;
  }
  return true;
}

/** 从规则正文提取 `[DNS]`，主备顺序就是失败切换顺序。 */
bool ParseDns(const std::string &routing_text, std::vector<Endpoint> *servers,
              std::string *error) {
  constexpr std::size_t maximum_content_bytes = 1024 * 1024;
  constexpr std::size_t maximum_line_bytes = 8192;
  if (routing_text.empty() || routing_text.size() > maximum_content_bytes ||
      routing_text.find('\0') != std::string::npos) {
    *error = "规则正文大小无效或包含 NUL";
    return false;
  }
  std::istringstream stream(routing_text);
  std::string line;
  enum class RuleSection {
    kNone,
    kDns,
    kRoutingRule,
    kGlobalRoutingRule,
    kProxyApp,
  };
  RuleSection current_section = RuleSection::kNone;
  std::size_t dns_section_count = 0;
  std::set<std::string> roles;
  while (std::getline(stream, line)) {
    if (line.size() > maximum_line_bytes) {
      *error = "规则单行超过 8192 字节";
      return false;
    }
    line = RemoveUtf8Bom(line);
    line = Trim(line.substr(0, line.find('#')));
    if (line.empty())
      continue;
    if (line.front() == '[' && line.back() == ']') {
      const std::string section = ToUpper(line);
      if (section == "[DNS]") {
        current_section = RuleSection::kDns;
      } else if (section == "[ROUTINGRULE]") {
        current_section = RuleSection::kRoutingRule;
      } else if (section == "[GROUTINGRULE]") {
        current_section = RuleSection::kGlobalRoutingRule;
      } else if (section == "[PROXY_APP]") {
        current_section = RuleSection::kProxyApp;
      } else {
        *error = "规则包含未知段：" + line;
        return false;
      }
      if (current_section == RuleSection::kDns && ++dns_section_count > 1) {
        *error = "规则只能包含一个 [DNS] 段";
        return false;
      }
      continue;
    }
    if (current_section == RuleSection::kNone) {
      *error = "规则包含已知段之外的有效文本";
      return false;
    }
    // 其他三段由 RoutingRules 负责严格解析；这里只提取 DNS，不能把合法规则误当
    // DNS 行。
    if (current_section != RuleSection::kDns)
      continue;
    const std::size_t comma = line.find(',');
    if (comma == std::string::npos ||
        line.find(',', comma + 1) != std::string::npos) {
      *error = "[DNS] 行必须使用 ROLE,HOST 格式";
      return false;
    }
    const std::string role = ToUpper(Trim(line.substr(0, comma)));
    const std::string host = Trim(line.substr(comma + 1));
    if ((role != "PRIMARY" && role != "SECONDARY") || host.empty() ||
        !roles.insert(role).second) {
      *error = "[DNS] 仅允许唯一的 PRIMARY 和 SECONDARY";
      return false;
    }
    in_addr ipv4{};
    in6_addr ipv6{};
    if (inet_pton(AF_INET, host.c_str(), &ipv4) != 1 &&
        inet_pton(AF_INET6, host.c_str(), &ipv6) != 1) {
      *error = "[DNS] 地址必须是 IPv4 或 IPv6 字面量";
      return false;
    }
    Endpoint server{host, 53};
    if (role == "PRIMARY") {
      servers->insert(servers->begin(), std::move(server));
    } else {
      servers->push_back(std::move(server));
    }
  }
  if (servers->empty() || roles.count("PRIMARY") == 0) {
    *error = "规则缺少 [DNS] PRIMARY";
    return false;
  }
  return true;
}

} // namespace

/**
 * 解析有界 Native 启动配置并联动校验规则 DNS；配置超过 4 KiB、含 NUL、缺字段
 * 或节点/凭据非法均返回 false，且只有全部成功后才替换输出对象。
 */
bool ParseRuntimeConfig(const std::string &configuration_text,
                        const std::string &routing_text, RuntimeConfig *output,
                        std::string *error) {
  if (output == nullptr || error == nullptr)
    return false;
  constexpr std::size_t kMaximumConfigurationBytes = 4 * 1024;
  if (configuration_text.empty() ||
      configuration_text.size() > kMaximumConfigurationBytes ||
      configuration_text.find('\0') != std::string::npos) {
    *error = "Native 配置大小无效或包含 NUL";
    return false;
  }
  RuntimeConfig parsed;
  std::set<std::string> keys;
  std::istringstream stream(configuration_text);
  std::string line;
  while (std::getline(stream, line)) {
    line = Trim(RemoveUtf8Bom(line));
    if (line.empty())
      continue;
    const std::size_t equals = line.find('=');
    if (equals == std::string::npos) {
      *error = "Native 配置行缺少等号";
      return false;
    }
    const std::string key = Trim(line.substr(0, equals));
    const std::string value = line.substr(equals + 1);
    if (key.empty() || !keys.insert(key).second) {
      *error = "Native 配置字段为空或重复：" + key;
      return false;
    }
    if (!AssignConfiguration(key, value, &parsed, error))
      return false;
  }
  if (parsed.upstream.host.empty() || parsed.upstream.port == 0 ||
      parsed.username.empty() || parsed.password.empty() ||
      parsed.local_username.empty() || parsed.local_password.empty()) {
    *error = "Native 配置缺少上游节点或本地数据面凭据";
    return false;
  }
  if (parsed.username.size() > 255 || parsed.password.size() > 255 ||
      parsed.local_username.size() > 255 ||
      parsed.local_password.size() > 255) {
    *error = "Native 配置凭据超过 RFC1929 单字段 255 字节上限";
    return false;
  }
  in_addr upstream_ipv4{};
  in6_addr upstream_ipv6{};
  if (inet_pton(AF_INET, parsed.upstream.host.c_str(), &upstream_ipv4) != 1 &&
      inet_pton(AF_INET6, parsed.upstream.host.c_str(), &upstream_ipv6) != 1) {
    *error = "内置上游节点必须是 IP 字面量，禁止启动期隐式使用系统 DNS";
    return false;
  }
  if (!ParseDnsServers(routing_text, &parsed.dns_servers, error))
    return false;
  *output = std::move(parsed);
  return true;
}

bool ParseDnsServers(const std::string &routing_text,
                     std::vector<Endpoint> *servers, std::string *error) {
  if (servers == nullptr || error == nullptr)
    return false;
  servers->clear();
  return ParseDns(routing_text, servers, error);
}

} // namespace routesocks::runtime
