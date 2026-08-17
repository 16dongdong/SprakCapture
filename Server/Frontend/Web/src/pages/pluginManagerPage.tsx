import {
  ExternalLink,
  PackagePlus,
  PlugZap,
  RefreshCcw,
  Trash2,
} from "lucide-react";
import { type ChangeEvent, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";

import type { PluginSnapshot } from "../api/protocol";
import { showIndependentWindow } from "../platform/independentWindowContract";
import { useServiceStore } from "../state/serviceStore";

const emptyPluginSnapshots: PluginSnapshot[] = [];

/**
 * 格式化插件 Hook 列表。
 *
 * 运行上下文：插件管理页只展示运行摘要，不读取插件配置或秘密字段。
 * 参数：plugin 是列表快照，emptyLabel 是本地化空状态文案。
 * 失败语义：空 Hook 集合返回明确占位文案，不产生空白元数据单元格。
 */
function formatHooks(plugin: PluginSnapshot, emptyLabel: string): string {
  return plugin.hooks.length === 0 ? emptyLabel : plugin.hooks.join(", ");
}

/**
 * 渲染插件包管理与生命周期摘要。
 *
 * 运行上下文：主窗口只承担安装、选择、启停、重载和卸载；插件声明的配置界面由“打开插件窗口”按钮在独立窗口挂载，避免把所有插件功能堆入宿主页。
 * 关键约束：插件配置不在本组件读取，因此插件按钮是插件 UI 的唯一宿主入口；列表运行态始终以 SSE 快照为准。
 * 失败语义：控制请求错误由全局状态栏呈现，文件选择器缺失属于结构损坏并直接抛错。
 */
export function PluginManagerPage() {
  const { t } = useTranslation();
  const {
    actionPending,
    installPluginPackage,
    refresh,
    reloadPlugin,
    setPluginEnabled,
    snapshot,
  } = useServiceStore();
  const [selectedPluginId, setSelectedPluginId] = useState<string | null>(null);
  const packageInputReference = useRef<HTMLInputElement>(null);
  const plugins = snapshot?.plugins ?? emptyPluginSnapshots;

  /**
   * 在插件列表变化后保留有效选择。
   *
   * 运行上下文：安装、卸载和事件流更新都可能替换数组引用。
   * 失败语义：原选择消失时选择首项；列表为空时回到空状态。
   */
  useEffect(() => {
    setSelectedPluginId((currentPluginId) =>
      currentPluginId !== null &&
      plugins.some((plugin) => plugin.id === currentPluginId)
        ? currentPluginId
        : (plugins[0]?.id ?? null),
    );
  }, [plugins]);

  const selectedPlugin =
    plugins.find((plugin) => plugin.id === selectedPluginId) ?? null;

  /**
   * 切换插件启停意图。
   *
   * 运行上下文：按钮点击后由宿主执行原子生命周期切换，页面等待 SSE 发布权威结果。
   * 参数：plugin 是当前选择的快照。
   * 失败语义：请求失败时保留原选择和快照，不伪造本地启停状态。
   */
  const toggleEnabled = async (plugin: PluginSnapshot) => {
    await setPluginEnabled(plugin.id, !plugin.enabled);
  };

  /**
   * 重建当前插件运行实例。
   *
   * 运行上下文：开发热重载和故障恢复共用该入口；插件独立窗口下次聚焦时会重新读取配置详情。
   * 失败语义：宿主拒绝重载时不修改列表状态。
   */
  const reloadSelectedPlugin = async () => {
    if (selectedPlugin === null) {
      return;
    }
    await reloadPlugin(selectedPlugin.id);
  };

  /**
   * 上传用户选择的插件包。
   *
   * 运行上下文：浏览器文件对象只在本次安装请求中存活，不写入页面状态。
   * 参数：event 来自隐藏文件输入。
   * 失败语义：用户取消选择时不发请求；安装错误由控制状态栏保留。
   */
  const installPackage = async (event: ChangeEvent<HTMLInputElement>) => {
    const packageFile = event.currentTarget.files?.[0];
    event.currentTarget.value = "";
    if (packageFile === undefined) {
      return;
    }
    await installPluginPackage(packageFile);
  };

  /**
   * 在同一用户手势内打开系统文件选择器。
   *
   * 运行上下文：可见按钮是唯一安装入口，隐藏输入不会进入辅助功能树。
   * 失败语义：输入节点未挂载代表组件结构损坏，直接抛出明确错误。
   */
  const openPackageChooser = () => {
    const packageInput = packageInputReference.current;
    if (packageInput === null) {
      throw new Error("插件包文件输入未挂载");
    }
    packageInput.click();
  };

  /**
   * 打开当前插件独占的 UI 窗口。
   *
   * 运行上下文：桌面端复用 Tauri WebView，浏览器端复用命名弹窗；插件 ID 决定唯一窗口实例。
   * 失败语义：原生窗口创建错误写入开发控制台，主窗口保持可操作。
   */
  const openPluginWindow = () => {
    if (selectedPlugin === null) {
      return;
    }
    void showIndependentWindow({
      kind: "plugin",
      pluginId: selectedPlugin.id,
      pluginName: selectedPlugin.name,
    }).catch((error: unknown) => {
      console.error("打开插件窗口失败", error);
    });
  };

  return (
    <main className="pageShell pluginManagerPage">
      <header className="pageHeader">
        <div>
          <h1>{t("plugins.title")}</h1>
          <p>{t("plugins.description")}</p>
        </div>
      </header>
      <div className="pluginManagerWorkspace">
        <div className="pluginManagerLayout">
          <aside aria-label={t("plugins.title")} className="pluginListPanel">
            <div className="pluginListActions">
              <button
                className="fileChoiceButton"
                disabled={actionPending}
                type="button"
                onClick={openPackageChooser}
              >
                <PackagePlus aria-hidden="true" size={15} />
                <span>{t("plugins.install")}</span>
              </button>
              <input
                accept=".zip,.tplugin.zip,application/zip"
                disabled={actionPending}
                hidden
                ref={packageInputReference}
                type="file"
                onChange={(event) => void installPackage(event)}
              />
              <button
                aria-label={t("plugins.refresh")}
                className="iconButton"
                disabled={snapshot === null || actionPending}
                title={t("plugins.refresh")}
                type="button"
                onClick={() => void refresh()}
              >
                <RefreshCcw aria-hidden="true" size={15} />
              </button>
            </div>
            {snapshot === null && (
              <p className="viewerNotice">{t("plugins.loading")}</p>
            )}
            {snapshot !== null && plugins.length === 0 && (
              <p className="pluginEmptyState">{t("plugins.empty")}</p>
            )}
            <ul className="pluginList">
              {plugins.map((plugin) => (
                <li key={plugin.id}>
                  <button
                    aria-current={
                      selectedPlugin?.id === plugin.id ? "true" : undefined
                    }
                    className={`pluginListItem${
                      selectedPlugin?.id === plugin.id ? " isSelected" : ""
                    }`}
                    type="button"
                    onClick={() => setSelectedPluginId(plugin.id)}
                  >
                    <PlugZap aria-hidden="true" size={16} />
                    <span>
                      <strong>{plugin.name}</strong>
                      <small>{plugin.id}</small>
                    </span>
                    <em className={`pluginState pluginState--${plugin.state}`}>
                      {t(`plugins.states.${plugin.state}`)}
                    </em>
                  </button>
                </li>
              ))}
            </ul>
          </aside>
          <div className="pluginDetailsPanel">
            {selectedPlugin === null ? (
              <p className="pluginEmptyState">{t("plugins.select")}</p>
            ) : (
              <section
                className="pluginOverview"
                aria-label={selectedPlugin.name}
              >
                <div>
                  <h3>{selectedPlugin.name}</h3>
                  <p>{selectedPlugin.id}</p>
                </div>
                <div className="pluginOverviewActions">
                  <button
                    className="primaryButton pluginWindowButton"
                    disabled={actionPending}
                    type="button"
                    onClick={openPluginWindow}
                  >
                    <ExternalLink aria-hidden="true" size={14} />
                    {t("plugins.openWindow")}
                  </button>
                  <button
                    className="pluginActionButton"
                    disabled={actionPending}
                    type="button"
                    onClick={() => void toggleEnabled(selectedPlugin)}
                  >
                    <PlugZap aria-hidden="true" size={14} />
                    {selectedPlugin.enabled
                      ? t("plugins.disable")
                      : t("plugins.enable")}
                  </button>
                  <button
                    className="pluginActionButton"
                    disabled={actionPending}
                    type="button"
                    onClick={() => void reloadSelectedPlugin()}
                  >
                    <RefreshCcw aria-hidden="true" size={14} />
                    {t("plugins.reload")}
                  </button>
                  <button
                    aria-label={t("plugins.uninstall")}
                    className="iconButton pluginUninstallButton"
                    disabled={
                      actionPending || selectedPlugin.activeConnections > 0
                    }
                    title={t("plugins.uninstall")}
                    type="button"
                    onClick={() =>
                      void showIndependentWindow({
                        kind: "pluginUninstall",
                        pluginId: selectedPlugin.id,
                        pluginName: selectedPlugin.name,
                      })
                    }
                  >
                    <Trash2 aria-hidden="true" size={15} />
                  </button>
                </div>
                <p className="pluginWindowDescription">
                  {t("plugins.windowDescription")}
                </p>
                <dl className="pluginMetadata">
                  <div>
                    <dt>{t("plugins.runtime")}</dt>
                    <dd>{selectedPlugin.runtime}</dd>
                  </div>
                  <div>
                    <dt>{t("plugins.apiVersion")}</dt>
                    <dd>{selectedPlugin.apiVersion}</dd>
                  </div>
                  <div>
                    <dt>{t("plugins.hooks")}</dt>
                    <dd>{formatHooks(selectedPlugin, t("plugins.noHooks"))}</dd>
                  </div>
                  <div>
                    <dt>{t("plugins.activeConnections", { count: 0 })}</dt>
                    <dd>{selectedPlugin.activeConnections}</dd>
                  </div>
                </dl>
                {selectedPlugin.errorCode !== null && (
                  <p className="pluginErrorCode">{selectedPlugin.errorCode}</p>
                )}
              </section>
            )}
          </div>
        </div>
      </div>
    </main>
  );
}
