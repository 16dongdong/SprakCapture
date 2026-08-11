import i18next from "i18next";
import { initReactI18next } from "react-i18next";

import de from "./locales/de/app.json";
import en from "./locales/en/app.json";
import es from "./locales/es/app.json";
import fr from "./locales/fr/app.json";
import ja from "./locales/ja/app.json";
import ko from "./locales/ko/app.json";
import ptBr from "./locales/pt-BR/app.json";
import ru from "./locales/ru/app.json";
import zhHans from "./locales/zh-Hans/app.json";
import zhHant from "./locales/zh-Hant/app.json";

export const supportedLocales = [
  "en",
  "zh-Hans",
  "zh-Hant",
  "ja",
  "ko",
  "es",
  "fr",
  "de",
  "pt-BR",
  "ru",
] as const;
export const automaticLocale = "auto" as const;
export const localeStorageKey = "capture.ui.locale";

export type SupportedLocale = (typeof supportedLocales)[number];
export type LocalePreference = SupportedLocale | typeof automaticLocale;

const localeResources = {
  en: { app: en },
  "zh-Hans": { app: zhHans },
  "zh-Hant": { app: zhHant },
  ja: { app: ja },
  ko: { app: ko },
  es: { app: es },
  fr: { app: fr },
  de: { app: de },
  "pt-BR": { app: ptBr },
  ru: { app: ru },
} as const;

/**
 * 把浏览器 BCP 47 候选映射到一等语言；区域变体优先保持简繁中文与巴西葡语语义。
 */
export function resolveSupportedLocale(
  languageCandidates: readonly string[],
): SupportedLocale {
  for (const languageCandidate of languageCandidates) {
    const normalized = languageCandidate
      .trim()
      .replaceAll("_", "-")
      .toLowerCase();
    const exactLocale = supportedLocales.find(
      (supportedLocale) => supportedLocale.toLowerCase() === normalized,
    );
    if (exactLocale !== undefined) {
      return exactLocale;
    }
    if (
      normalized === "zh" ||
      normalized === "zh-hans" ||
      normalized.startsWith("zh-hans-") ||
      normalized === "zh-cn" ||
      normalized.startsWith("zh-cn-") ||
      normalized === "zh-sg" ||
      normalized.startsWith("zh-sg-")
    ) {
      return "zh-Hans";
    }
    if (
      normalized === "zh-hant" ||
      normalized.startsWith("zh-hant-") ||
      normalized === "zh-tw" ||
      normalized.startsWith("zh-tw-") ||
      normalized === "zh-hk" ||
      normalized.startsWith("zh-hk-") ||
      normalized === "zh-mo" ||
      normalized.startsWith("zh-mo-")
    ) {
      return "zh-Hant";
    }
    if (normalized === "pt" || normalized.startsWith("pt-")) {
      return "pt-BR";
    }
    const baseLocale = supportedLocales.find(
      (supportedLocale) =>
        supportedLocale.toLowerCase().split("-")[0] ===
        normalized.split("-")[0],
    );
    if (baseLocale !== undefined) {
      return baseLocale;
    }
  }
  return "en";
}

/**
 * 读取持久语言选择；损坏或过期值视为 auto，禁止把未知 locale 交给运行时。
 */
export function readLocalePreference(): LocalePreference {
  if (typeof window === "undefined") {
    return automaticLocale;
  }
  const storedPreference = window.localStorage.getItem(localeStorageKey);
  return storedPreference === automaticLocale ||
    supportedLocales.includes(storedPreference as SupportedLocale)
    ? (storedPreference as LocalePreference)
    : automaticLocale;
}

/**
 * 解析当前有效语言；auto 只读取浏览器候选，不把推断结果覆盖用户持久选择。
 */
export function resolveLocalePreference(
  preference: LocalePreference,
  browserLanguages: readonly string[] =
    typeof navigator === "undefined" ? [] : navigator.languages,
): SupportedLocale {
  return preference === automaticLocale
    ? resolveSupportedLocale(browserLanguages)
    : preference;
}

const initialPreference = readLocalePreference();

void i18next.use(initReactI18next).init({
  resources: localeResources,
  lng: resolveLocalePreference(initialPreference),
  fallbackLng: ["en", "zh-Hans"],
  supportedLngs: [...supportedLocales],
  ns: ["app"],
  defaultNS: "app",
  interpolation: {
    escapeValue: false,
  },
});

/**
 * 持久化并立即应用语言选择；auto 保留选择本身，系统语言变化时仍可重新协商。
 */
export async function changeLocalePreference(
  preference: LocalePreference,
): Promise<void> {
  window.localStorage.setItem(localeStorageKey, preference);
  await i18next.changeLanguage(resolveLocalePreference(preference));
}

/**
 * 返回 HTTP 控制请求使用的具体语言，避免把 auto 作为无效 Accept-Language 发送。
 */
export function currentRequestLocale(): SupportedLocale {
  return resolveSupportedLocale([i18next.resolvedLanguage ?? i18next.language]);
}

if (typeof document !== "undefined") {
  i18next.on("languageChanged", (language) => {
    document.documentElement.lang = resolveSupportedLocale([language]);
  });
  document.documentElement.lang = resolveLocalePreference(initialPreference);
}

if (typeof window !== "undefined") {
  window.addEventListener("languagechange", () => {
    if (readLocalePreference() === automaticLocale) {
      void i18next.changeLanguage(resolveLocalePreference(automaticLocale));
    }
  });
}

export default i18next;
