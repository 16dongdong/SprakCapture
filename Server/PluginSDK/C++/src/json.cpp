#include "trafficMod/sdk.hpp"

#include <charconv>
#include <cmath>
#include <cstdio>
#include <limits>
#include <stdexcept>
#include <type_traits>
#include <utility>

namespace trafficMod {
namespace {

/// 实现无依赖 JSON 递归下降解析；解析器只持有调用期间有效的 UTF-8 输入视图。
class JsonParser final {
public:
  explicit JsonParser(std::string_view text) : text_(text) {}

  /// 解析一个完整根节点；尾随非空白内容表示调用帧损坏。
  [[nodiscard]] Json parseDocument() {
    Json result = parseValue();
    skipWhitespace();
    if (offset_ != text_.size()) {
      fail("JSON 根节点后存在额外字符");
    }
    return result;
  }

private:
  /// 跳过 JSON 允许的四种 ASCII 空白；不接受其他 Unicode 空白以保持协议确定性。
  void skipWhitespace() noexcept {
    while (offset_ < text_.size()) {
      const char character = text_[offset_];
      if (character != ' ' && character != '\n' && character != '\r' &&
          character != '\t') {
        break;
      }
      ++offset_;
    }
  }

  /// 解析任意 JSON 值；非法首字符或意外结束均产生包含偏移的异常。
  [[nodiscard]] Json parseValue() {
    skipWhitespace();
    if (offset_ >= text_.size()) {
      fail("JSON 意外结束");
    }
    switch (text_[offset_]) {
    case 'n':
      consumeLiteral("null");
      return nullptr;
    case 't':
      consumeLiteral("true");
      return true;
    case 'f':
      consumeLiteral("false");
      return false;
    case '"':
      return parseString();
    case '[':
      return parseArray();
    case '{':
      return parseObject();
    default:
      return parseNumber();
    }
  }

  /// 消费固定关键字；拼写不完整时直接拒绝当前 ABI 帧。
  void consumeLiteral(std::string_view literal) {
    if (text_.substr(offset_, literal.size()) != literal) {
      fail("JSON 关键字无效");
    }
    offset_ += literal.size();
  }

  /// 解析 JSON 字符串并把 Unicode 转义规范化为
  /// UTF-8；非法代理项不会被替换吞掉。
  [[nodiscard]] std::string parseString() {
    expect('"');
    std::string result;
    while (offset_ < text_.size()) {
      const unsigned char character =
          static_cast<unsigned char>(text_[offset_++]);
      if (character == '"') {
        return result;
      }
      if (character < 0x20) {
        fail("JSON 字符串包含控制字符");
      }
      if (character != '\\') {
        result.push_back(static_cast<char>(character));
        continue;
      }
      parseEscape(result);
    }
    fail("JSON 字符串未闭合");
  }

  /// 解析反斜线转义；高代理项必须紧邻合法低代理项，避免生成非法 UTF-8。
  void parseEscape(std::string &output) {
    if (offset_ >= text_.size()) {
      fail("JSON 转义意外结束");
    }
    switch (text_[offset_++]) {
    case '"':
      output.push_back('"');
      return;
    case '\\':
      output.push_back('\\');
      return;
    case '/':
      output.push_back('/');
      return;
    case 'b':
      output.push_back('\b');
      return;
    case 'f':
      output.push_back('\f');
      return;
    case 'n':
      output.push_back('\n');
      return;
    case 'r':
      output.push_back('\r');
      return;
    case 't':
      output.push_back('\t');
      return;
    case 'u':
      break;
    default:
      fail("JSON 转义字符无效");
    }
    std::uint32_t codePoint = parseHexCodeUnit();
    if (codePoint >= 0xD800 && codePoint <= 0xDBFF) {
      if (text_.substr(offset_, 2) != "\\u") {
        fail("JSON 高代理项缺少低代理项");
      }
      offset_ += 2;
      const std::uint32_t low = parseHexCodeUnit();
      if (low < 0xDC00 || low > 0xDFFF) {
        fail("JSON 低代理项无效");
      }
      codePoint = 0x10000 + ((codePoint - 0xD800) << 10) + (low - 0xDC00);
    } else if (codePoint >= 0xDC00 && codePoint <= 0xDFFF) {
      fail("JSON 孤立低代理项无效");
    }
    appendUtf8(output, codePoint);
  }

