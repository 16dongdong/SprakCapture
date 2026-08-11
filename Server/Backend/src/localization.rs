use std::{
    collections::{BTreeMap, HashMap},
    convert::Infallible,
    sync::LazyLock,
};

use axum::{
    extract::FromRequestParts,
    http::{HeaderMap, Uri, header::ACCEPT_LANGUAGE, request::Parts},
};
use serde::Serialize;

pub type MessageParams = BTreeMap<String, String>;

/// 定义控制协议支持的一等语言；枚举值必须与前端目录名和语言清单保持一致。
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Locale {
    En,
    ZhHans,
    ZhHant,
    Ja,
    Ko,
    Es,
    Fr,
    De,
    PtBr,
    Ru,
}

impl Locale {
    /// 返回稳定 BCP 47 语言代码；该值可直接写入 HTML、HTTP 和 WebSocket 查询参数。
    pub const fn code(self) -> &'static str {
        match self {
            Self::En => "en",
            Self::ZhHans => "zh-Hans",
            Self::ZhHant => "zh-Hant",
            Self::Ja => "ja",
            Self::Ko => "ko",
            Self::Es => "es",
            Self::Fr => "fr",
            Self::De => "de",
            Self::PtBr => "pt-BR",
            Self::Ru => "ru",
        }
    }
}

/// 定义控制 API 的稳定错误码；HTTP 文案变化不得改变这些机器可读值。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ErrorCode {
    InvalidListenPort,
    InvalidListenHost,
    CredentialsForbiddenWithoutAuthentication,
    CredentialsRequired,
    TimeoutMustBePositiveFinite,
    TimeoutOutOfRange,
    TimeoutBelowMillisecond,
    InvalidConfiguration,
    ServiceNotStartable,
    ServiceStartFailed,
    ServiceNotStoppable,
    ServiceStopFailed,
    OriginForbidden,
    InvalidConfigurationRequest,
    InvalidHttpProxyConfiguration,
    ListenerConfigurationConflict,
    HttpProxyListenerFailed,
    InvalidRecordingRequest,
    InvalidRecordingLimits,
    InvalidRecordingLocation,
    RecordingOperationFailed,
    InvalidSslRequest,
    InvalidSslConfiguration,
    SslOperationFailed,
    InvalidTransactionsQuery,
    TransactionsCollectionChanged,
    TransactionNotFound,
    BodyNotFound,
    InvalidToolRequest,
    ToolNotFound,
    InvalidToolConfiguration,
    ToolOperationFailed,
    BreakpointNotFound,
    BreakpointOperationFailed,
    InvalidExportRequest,
    ExportOperationFailed,
    InvalidRepeatRequest,
    RepeatBodyUnavailable,
    UnsupportedRepeatTransaction,
    RepeatRecordingUnavailable,
    AdvancedRepeatConfirmationRequired,
    InvalidAdvancedRepeatPlan,
    LoadTestNotFound,
    PluginNotFound,
    PluginOperationFailed,
    InvalidProcessSelectionRequest,
    ProcessSelectionOperationFailed,
    ConfigurationPersistenceFailed,
}

