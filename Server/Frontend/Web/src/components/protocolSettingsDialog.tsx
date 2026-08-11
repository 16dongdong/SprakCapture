import { RefreshCcw } from "lucide-react";
import {
  type KeyboardEvent,
  useCallback,
  useEffect,
  useId,
  useRef,
  useState,
} from "react";
import { useTranslation } from "react-i18next";

import {
  type ProtobufConfiguration,
  type ProtobufDescriptorUpload,
} from "../api/protocol";
import { useServiceStore } from "../state/serviceStore";
import { useModalFocus } from "./modalFocus";
import { ProtobufRouteEditor } from "./protobufRouteEditor";
import { ProtobufSchemaPanel } from "./protobufSchemaPanel";

interface ProtocolSettingsDialogProps {
  open: boolean;
  onClose(): void;
}

/** 渲染协议工具的 L3 配置入口；描述符上传和路由编辑保持字段化，不提供 JSON 文本配置。 */
export function ProtocolSettingsDialog({
  open,
  onClose,
}: ProtocolSettingsDialogProps) {
  const { t } = useTranslation();
  const {
    actionPending,
    getProtobufConfiguration,
    updateProtobufConfiguration,
    uploadProtobufDescriptor,
  } = useServiceStore();
  const [configuration, setConfiguration] = useState<ProtobufConfiguration | null>(null);
  const [loading, setLoading] = useState(false);
  const [loadFailed, setLoadFailed] = useState(false);
  const titleId = useId();
  const descriptionId = useId();
  const dialogRef = useRef<HTMLElement>(null);
  const cancelButtonRef = useRef<HTMLButtonElement>(null);
  const requestSequence = useRef(0);
  const { onKeyDown: handleModalFocusKeyDown } = useModalFocus({
    containerRef: dialogRef,
    initialFocusRef: cancelButtonRef,
    open,
  });

  /** 读取权威配置并丢弃已过期响应；打开、刷新和上传后的重读共享同一请求序号。 */
  const loadConfiguration = useCallback(
    async (signal?: AbortSignal): Promise<ProtobufConfiguration | null> => {
      const sequence = requestSequence.current + 1;
      requestSequence.current = sequence;
      setLoading(true);
      setLoadFailed(false);
      try {
        const nextConfiguration = await getProtobufConfiguration(signal);
        if (requestSequence.current === sequence) {
          setConfiguration(nextConfiguration);
        }
        return nextConfiguration;
      } catch (error) {
        if (
          requestSequence.current === sequence &&
          !(error instanceof DOMException && error.name === "AbortError")
        ) {
          setLoadFailed(true);
        }
        return null;
      } finally {
        if (requestSequence.current === sequence) {
          setLoading(false);
        }
      }
    },
    [getProtobufConfiguration],
  );

  useEffect(() => {
    if (!open) {
      return undefined;
    }
    const abortController = new AbortController();
    void loadConfiguration(abortController.signal);
    return () => {
      requestSequence.current += 1;
      abortController.abort();
    };
  }, [loadConfiguration, open]);

  /** 上传后立即重读服务端配置；Base64 仅由子组件在请求期间持有，不写入此对话框状态。 */
  const uploadDescriptor = async (
    upload: ProtobufDescriptorUpload,
  ): Promise<ProtobufConfiguration | null> => {
    const succeeded = await uploadProtobufDescriptor(upload);
    return succeeded ? loadConfiguration() : null;
  };

  /** 提交配置中的开关和路由，描述符清单始终保持由服务端登记的只读元数据。 */
  const applyConfiguration = async () => {
    if (configuration === null || actionPending) {
      return;
    }
    const succeeded = await updateProtobufConfiguration({
      enabled: configuration.enabled,
      routes: configuration.routes,
    });
    if (!succeeded) {
      return;
    }
    await loadConfiguration();
  };

  /** Escape 和取消按钮在无写入时关闭模态；进行中的请求不会丢失当前路由草稿。 */
  const closeDialog = () => {
    if (!actionPending) {
      onClose();
    }
  };

  const handleKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    handleModalFocusKeyDown(event);
    if (event.key === "Escape") {
      event.preventDefault();
      closeDialog();
    }
  };

  if (!open) {
    return null;
  }

  const disabled = loading || configuration === null || actionPending;
  return (
    <div className="dialogBackdrop" role="presentation" onKeyDown={handleKeyDown}>
      <section
        aria-describedby={descriptionId}
        aria-labelledby={titleId}
        aria-modal="true"
        className="protocolSettingsDialog"
        ref={dialogRef}
        role="dialog"
        tabIndex={-1}
      >
        <header className="toolDialogHeader">
          <div>
            <h2 id={titleId}>{t("protocolSettings.title")}</h2>
            <p id={descriptionId}>{t("protocolSettings.description")}</p>
          </div>
        </header>
        <div className="protocolSettingsBody">
          {loading && <p className="viewerNotice">{t("protocolSettings.loading")}</p>}
          {loadFailed && (
            <p className="viewerNotice viewerNotice--error">
              {t("protocolSettings.loadFailed")}
            </p>
          )}
          {configuration !== null && (
            <>
              <label className="toolEnabledRow">
                <input
                  checked={configuration.enabled}
                  disabled={disabled}
                  type="checkbox"
                  onChange={(event) =>
                    setConfiguration({
                      ...configuration,
                      enabled: event.target.checked,
                    })
                  }
                />
                <span>
                  <strong>{t("protocolSettings.enabled")}</strong>
                  <small>{t("protocolSettings.enabledHint")}</small>
                </span>
              </label>
              <ProtobufSchemaPanel
                configuration={configuration}
                disabled={disabled}
                upload={uploadDescriptor}
                onChange={setConfiguration}
              />
              <ProtobufRouteEditor
                configuration={configuration}
                disabled={disabled}
                onChange={setConfiguration}
              />
            </>
          )}
        </div>
        <footer className="toolDialogFooter">
          <button
            disabled={actionPending}
            ref={cancelButtonRef}
            type="button"
            onClick={closeDialog}
          >
            {t("tools.cancel")}
          </button>
          <button
            className="primaryButton"
            disabled={disabled}
            type="button"
            onClick={() => void applyConfiguration()}
          >
            <RefreshCcw aria-hidden="true" size={14} />
            {actionPending ? t("tools.applying") : t("tools.apply")}
          </button>
        </footer>
      </section>
    </div>
  );
}
