import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { ToolbarIcon } from "../../../src/components/toolbarIcon";

describe("ToolbarIcon", () => {
  it("不同控件状态使用独立位图并保持装饰图标不进入读屏顺序", () => {
    render(
      <>
        <ToolbarIcon data-testid="disabled" name="clearDisabled" size={20} />
        <ToolbarIcon data-testid="enabled" name="clearEnabled" size={20} />
      </>,
    );

    const disabledIcon = screen.getByTestId("disabled");
    const enabledIcon = screen.getByTestId("enabled");
    expect(disabledIcon).toHaveAttribute("aria-hidden", "true");
    expect(disabledIcon).toHaveAttribute(
      "data-toolbar-icon",
      "clearDisabled",
    );
    expect(disabledIcon).toHaveStyle({
      "--toolbar-icon-source":
        'url("/assets/toolbar/clearDisabled.png")',
      "--toolbar-icon-size": "20px",
    });
    expect(enabledIcon).toHaveStyle({
      "--toolbar-icon-source": 'url("/assets/toolbar/clearEnabled.png")',
      "--toolbar-icon-size": "20px",
    });
  });
});