  /// 读取四位十六进制 UTF-16 代码单元；非十六进制字符会终止解析。
  [[nodiscard]] std::uint32_t parseHexCodeUnit() {
    if (offset_ + 4 > text_.size()) {
      fail("JSON Unicode 转义不完整");
    }
    std::uint32_t value = 0;
    for (int index = 0; index < 4; ++index) {
      const char character = text_[offset_++];
      value <<= 4;
      if (character >= '0' && character <= '9')
        value += character - '0';
      else if (character >= 'a' && character <= 'f')
        value += character - 'a' + 10;
      else if (character >= 'A' && character <= 'F')
        value += character - 'A' + 10;
      else
        fail("JSON Unicode 转义包含非十六进制字符");
    }
    return value;
  }

  /// 把 Unicode 标量编码为 UTF-8；调用方已排除代理区，因此不存在替代字符分支。
  static void appendUtf8(std::string &output, std::uint32_t codePoint) {
    if (codePoint <= 0x7F) {
      output.push_back(static_cast<char>(codePoint));
    } else if (codePoint <= 0x7FF) {
      output.push_back(static_cast<char>(0xC0 | (codePoint >> 6)));
      output.push_back(static_cast<char>(0x80 | (codePoint & 0x3F)));
    } else if (codePoint <= 0xFFFF) {
      output.push_back(static_cast<char>(0xE0 | (codePoint >> 12)));
      output.push_back(static_cast<char>(0x80 | ((codePoint >> 6) & 0x3F)));
      output.push_back(static_cast<char>(0x80 | (codePoint & 0x3F)));
    } else {
      output.push_back(static_cast<char>(0xF0 | (codePoint >> 18)));
      output.push_back(static_cast<char>(0x80 | ((codePoint >> 12) & 0x3F)));
      output.push_back(static_cast<char>(0x80 | ((codePoint >> 6) & 0x3F)));
      output.push_back(static_cast<char>(0x80 | (codePoint & 0x3F)));
    }
  }

  /// 解析数组并保持元素顺序；缺失逗号或尾随逗号均按标准 JSON 拒绝。
  [[nodiscard]] Json parseArray() {
    expect('[');
    Json::Array values;
    skipWhitespace();
    if (consume(']'))
      return values;
    while (true) {
      values.push_back(parseValue());
      skipWhitespace();
      if (consume(']'))
        return values;
      expect(',');
    }
  }

  /// 解析对象并拒绝重复键；静默覆盖配置字段会掩盖插件包错误，因此明确失败。
  [[nodiscard]] Json parseObject() {
    expect('{');
    Json::Object values;
    skipWhitespace();
    if (consume('}'))
      return values;
    while (true) {
      skipWhitespace();
      if (offset_ >= text_.size() || text_[offset_] != '"')
        fail("JSON 对象键必须为字符串");
      std::string key = parseString();
      skipWhitespace();
      expect(':');
      Json child = parseValue();
      if (!values.emplace(std::move(key), std::move(child)).second)
        fail("JSON 对象包含重复键");
      skipWhitespace();
      if (consume('}'))
        return values;
      expect(',');
    }
  }

  /// 解析 JSON 数字；整数覆盖完整 int64/u64，只有含小数或指数的数才使用
  /// double。
  ///
  /// Native ABI 明确保留完整 u64。整数溢出后回退 double 会在插件读取代际、配置或
  /// 私有协议字段前静默丢位，因此超出 int64/u64 联合集合的整数必须直接报告越界。
  [[nodiscard]] Json parseNumber() {
    const std::size_t start = offset_;
    if (consume('-') && offset_ >= text_.size())
      fail("JSON 数字不完整");
    if (consume('0')) {
      if (offset_ < text_.size() && text_[offset_] >= '0' &&
          text_[offset_] <= '9')
        fail("JSON 数字包含前导零");
    } else {
      consumeDigits(true);
    }
    bool floating = false;
    if (consume('.')) {
      floating = true;
      consumeDigits(true);
    }
    if (offset_ < text_.size() &&
        (text_[offset_] == 'e' || text_[offset_] == 'E')) {
      floating = true;
      ++offset_;
      if (offset_ < text_.size() &&
          (text_[offset_] == '+' || text_[offset_] == '-'))
        ++offset_;
      consumeDigits(true);
    }
    const std::string_view number = text_.substr(start, offset_ - start);
    if (!floating) {
      if (number.front() == '-') {
        std::int64_t value = 0;
        const auto result = std::from_chars(
            number.data(), number.data() + number.size(), value);
        if (result.ec == std::errc() &&
            result.ptr == number.data() + number.size())
          return value;
        fail("JSON 有符号整数越界");
      }
      std::uint64_t value = 0;
      const auto result =
          std::from_chars(number.data(), number.data() + number.size(), value);
      if (result.ec != std::errc() ||
          result.ptr != number.data() + number.size())
        fail("JSON 无符号整数越界");
      if (value <=
          static_cast<std::uint64_t>(std::numeric_limits<std::int64_t>::max()))
        return static_cast<std::int64_t>(value);
      return value;
    }
    double value = 0;
    const auto result =
        std::from_chars(number.data(), number.data() + number.size(), value,
                        std::chars_format::general);
    if (result.ec != std::errc() ||
        result.ptr != number.data() + number.size() || !std::isfinite(value))
      fail("JSON 数字越界");
    return value;
  }

