import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import type { EventMessage, ProtobufConfiguration } from "@/api/protocol";
import { ProtocolSettingsDialog } from "@/components/protocolSettingsDialog";
import i18n from "@/i18n";
import { ServiceProvider } from "@/state/serviceStore";
import {
  createControlClientStub,
  createServiceSnapshot,
} from "#tests/testFixtures";

/** 创建已连接事件流替身，使此用例只验证 L3 字段表单和控制契约。 */
function createConnectedEventClient() {
  return {
    start(callbacks: {
      onConnectionState(state: "connected", message: string): void;
      onMessage?(message: EventMessage): void;
    }) {
      callbacks.onConnectionState("connected", "事件流已连接");
    },
    stop() {},
  };
}

describe("Protobuf 协议工具设置", () => {
  /** 描述符已登记时，路由编辑只提交字段化的开关与路由，不把 schema 文件元数据回写到更新请求。 */
  it("以字段化路由提交 Protobuf 配置", async () => {
    const user = userEvent.setup();
    const snapshot = createServiceSnapshot();
    const configuration: ProtobufConfiguration = {
      enabled: false,
      schemas: [
        {
          id: "00000000-0000-4000-8000-000000000010",
          name: "订单描述符",
          descriptorPath: "protobufDescriptors/orders.desc",
          defaultMessageType: "orders.v1.Order",
        },
      ],
      routes: [],
    };
    const updateProtobufConfiguration = vi.fn(async () => configuration);
    const controlClient = createControlClientStub(snapshot, {
      getProtobufConfiguration: async () => configuration,
      updateProtobufConfiguration,
    });

    const { container } = render(
      <ServiceProvider
        controlClient={controlClient}
        eventClient={createConnectedEventClient()}
      >
        <ProtocolSettingsDialog open onClose={() => undefined} />
      </ServiceProvider>,
    );

    await screen.findByRole("dialog");
    expect(container.querySelector(".toolDialogHeader button")).toBeNull();
    expect(container.querySelectorAll(".toolDialogFooter > button")).toHaveLength(2);
    await user.click(
      screen.getByRole("button", { name: i18n.t("protocolSettings.routes.add") }),
    );
    await user.type(
      screen.getByRole("textbox", { name: i18n.t("protocolSettings.routes.id") }),
      "-orders",
    );
    await user.type(
      screen.getByRole("textbox", { name: i18n.t("tools.form.host") }),
      "orders.example",
    );
    await user.click(
      screen.getByRole("button", { name: i18n.t("tools.apply") }),
    );

    await waitFor(() => expect(updateProtobufConfiguration).toHaveBeenCalledTimes(1));
    expect(updateProtobufConfiguration).toHaveBeenCalledWith({
      enabled: false,
      routes: [
        {
          id: "protobuf-route-1-orders",
          location: {
            protocol: "",
            host: "orders.example",
            port: "",
            path: "",
            query: null,
          },
          messageType: "orders.v1.Order",
          responseMessageType: null,
          schemaId: "00000000-0000-4000-8000-000000000010",
        },
      ],
    });
  });
});
