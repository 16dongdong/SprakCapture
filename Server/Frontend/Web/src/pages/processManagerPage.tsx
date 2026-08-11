import { DropdownMenu, Theme } from "@radix-ui/themes";
import {
  ChevronDown,
  CircleStop,
  AppWindow,
  Plus,
  RefreshCw,
  Search,
  Trash2,
} from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";

import type {
  ProcessCandidate,
  ProcessSelectionSnapshot,
} from "../api/protocol";
import { useServiceStore } from "../state/serviceStore";

interface ProcessChoice {
  executablePath: string;
  name: string;
  processIds: number[];
}

/**
 * 渲染基于可执行路径的进程管理页；运行中实例用于选择，已保存路径负责跨进程重启恢复。
 * 读取或提交失败时保留当前界面状态并显示明确错误，不伪造已经应用的选择结果。
 */
export function ProcessManagerPage() {
  const { t } = useTranslation();
  const {
    getProcesses,
    snapshot: serviceSnapshot,
    updateProcessSelection,
  } = useServiceStore();
  const [snapshot, setSnapshot] = useState<ProcessSelectionSnapshot | null>(null);
  const [searchText, setSearchText] = useState("");
  const [selectedCandidatePath, setSelectedCandidatePath] = useState("");
  const [pending, setPending] = useState(false);
  const [errorMessage, setErrorMessage] = useState("");

  /**
   * 读取一次进程权威视图；运行时由 SSE 的 processCapture 快照变化触发，而不是定时轮询。
   * AbortSignal 用于页面卸载和事件代际切换，失败时保留最近一次成功视图并显示明确错误。
   */
  const refreshProcesses = useCallback(async (signal?: AbortSignal) => {
    setPending(true);
    setErrorMessage("");
    try {
      setSnapshot(await getProcesses(signal));
    } catch (error) {
      if (signal?.aborted) {
        return;
      }
      setErrorMessage(error instanceof Error ? error.message : String(error));
    } finally {
      if (!signal?.aborted) {
        setPending(false);
      }
    }
  }, [getProcesses]);

  /**
   * 从实时快照提取进程视图身份；排序后的 PID 集消除后端枚举顺序变化造成的无效刷新。
   * 该键刻意排除流量计数，避免每秒捕获统计事件重复请求候选列表和图标。
   */
  const processCaptureIdentity = useMemo(() => {
    if (serviceSnapshot === null) {
      return null;
    }
    const configuredProcessIds = [
      ...serviceSnapshot.processCapture.configuredProcessIds,
    ].sort((left, right) => left - right);
    return [
      serviceSnapshot.serverInstanceId,
      serviceSnapshot.configuration.processCapture.enabled
        ? "enabled"
        : "disabled",
      configuredProcessIds.join(","),
    ].join(":");
  }, [serviceSnapshot]);
  const loadedProcessCaptureIdentity = useRef<string | null>(null);

  /**
   * 首次进入读取完整候选表；后续只有 SSE 推送的捕获启停或已解析 PID 集变化才重读。
   * 高频字节计数不会进入身份键，因此真实流量不会放大为重复 HTTP 请求。
   */
  useEffect(() => {
    if (processCaptureIdentity === null) {
      return undefined;
    }
    if (loadedProcessCaptureIdentity.current === processCaptureIdentity) {
      return undefined;
    }
    loadedProcessCaptureIdentity.current = processCaptureIdentity;
    const controller = new AbortController();
    void refreshProcesses(controller.signal);
    return () => controller.abort();
  }, [processCaptureIdentity, refreshProcesses]);

  const selectedPathKeys = useMemo(
    () =>
      new Set(
        (snapshot?.selectedPaths ?? []).map((path) => path.toLocaleLowerCase()),
      ),
    [snapshot?.selectedPaths],
  );
  const filteredProcessChoices = useMemo(() => {
    const query = searchText.trim().toLocaleLowerCase();
    const choices = new Map<string, ProcessChoice>();
    for (const process of snapshot?.processes ?? []) {
      const pathKey = process.executablePath.toLocaleLowerCase();
      if (selectedPathKeys.has(pathKey)) {
        continue;
      }
      const existing = choices.get(pathKey);
      if (existing === undefined) {
        choices.set(pathKey, {
          executablePath: process.executablePath,
          name: process.name,
          processIds: [process.processId],
        });
      } else {
        existing.processIds.push(process.processId);
      }
    }
    return [...choices.values()]
      .filter(
        (choice) =>
          query === "" ||
          choice.name.toLocaleLowerCase().includes(query) ||
          choice.executablePath.toLocaleLowerCase().includes(query) ||
          choice.processIds.some((processId) =>
            String(processId).includes(query),
          ),
      )
      .sort((left, right) => left.name.localeCompare(right.name));
  }, [searchText, selectedPathKeys, snapshot?.processes]);

  const selectedCandidate = useMemo(
    () =>
      filteredProcessChoices.find(
        (choice) => choice.executablePath === selectedCandidatePath,
      ) ?? null,
    [filteredProcessChoices, selectedCandidatePath],
  );

  /** 按后端规范化键读取可执行文件图标；缺少图标时由界面显示统一应用占位符。 */
  const processIcon = (executablePath: string) =>
    snapshot?.processIcons[executablePath.toLocaleLowerCase()] ?? null;

  /** 提交完整路径集合并采用后端返回的权威视图，避免并发刷新覆盖刚保存的选择。 */
  const applySelection = async (enabled: boolean, selectedPaths: string[]) => {
    setPending(true);
    setErrorMessage("");
    try {
      setSnapshot(await updateProcessSelection({ enabled, selectedPaths }));
      setSelectedCandidatePath("");
    } catch (error) {
      setErrorMessage(error instanceof Error ? error.message : String(error));
    } finally {
      setPending(false);
    }
  };

  /** 将下拉框当前实例的可执行路径加入持久化集合；同一路径的多实例只保存一次。 */
  const addSelectedProcess = () => {
    if (snapshot === null || selectedCandidatePath === "") {
      return;
    }
    void applySelection(true, [
      ...snapshot.selectedPaths,
      selectedCandidatePath,
    ]);
  };

  /** 删除单个已保存路径；删除最后一项时同步关闭捕获，保持后端配置始终有效。 */
  const removeSelectedPath = (executablePath: string) => {
    if (snapshot === null) {
      return;
    }
    const selectedPaths = snapshot.selectedPaths.filter(
      (path) => path.toLocaleLowerCase() !== executablePath.toLocaleLowerCase(),
    );
    void applySelection(snapshot.enabled && selectedPaths.length > 0, selectedPaths);
  };

  const processByPath = useMemo(() => {
    const grouped = new Map<string, ProcessCandidate[]>();
    for (const process of snapshot?.processes ?? []) {
      const key = process.executablePath.toLocaleLowerCase();
      grouped.set(key, [...(grouped.get(key) ?? []), process]);
    }
    return grouped;
  }, [snapshot?.processes]);

  return (
    <Theme asChild>
      <main className="pageShell processManagerPage">
      <header className="pageHeader">
        <div>
          <h1>{t("page.processManager.title")}</h1>
          <p>{t("page.processManager.description")}</p>
        </div>
        <button
          disabled={pending}
          type="button"
          onClick={() => void refreshProcesses()}
        >
          <RefreshCw aria-hidden="true" size={16} />
          {t("page.processManager.refresh")}
        </button>
      </header>

      {errorMessage ? <div className="processManagerError" role="alert">{errorMessage}</div> : null}

      <section className="processPickerPanel">
        <label className="processSearchField">
          <span>{t("page.processManager.searchLabel")}</span>
          <div>
            <Search aria-hidden="true" size={16} />
            <input
              placeholder={t("page.processManager.searchPlaceholder")}
              value={searchText}
              onChange={(event) => setSearchText(event.target.value)}
            />
          </div>
        </label>
        <div className="processCandidateField">
          <span>{t("page.processManager.processLabel")}</span>
          <DropdownMenu.Root>
            <DropdownMenu.Trigger>
              <button
                aria-label={t("page.processManager.processLabel")}
                className="processCandidateTrigger"
                disabled={pending || filteredProcessChoices.length === 0}
                type="button"
              >
                <span>
                  {selectedCandidate?.name ??
                    t("page.processManager.processPlaceholder")}
                </span>
                <ChevronDown aria-hidden="true" size={16} />
              </button>
            </DropdownMenu.Trigger>
            <DropdownMenu.Content
              align="start"
              className="processCandidateMenu"
              sideOffset={6}
            >
              {filteredProcessChoices.map((choice) => (
                <DropdownMenu.Item
                  key={choice.executablePath}
                  className="processCandidateOption"
                  onSelect={() => setSelectedCandidatePath(choice.executablePath)}
                >
                  <span className="processIconFrame">
                    {processIcon(choice.executablePath) ? (
                      <img alt="" src={processIcon(choice.executablePath) ?? undefined} />
                    ) : (
                      <AppWindow aria-hidden="true" size={22} />
                    )}
                  </span>
                  <span className="processCandidateDetails">
                    <strong>{choice.name}</strong>
                    <span>{choice.executablePath}</span>
                    <small>PID {choice.processIds.join(", ")}</small>
                  </span>
                </DropdownMenu.Item>
              ))}
            </DropdownMenu.Content>
          </DropdownMenu.Root>
        </div>
        <button
          disabled={pending || selectedCandidatePath === ""}
          type="button"
          onClick={addSelectedProcess}
        >
          <Plus aria-hidden="true" size={16} />
          {t("page.processManager.add")}
        </button>
      </section>

      <section className="selectedProcessesPanel">
        <header>
          <div>
            <h2>{t("page.processManager.selectedTitle")}</h2>
            <p>{t("page.processManager.rememberHint")}</p>
          </div>
          <label className="processCaptureToggle">
            <input
              checked={snapshot?.enabled ?? false}
              disabled={pending || (snapshot?.selectedPaths.length ?? 0) === 0}
              type="checkbox"
              onChange={(event) =>
                snapshot &&
                void applySelection(event.target.checked, snapshot.selectedPaths)
              }
            />
            <span>{t("page.processManager.captureEnabled")}</span>
          </label>
        </header>
        {snapshot === null || snapshot.selectedPaths.length === 0 ? (
          <div className="processManagerEmpty">
            <CircleStop aria-hidden="true" size={28} />
            <span>{t("page.processManager.empty")}</span>
          </div>
        ) : (
          <div className="selectedProcessList">
            {snapshot.selectedPaths.map((executablePath) => {
              const instances = processByPath.get(executablePath.toLocaleLowerCase()) ?? [];
              return (
                <article key={executablePath}>
                  <div className="selectedProcessIdentity">
                    <span className="processIconFrame processIconFrameLarge">
                      {processIcon(executablePath) ? (
                        <img alt="" src={processIcon(executablePath) ?? undefined} />
                      ) : (
                        <AppWindow aria-hidden="true" size={24} />
                      )}
                    </span>
                    <div>
                      <strong>{instances[0]?.name ?? executablePath.split(/[\\/]/u).at(-1)}</strong>
                      <span>{executablePath}</span>
                      <small>
                        {instances.length > 0
                          ? t("page.processManager.running", {
                              pids: instances.map((process) => process.processId).join(", "),
                            })
                          : t("page.processManager.notRunning")}
                      </small>
                    </div>
                  </div>
                  <button
                    aria-label={t("page.processManager.remove", { path: executablePath })}
                    disabled={pending}
                    type="button"
                    onClick={() => removeSelectedPath(executablePath)}
                  >
                    <Trash2 aria-hidden="true" size={16} />
                  </button>
                </article>
              );
            })}
          </div>
        )}
      </section>
      </main>
    </Theme>
  );
}
