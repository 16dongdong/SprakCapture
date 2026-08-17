#include "net/domain_sniffer.h"

#include <algorithm>
#include <array>
#include <charconv>
#include <string>
#include <string_view>

namespace routesocks::net {

namespace {

/** 严格校验可路由域名；拒绝空标签、超长标签和标签首尾连字符，失败返回 false。
 */
bool IsLikelyDomain(const std::string &s) {
  if (s.empty() || s.size() > 253) {
    return false;
  }

  if (s.front() == '.' || s.back() == '.') {
    return false;
  }

  std::size_t label_len = 0;
  bool has_dot = false;
  bool label_starts = true;
  char previous = '\0';
  for (char ch : s) {
    const unsigned char c = static_cast<unsigned char>(ch);
    if (ch == '.') {
      has_dot = true;
      if (label_len == 0 || label_len > 63 || previous == '-') {
        return false;
      }
      label_len = 0;
      label_starts = true;
      previous = ch;
      continue;
    }

    const bool ascii_letter = (c >= 'a' && c <= 'z') ||
                              (c >= 'A' && c <= 'Z');
    const bool ascii_digit = c >= '0' && c <= '9';
    if (!(ascii_letter || ascii_digit || ch == '-') ||
        (label_starts && ch == '-')) {
      return false;
    }

    label_starts = false;
    previous = ch;
    ++label_len;
    if (label_len > 63) {
      return false;
    }
  }

  if (label_len == 0 || label_len > 63 || previous == '-') {
    return false;
  }

  return has_dot;
}

/** 去除 HTTP OWS 定义的空格与制表符；不受进程 locale 影响，也不改变字段内部字节。 */
std::string Trim(std::string s) {
  auto is_space = [](unsigned char c) { return c == ' ' || c == '\t'; };
  s.erase(s.begin(), std::find_if(s.begin(), s.end(),
                                  [&](char c) { return !is_space(c); }));
  s.erase(
      std::find_if(s.rbegin(), s.rend(), [&](char c) { return !is_space(c); })
          .base(),
      s.end());
  return s;
}

/** 按 ASCII 规则把域名或头名称标准化为小写；高位字节保持原值并由域名校验拒绝。 */
std::string ToLower(std::string s) {
  std::transform(s.begin(), s.end(), s.begin(), [](unsigned char c) {
    return static_cast<char>(c >= 'A' && c <= 'Z' ? c + ('a' - 'A') : c);
  });
  return s;
}

struct HttpAuthority {
  std::string host;
  uint16_t port = 0;
};

/** 判断请求行或头字段是否含控制字符；HTTP/1
 * 折行和解析器差异都在嗅探边界直接拒绝。 */
bool ContainsHttpControl(const std::string &value) {
  return std::any_of(value.begin(), value.end(), [](unsigned char character) {
    return character < 0x20 || character == 0x7F;
  });
}

/**
 * 校验 HTTP/1 field-name 的 ASCII tchar 集合；空白、分隔符和高位字节均拒绝，
 * 防止下游对畸形字段跳过或归一化后与嗅探器产生 Host 信任差异。
 */
bool IsHttpFieldName(const std::string &name) {
  if (name.empty()) return false;
  constexpr std::string_view kPunctuation = "!#$%&'*+-.^_`|~";
  return std::all_of(name.begin(), name.end(), [&](unsigned char character) {
    const bool letter = (character >= 'a' && character <= 'z') ||
                        (character >= 'A' && character <= 'Z');
    const bool digit = character >= '0' && character <= '9';
    return letter || digit || kPunctuation.find(static_cast<char>(character)) !=
                                  std::string_view::npos;
  });
}

/**
 * 规范化 HTTP authority 的域名与端口；默认端口由 URI scheme 决定。
 * 用户信息、IPv6 字面量、重复冒号、空端口或越界端口均返回
 * false，避免与下游解析产生歧义。
 */
bool ParseHttpAuthority(const std::string &raw, uint16_t default_port,
                        HttpAuthority *authority) {
  if (authority == nullptr)
    return false;
  const std::string value = Trim(raw);
  if (value.empty() || ContainsHttpControl(value) ||
      value.find('@') != std::string::npos || value.front() == '[') {
    return false;
  }
  const std::size_t colon = value.find(':');
  if (colon != std::string::npos &&
      value.find(':', colon + 1) != std::string::npos)
    return false;
  std::string host =
      colon == std::string::npos ? value : value.substr(0, colon);
  uint16_t port = default_port;
  if (colon != std::string::npos) {
    const std::string port_text = value.substr(colon + 1);
    unsigned int parsed_port = 0;
    const auto result = std::from_chars(
        port_text.data(), port_text.data() + port_text.size(), parsed_port);
    if (port_text.empty() || result.ec != std::errc() ||
        result.ptr != port_text.data() + port_text.size() || parsed_port == 0 ||
        parsed_port > 65535) {
      return false;
    }
    port = static_cast<uint16_t>(parsed_port);
  }
  host = ToLower(host);
  if (!IsLikelyDomain(host))
    return false;
  authority->host = std::move(host);
  authority->port = port;
  return true;
}

/** 从网络字节序缓冲读取 16 位整数；越界返回 false 且不产生部分值。 */
bool ReadU16(const uint8_t *data, std::size_t len, std::size_t *off,
             uint16_t *out) {
  if (*off + 2 > len) {
    return false;
  }
  *out = static_cast<uint16_t>((static_cast<uint16_t>(data[*off]) << 8) |
                               static_cast<uint16_t>(data[*off + 1]));
  *off += 2;
  return true;
}

/** 从 TLS 三字节长度字段读取整数；越界返回 false。 */
bool ReadU24(const uint8_t *data, std::size_t len, std::size_t *off,
             uint32_t *out) {
  if (*off + 3 > len) {
    return false;
  }
  *out = (static_cast<uint32_t>(data[*off]) << 16) |
         (static_cast<uint32_t>(data[*off + 1]) << 8) |
         static_cast<uint32_t>(data[*off + 2]);
  *off += 3;
  return true;
}

/** 构造未匹配诊断，供有界增量读取判断是否继续收集首包。 */
DomainSniffResult MakeNoMatch(std::string detail) {
  DomainSniffResult r;
  r.matched = false;
  r.detail = std::move(detail);
  return r;
}

/** 构造已匹配结果并标准化域名；调用方据此执行当前作用域规则。 */
DomainSniffResult MakeMatch(const std::string &domain, SniffSource source,
                            const std::string &detail) {
  DomainSniffResult r;
  r.matched = true;
  r.domain = ToLower(domain);
  r.source = source;
  r.detail = detail;
  return r;
}

/**
 * 完整解析唯一的 server_name 扩展；扩展正文、名称列表与每个条目都必须精确消费。
 * 调用方只在整个 ClientHello 解析成功后采用输出域名，重复 host_name、尾随字节或
 * 非规范域名均返回 false，避免不同 TLS 实现对歧义列表作出不同规则决策。
 */
bool ParseServerNameExtension(const uint8_t *data, std::size_t begin,
                              std::size_t end, std::string *domain) {
  if (domain == nullptr || begin > end) {
    return false;
  }
  std::size_t offset = begin;
  uint16_t list_length = 0;
  if (!ReadU16(data, end, &offset, &list_length) ||
      offset + list_length != end) {
    return false;
  }

  const std::size_t list_end = offset + list_length;
  bool host_name_seen = false;
  while (offset < list_end) {
    if (list_end - offset < 3) {
      return false;
    }
    const uint8_t name_type = data[offset++];
    uint16_t name_length = 0;
    if (!ReadU16(data, list_end, &offset, &name_length) ||
        name_length == 0 || offset + name_length > list_end) {
      return false;
    }
    if (name_type == 0) {
      if (host_name_seen) {
        return false;
      }
      std::string candidate(reinterpret_cast<const char *>(data + offset),
                            name_length);
      if (!IsLikelyDomain(candidate)) {
        return false;
      }
      *domain = std::move(candidate);
      host_name_seen = true;
    }
    offset += name_length;
  }
  return offset == list_end && host_name_seen;
}

} // namespace

DomainSniffResult DomainSniffer::Sniff(const uint8_t *data, std::size_t len) {
  if (data == nullptr || len == 0) {
    return MakeNoMatch("输入为空");
  }

  DomainSniffResult tls = SniffTlsClientHelloSni(data, len);
  if (tls.matched) {
    return tls;
  }

  DomainSniffResult http = SniffHttpHostHeader(data, len);
  if (http.matched) {
    return http;
  }

  DomainSniffResult out;
  out.matched = false;
  out.detail = "未识别域名（TLS=" + tls.detail + "，HTTP=" + http.detail + "）";
  return out;
}

DomainSniffResult DomainSniffer::Sniff(const std::vector<uint8_t> &data) {
  if (data.empty()) {
    return MakeNoMatch("输入为空");
  }
  return Sniff(data.data(), data.size());
}

DomainSniffResult DomainSniffer::SniffTlsClientHelloSni(const uint8_t *data,
                                                        std::size_t len) {
  if (data == nullptr || len < 5) {
    return MakeNoMatch("TLS record 长度不足");
  }

  // 只接受握手 record；其他 record 类型不能作为 ClientHello 域名证据。
  if (data[0] != 0x16) {
    return MakeNoMatch("不是 TLS 握手 record");
  }

  // ClientHello 可以横跨多个 handshake record；先拼接 record
  // payload，再按握手长度解析。
  std::vector<uint8_t> handshake;
  std::size_t record_offset = 0;
  std::size_t required_handshake = 4;
  while (record_offset + 5 <= len && handshake.size() < required_handshake) {
    if (data[record_offset] != 0x16)
      return MakeNoMatch("TLS 握手跨入了非握手 record");
    const std::size_t record_length = static_cast<std::size_t>(
        (data[record_offset + 3] << 8U) | data[record_offset + 4]);
    if (record_offset + 5 + record_length > len)
      return MakeNoMatch("TLS record 尚未完整");
    handshake.insert(handshake.end(), data + record_offset + 5,
                     data + record_offset + 5 + record_length);
    if (handshake.size() >= 4) {
      required_handshake = 4 + (static_cast<std::size_t>(handshake[1]) << 16U) +
                           (static_cast<std::size_t>(handshake[2]) << 8U) +
                           handshake[3];
    }
    record_offset += 5 + record_length;
  }
  if (handshake.size() < required_handshake)
    return MakeNoMatch("ClientHello 尚未完整");
  data = handshake.data();
  len = required_handshake;
  std::size_t off = 0;

  if (data[off] != 0x01) {
    return MakeNoMatch("不是 ClientHello");
  }
  ++off;

  uint32_t hello_len = 0;
  if (!ReadU24(data, len, &off, &hello_len)) {
    return MakeNoMatch("ClientHello 长度字段无效");
  }

  if (off + hello_len > len) {
    return MakeNoMatch("ClientHello 数据不完整");
  }

  const std::size_t hello_end = off + hello_len;

  // 固定头包含协议版本与随机数，必须完整位于声明的 ClientHello 边界内。
  if (off + 34 > hello_end) {
    return MakeNoMatch("ClientHello 版本或随机数不完整");
  }
  off += 34;

  // 变长会话标识只按声明长度前进，禁止越过握手边界。
  if (off + 1 > hello_end) {
    return MakeNoMatch("缺少 session id 长度");
  }
  const uint8_t session_id_len = data[off++];
  if (off + session_id_len > hello_end) {
    return MakeNoMatch("session id 长度无效");
  }
  off += session_id_len;

  // 密码套件是成对字节编码，空列表或奇数长度均属于畸形握手。
  uint16_t cipher_len = 0;
  if (!ReadU16(data, hello_end, &off, &cipher_len)) {
    return MakeNoMatch("缺少密码套件长度");
  }
  if (cipher_len == 0 || (cipher_len % 2) != 0 ||
      off + cipher_len > hello_end) {
    return MakeNoMatch("密码套件长度无效");
  }
  off += cipher_len;

  // 压缩方法至少包含一个条目，长度字段不得吞入扩展块。
  if (off + 1 > hello_end) {
    return MakeNoMatch("缺少压缩方法长度");
  }
  const uint8_t comp_len = data[off++];
  if (comp_len == 0 || off + comp_len > hello_end) {
    return MakeNoMatch("压缩方法长度无效");
  }
  off += comp_len;

  // 规则只信任完整扩展块；声明长度必须恰好覆盖 ClientHello 剩余内容。
  if (off == hello_end) {
    return MakeNoMatch("ClientHello 没有扩展");
  }

  uint16_t ext_total_len = 0;
  if (!ReadU16(data, hello_end, &off, &ext_total_len)) {
    return MakeNoMatch("缺少扩展总长度");
  }
  if (off + ext_total_len != hello_end) {
    return MakeNoMatch("扩展块长度无效");
  }

  const std::size_t ext_end = off + ext_total_len;
  bool server_name_seen = false;
  std::string server_name;
  while (off < ext_end) {
    if (ext_end - off < 4) {
      return MakeNoMatch("扩展头损坏");
    }
    uint16_t ext_type = 0;
    uint16_t ext_len = 0;
    if (!ReadU16(data, ext_end, &off, &ext_type) ||
        !ReadU16(data, ext_end, &off, &ext_len)) {
      return MakeNoMatch("扩展头损坏");
    }
    if (off + ext_len > ext_end) {
      return MakeNoMatch("扩展正文损坏");
    }

    if (ext_type == 0x0000) {
      if (server_name_seen) {
        return MakeNoMatch("ClientHello 包含重复 SNI 扩展");
      }
      if (!ParseServerNameExtension(data, off, off + ext_len, &server_name)) {
        return MakeNoMatch("SNI 扩展结构或域名无效");
      }
      server_name_seen = true;
    }

    off += ext_len;
  }
  if (off != ext_end) {
    return MakeNoMatch("扩展块未完整消费");
  }
  return server_name_seen
             ? MakeMatch(server_name, SniffSource::kTlsSni, "TLS SNI")
             : MakeNoMatch("未找到 SNI 扩展");
}

/**
 * 严格解析完整 HTTP/1 请求头并提取唯一 Host。
 * 透明代理只信任与 absolute-form authority 一致的
 * Host；重复头、折行、控制字符或歧义 authority 均不参与规则决策。
 */
DomainSniffResult DomainSniffer::SniffHttpHostHeader(const uint8_t *data,
                                                     std::size_t len) {
  if (data == nullptr || len == 0)
    return MakeNoMatch("输入为空");

  constexpr std::size_t kMaxInspect = 16 * 1024;
  const std::size_t use_len = std::min(len, kMaxInspect);
  std::string text(reinterpret_cast<const char *>(data), use_len);
  const std::size_t header_end = text.find("\r\n\r\n");
  if (header_end == std::string::npos)
    return MakeNoMatch("HTTP 请求头尚未完整");
  text.resize(header_end);

  const std::size_t request_line_end = text.find("\r\n");
  if (request_line_end == std::string::npos)
    return MakeNoMatch("HTTP 请求行不完整");
  const std::string request_line = text.substr(0, request_line_end);
  if (ContainsHttpControl(request_line))
    return MakeNoMatch("HTTP 请求行包含控制字符");
  const std::size_t first_space = request_line.find(' ');
  const std::size_t second_space =
      first_space == std::string::npos
          ? std::string::npos
          : request_line.find(' ', first_space + 1);
  if (first_space == std::string::npos || second_space == std::string::npos ||
      request_line.find(' ', second_space + 1) != std::string::npos) {
    return MakeNoMatch("HTTP 请求行格式错误");
  }
  const std::string method = request_line.substr(0, first_space);
  const std::string request_target =
      request_line.substr(first_space + 1, second_space - first_space - 1);
  const std::string version = request_line.substr(second_space + 1);
  constexpr std::array<const char *, 9> kMethods = {
      "GET",     "POST",  "HEAD",    "PUT",   "DELETE",
      "OPTIONS", "PATCH", "CONNECT", "TRACE",
  };
  if (std::find(kMethods.begin(), kMethods.end(), method) == kMethods.end() ||
      request_target.empty() ||
      (version != "HTTP/1.0" && version != "HTTP/1.1")) {
    return MakeNoMatch("不是规范 HTTP/1 请求");
  }

  bool absolute_form = false;
  uint16_t default_port = 0;
  std::string target_authority;
  const std::string lower_target = ToLower(request_target);
  std::size_t authority_begin = 0;
  if (lower_target.rfind("http://", 0) == 0) {
    absolute_form = true;
    default_port = 80;
    authority_begin = 7;
  } else if (lower_target.rfind("https://", 0) == 0) {
    absolute_form = true;
    default_port = 443;
    authority_begin = 8;
  }
  if (absolute_form) {
    const std::size_t authority_end =
        request_target.find_first_of("/?#", authority_begin);
    target_authority = request_target.substr(
        authority_begin, authority_end == std::string::npos
                             ? std::string::npos
                             : authority_end - authority_begin);
  }

  std::string host_value;
  std::size_t cursor = request_line_end + 2;
  while (cursor < text.size()) {
    const std::size_t line_end = text.find("\r\n", cursor);
    const std::size_t current_end =
        line_end == std::string::npos ? text.size() : line_end;
    const std::string line = text.substr(cursor, current_end - cursor);
    if (line.empty())
      break;
    if (line.front() == ' ' || line.front() == '\t' ||
        ContainsHttpControl(line)) {
      return MakeNoMatch("HTTP 请求头包含折行或控制字符");
    }
    const std::size_t colon = line.find(':');
    if (colon == std::string::npos ||
        !IsHttpFieldName(line.substr(0, colon)))
      return MakeNoMatch("HTTP 请求头格式错误");
    const std::string name = ToLower(line.substr(0, colon));
    if (name == "host") {
      if (!host_value.empty())
        return MakeNoMatch("HTTP 请求包含重复 Host");
      host_value = Trim(line.substr(colon + 1));
      if (host_value.empty())
        return MakeNoMatch("Host 请求头为空");
    }
    if (line_end == std::string::npos)
      break;
    cursor = line_end + 2;
  }
  if (host_value.empty())
    return MakeNoMatch("未找到 Host 请求头");

  HttpAuthority host;
  if (!ParseHttpAuthority(host_value, default_port, &host)) {
    return MakeNoMatch("Host 请求头不是规范域名 authority");
  }
  if (absolute_form) {
    HttpAuthority target;
    if (!ParseHttpAuthority(target_authority, default_port, &target) ||
        target.host != host.host || target.port != host.port) {
      return MakeNoMatch("absolute-form authority 与 Host 不一致");
    }
  }
  return MakeMatch(host.host, SniffSource::kHttpHost, "HTTP Host");
}

} // namespace routesocks::net
