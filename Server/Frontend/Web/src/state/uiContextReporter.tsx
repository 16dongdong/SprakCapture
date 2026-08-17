import {
  createContext,
  type ReactNode,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { useLocation } from "react-router-dom";

import { HttpControlClient } from "../api/controlClient";
import type {
  UiContextUpdate,
  UiDataSelection,
} from "../api/protocol";

const heartbeatMilliseconds = 5_000;

interface UiDataView {
  view: string | null;
  selection: UiDataSelection | null;
}

interface UiContextReporterValue {
  reportDataView(view: UiDataView): void;
}

const UiContextReporterContext = createContext<UiContextReporterValue>({
  reportDataView: () => undefined,
});

/**
 * 根据当前 React 路由生成稳定页面描述；动态参数只保留短 section，不上报完整 URL。
 *
 * 运行上下文：窗口路由变化或心跳发布时调用；参数 pathname 来自 React Router。
 * 未识别路径稳定归入 overview，不抛出异常或猜测查询参数。
 */
function describeRoute(pathname: string): Pick<
  UiContextUpdate,
  "windowKind" | "page" | "section"
> {
  const segments = pathname.split("/").filter(Boolean);
  if (segments[0] === "window") {
    if (segments[1] === "settings") {
      return {
        windowKind: "independent",
        page: "settings",
        section: segments[2] ?? "general",
      };
    }
    if (segments[1] === "account-management") {
      return {
        windowKind: "independent",
        page: "accountManagement",
        section: null,
      };
    }
    if (segments[1] === "plugin") {
      return {
        windowKind: "independent",
        page: "plugins",
        section: segments[2] ?? null,
      };
    }
    return {
      windowKind: "independent",
      page: "dialog",
      section: segments[2] ?? "dialog",
    };
  }
  if (segments[0] === "floating") {
    return { windowKind: "floating", page: "floating", section: null };
  }
  if (segments[0] === "connections") {
    return { windowKind: "main", page: "connections", section: null };
  }
  if (segments[0] === "account-management") {
    return { windowKind: "main", page: "accountManagement", section: null };
  }
  if (segments[0] === "settings") {
    return {
      windowKind: "main",
      page: "settings",
      section: segments[1] ?? "general",
    };
  }
  if (segments[0] === "plugins") {
    return { windowKind: "main", page: "plugins", section: null };
  }
  return { windowKind: "main", page: "overview", section: null };
}

/**
 * 维护当前窗口的界面心跳；路由、选择、焦点和可见性变化都会立即上报，静止页面每五秒续期。
 *
 * 运行上下文：挂载在已通过远程认证且 ServiceProvider 已创建的 React 根节点内。
 * 参数：children 是当前窗口应用树；业务组件通过 useReportUiDataView 只提交稳定选择。
 * 失败语义：短暂控制面失败只输出一次中文诊断并由下次心跳恢复，不污染用户业务错误状态。
 */
export function UiContextReporterProvider({ children }: { children: ReactNode }) {
  const location = useLocation();
  const client = useMemo(() => new HttpControlClient(), []);
  const instanceId = useRef(crypto.randomUUID());
  const sequence = useRef(0);
  const [dataView, setDataView] = useState<UiDataView>({
    view: null,
    selection: null,
  });
  const visibility = useRef({
    focused: document.hasFocus(),
    visible: document.visibilityState === "visible",
  });
  const warningVisible = useRef(false);

  /** 发送一帧自洽上下文；服务端 sequence 校验允许并发请求乱序完成。 */
  const publish = useCallback(() => {
    sequence.current += 1;
    const route = describeRoute(location.pathname);
    const update: UiContextUpdate = {
      instanceId: instanceId.current,
      sequence: sequence.current,
      ...route,
      view: dataView.view,
      selection: dataView.selection,
      focused: visibility.current.focused,
      visible: visibility.current.visible,
    };
    void client.updateUiContext(update).then(
      () => {
        warningVisible.current = false;
      },
      (error: unknown) => {
        if (!warningVisible.current) {
          console.warn("同步当前界面上下文失败", error);
          warningVisible.current = true;
        }
      },
    );
  }, [client, dataView, location.pathname]);

  useEffect(() => {
    publish();
    const interval = window.setInterval(publish, heartbeatMilliseconds);
    return () => window.clearInterval(interval);
  }, [publish]);

  useEffect(() => {
    /** 同步浏览器焦点；焦点窗口会成为 MCP 主上下文。 */
    const updateFocus = () => {
      visibility.current.focused = document.hasFocus();
      visibility.current.visible = document.visibilityState === "visible";
      publish();
    };
    window.addEventListener("focus", updateFocus);
    window.addEventListener("blur", updateFocus);
    document.addEventListener("visibilitychange", updateFocus);
    return () => {
      window.removeEventListener("focus", updateFocus);
      window.removeEventListener("blur", updateFocus);
      document.removeEventListener("visibilitychange", updateFocus);
    };
  }, [publish]);

  const value = useMemo<UiContextReporterValue>(
    () => ({ reportDataView: setDataView }),
    [],
  );
  return (
    <UiContextReporterContext.Provider value={value}>
      {children}
    </UiContextReporterContext.Provider>
  );
}

/**
 * 把业务组件当前查看的数据投影到窗口上下文；缺少 Provider 的独立测试保持无副作用。
 *
 * 参数：view 是稳定页签名，selection 只包含可供后续 MCP 查询的资源标识。
 * 失败语义：该 Hook 不执行网络请求，组件卸载时自动清空旧选择。
 */
export function useReportUiDataView(
  view: string | null,
  selection: UiDataSelection | null,
): void {
  const { reportDataView } = useContext(UiContextReporterContext);
  useEffect(() => {
    reportDataView({ view, selection });
  }, [reportDataView, selection, view]);
  useEffect(
    () => () => reportDataView({ view: null, selection: null }),
    [reportDataView],
  );
}
