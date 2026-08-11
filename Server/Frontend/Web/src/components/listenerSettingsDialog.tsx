import { Plus, Trash2 } from "lucide-react";
import { useEffect, useId, useState } from "react";
import { useTranslation } from "react-i18next";

import {
  type PortForwardEntry,
  type ReverseProxyEntry,
} from "../api/protocol";
import { useServiceStore } from "../state/serviceStore";

/** 标识辅助监听规则的两个独立配置对话框。 */
export type ListenerDialogId = "reverseProxies" | "portForwards";

interface ListenerSettingsDialogProps {
  open: ListenerDialogId | null;
  onClose(): void;
}

/** 创建新增反向代理的本机安全默认项；用户必须显式填写上游和非冲突监听端口后才能应用。 */
function createReverseProxyEntry(index: number): ReverseProxyEntry {
  return {
    id: `reverse-${index + 1}`,
    enabled: true,
    listenHost: "127.0.0.1",
    listenPort: 18_000 + index,
    remoteHost: "",
    remotePort: 80,
    remoteScheme: "http",
    preserveHostHeader: false,
    stripPathPrefix: "",
  };
}

/** 创建新增 TCP 转发的本机安全默认项；目标地址保持空白，避免误把流量发送到默认远端。 */
function createPortForwardEntry(index: number): PortForwardEntry {
  return {
    id: `forward-${index + 1}`,
    enabled: true,
    listenHost: "127.0.0.1",
    listenPort: 19_000 + index,
    targetHost: "",
    targetPort: 80,
  };
}

/** 将输入转换为有效端口；临时空文本不覆盖当前数字，原生 required/min/max 负责提交前提示。 */
function readPort(value: string, previous: number): number {
  const parsed = Number(value);
  return Number.isInteger(parsed) && parsed >= 1 && parsed <= 65_535
    ? parsed
    : previous;
}

/** 渲染反向代理与 TCP 转发的字段化规则表；整个数组原子提交，后端负责重启监听器和冲突校验。 */
export function ListenerSettingsDialog({
  open,
  onClose,
}: ListenerSettingsDialogProps) {
  const { t } = useTranslation();
  const {
    actionPending,
    getPortForwards,
    getReverseProxies,
    updatePortForwards,
    updateReverseProxies,
  } = useServiceStore();
  const [reverseProxies, setReverseProxies] = useState<ReverseProxyEntry[]>([]);
  const [portForwards, setPortForwards] = useState<PortForwardEntry[]>([]);
  const [loaded, setLoaded] = useState(false);
  const titleId = useId();

  /** 打开时只读取当前类别的权威规则；其它类别继续由同一后端冲突域保留。 */
  useEffect(() => {
    if (open === null) {
      setLoaded(false);
      return;
    }
    // 只在当前对话框存活期间读取，失败时保留空状态并让共享 Store 展示控制错误。
    const load = async () => {
      try {
        const state =
          open === "reverseProxies"
            ? await getReverseProxies()
            : await getPortForwards();
        setReverseProxies(state.configuration.reverseProxies);
        setPortForwards(state.configuration.portForwards);
        setLoaded(true);
      } catch {
        setLoaded(false);
      }
    };
    void load();
  }, [getPortForwards, getReverseProxies, open]);

  if (open === null) {
    return null;
  }
  const isReverseProxy = open === "reverseProxies";
  const entries = isReverseProxy ? reverseProxies : portForwards;
  // 新条目仅生成本机监听草稿，远端主机必须由用户明确填写后才会进入后端验证。
  const addEntry = () => {
    if (isReverseProxy) {
      setReverseProxies([...reverseProxies, createReverseProxyEntry(reverseProxies.length)]);
      return;
    }
    setPortForwards([...portForwards, createPortForwardEntry(portForwards.length)]);
  };
  /**
   * 原子提交整组监听规则；应用后保留窗口，便于连续调整多条监听配置。
   * 失败语义：后端拒绝配置时保留字段草稿，用户可以直接修正端口或主机后重试。
  */
  const apply = async () => {
    if (isReverseProxy) {
      await updateReverseProxies(reverseProxies);
      return;
    }
    await updatePortForwards(portForwards);
  };

  return (
    <div className="dialogBackdrop" role="presentation">
      <section
        aria-labelledby={titleId}
        aria-modal="true"
        className="toolSettingsDialog listenerSettingsDialog"
        role="dialog"
        tabIndex={-1}
      >
        <header className="toolDialogHeader">
          <div>
            <h2 id={titleId}>{t(isReverseProxy ? "listeners.reverse.title" : "listeners.forward.title")}</h2>
            <p>
              {t(
                isReverseProxy
                  ? "listeners.reverse.description"
                  : "listeners.forward.description",
              )}
            </p>
          </div>
        </header>
        <div className="toolDialogBody listenerRuleList">
          {!loaded ? (
            <p>{t("listeners.loading")}</p>
          ) : entries.length === 0 ? (
            <p className="dialogEmptyHint">{t("listeners.empty")}</p>
          ) : isReverseProxy ? (
            reverseProxies.map((entry, index) => (
              <ReverseProxyFields
                disabled={actionPending}
                entry={entry}
                key={entry.id}
                onChange={(next) =>
                  setReverseProxies(reverseProxies.map((candidate, candidateIndex) =>
                    candidateIndex === index ? next : candidate,
                  ))
                }
                onRemove={() => setReverseProxies(reverseProxies.filter((_, candidateIndex) => candidateIndex !== index))}
              />
            ))
          ) : (
            portForwards.map((entry, index) => (
              <PortForwardFields
                disabled={actionPending}
                entry={entry}
                key={entry.id}
                onChange={(next) =>
                  setPortForwards(portForwards.map((candidate, candidateIndex) =>
                    candidateIndex === index ? next : candidate,
                  ))
                }
                onRemove={() => setPortForwards(portForwards.filter((_, candidateIndex) => candidateIndex !== index))}
              />
            ))
          )}
          <button disabled={actionPending} type="button" onClick={addEntry}>
            <Plus aria-hidden="true" size={15} />
            {t("listeners.add")}
          </button>
        </div>
        <footer className="toolDialogFooter">
          <button disabled={actionPending} type="button" onClick={onClose}>
            {t("tools.cancel")}
          </button>
          <button
            className="primaryButton"
            disabled={actionPending || !loaded}
            type="button"
            onClick={() => void apply()}
          >
            {actionPending ? t("tools.applying") : t("tools.apply")}
          </button>
        </footer>
      </section>
    </div>
  );
}

