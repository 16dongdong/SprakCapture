import type {
  LocationPattern,
  TransactionSummary,
} from "../api/protocol";

/** 描述事务右键菜单传给规则编辑器的稳定上下文，不携带请求正文或其他大对象。 */
export interface TransactionToolSeed {
  transactionId: string;
  contentType: string;
  location: LocationPattern;
}

/**
 * 从事务摘要构造工具规则种子；URL 中已识别的 Web 协议优先于底层 SOCKS 记录类型。
 *
 * 运行上下文：用户在来源、目录、资源或原始 HTTPS 流上打开右键工具时调用。
 * 参数：transaction 为右键目标；pathOverride 与 queryOverride 用于显式表达树节点代表的匹配范围。
 * 失败语义：非 Web 原始流保留空协议，规则编辑器将其显示为任意协议并等待用户确认。
 */
export function createTransactionToolSeed(
  transaction: TransactionSummary,
  pathOverride?: string,
  queryOverride?: string | null,
): TransactionToolSeed {
  const schemeMatch = /^(https?|wss?):\/\//i.exec(transaction.urlDisplay);
  return {
    transactionId: transaction.transactionId,
    contentType: transaction.contentType,
    location: {
      protocol: schemeMatch?.[1].toLocaleLowerCase() ?? "",
      host: transaction.host,
      port: transaction.port === 0 ? "" : String(transaction.port),
      path: pathOverride ?? transaction.path,
      // 来源和目录节点代表集合，默认不继承代表事务的查询串；资源叶节点会显式传入自身查询串。
      query:
        queryOverride !== undefined
          ? queryOverride
          : pathOverride === undefined
            ? transaction.query || null
            : null,
    },
  };
}
