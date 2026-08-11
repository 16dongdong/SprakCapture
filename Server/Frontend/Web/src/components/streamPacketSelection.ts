import type { MessageSide } from "../api/protocol";

/**
 * 标识连接树中被选中的方向或单个流片段；sequence 为 null 时代表方向聚合视图，非空时精确定位单包。
 */
export interface StreamPacketSelection {
  transactionId: string;
  side: MessageSide;
  sequence: number | null;
}