  /// 消费一段十进制数字；required 为真且没有数字时报告格式错误。
  void consumeDigits(bool required) {
    const std::size_t start = offset_;
    while (offset_ < text_.size() && text_[offset_] >= '0' &&
           text_[offset_] <= '9')
      ++offset_;
    if (required && start == offset_)
      fail("JSON 数字缺少数字");
  }

  /// 在当前位置存在目标字符时消费它；用于可选分隔符和数值符号。
  [[nodiscard]] bool consume(char expected) noexcept {
    if (offset_ >= text_.size() || text_[offset_] != expected)
      return false;
    ++offset_;
    return true;
  }

  /// 要求当前位置为指定字符；不匹配时给出统一偏移诊断。
  void expect(char expected) {
    if (!consume(expected))
      fail("JSON 缺少必要分隔符");
  }

  /// 抛出带字节偏移的解析错误；错误只进入 SDK ABI 状态码，不跨越 C 回调边界。
  [[noreturn]] void fail(std::string_view message) const {
    throw std::runtime_error(std::string(message) + "，偏移 " +
                             std::to_string(offset_));
  }

  std::string_view text_;
  std::size_t offset_ = 0;
};

/// 序列化 JSON 字符串并转义控制字符；有效 UTF-8 非 ASCII 字节保持原样。
void appendQuoted(std::string &output, std::string_view value) {
  static constexpr char hex[] = "0123456789abcdef";
  output.push_back('"');
  for (const unsigned char character : value) {
    switch (character) {
    case '"':
      output += "\\\"";
      break;
    case '\\':
      output += "\\\\";
      break;
    case '\b':
      output += "\\b";
      break;
    case '\f':
      output += "\\f";
      break;
    case '\n':
      output += "\\n";
      break;
    case '\r':
      output += "\\r";
      break;
    case '\t':
      output += "\\t";
      break;
    default:
      if (character < 0x20) {
        output += "\\u00";
        output.push_back(hex[character >> 4]);
        output.push_back(hex[character & 0x0F]);
      } else {
        output.push_back(static_cast<char>(character));
      }
    }
  }
  output.push_back('"');
}

/// 递归序列化 JSON 树；该函数与 JsonParser 共同构成 Native ABI 的唯一 JSON
/// 边界。
void appendJson(std::string &output, const Json &json) {
  std::visit(
      [&output](const auto &value) {
        using Type = std::decay_t<decltype(value)>;
        if constexpr (std::is_same_v<Type, std::nullptr_t>)
          output += "null";
        else if constexpr (std::is_same_v<Type, bool>)
          output += value ? "true" : "false";
        else if constexpr (std::is_same_v<Type, std::int64_t>)
          output += std::to_string(value);
        else if constexpr (std::is_same_v<Type, std::uint64_t>)
          output += std::to_string(value);
        else if constexpr (std::is_same_v<Type, double>) {
          if (!std::isfinite(value))
            throw std::runtime_error("JSON 不支持非有限浮点数");
          char buffer[64]{};
          const auto result =
              std::to_chars(buffer, buffer + sizeof(buffer), value,
                            std::chars_format::general,
                            std::numeric_limits<double>::max_digits10);
          if (result.ec != std::errc())
            throw std::runtime_error("JSON 浮点数序列化失败");
          output.append(buffer, result.ptr);
        } else if constexpr (std::is_same_v<Type, std::string>)
          appendQuoted(output, value);
        else if constexpr (std::is_same_v<Type, Json::Array>) {
          output.push_back('[');
          for (std::size_t index = 0; index < value.size(); ++index) {
            if (index != 0)
              output.push_back(',');
            appendJson(output, value[index]);
          }
          output.push_back(']');
        } else {
          output.push_back('{');
          std::size_t index = 0;
          for (const auto &[key, child] : value) {
            if (index++ != 0)
              output.push_back(',');
            appendQuoted(output, key);
            output.push_back(':');
            appendJson(output, child);
          }
          output.push_back('}');
        }
      },
      json.value());
}

} // namespace

// 构造函数覆盖协议 JSON 的全部标量和容器类型，调用方无需操作底层 variant。
Json::Json() noexcept : value_(nullptr) {}
Json::Json(std::nullptr_t) noexcept : value_(nullptr) {}
Json::Json(bool value) noexcept : value_(value) {}
Json::Json(std::int32_t value) noexcept
    : value_(static_cast<std::int64_t>(value)) {}
Json::Json(std::uint32_t value) noexcept
    : value_(static_cast<std::int64_t>(value)) {}
Json::Json(std::int64_t value) noexcept : value_(value) {}
Json::Json(std::uint64_t value) noexcept : value_(value) {}
Json::Json(double value) noexcept : value_(value) {}
Json::Json(const char *value)
    : value_(std::string(value == nullptr ? "" : value)) {}
Json::Json(std::string value) : value_(std::move(value)) {}
Json::Json(Array value) : value_(std::move(value)) {}
Json::Json(Object value) : value_(std::move(value)) {}

/// 解析完整 UTF-8 JSON 文本；解析失败抛出带偏移的异常供 SDK 边界转换为状态码。
Json Json::parse(std::string_view text) {
  return JsonParser(text).parseDocument();
}

/// 生成紧凑稳定的 JSON 文本；不添加影响热路径体积的格式化空白。
std::string Json::dump() const {
  std::string output;
  appendJson(output, *this);
  return output;
}

/// 判断节点是否为空值，不执行隐式转换。
bool Json::isNull() const noexcept {
  return std::holds_alternative<std::nullptr_t>(value_);
}
/// 判断节点是否为对象，不执行隐式转换。
bool Json::isObject() const noexcept {
  return std::holds_alternative<Object>(value_);
}
/// 判断节点是否为数组，不执行隐式转换。
bool Json::isArray() const noexcept {
  return std::holds_alternative<Array>(value_);
}

/// 查找对象字段；类型不匹配与键缺失均返回空指针，便于处理可选协议字段。
const Json *Json::find(std::string_view key) const noexcept {
  const auto *object = std::get_if<Object>(&value_);
  if (object == nullptr)
    return nullptr;
  const auto iterator = object->find(key);
  return iterator == object->end() ? nullptr : &iterator->second;
}

/// 返回可写对象字段；该 API 不会把标量静默改成对象，避免隐藏插件逻辑错误。
Json &Json::operator[](std::string key) {
  auto *object = std::get_if<Object>(&value_);
  if (object == nullptr)
    throw std::runtime_error("JSON 节点不是对象");
  return (*object)[std::move(key)];
}

/// 返回指定数组元素；非法类型和越界统一由标准异常明确报告。
const Json &Json::at(std::size_t index) const { return asArray().at(index); }
/// 返回字符串值；类型不匹配时拒绝隐式格式转换。
const std::string &Json::asString() const {
  return std::get<std::string>(value_);
}
/// 返回布尔值；类型不匹配时拒绝整数到布尔的隐式转换。
bool Json::asBool() const { return std::get<bool>(value_); }
/// 返回精确有符号整数；可表示的无符号节点允许无损读取，越界时明确失败。
std::int64_t Json::asInteger() const {
  if (const auto *signedValue = std::get_if<std::int64_t>(&value_))
    return *signedValue;
  if (const auto *unsignedValue = std::get_if<std::uint64_t>(&value_);
      unsignedValue != nullptr &&
      *unsignedValue <=
          static_cast<std::uint64_t>(std::numeric_limits<std::int64_t>::max()))
    return static_cast<std::int64_t>(*unsignedValue);
  throw std::runtime_error("JSON 整数超出 int64 范围或节点不是整数");
}
/// 返回精确无符号整数；非负有符号节点允许无损读取，负数和其他类型明确失败。
std::uint64_t Json::asUnsignedInteger() const {
  if (const auto *unsignedValue = std::get_if<std::uint64_t>(&value_))
    return *unsignedValue;
  if (const auto *signedValue = std::get_if<std::int64_t>(&value_);
      signedValue != nullptr && *signedValue >= 0)
    return static_cast<std::uint64_t>(*signedValue);
  throw std::runtime_error("JSON 整数为负数或节点不是整数");
}
/// 返回数组常量引用；引用生命周期受当前 Json 节点约束。
const Json::Array &Json::asArray() const { return std::get<Array>(value_); }
/// 返回对象常量引用；引用生命周期受当前 Json 节点约束。
const Json::Object &Json::asObject() const { return std::get<Object>(value_); }
/// 返回底层变体供高级作者访问；该方法不复制大正文。
const Json::Value &Json::value() const noexcept { return value_; }

} // namespace trafficMod
