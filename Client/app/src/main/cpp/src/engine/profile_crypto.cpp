#include "engine/profile_crypto.h"

#include <arpa/inet.h>

#include <algorithm>
#include <array>
#include <string_view>

#include "monocypher.h"

namespace routesocks::runtime {
namespace {

constexpr std::array<uint8_t, 8> kContainerMagic{
    {'S', 'P', 'R', 'K', 'P', 'F', '0', '1'}};
constexpr uint8_t kContainerVersion = 1;
constexpr uint8_t kAlgorithmXChaCha20Poly1305 = 1;
constexpr std::size_t kHeaderSize = 40;
constexpr std::size_t kTagSize = 16;
constexpr std::size_t kKeySize = 32;
constexpr std::size_t kMaximumPlaintextSize = 4096;

struct ProfileKeySlot {
  uint8_t begin[16];
  uint8_t key[kKeySize];
  uint8_t end[16];
};

static_assert(sizeof(ProfileKeySlot) == 64, "profile key slot layout changed");

// 打包器按两端标记唯一定位中间 32 字节；volatile 强制运行时从已补丁 SO
// 读取，禁止编译器把模板零值常量传播到认证路径。
__attribute__((used, section(".sprk_profile_key"),
               aligned(1))) volatile const ProfileKeySlot kProfileKeySlot{
    {'S', 'P', 'R', 'K', 'P', 'R', 'O', 'F', 'K', 'E', 'Y', 'S', 'L', 'O', 'T',
     '1'},
    {},
    {'S', 'P', 'R', 'K', 'P', 'R', 'O', 'F', 'K', 'E', 'Y', 'E', 'N', 'D', '0',
     '1'}};

/** 从容器大端字段读取 16 位整数；越界时不推进游标。 */
bool ReadU16(const std::vector<uint8_t> &bytes, std::size_t *cursor,
             uint16_t *value) {
  if (*cursor > bytes.size() || bytes.size() - *cursor < 2)
    return false;
  *value = static_cast<uint16_t>((bytes[*cursor] << 8U) | bytes[*cursor + 1]);
  *cursor += 2;
  return true;
}

/** 从容器大端字段读取 32 位整数；头部长度不足时返回 false。 */
bool ReadU32(const uint8_t *bytes, std::size_t size, std::size_t offset,
             uint32_t *value) {
  if (bytes == nullptr || value == nullptr || offset > size ||
      size - offset < 4)
    return false;
  *value = (static_cast<uint32_t>(bytes[offset]) << 24U) |
           (static_cast<uint32_t>(bytes[offset + 1]) << 16U) |
           (static_cast<uint32_t>(bytes[offset + 2]) << 8U) |
           static_cast<uint32_t>(bytes[offset + 3]);
  return true;
}

/** 严格验证标准 UTF-8；拒绝 NUL、控制字符、过长编码、代理项和越界码点。 */
bool IsStrictUtf8(std::string_view text) {
  for (std::size_t cursor = 0; cursor < text.size();) {
    const uint8_t first = static_cast<uint8_t>(text[cursor]);
    if (first < 0x80) {
      if (first == 0 || first < 0x20 || first == 0x7F)
        return false;
      ++cursor;
      continue;
    }
    std::size_t count = 0;
    uint32_t code_point = 0;
    uint32_t minimum = 0;
    if ((first & 0xE0U) == 0xC0U) {
      count = 2;
      code_point = first & 0x1FU;
      minimum = 0x80;
    } else if ((first & 0xF0U) == 0xE0U) {
      count = 3;
      code_point = first & 0x0FU;
      minimum = 0x800;
    } else if ((first & 0xF8U) == 0xF0U) {
      count = 4;
      code_point = first & 0x07U;
      minimum = 0x10000;
    } else {
      return false;
    }
    if (text.size() - cursor < count)
      return false;
    for (std::size_t index = 1; index < count; ++index) {
      const uint8_t next = static_cast<uint8_t>(text[cursor + index]);
      if ((next & 0xC0U) != 0x80U)
        return false;
      code_point = (code_point << 6U) | (next & 0x3FU);
    }
    if (code_point < minimum || code_point > 0x10FFFF ||
        (code_point >= 0xD800 && code_point <= 0xDFFF)) {
      return false;
    }
    cursor += count;
  }
  return true;
}

/** 读取一个 u16 长度前缀字段；空值、越界或 UTF-8 非法时返回 false。 */
bool ReadProfileField(const std::vector<uint8_t> &bytes, std::size_t *cursor,
                      std::string_view *field) {
  uint16_t length = 0;
  if (!ReadU16(bytes, cursor, &length) || length == 0 ||
      *cursor > bytes.size() || bytes.size() - *cursor < length) {
    return false;
  }
  *field = std::string_view(
      reinterpret_cast<const char *>(bytes.data() + *cursor), length);
  *cursor += length;
  return IsStrictUtf8(*field);
}

/**
 * 校验节点为 IPv4/IPv6 字面量；使用固定栈缓冲满足 inet_pton 的 NUL 结尾要求，
 * 并在返回前擦除副本，避免为验证节点额外保留堆明文。
 */
bool IsIpLiteral(std::string_view host) {
  std::array<char, INET6_ADDRSTRLEN> host_text{};
  if (host.empty() || host.size() >= host_text.size()) return false;
  std::copy(host.begin(), host.end(), host_text.begin());
  in_addr ipv4{};
  in6_addr ipv6{};
  const bool valid = inet_pton(AF_INET, host_text.data(), &ipv4) == 1 ||
                     inet_pton(AF_INET6, host_text.data(), &ipv6) == 1;
  crypto_wipe(host_text.data(), host_text.size());
  return valid;
}

/** 校验解密后的固定字段顺序和业务上限；解析只借用明文视图，不复制秘密。 */
bool ValidatePlaintext(const std::vector<uint8_t> &bytes) {
  if (bytes.empty() || bytes[0] != 1)
    return false;
  std::size_t cursor = 1;
  std::string_view host;
  std::string_view username;
  std::string_view password;
  std::string_view rules_url;
  uint16_t port = 0;
  if (!ReadProfileField(bytes, &cursor, &host) || host.size() > 253 ||
      !ReadU16(bytes, &cursor, &port) || port == 0 ||
      !ReadProfileField(bytes, &cursor, &username) || username.size() > 255 ||
      !ReadProfileField(bytes, &cursor, &password) || password.size() > 255 ||
      !ReadProfileField(bytes, &cursor, &rules_url) ||
      rules_url.size() > 2048 || cursor != bytes.size()) {
    return false;
  }
  // 规则分发当前只开放 HTTP；Native 必须与打包器和 Kotlin 使用同一接受集，
  // 不能因密文已通过认证就放宽协议字段。
  const bool valid_url = rules_url.rfind("http://", 0) == 0;
  return valid_url && IsIpLiteral(host);
}

/** 从固定槽复制每包密钥并检测模板零槽；返回 false 表示 APK 尚未由打包器封装。
 */
bool CopyProfileKey(std::array<uint8_t, kKeySize> *key) {
  bool nonzero = false;
  for (std::size_t index = 0; index < key->size(); ++index) {
    (*key)[index] = kProfileKeySlot.key[index];
    nonzero = nonzero || (*key)[index] != 0;
  }
  return nonzero;
}

} // namespace

bool DecryptProfile(const uint8_t *container, std::size_t container_size,
                    std::vector<uint8_t> *plaintext, std::string *error) {
  if (plaintext == nullptr || error == nullptr)
    return false;
  plaintext->clear();
  if (container == nullptr || container_size < kHeaderSize + kTagSize ||
      container_size > kHeaderSize + kMaximumPlaintextSize + kTagSize ||
      !std::equal(kContainerMagic.begin(), kContainerMagic.end(), container) ||
      container[8] != kContainerVersion ||
      container[9] != kAlgorithmXChaCha20Poly1305 || container[10] != 0 ||
      container[11] != 0) {
    *error = "节点配置密文容器无效";
    return false;
  }
  uint32_t cipher_size = 0;
  if (!ReadU32(container, container_size, 36, &cipher_size) ||
      cipher_size == 0 || cipher_size > kMaximumPlaintextSize ||
      container_size != kHeaderSize + cipher_size + kTagSize) {
    *error = "节点配置密文长度无效";
    return false;
  }
  std::array<uint8_t, kKeySize> key{};
  if (!CopyProfileKey(&key)) {
    *error = "节点配置密钥槽尚未封装";
    return false;
  }
  try {
    plaintext->resize(cipher_size);
  } catch (...) {
    crypto_wipe(key.data(), key.size());
    throw;
  }
  const uint8_t *nonce = container + 12;
  const uint8_t *cipher = container + kHeaderSize;
  const uint8_t *tag = cipher + cipher_size;
  const int result =
      crypto_aead_unlock(plaintext->data(), tag, key.data(), nonce, container,
                         kHeaderSize, cipher, cipher_size);
  crypto_wipe(key.data(), key.size());
  if (result != 0 || !ValidatePlaintext(*plaintext)) {
    WipeProfile(plaintext);
    *error = result != 0 ? "节点配置认证失败" : "节点配置明文字段无效";
    return false;
  }
  return true;
}

void WipeProfile(std::vector<uint8_t> *plaintext) noexcept {
  if (plaintext == nullptr)
    return;
  if (!plaintext->empty())
    crypto_wipe(plaintext->data(), plaintext->size());
  plaintext->clear();
}

} // namespace routesocks::runtime
