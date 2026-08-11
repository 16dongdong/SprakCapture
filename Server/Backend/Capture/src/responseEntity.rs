//! 定义 HTTP 分段实体的严格响应头解析规则，供录制索引与媒体预览共同使用。
//!
//! `ETag` 与 `Content-Range` 决定不同事务能否被视为同一字节实体。若两个调用层各自采用
//! “取首值”语义，重复或冲突字段会让索引与预览产生不同结论，最终拼接出不存在的媒体。
//! 因此本模块只接受恰好出现一次且语法有效的字段；缺失、重复或畸形均返回 `None`。

use crate::HeaderField;

/// 返回恰好出现一次的响应头值；重复字段即使文本相同也不具备唯一实体证明能力。
///
/// 运行上下文：该函数仅解析已经录制的响应头，不修改原始事务。`name` 按 HTTP 字段名规则
/// 忽略 ASCII 大小写；字段缺失或出现两次及以上时返回 `None`，调用方必须排除跨事务重组。
fn uniqueResponseHeaderValue<'a>(headers: &'a [HeaderField], name: &str) -> Option<&'a str> {
    let mut values = headers
        .iter()
        .filter(|header| header.name.eq_ignore_ascii_case(name))
        .map(|header| header.value.trim());
    let value = values.next()?;
    values.next().is_none().then_some(value)
}

/// 校验并返回唯一强 ETag；弱标签、重复字段和畸形引号都不能证明分段属于同一实体。
///
/// 返回值借用原始响应头，仅用于同一事务读取或构造索引键。失败返回 `None`，禁止调用方再用
/// `Last-Modified` 或 URL 猜测实体代际，否则 CDN 内容更新时可能拼出坏文件。
pub fn strongResponseEntityTag(headers: &[HeaderField]) -> Option<&str> {
    let entityTag = uniqueResponseHeaderValue(headers, "etag")?;
    if entityTag.starts_with("W/")
        || entityTag.len() < 2
        || !entityTag.starts_with('"')
        || !entityTag.ends_with('"')
        || entityTag[1..entityTag.len() - 1].contains('"')
    {
        return None;
    }
    Some(entityTag)
}

/// 解析唯一的 `Content-Range: bytes START-END/TOTAL`，并返回闭区间与实体总长度。
///
/// 未知总长、重复字段、倒置区间、越界和整数溢出均返回 `None`。录制索引与预览端点必须复用
/// 此函数，保证“可进入索引”和“可被读取重组”采用完全相同的协议边界。
pub fn responseContentRange(headers: &[HeaderField]) -> Option<(u64, u64, u64)> {
    let value = uniqueResponseHeaderValue(headers, "content-range")?;
    let (unit, range) = value.split_once(' ')?;
    if !unit.eq_ignore_ascii_case("bytes") || range.contains(' ') {
        return None;
    }
    let (bounds, totalText) = range.split_once('/')?;
    let (startText, endText) = bounds.split_once('-')?;
    let start = startText.parse::<u64>().ok()?;
    let end = endText.parse::<u64>().ok()?;
    let total = totalText.parse::<u64>().ok()?;
    (start <= end && end < total).then_some((start, end, total))
}
