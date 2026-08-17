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
const localeBroadcastChannelName = "capture.ui.locale.sync";
const localePreferenceChangedEventName = "capture.ui.locale.changed";

export type SupportedLocale = (typeof supportedLocales)[number];
export type LocalePreference = SupportedLocale | typeof automaticLocale;

interface LocalePreferenceMessage {
  type: "localePreference";
  preference: LocalePreference;
}

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

/** 校验跨窗口传入的语言偏好；运行时消息不能依赖 TypeScript 静态类型。 */
function isLocalePreference(value: unknown): value is LocalePreference {
  return (
    value === automaticLocale ||
    supportedLocales.includes(value as SupportedLocale)
  );
}

/** 校验多窗口消息结构；未知消息由当前窗口忽略，不改变已应用语言。 */
function isLocalePreferenceMessage(
  value: unknown,
): value is LocalePreferenceMessage {
  if (typeof value !== "object" || value === null) {
    return false;
  }
  const message = value as Partial<LocalePreferenceMessage>;
  return (
    message.type === "localePreference" &&
    isLocalePreference(message.preference)
  );
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

/** 通知当前窗口内的受控选择器同步偏好值，正文刷新继续由 i18next 负责。 */
function notifyLocalePreferenceChanged(preference: LocalePreference): void {
  window.dispatchEvent(
    new CustomEvent<LocalePreference>(localePreferenceChangedEventName, {
      detail: preference,
    }),
  );
}

/** 应用已校验的偏好；远端同步不会再次广播，避免多窗口之间形成消息回环。 */
async function applyLocalePreference(
  preference: LocalePreference,
  persist: boolean,
): Promise<void> {
  await i18next.changeLanguage(resolveLocalePreference(preference));
  if (persist) {
    window.localStorage.setItem(localeStorageKey, preference);
  }
  notifyLocalePreferenceChanged(preference);
}

/** 持久化并广播语言选择；主窗口、设置窗口和悬浮窗口会在同一事件周期内切换。 */
export async function changeLocalePreference(
  preference: LocalePreference,
): Promise<void> {
  await applyLocalePreference(preference, true);
  localeBroadcastChannel?.postMessage({
    type: "localePreference",
    preference,
  } satisfies LocalePreferenceMessage);
}

/** 订阅当前窗口的语言偏好变化；返回清理函数，组件卸载后不会残留监听器。 */
export function subscribeLocalePreference(
  listener: (preference: LocalePreference) => void,
): () => void {
  const eventListener = (event: Event) => {
    const preference = (event as CustomEvent<unknown>).detail;
    if (isLocalePreference(preference)) {
      listener(preference);
    }
  };
  window.addEventListener(localePreferenceChangedEventName, eventListener);
  return () =>
    window.removeEventListener(localePreferenceChangedEventName, eventListener);
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
  window.addEventListener("storage", (event) => {
    if (event.key === localeStorageKey && isLocalePreference(event.newValue)) {
      void applyLocalePreference(event.newValue, false);
    }
  });
  window.addEventListener("languagechange", () => {
    if (readLocalePreference() === automaticLocale) {
      void i18next.changeLanguage(resolveLocalePreference(automaticLocale));
    }
  });
}

// 独立 WebView 不一定派发同源 storage 事件，因此桌面多窗口使用 BroadcastChannel 作为即时主路径。
const LocaleBroadcastChannel =
  typeof window === "undefined" ? undefined : window.BroadcastChannel;
const localeBroadcastChannel = LocaleBroadcastChannel
  ? new LocaleBroadcastChannel(localeBroadcastChannelName)
  : null;
localeBroadcastChannel?.addEventListener("message", (event) => {
  const message: unknown = event.data;
  if (isLocalePreferenceMessage(message)) {
    void applyLocalePreference(message.preference, true);
  }
});

export default i18next;
