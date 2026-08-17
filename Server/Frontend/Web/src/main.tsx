import { StrictMode, useEffect } from "react";
import { createRoot } from "react-dom/client";
import { BrowserRouter } from "react-router-dom";
import { Theme } from "@radix-ui/themes";

import { App } from "./app";
import { MainWindowCloseDialog } from "./components/mainWindowCloseDialog";
import { RemoteAuthenticationGate } from "./components/remoteAuthenticationGate";
import i18n from "./i18n";
import { dismissStartupLoading } from "./platform/startupLoading";
import { ServiceProvider } from "./state/serviceStore";
import { UiContextReporterProvider } from "./state/uiContextReporter";
import "@radix-ui/themes/styles.css";
import "./app.css";
import "./styles/dialogWorkspaces.css";
import "./styles/packetFilters.css";
import "./styles/captureWorkspace.css";
import "./styles/radixWorkspace.css";
import "./styles/floatingPanel.css";
import "./styles/controlSystem.css";
import "./styles/mainWindowCloseDialog.css";

const rootElement = document.getElementById("root");
if (rootElement === null) {
  throw new Error(i18n.t("error.web.rootMissing"));
}

/**
 * 在根组件完成首次提交后撤下内联启动层；Effect 晚于首帧 DOM 提交，避免脚本开始执行时过早露出空白 WebView。
 */
function StartupReadySignal() {
  useEffect(dismissStartupLoading, []);
  return null;
}

createRoot(rootElement).render(
  <StrictMode>
    <StartupReadySignal />
    <Theme
      accentColor="blue"
      grayColor="gray"
      panelBackground="translucent"
      radius="large"
      scaling="95%"
    >
      <BrowserRouter>
        <RemoteAuthenticationGate>
          <ServiceProvider>
            <UiContextReporterProvider>
              <App />
              <MainWindowCloseDialog />
            </UiContextReporterProvider>
          </ServiceProvider>
        </RemoteAuthenticationGate>
      </BrowserRouter>
    </Theme>
  </StrictMode>,
);
