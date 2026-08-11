import { ContextMenu, Theme, ThemeContext } from "@radix-ui/themes";
import {
  Ban,
  Braces,
  ClipboardCopy,
  Download,
  FileKey2,
  FileInput,
  FileOutput,
  Focus,
  LockKeyhole,
  Play,
  Route,
} from "lucide-react";
import { type ReactElement, useContext } from "react";
import { useTranslation } from "react-i18next";

import type { TransactionSummary } from "../api/protocol";
import { useServiceStore } from "../state/serviceStore";
import { downloadArchive } from "./downloadArchive";
import {
  createTransactionToolSeed,
  type TransactionToolSeed,
} from "./transactionToolSeed";
import type { ToolDialogId } from "./toolSettingsDialog";

interface TransactionContextMenuProps {
  children: ReactElement;
  transaction: TransactionSummary;
  transactionIds?: string[];
  seedPath?: string;
  seedQuery?: string | null;
  focusActive: boolean;
  onFocusHost(host: string): void;
  onClearFocus(): void;
  onOpenSslSettings?(seed: TransactionToolSeed, focusClientCertificate?: boolean): void;
  onOpenToolSettings?(tool: ToolDialogId, seed: TransactionToolSeed): void;
  onCommandError(message: string): void;
}

/**
 * 将主机名转换为可用于下载文件的稳定片段，避免 Windows 保留字符破坏 HAR 文件名。
 *
 * 运行上下文：右键导出收到归档后调用；参数为事务主机名。
 * 失败语义：空主机名返回空串，由调用方在进入函数前提供 transactions 作为替代值。
 */
