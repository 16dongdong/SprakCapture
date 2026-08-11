import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { BrowserRouter } from "react-router-dom";
import { Theme } from "@radix-ui/themes";

import { App } from "./app";
import i18n from "./i18n";
import { ServiceProvider } from "./state/serviceStore";
import "@radix-ui/themes/styles.css";
import "./app.css";
import "./styles/dialogWorkspaces.css";
import "./styles/packetFilters.css";
import "./styles/captureWorkspace.css";
import "./styles/radixWorkspace.css";
import "./styles/controlSystem.css";

const rootElement = document.getElementById("root");
if (rootElement === null) {
  throw new Error(i18n.t("error.web.rootMissing"));
}

createRoot(rootElement).render(
  <StrictMode>
    <Theme
      accentColor="blue"
      grayColor="gray"
      panelBackground="translucent"
      radius="large"
      scaling="95%"
    >
      <BrowserRouter>
        <ServiceProvider>
          <App />
        </ServiceProvider>
      </BrowserRouter>
    </Theme>
  </StrictMode>,
);
