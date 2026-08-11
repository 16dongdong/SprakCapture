import { useEffect, useRef } from "react";
import {
  Navigate,
  Route,
  Routes,
  useLocation,
  useNavigate,
  useParams,
} from "react-router-dom";

import { ConnectionStatusBar } from "./components/connectionStatusBar";
import { TopToolbar } from "./components/topToolbar";
import {
  readSettingsSection,
  showIndependentWindow,
  type IndependentWindowRequest,
} from "./platform/independentWindowContract";
import { ConnectionsPage } from "./pages/connectionsPage";
import { FloatingWindowPage } from "./pages/floatingWindowPage";
import {
  IndependentDialogWindowPage,
  IndependentSettingsWindowPage,
} from "./pages/independentWindowPage";
import { OverviewPage } from "./pages/overviewPage";
import { PluginManagerPage } from "./pages/pluginManagerPage";
import { useServiceStore } from "./state/serviceStore";

/** 打开独立窗口并保留原生错误证据；入口不会回退到遮罩主窗口的旧实现。 */
function openIndependentWindow(request: IndependentWindowRequest): void {
  void showIndependentWindow(request).catch((error: unknown) => {
    console.error("打开独立窗口失败", error);
  });
}

/**
 * 把旧设置地址兼容为独立窗口入口；主窗口立即返回连接页，设置表单不会占用主内容区域。
 */
function SettingsWindowRedirect() {
  const navigate = useNavigate();
  const { section } = useParams<{ section?: string }>();

  useEffect(() => {
    openIndependentWindow({
      kind: "settings",
      section: readSettingsSection(section),
    });
    navigate("/connections", { replace: true });
  }, [navigate, section]);
  return null;
}

/**
 * 监听必须即时处理的控制面状态，并把恢复提示和断点编辑器放入独立窗口。
 * 事务签名只在队列新增或替换时聚焦窗口，普通快照刷新不会反复抢占用户焦点。
 */
function BackgroundWorkflowWindows() {
  const { suspendedBreakpoints } = useServiceStore();
  const breakpointSignature = suspendedBreakpoints
    .map((breakpoint) => breakpoint.transactionId)
    .join("\u0000");
  const previousBreakpointSignature = useRef("");

  useEffect(() => {
    if (
      breakpointSignature !== "" &&
      breakpointSignature !== previousBreakpointSignature.current
    ) {
      openIndependentWindow({ kind: "breakpointHit" });
    }
    previousBreakpointSignature.current = breakpointSignature;
  }, [breakpointSignature]);

  return null;
}

/**
 * 渲染主工作区；设置、规则编辑和监听配置均通过独立窗口打开，主窗口不再挂载业务遮罩层。
 */
function MainWindowLayout() {
  const location = useLocation();
  return (
    <div className="applicationWindow">
      <TopToolbar
        onOpenSslSettings={() => openIndependentWindow({ kind: "ssl" })}
        onOpenProtocolSettings={() =>
          openIndependentWindow({ kind: "protocol" })
        }
        onOpenToolSettings={(tool) =>
          openIndependentWindow({ kind: "tool", tool })
        }
        onOpenListenerSettings={(listener) =>
          openIndependentWindow({ kind: "listener", listener })
        }
      />
      <div className="routeContent">
        <div className="routeTransition" key={location.pathname}>
          <Routes>
            <Route path="/overview" element={<OverviewPage />} />
            <Route
              path="/connections"
              element={
                <ConnectionsPage
                  onOpenSslSettings={(seed, focusClientCertificate) =>
                    openIndependentWindow({
                      kind: "ssl",
                      seed,
                      focusClientCertificate,
                    })
                  }
                  onOpenToolSettings={(tool, seed) =>
                    openIndependentWindow({ kind: "tool", tool, seed })
                  }
                />
              }
            />
            <Route
              path="/settings/:section?"
              element={<SettingsWindowRedirect />}
            />
            <Route path="/plugins" element={<PluginManagerPage />} />
            <Route path="*" element={<Navigate replace to="/connections" />} />
          </Routes>
        </div>
      </div>
      <ConnectionStatusBar />
      <BackgroundWorkflowWindows />
    </div>
  );
}

/** 根据路由选择主窗口、悬浮窗或独立业务窗口；各窗口拥有独立 React 根布局。 */
export function App() {
  return (
    <Routes>
      <Route path="/floating" element={<FloatingWindowPage />} />
      <Route
        path="/window/settings/:section?"
        element={<IndependentSettingsWindowPage />}
      />
      <Route
        path="/window/dialog/:dialogKind"
        element={<IndependentDialogWindowPage />}
      />
      <Route path="/*" element={<MainWindowLayout />} />
    </Routes>
  );
}