function safeFileNamePart(host: string): string {
  return host.replace(/[<>:"/\\|?*\u0000-\u001f]/g, "-");
}

/**
 * 渲染 Charles 风格的事务右键菜单；菜单项只连接现有控制能力，不展示尚未落地的占位操作。
 *
 * 运行上下文：来源根、HTTP 资源、原始流与序列行共用该组件；Radix 负责定位、键盘导航和焦点恢复。
 * 参数：transaction 为右键目标，transactionIds 限定导出集合，seedPath/seedQuery 明确当前树节点的规则范围。
 * 失败语义：复制、导出或重复失败时通过 onCommandError 显示可见错误，菜单不会伪造成功状态。
 */
export function TransactionContextMenu({
  children,
  transaction,
  transactionIds = [transaction.transactionId],
  seedPath,
  seedQuery,
  focusActive,
  onFocusHost,
  onClearFocus,
  onOpenSslSettings,
  onOpenToolSettings,
  onCommandError,
}: TransactionContextMenuProps) {
  const { t } = useTranslation();
  const parentTheme = useContext(ThemeContext);
  const { exportRecording, repeatTransaction } = useServiceStore();
  const canRepeat = transaction.protocol !== "socks";
  const toolSeed = createTransactionToolSeed(
    transaction,
    seedPath,
    seedQuery,
  );
  const canAddClientCertificate =
    toolSeed.location.protocol === "https" ||
    toolSeed.location.protocol === "wss";

  /** 复制当前右键目标的完整 URL；剪贴板异常必须反馈到事务导航区。 */
  const copyUrl = () => {
    if (navigator.clipboard === undefined) {
      onCommandError(t("transactions.context.copyFailed"));
      return;
    }
    void navigator.clipboard
      .writeText(transaction.urlDisplay)
      .catch(() => onCommandError(t("transactions.context.copyFailed")));
  };

  /** 导出右键目标对应的事务集合；正文包含在 HAR 中，便于直接交给其他分析工具。 */
  const exportTransactions = () => {
    void exportRecording({
      format: "har",
      includeBodies: true,
      transactionIds,
    })
      .then((archive) => {
        downloadArchive(
          archive,
          `${safeFileNamePart(transaction.host || "transactions")}.har`,
        );
      })
      .catch(() => onCommandError(t("transactions.context.exportFailed")));
  };

  /** 重放 HTTP 类事务；原始流没有可重建的 HTTP 请求契约，因此不显示该菜单项。 */
  const repeatRequest = () => {
    void repeatTransaction(transaction.transactionId).then((result) => {
      if (result === null) {
        onCommandError(t("transactions.context.repeatFailed"));
      }
    });
  };

  const menu = (
    <ContextMenu.Root>
      <ContextMenu.Trigger>{children}</ContextMenu.Trigger>
      <ContextMenu.Content size="1" className="transactionContextMenu">
        <ContextMenu.Item onSelect={copyUrl}>
          <ClipboardCopy aria-hidden="true" size={14} />
          {t("transactions.context.copyUrl")}
        </ContextMenu.Item>
        <ContextMenu.Item onSelect={exportTransactions}>
          <Download aria-hidden="true" size={14} />
          {t("transactions.context.exportHar")}
        </ContextMenu.Item>
        {canRepeat && (
          <ContextMenu.Item onSelect={repeatRequest}>
            <Play aria-hidden="true" size={14} />
            {t("transactions.context.repeat")}
          </ContextMenu.Item>
        )}
        <ContextMenu.Separator />
        {focusActive ? (
          <ContextMenu.Item onSelect={onClearFocus}>
            <Ban aria-hidden="true" size={14} />
            {t("transactions.context.clearFocus")}
          </ContextMenu.Item>
        ) : (
          <ContextMenu.Item onSelect={() => onFocusHost(transaction.host)}>
            <Focus aria-hidden="true" size={14} />
            {t("transactions.context.focusHost")}
          </ContextMenu.Item>
        )}
        {(onOpenSslSettings !== undefined || onOpenToolSettings !== undefined) && (
          <ContextMenu.Sub>
            <ContextMenu.SubTrigger
              aria-label={t("transactions.context.tools")}
            >
              <Route aria-hidden="true" size={14} />
              {t("transactions.context.tools")}
            </ContextMenu.SubTrigger>
            <ContextMenu.SubContent>
              {onOpenSslSettings !== undefined && (
                <>
                  <ContextMenu.Item
                    onSelect={() => onOpenSslSettings(toolSeed)}
                  >
                    <LockKeyhole aria-hidden="true" size={14} />
                    {t("transactions.context.sslSettings")}
                  </ContextMenu.Item>
                  {canAddClientCertificate && (
                    <ContextMenu.Item
                      onSelect={() => onOpenSslSettings(toolSeed, true)}
                    >
                      <FileKey2 aria-hidden="true" size={14} />
                      {t("transactions.context.addClientCertificate")}
                    </ContextMenu.Item>
                  )}
                </>
              )}
              {onOpenToolSettings !== undefined && (
                <>
                  <ContextMenu.Item
                    onSelect={() =>
                      onOpenToolSettings("noCaching", toolSeed)
                    }
                  >
                    <Ban aria-hidden="true" size={14} />
                    {t("tools.names.noCaching")}
                  </ContextMenu.Item>
                  <ContextMenu.Item
                    onSelect={() =>
                      onOpenToolSettings("blockCookies", toolSeed)
                    }
                  >
                    <Ban aria-hidden="true" size={14} />
                    {t("tools.names.blockCookies")}
                  </ContextMenu.Item>
                  <ContextMenu.Item
                    onSelect={() =>
                      onOpenToolSettings("breakpoints", toolSeed)
                    }
                  >
                    <Ban aria-hidden="true" size={14} />
                    {t("transactions.context.breakpoints")}
                  </ContextMenu.Item>
                  <ContextMenu.Item
                    onSelect={() => onOpenToolSettings("mapLocal", toolSeed)}
                  >
                    <FileInput aria-hidden="true" size={14} />
                    {t("transactions.context.mapLocal")}
                  </ContextMenu.Item>
                  <ContextMenu.Item
                    onSelect={() => onOpenToolSettings("mapRemote", toolSeed)}
                  >
                    <FileOutput aria-hidden="true" size={14} />
                    {t("transactions.context.mapRemote")}
                  </ContextMenu.Item>
                  <ContextMenu.Item
                    onSelect={() => onOpenToolSettings("rewrite", toolSeed)}
                  >
                    <Braces aria-hidden="true" size={14} />
                    {t("transactions.context.rewrite")}
                  </ContextMenu.Item>
                </>
              )}
            </ContextMenu.SubContent>
          </ContextMenu.Sub>
        )}
      </ContextMenu.Content>
    </ContextMenu.Root>
  );
  // 独立组件测试和嵌入式宿主可能不提供应用级 Theme；只在缺失时补一层，正式主窗口不产生重复主题作用域。
  return parentTheme === undefined ? <Theme>{menu}</Theme> : menu;
}
