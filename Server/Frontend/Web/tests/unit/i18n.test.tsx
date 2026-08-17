import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it } from "vitest";

import { LanguageSelector } from "@/components/languageSelector";
import i18n, {
  changeLocalePreference,
  localeStorageKey,
  resolveSupportedLocale,
} from "@/i18n";

afterEach(async () => {
  window.localStorage.setItem(localeStorageKey, "zh-Hans");
  await i18n.changeLanguage("zh-Hans");
});

describe("界面语言运行时", () => {
  it("按 BCP 47 地区信息映射受支持语言", () => {
    expect(resolveSupportedLocale(["zh-TW"])).toBe("zh-Hant");
    expect(resolveSupportedLocale(["zh-Hans-CN"])).toBe("zh-Hans");
    expect(resolveSupportedLocale(["zh-Hant-TW"])).toBe("zh-Hant");
    expect(resolveSupportedLocale(["zh_Hant_HK"])).toBe("zh-Hant");
    expect(resolveSupportedLocale(["pt-PT"])).toBe("pt-BR");
    expect(resolveSupportedLocale(["xx-YY"])).toBe("en");
  });

  it("持久化选择并立即切换英文、简体中文和日文", async () => {
    await changeLocalePreference("en");
    expect(i18n.t("app.navigation.overview")).toBe("Overview");

    await changeLocalePreference("zh-Hans");
    expect(i18n.t("app.navigation.overview")).toBe("概览");

    await changeLocalePreference("ja");
    expect(i18n.t("app.navigation.overview")).toBe("概要");
    expect(window.localStorage.getItem(localeStorageKey)).toBe("ja");
  });

  /**
   * 配置工具与事务操作直接复用同一份翻译资源；固定这些高频字段，防止新增功能在简体中文界面回退为英文键值。
   */
  it("完整提供镜像、自动保存与重复请求的简体中文文案", async () => {
    await changeLocalePreference("zh-Hans");

    expect(i18n.t("tools.mirror.rootDirectory")).toBe("镜像根目录");
    expect(i18n.t("tools.autoSave.includeBodies")).toBe("包含正文");
    expect(i18n.t("repeat.advancedTitle")).toBe("高级重复");
    expect(i18n.t("repeat.viaProxy")).toBe("应用当前工具与录制管线");
  });

  it("通过设置页复用选择器持久化并应用语言", async () => {
    const user = userEvent.setup();
    render(<LanguageSelector />);

    await user.selectOptions(
      screen.getByRole("combobox", { name: "界面语言" }),
      "ja",
    );

    await waitFor(() => {
      expect(i18n.language).toBe("ja");
      expect(window.localStorage.getItem(localeStorageKey)).toBe("ja");
      expect(
        screen.getByRole("combobox", { name: "表示言語" }),
      ).toBeInTheDocument();
    });
  });

  it("接收其他窗口写入的语言偏好并立即刷新当前窗口", async () => {
    render(<LanguageSelector />);
    window.localStorage.setItem(localeStorageKey, "en");
    window.dispatchEvent(
      new StorageEvent("storage", {
        key: localeStorageKey,
        newValue: "en",
      }),
    );

    await waitFor(() => {
      expect(i18n.language).toBe("en");
      expect(
        screen.getByRole("combobox", { name: "Interface language" }),
      ).toHaveValue("en");
    });
  });
});
