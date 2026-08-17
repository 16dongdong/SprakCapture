/** 协议各职责模块共享的数值边界与整数模式；内部模式不从公共入口导出。 */
import { z } from "zod";

export const maximumConnections = 16_384;
export const maximumRelayBufferSize = 1_048_576;
// 数据面 512 MiB 合计预算中固定预留 64 MiB 给自动 SOCKS5 正文镜像。
export const maximumTotalRelayBufferSize = 448 * 1024 * 1024;
export const maximumShutdownTimeoutSeconds = 30;
export const maximumUdpPacketSize = 65_507;
export const maximumHttpHeaderBytes = 1024 * 1024;
export const maximumHttpCaptureBodyBytes = 64 * 1024 * 1024;
export const maximumHttpTimeoutMilliseconds = 5 * 60 * 1000;
// 录制更新和正文响应必须复用后端固定边界，避免客户端接受随后必定被拒绝或无界展开的值。
export const maximumRecordingTransactions = Number.MAX_SAFE_INTEGER;
export const maximumRecordingBodyBytes = Number.MAX_SAFE_INTEGER;
export const maximumRecordingTotalBodyBytes = Number.MAX_SAFE_INTEGER;
export const maximumTransactionCollectionTokenCharacters = 128;
export const maximumCachedCertificates = 4_096;
export const maximumPluginPackageBytes = 64 * 1024 * 1024;
// JavaScript 字符串长度远低于安全整数上限；保持精确上界即可，不用乘法制造不可精确数字。
export const maximumEncodedBodyCharacters = Number.MAX_SAFE_INTEGER;
// FileDescriptorSet 的后端上限为 16 MiB；Base64 传输按 4/3 展开并保留完整填充字符。
export const maximumDescriptorEncodedCharacters =
  Math.ceil((16 * 1024 * 1024) / 3) * 4;
// Rust u64 只有在 JavaScript 安全整数范围内才能保持 revision、sequence 与计数的精确顺序。
export const safeUnsignedIntegerSchema = z
  .number()
  .int()
  .nonnegative()
  .max(Number.MAX_SAFE_INTEGER);
export const safePositiveIntegerSchema = z
  .number()
  .int()
  .positive()
  .max(Number.MAX_SAFE_INTEGER);
// 后台实例标识采用随机 UUID；revision 只能在同一标识内建立顺序。