impl ErrorCode {
    /// 返回错误目录中的稳定键；键名属于公开控制协议，新增错误必须同步十份目录。
    pub const fn messageKey(self) -> &'static str {
        match self {
            Self::InvalidListenPort => "error.invalidListenPort",
            Self::InvalidListenHost => "error.invalidListenHost",
            Self::CredentialsForbiddenWithoutAuthentication => {
                "error.credentialsForbiddenWithoutAuthentication"
            }
            Self::CredentialsRequired => "error.credentialsRequired",
            Self::TimeoutMustBePositiveFinite => "error.timeoutMustBePositiveFinite",
            Self::TimeoutOutOfRange => "error.timeoutOutOfRange",
            Self::TimeoutBelowMillisecond => "error.timeoutBelowMillisecond",
            Self::InvalidConfiguration => "error.invalidConfiguration",
            Self::ServiceNotStartable => "error.serviceNotStartable",
            Self::ServiceStartFailed => "error.serviceStartFailed",
            Self::ServiceNotStoppable => "error.serviceNotStoppable",
            Self::ServiceStopFailed => "error.serviceStopFailed",
            Self::OriginForbidden => "error.originForbidden",
            Self::InvalidConfigurationRequest => "error.invalidConfigurationRequest",
            Self::InvalidHttpProxyConfiguration => "error.invalidHttpProxyConfiguration",
            Self::ListenerConfigurationConflict => "error.listenerConfigurationConflict",
            Self::HttpProxyListenerFailed => "error.httpProxyListenerFailed",
            Self::InvalidRecordingRequest => "error.invalidRecordingRequest",
            Self::InvalidRecordingLimits => "error.invalidRecordingLimits",
            Self::InvalidRecordingLocation => "error.invalidRecordingLocation",
            Self::RecordingOperationFailed => "error.recordingOperationFailed",
            Self::InvalidSslRequest => "error.invalidSslRequest",
            Self::InvalidSslConfiguration => "error.invalidSslConfiguration",
            Self::SslOperationFailed => "error.sslOperationFailed",
            Self::InvalidTransactionsQuery => "error.invalidTransactionsQuery",
            Self::TransactionsCollectionChanged => "error.transactionsCollectionChanged",
            Self::TransactionNotFound => "error.transactionNotFound",
            Self::BodyNotFound => "error.bodyNotFound",
            Self::InvalidToolRequest => "error.invalidToolRequest",
            Self::ToolNotFound => "error.toolNotFound",
            Self::InvalidToolConfiguration => "error.invalidToolConfiguration",
            Self::ToolOperationFailed => "error.toolOperationFailed",
            Self::BreakpointNotFound => "error.breakpointNotFound",
            Self::BreakpointOperationFailed => "error.breakpointOperationFailed",
            Self::InvalidExportRequest => "error.invalidExportRequest",
            Self::ExportOperationFailed => "error.exportOperationFailed",
            Self::InvalidRepeatRequest => "error.invalidRepeatRequest",
            Self::RepeatBodyUnavailable => "error.repeatBodyUnavailable",
            Self::UnsupportedRepeatTransaction => "error.unsupportedRepeatTransaction",
            Self::RepeatRecordingUnavailable => "error.repeatRecordingUnavailable",
            Self::AdvancedRepeatConfirmationRequired => "error.advancedRepeatConfirmationRequired",
            Self::InvalidAdvancedRepeatPlan => "error.invalidAdvancedRepeatPlan",
            Self::LoadTestNotFound => "error.loadTestNotFound",
            Self::PluginNotFound => "error.pluginNotFound",
            Self::PluginOperationFailed => "error.pluginOperationFailed",
            Self::InvalidProcessSelectionRequest => "error.invalidProcessSelectionRequest",
            Self::ProcessSelectionOperationFailed => "error.processSelectionOperationFailed",
            Self::ConfigurationPersistenceFailed => "error.configurationPersistenceFailed",
        }
    }
}

const localeCatalogSources: [(Locale, &str); 10] = [
    (Locale::En, include_str!("../locales/en/errors.json")),
    (
        Locale::ZhHans,
        include_str!("../locales/zh-Hans/errors.json"),
    ),
    (
        Locale::ZhHant,
        include_str!("../locales/zh-Hant/errors.json"),
    ),
    (Locale::Ja, include_str!("../locales/ja/errors.json")),
    (Locale::Ko, include_str!("../locales/ko/errors.json")),
    (Locale::Es, include_str!("../locales/es/errors.json")),
    (Locale::Fr, include_str!("../locales/fr/errors.json")),
    (Locale::De, include_str!("../locales/de/errors.json")),
    (Locale::PtBr, include_str!("../locales/pt-BR/errors.json")),
    (Locale::Ru, include_str!("../locales/ru/errors.json")),
];

static localeCatalogs: LazyLock<HashMap<Locale, HashMap<String, String>>> = LazyLock::new(|| {
    localeCatalogSources
        .into_iter()
        .map(|(locale, source)| {
            // Windows 目录编辑器会在 UTF-8 JSON 首部保留 BOM；仅在边界移除该标记，
            // 目录正文仍由 serde 严格解析，避免宽松读取掩盖翻译文件错误。
            let catalog = serde_json::from_str(source.trim_start_matches('\u{feff}'))
                .unwrap_or_else(|error| panic!("{} 错误目录无效：{error}", locale.code()));
            (locale, catalog)
        })
        .collect()
});

/// 把受控参数代入目录模板；参数只替换同名占位符，不执行格式化或表达式求值。
pub fn localizeError(code: ErrorCode, locale: Locale, params: &MessageParams) -> String {
    let messageKey = code.messageKey();
    let template = localeCatalogs
        .get(&locale)
        .and_then(|catalog| catalog.get(messageKey))
        .or_else(|| {
            localeCatalogs
                .get(&Locale::En)
                .and_then(|catalog| catalog.get(messageKey))
        })
        .unwrap_or_else(|| panic!("错误目录缺少稳定键：{messageKey}"));
    params
        .iter()
        .fold(template.clone(), |message, (name, value)| {
            message.replace(&format!("{{{name}}}"), value)
        })
}

