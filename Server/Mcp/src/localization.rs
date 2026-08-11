use std::{collections::HashMap, sync::Arc};

use serde::Deserialize;

pub const supportedLocales: [&str; 10] = [
    "en", "zh-Hans", "zh-Hant", "ja", "ko", "es", "fr", "de", "pt-BR", "ru",
];

const localeResources: [(&str, &str); 10] = [
    ("en", include_str!("../locales/en/messages.json")),
    ("zh-Hans", include_str!("../locales/zh-Hans/messages.json")),
    ("zh-Hant", include_str!("../locales/zh-Hant/messages.json")),
    ("ja", include_str!("../locales/ja/messages.json")),
    ("ko", include_str!("../locales/ko/messages.json")),
    ("es", include_str!("../locales/es/messages.json")),
    ("fr", include_str!("../locales/fr/messages.json")),
    ("de", include_str!("../locales/de/messages.json")),
    ("pt-BR", include_str!("../locales/pt-BR/messages.json")),
    ("ru", include_str!("../locales/ru/messages.json")),
];

/// 保存 MCP 自有文案的只读多语言目录；所有实例共享解析后的不可变映射。
#[derive(Clone)]
pub struct MessageCatalog {
    messages: Arc<HashMap<String, HashMap<String, String>>>,
}

#[derive(Deserialize)]
struct LocaleMessages(HashMap<String, String>);

impl MessageCatalog {
    /// 解析编译期嵌入的十语资源；任一文件无效时拒绝启动，避免向客户端暴露裸键。
    pub fn load() -> Result<Self, String> {
        let mut messages = HashMap::with_capacity(localeResources.len());
        for (locale, resource) in localeResources {
            let LocaleMessages(localeMessages) = serde_json::from_str(resource)
                .map_err(|error| format!("MCP locale catalog {locale} is invalid: {error}"))?;
            if let Some(invalidKey) = localeMessages.iter().find_map(|(key, message)| {
                (message.trim().is_empty() || message == key).then_some(key)
            }) {
                return Err(format!(
                    "MCP locale catalog {locale} has an empty or unresolved value for {invalidKey}"
                ));
            }
            messages.insert(locale.to_owned(), localeMessages);
        }
        let Some(referenceKeys) = messages.get("en").map(sortedKeys) else {
            return Err("MCP locale catalog is missing en".to_owned());
        };
        for locale in supportedLocales {
            let localeKeys = messages
                .get(locale)
                .map(sortedKeys)
                .ok_or_else(|| format!("MCP locale catalog is missing {locale}"))?;
            if localeKeys != referenceKeys {
                return Err(format!(
                    "MCP locale catalog {locale} does not match the en key set"
                ));
            }
        }
        Ok(Self {
            messages: Arc::new(messages),
        })
    }

    /// 按 BCP 47 常用别名解析区域设置；未知值回退 en，保证机器调用得到稳定文本。
    pub fn resolveLocale(&self, requestedLocale: Option<&str>) -> &'static str {
        normalizeLocale(requestedLocale.unwrap_or("en"))
    }

    /// 返回指定键的本地化文案；构建期键集合校验保证正常路径不会返回裸键。
    pub fn message(&self, locale: &str, key: &str) -> String {
        self.messages
            .get(locale)
            .and_then(|catalog| catalog.get(key))
            .or_else(|| self.messages.get("en").and_then(|catalog| catalog.get(key)))
            .cloned()
            .unwrap_or_else(|| key.to_owned())
    }
}

/// 生成稳定排序的键集合，使语言文件的 JSON 书写顺序不影响一致性判断。
fn sortedKeys(messages: &HashMap<String, String>) -> Vec<&str> {
    let mut keys: Vec<&str> = messages.keys().map(String::as_str).collect();
    keys.sort_unstable();
    keys
}

/// 把常见地区标签映射到十个一等语言；匹配只影响文案，不改变控制面业务语义。
fn normalizeLocale(locale: &str) -> &'static str {
    let normalized = locale.trim().replace('_', "-").to_ascii_lowercase();
    if normalized == "zh-hant"
        || normalized.starts_with("zh-hant-")
        || normalized.starts_with("zh-tw")
        || normalized.starts_with("zh-hk")
        || normalized.starts_with("zh-mo")
    {
        return "zh-Hant";
    }
    if normalized == "zh-hans"
        || normalized.starts_with("zh-hans-")
        || normalized == "zh"
        || normalized.starts_with("zh-cn")
        || normalized.starts_with("zh-sg")
    {
        return "zh-Hans";
    }
    if normalized == "pt" || normalized.starts_with("pt-") {
        return "pt-BR";
    }
    for locale in ["en", "ja", "ko", "es", "fr", "de", "ru"] {
        if normalized == locale || normalized.starts_with(&format!("{locale}-")) {
            return match locale {
                "en" => "en",
                "ja" => "ja",
                "ko" => "ko",
                "es" => "es",
                "fr" => "fr",
                "de" => "de",
                "ru" => "ru",
                _ => unreachable!(),
            };
        }
    }
    "en"
}
