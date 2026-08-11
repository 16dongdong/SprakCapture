import type { TFunction } from "i18next";

import type { TransactionSummary } from "../api/protocol";

/**
 * 把字节数格式化为紧凑的二进制单位；仅处理协议保证的非负整数，避免列表渲染阶段产生区域相关歧义。
 */
export function formatTransactionBytes(byteCount: number): string {
  if (byteCount < 1024) {
    return `${byteCount} B`;
  }
  const units = ["KiB", "MiB", "GiB"] as const;
  let scaledBytes = byteCount;
  let unitIndex = -1;
  while (scaledBytes >= 1024 && unitIndex < units.length - 1) {
    scaledBytes /= 1024;
    unitIndex += 1;
  }
  return `${scaledBytes.toFixed(scaledBytes >= 10 ? 1 : 2)} ${units[unitIndex]}`;
}

/**
 * 格式化 Unix 毫秒时间；零值表示后端尚未观察到该阶段，因此返回统一空值文案。
 */
export function formatTransactionTimestamp(
  timestampMilliseconds: number | null,
  emptyValue: string,
): string {
  if (timestampMilliseconds === null || timestampMilliseconds === 0) {
    return emptyValue;
  }
  return new Intl.DateTimeFormat(undefined, {
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
    fractionalSecondDigits: 3,
  }).format(new Date(timestampMilliseconds));
}

/**
 * 计算事务已观测时长；活动事务使用当前时间，终态事务使用后端 endAt，异常负值被钳制为零。
 */
export function formatTransactionDuration(
  transaction: TransactionSummary,
): string {
  const endAtMilliseconds =
    transaction.timings.endAtMilliseconds ?? Date.now();
  const durationMilliseconds = Math.max(
    0,
    endAtMilliseconds - transaction.timings.startAtMilliseconds,
  );
  if (durationMilliseconds < 1000) {
    return `${durationMilliseconds} ms`;
  }
  return `${(durationMilliseconds / 1000).toFixed(2)} s`;
}

/**
 * 格式化原始流会话的持续时间；连接树根使用 s、min、h、d 四级单位，避免长连接保留冗长的秒数。
 */
export function formatStreamDuration(transaction: TransactionSummary): string {
  const endAtMilliseconds =
    transaction.timings.endAtMilliseconds ?? Date.now();
  const durationSeconds = Math.max(
    0,
    (endAtMilliseconds - transaction.timings.startAtMilliseconds) / 1000,
  );
  if (durationSeconds < 60) {
    return `${Math.max(1, Math.round(durationSeconds))}s`;
  }
  if (durationSeconds < 3_600) {
    return `${Math.floor(durationSeconds / 60)}min`;
  }
  if (durationSeconds < 86_400) {
    return `${Math.floor(durationSeconds / 3_600)}h`;
  }
  return `${Math.floor(durationSeconds / 86_400)}d`;
}

/**
 * 汇总请求和响应的头体字节；采用安全整数上限钳制，防止异常快照使展示值溢出。
 */
export function totalTransactionBytes(
  transaction: TransactionSummary,
): number {
  const totalBytes =
    transaction.sizes.requestHeaderBytes +
    transaction.sizes.requestBodyBytes +
    transaction.sizes.responseHeaderBytes +
    transaction.sizes.responseBodyBytes;
  return Math.min(Number.MAX_SAFE_INTEGER, totalBytes);
}

/**
 * 返回事务状态本地化文案；状态枚举与 locale 目录一一对应，不使用未知状态兜底分支。
 */
export function presentTransactionStatus(
  transaction: TransactionSummary,
  translate: TFunction,
): string {
  return translate(`transactions.status.${transaction.status}`);
}

/**
 * 返回详情面板使用的完整事务状态；失败事务把后端结构化原因紧邻状态展示，避免用户再展开错误组才能判断问题。
 *
 * 运行上下文：概览和摘要页共用此文本，错误原因按当前界面语言及后端参数插值生成。
 * 失败语义：非失败状态或没有结构化错误的旧事务只返回状态，不编造未知失败原因。
 */
export function presentTransactionStatusDetail(
  transaction: TransactionSummary,
  translate: TFunction,
): string {
  const status = presentTransactionStatus(transaction, translate);
  if (transaction.status !== "failed" || transaction.error === null) {
    return status;
  }
  const failureReason = translate(
    transaction.error.messageKey,
    transaction.error.params,
  );
  return `${status} · ${failureReason}`;
}

/**
 * 返回协议本地化文案；协议枚举由严格控制协议约束，稳定用于结构树与事务详情。
 */
export function presentTransactionProtocol(
  transaction: TransactionSummary,
  translate: TFunction,
): string {
  return translate(`transactions.protocol.${transaction.protocol}`);
}

/**
 * 返回原始流已经确认的载荷类型；SOCKS5 只负责建立通道，不作为用户分析数据时的协议名称。
 *
 * 运行上下文：后端对未解密 TLS 使用 https:// 展示地址，对普通 CONNECT 和 UDP 分别使用 tcp://、udp://。
 * 失败语义：旧录制缺少新展示协议时仍按命令区分 TCP/UDP，不把端口号猜测成 HTTPS。
 */
export function presentStreamTransport(
  transaction: TransactionSummary,
): "HTTPS" | "TCP" | "UDP" {
  if (transaction.urlDisplay.startsWith("https://")) {
    return "HTTPS";
  }
  return transaction.urlDisplay.startsWith("udp://") ||
    ["UDP ASSOCIATE", "UDP SEND", "UDP RECEIVE"].includes(transaction.method)
    ? "UDP"
    : "TCP";
}

/**
 * 把事务状态映射到视觉语气；失败、阻止和取消均使用终止语气，活动事务使用等待语气。
 */
export function transactionStatusTone(
  status: TransactionSummary["status"],
): "danger" | "neutral" | "success" | "warning" {
  if (status === "complete") {
    return "success";
  }
  if (status === "pending") {
    return "warning";
  }
  if (status === "failed" || status === "blocked") {
    return "danger";
  }
  return "neutral";
}
