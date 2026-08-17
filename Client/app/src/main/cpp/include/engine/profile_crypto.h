#pragma once

#include <cstddef>
#include <cstdint>
#include <string>
#include <vector>

namespace routesocks::runtime {

/**
 * 使用当前 ABI SO 内的每包密钥槽解封
 * profile.bin，并严格校验连接字段二进制格式。 认证、版本、长度、UTF-8
 * 或字段约束失败时返回 false，输出保持为空且错误不包含任何明文。
 */
bool DecryptProfile(const uint8_t *container, std::size_t container_size,
                    std::vector<uint8_t> *plaintext, std::string *error);

/** 使用抗优化擦除清理调用方持有的明文字节；空容器可直接调用。 */
void WipeProfile(std::vector<uint8_t> *plaintext) noexcept;

} // namespace routesocks::runtime
