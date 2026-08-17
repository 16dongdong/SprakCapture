import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { ToolbarIcon } from "../../../src/components/toolbarIcon";

describe("ToolbarIcon", () => {
  /** 每个状态必须直接引用自己的图片节点，避免背景图层在远程浏览器中只绘制部分状态。 */
  it("不同控件状态使用独立位图并保持装饰图标不进入读屏顺序", () => {
    render(
      <>
        <ToolbarIcon data-testid="disabled" name="clearDisabled" size={20} />
        <ToolbarIcon data-testid="enabled" name="clearEnabled" size={20} />
      </>,
    );

    const disabledIcon = screen.getByTestId("disabled");
    const enabledIcon = screen.getByTestId("enabled");
    expect(disabledIcon.tagName).toBe("IMG");
    expect(disabledIcon).toHaveAttribute("aria-hidden", "true");
    expect(disabledIcon).toHaveAttribute("alt", "");
    expect(disabledIcon).toHaveAttribute(
      "src",
      "/assets/toolbar/clearDisabled.png?v=20260814-1",
    );
    expect(disabledIcon).toHaveAttribute(
      "data-toolbar-icon",
      "clearDisabled",
    );
    expect(disabledIcon).toHaveStyle({
      width: "20px",
      height: "20px",
    });
    expect(enabledIcon).toHaveAttribute(
      "src",
      "/assets/toolbar/clearEnabled.png?v=20260814-1",
    );
    expect(enabledIcon).toHaveStyle({
      width: "20px",
      height: "20px",
    });
  });
});
