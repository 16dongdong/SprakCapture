import { Languages } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";

import {
  automaticLocale,
  changeLocalePreference,
  type LocalePreference,
  readLocalePreference,
  supportedLocales,
} from "../i18n";

interface LanguageSelectorProps {
  className?: string;
}

/**
 * 渲染持久语言选择器；auto 与十种明确语言共用同一即时切换路径。
 *
 * 运行上下文：选择器可嵌入设置页等不同布局；className 仅扩展容器样式，不改变持久化和切换语义。
 * 参数：className 为调用方提供的可选布局类名。
 * 失败语义：语言切换失败由 i18next 保持当前已解析语言，选择器不会写入额外的临时状态。
 */
export function LanguageSelector({ className }: LanguageSelectorProps) {
  const { t } = useTranslation();
  const [preference, setPreference] = useState<LocalePreference>(
    readLocalePreference,
  );

  /**
   * 先更新受控选择值再切换运行时语言；异步资源已静态加载，不会产生中间裸键。
   */
  const selectLocale = (nextPreference: LocalePreference) => {
    setPreference(nextPreference);
    void changeLocalePreference(nextPreference);
  };

  return (
    <label
      className={`languageSelector${className === undefined ? "" : ` ${className}`}`}
      title={t("app.language.label")}
    >
      <Languages aria-hidden="true" size={15} />
      <span className="visuallyHidden">{t("app.language.label")}</span>
      <select
        aria-label={t("app.language.label")}
        value={preference}
        onChange={(event) =>
          selectLocale(event.target.value as LocalePreference)
        }
      >
        <option value={automaticLocale}>{t("app.language.auto")}</option>
        {supportedLocales.map((locale) => (
          <option key={locale} value={locale}>
            {t(`app.language.${locale}`)}
          </option>
        ))}
      </select>
    </label>
  );
}
