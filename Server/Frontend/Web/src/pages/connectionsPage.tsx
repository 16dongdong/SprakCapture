import { ConnectionsWorkspace } from "../components/connectionsWorkspace";
import type { ToolDialogId } from "../components/toolSettingsDialog";
import type { TransactionToolSeed } from "../components/transactionToolSeed";

interface ConnectionsPageProps {
  onOpenSslSettings?(seed: TransactionToolSeed, focusClientCertificate?: boolean): void;
  onOpenToolSettings?(tool: ToolDialogId, seed: TransactionToolSeed): void;
}

/**
 * 提供连接会话路由入口；工作台本身占满主窗口剩余空间。
 *
 * 运行上下文：由主窗口路由创建，并把应用级设置对话框动作传入事务树。
 * 参数：两个可选回调用于打开 SSL 设置和指定工具设置。
 * 失败语义：嵌入式宿主未提供回调时仍可浏览事务，只隐藏不可执行的工具菜单项。
 */
export function ConnectionsPage({
  onOpenSslSettings,
  onOpenToolSettings,
}: ConnectionsPageProps) {
  return (
    <ConnectionsWorkspace
      onOpenSslSettings={onOpenSslSettings}
      onOpenToolSettings={onOpenToolSettings}
    />
  );
}