/// 把单个 BCP 47 候选映射到一等语言；未知候选返回 None 供调用方继续协商。
pub fn resolveLocaleTag(languageTag: &str) -> Option<Locale> {
    let normalized = languageTag.trim().replace('_', "-").to_ascii_lowercase();
    match normalized.as_str() {
        "en" => Some(Locale::En),
        "ja" => Some(Locale::Ja),
        "ko" => Some(Locale::Ko),
        "es" => Some(Locale::Es),
        "fr" => Some(Locale::Fr),
        "de" => Some(Locale::De),
        "pt" | "pt-br" => Some(Locale::PtBr),
        "ru" => Some(Locale::Ru),
        _ if normalized == "zh-hant"
            || normalized.starts_with("zh-hant-")
            || normalized == "zh-tw"
            || normalized.starts_with("zh-tw-")
            || normalized == "zh-hk"
            || normalized.starts_with("zh-hk-")
            || normalized == "zh-mo"
            || normalized.starts_with("zh-mo-") =>
        {
            Some(Locale::ZhHant)
        }
        _ if normalized == "zh"
            || normalized == "zh-hans"
            || normalized.starts_with("zh-hans-")
            || normalized == "zh-cn"
            || normalized.starts_with("zh-cn-")
            || normalized == "zh-sg"
            || normalized.starts_with("zh-sg-") =>
        {
            Some(Locale::ZhHans)
        }
        _ => match normalized.split('-').next() {
            Some("en") => Some(Locale::En),
            Some("ja") => Some(Locale::Ja),
            Some("ko") => Some(Locale::Ko),
            Some("es") => Some(Locale::Es),
            Some("fr") => Some(Locale::Fr),
            Some("de") => Some(Locale::De),
            Some("pt") => Some(Locale::PtBr),
            Some("ru") => Some(Locale::Ru),
            _ => None,
        },
    }
}

/// 按 RFC qvalue 语法把质量值转换为千分整数；范围外、小数超过三位或非数字均拒绝，
/// 避免无效权重被误当成默认最高优先级。
fn parseQualityValue(rawValue: &str) -> Option<u16> {
    let value = rawValue.trim();
    let (whole, fraction) = value.split_once('.').unwrap_or((value, ""));
    if value.matches('.').count() > 1 || fraction.len() > 3 {
        return None;
    }
    if !fraction.bytes().all(|character| character.is_ascii_digit()) {
        return None;
    }
    match whole {
        "0" => {
            let mut thousandths = fraction.parse::<u16>().ok().unwrap_or(0);
            for _ in fraction.len()..3 {
                thousandths *= 10;
            }
            Some(thousandths)
        }
        "1" if fraction.bytes().all(|character| character == b'0') => Some(1_000),
        _ => None,
    }
}

/// 解析 Accept-Language 的质量权重；非法或 q=0 候选被忽略，相同权重保持客户端顺序。
fn resolveAcceptLanguage(headers: &HeaderMap) -> Option<Locale> {
    let mut candidateIndex = 0_usize;
    let mut candidates = Vec::new();
    for headerValue in headers.get_all(ACCEPT_LANGUAGE).iter() {
        let Ok(header) = headerValue.to_str() else {
            continue;
        };
        for item in header.split(',') {
            let index = candidateIndex;
            candidateIndex += 1;
            let mut parts = item.trim().split(';');
            let Some(languageTag) = parts.next().map(str::trim).filter(|tag| !tag.is_empty())
            else {
                continue;
            };
            let mut quality = Some(1_000_u16);
            let mut qualitySeen = false;
            for parameter in parts
                .map(str::trim)
                .filter(|parameter| !parameter.is_empty())
            {
                let Some((name, value)) = parameter.split_once('=') else {
                    if parameter.eq_ignore_ascii_case("q") {
                        quality = None;
                        break;
                    }
                    continue;
                };
                if !name.trim().eq_ignore_ascii_case("q") {
                    continue;
                }
                if qualitySeen {
                    quality = None;
                    break;
                }
                qualitySeen = true;
                quality = parseQualityValue(value);
            }
            if let Some(quality) = quality.filter(|quality| *quality > 0) {
                candidates.push((languageTag, quality, index));
            }
        }
    }
    candidates.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.2.cmp(&right.2)));
    candidates
        .into_iter()
        .find_map(|(languageTag, _, _)| resolveLocaleTag(languageTag))
}

/// 按 query locale、Accept-Language、英文的固定优先级解析请求语言。
pub fn resolveRequestLocale(uri: &Uri, headers: &HeaderMap) -> Locale {
    let queryLocale = uri.query().and_then(|query| {
        query.split('&').find_map(|pair| {
            let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
            (name == "locale")
                .then(|| resolveLocaleTag(value))
                .flatten()
        })
    });
    queryLocale
        .or_else(|| resolveAcceptLanguage(headers))
        .unwrap_or(Locale::En)
}

/// 作为 Axum 提取器暴露请求语言；协商始终有英文结果，因此提取不会拒绝请求。
#[derive(Clone, Copy, Debug)]
pub struct RequestLocale(pub Locale);

impl<S> FromRequestParts<S> for RequestLocale
where
    S: Send + Sync,
{
    type Rejection = Infallible;

    /// 在消费请求体之前读取 URI 与头部，供后续任意错误路径复用同一语言。
    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        Ok(Self(resolveRequestLocale(&parts.uri, &parts.headers)))
    }
}