interface ReverseProxyFieldsProps {
  entry: ReverseProxyEntry;
  disabled: boolean;
  onChange(entry: ReverseProxyEntry): void;
  onRemove(): void;
}

/** 编辑一条反向 HTTP 代理规则；字段与控制 API 一一对应，避免将规则折叠为不透明 JSON。 */
function ReverseProxyFields({ entry, disabled, onChange, onRemove }: ReverseProxyFieldsProps) {
  const { t } = useTranslation();
  return (
    <fieldset className="listenerRule" disabled={disabled}>
      <legend>{entry.id || t("listeners.unnamed")}</legend>
      <label className="toolEnabledRow"><input checked={entry.enabled} type="checkbox" onChange={(event) => onChange({ ...entry, enabled: event.target.checked })} /><span>{t("listeners.enabled")}</span></label>
      <div className="toolFormGrid">
        <label><span>{t("listeners.id")}</span><input required value={entry.id} onChange={(event) => onChange({ ...entry, id: event.target.value })} /></label>
        <label><span>{t("listeners.listenHost")}</span><input required value={entry.listenHost} onChange={(event) => onChange({ ...entry, listenHost: event.target.value })} /></label>
        <label><span>{t("listeners.listenPort")}</span><input max="65535" min="1" required type="number" value={entry.listenPort} onChange={(event) => onChange({ ...entry, listenPort: readPort(event.target.value, entry.listenPort) })} /></label>
        <label><span>{t("listeners.remoteHost")}</span><input required value={entry.remoteHost} onChange={(event) => onChange({ ...entry, remoteHost: event.target.value })} /></label>
        <label><span>{t("listeners.remotePort")}</span><input max="65535" min="1" required type="number" value={entry.remotePort} onChange={(event) => onChange({ ...entry, remotePort: readPort(event.target.value, entry.remotePort) })} /></label>
        <label><span>{t("listeners.scheme")}</span><select value={entry.remoteScheme} onChange={(event) => onChange({ ...entry, remoteScheme: event.target.value as ReverseProxyEntry["remoteScheme"] })}><option value="http">HTTP</option><option value="https">HTTPS</option></select></label>
      </div>
      <label><span>{t("listeners.stripPathPrefix")}</span><input value={entry.stripPathPrefix} onChange={(event) => onChange({ ...entry, stripPathPrefix: event.target.value })} /></label>
      <label className="toolEnabledRow"><input checked={entry.preserveHostHeader} type="checkbox" onChange={(event) => onChange({ ...entry, preserveHostHeader: event.target.checked })} /><span>{t("listeners.preserveHostHeader")}</span></label>
      <button className="dangerTextButton" type="button" onClick={onRemove}><Trash2 aria-hidden="true" size={15} />{t("listeners.remove")}</button>
    </fieldset>
  );
}

interface PortForwardFieldsProps {
  entry: PortForwardEntry;
  disabled: boolean;
  onChange(entry: PortForwardEntry): void;
  onRemove(): void;
}

/** 编辑一条透明 TCP 转发规则；目标仅接收主机和端口，明确不承载 HTTP 改写字段。 */
function PortForwardFields({ entry, disabled, onChange, onRemove }: PortForwardFieldsProps) {
  const { t } = useTranslation();
  return (
    <fieldset className="listenerRule" disabled={disabled}>
      <legend>{entry.id || t("listeners.unnamed")}</legend>
      <label className="toolEnabledRow"><input checked={entry.enabled} type="checkbox" onChange={(event) => onChange({ ...entry, enabled: event.target.checked })} /><span>{t("listeners.enabled")}</span></label>
      <div className="toolFormGrid">
        <label><span>{t("listeners.id")}</span><input required value={entry.id} onChange={(event) => onChange({ ...entry, id: event.target.value })} /></label>
        <label><span>{t("listeners.listenHost")}</span><input required value={entry.listenHost} onChange={(event) => onChange({ ...entry, listenHost: event.target.value })} /></label>
        <label><span>{t("listeners.listenPort")}</span><input max="65535" min="1" required type="number" value={entry.listenPort} onChange={(event) => onChange({ ...entry, listenPort: readPort(event.target.value, entry.listenPort) })} /></label>
        <label><span>{t("listeners.targetHost")}</span><input required value={entry.targetHost} onChange={(event) => onChange({ ...entry, targetHost: event.target.value })} /></label>
        <label><span>{t("listeners.targetPort")}</span><input max="65535" min="1" required type="number" value={entry.targetPort} onChange={(event) => onChange({ ...entry, targetPort: readPort(event.target.value, entry.targetPort) })} /></label>
      </div>
      <button className="dangerTextButton" type="button" onClick={onRemove}><Trash2 aria-hidden="true" size={15} />{t("listeners.remove")}</button>
    </fieldset>
  );
}
