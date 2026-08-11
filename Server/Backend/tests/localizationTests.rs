#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]

use axum::http::{HeaderMap, HeaderValue, Uri, header::ACCEPT_LANGUAGE};

use proxy_backend::localization::{
    ErrorCode, Locale, MessageParams, localizeError, resolveLocaleTag, resolveRequestLocale,
};

/// 验证显式 query 覆盖请求头，地区变体和质量权重仍按 BCP 47 语义解析。
#[test]
fn requestLocaleUsesStablePriority() {
    let mut headers = HeaderMap::new();
    headers.insert(
        ACCEPT_LANGUAGE,
        HeaderValue::from_static("de;q=0.7, zh-TW;q=0.9"),
    );
    let explicitUri: Uri = "/api/v1/snapshot?locale=ja".parse().expect("解析测试 URI");
    expectLocale(explicitUri, &headers, Locale::Ja);

    let negotiatedUri: Uri = "/api/v1/snapshot".parse().expect("解析测试 URI");
    expectLocale(negotiatedUri, &headers, Locale::ZhHant);
}

/// 验证脚本与区域子标签不会让简繁中文退化为同一个基础语言。
#[test]
fn chineseScriptAndRegionTagsRemainDistinct() {
    assert_eq!(resolveLocaleTag("zh-Hans-CN"), Some(Locale::ZhHans));
    assert_eq!(resolveLocaleTag("zh_Hans_SG"), Some(Locale::ZhHans));
    assert_eq!(resolveLocaleTag("zh-Hant-TW"), Some(Locale::ZhHant));
    assert_eq!(resolveLocaleTag("zh_Hant_HK"), Some(Locale::ZhHant));
}

/// 验证合法 qvalue 排序、零权重排除及非法权重拒绝；无效 q 不得回升为默认 1。
#[test]
fn acceptLanguageRejectsInvalidQualityValues() {
    let mut headers = HeaderMap::new();
    headers.insert(
        ACCEPT_LANGUAGE,
        HeaderValue::from_static("en;q=broken, de;q=1.001, zh-Hans;q=0, ja;Q=0.750, fr;q=0.500"),
    );
    let uri: Uri = "/api/v1/snapshot".parse().expect("解析测试 URI");
    expectLocale(uri, &headers, Locale::Ja);

    headers.insert(
        ACCEPT_LANGUAGE,
        HeaderValue::from_static("de;q=0.999, ru;q=1.000"),
    );
    let uri: Uri = "/api/v1/snapshot".parse().expect("解析测试 URI");
    expectLocale(uri, &headers, Locale::Ru);
}

/// 验证运行时 detail 仅保留在结构化参数，非中文 message 不拼入底层中文诊断。
#[test]
fn localizedMessageDoesNotInterpolateRawDetail() {
    let mut params = MessageParams::new();
    params.insert("detail".to_owned(), "底层框架原始错误".to_owned());
    let message = localizeError(ErrorCode::InvalidConfiguration, Locale::En, &params);
    assert_eq!(message, "Configuration is invalid.");
    assert!(!message.contains("底层框架原始错误"));
}

/// 复用断言入口，失败信息同时显示期望与实际 BCP 47 代码。
fn expectLocale(uri: Uri, headers: &HeaderMap, expected: Locale) {
    let actual = resolveRequestLocale(&uri, headers);
    assert_eq!(
        actual,
        expected,
        "期望 {}，实际 {}",
        expected.code(),
        actual.code()
    );
}
