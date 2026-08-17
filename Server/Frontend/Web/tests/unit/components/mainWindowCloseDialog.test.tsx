import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import {
  MainWindowCloseDialog,
  type MainWindowClosePlatform,
} from "@/components/mainWindowCloseDialog";

/** 创建可控桌面平台，直接触发原生关闭事件并核对是非答案。 */
function createPlatform(pending = false) {
  let closeListener: (() => void) | undefined;
  const platform: MainWindowClosePlatform = {
    isMainDesktopWindow: () => true,
    listenForCloseRequest: vi.fn(async (listener) => {
      closeListener = listener;
      return () => {
        closeListener = undefined;
      };
    }),
    hasPendingCloseRequest: vi.fn(async () => pending),
    cancelCloseRequest: vi.fn(async () => undefined),
    resolveCloseRequest: vi.fn(async () => undefined),
  };
  return { platform, requestClose: () => closeListener?.() };
}

describe("主窗口关闭询问", () => {
  it("选择否时停止后台并退出，记住状态随答案一次提交", async () => {
    const user = userEvent.setup();
    const { platform, requestClose } = createPlatform();
    render(<MainWindowCloseDialog platform={platform} />);
    await waitFor(() => expect(platform.listenForCloseRequest).toHaveBeenCalledOnce());

    requestClose();
    expect(await screen.findByText("是否进入托盘运行？")).toBeInTheDocument();
    await user.click(screen.getByRole("checkbox", { name: "记住我的选择" }));
    await user.click(screen.getByRole("button", { name: "否" }));

    await waitFor(() =>
      expect(platform.resolveCloseRequest).toHaveBeenCalledWith(false, true),
    );
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
  });

  it("原生待处理状态会显示询问，选择是时进入托盘", async () => {
    const user = userEvent.setup();
    const { platform } = createPlatform(true);
    render(<MainWindowCloseDialog platform={platform} />);

    expect(await screen.findByRole("dialog", { name: "关闭主窗口" })).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "是" }));
    await waitFor(() =>
      expect(platform.resolveCloseRequest).toHaveBeenCalledWith(true, false),
    );
  });

  it("按 Escape 仅取消关闭请求并保留应用", async () => {
    const user = userEvent.setup();
    const { platform, requestClose } = createPlatform();
    render(<MainWindowCloseDialog platform={platform} />);
    await waitFor(() => expect(platform.listenForCloseRequest).toHaveBeenCalledOnce());

    requestClose();
    await screen.findByRole("dialog");
    await user.keyboard("{Escape}");
    await waitFor(() => expect(platform.cancelCloseRequest).toHaveBeenCalledOnce());
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
  });

  it("提交失败时保留弹框和诊断以便重试", async () => {
    const user = userEvent.setup();
    const { platform, requestClose } = createPlatform();
    vi.mocked(platform.resolveCloseRequest).mockRejectedValueOnce(
      new Error("安装目录配置不可写"),
    );
    render(<MainWindowCloseDialog platform={platform} />);
    await waitFor(() => expect(platform.listenForCloseRequest).toHaveBeenCalledOnce());

    requestClose();
    await user.click(await screen.findByRole("button", { name: "是" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("安装目录配置不可写");
    expect(screen.getByRole("dialog")).toBeInTheDocument();
  });
});
