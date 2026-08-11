use std::net::IpAddr;

use crate::{LocationError, LocationMatchOptions, LocationPattern, ResolvedLocation};

/// 校验可持久化的 Location 规则；失败只返回结构化错误，不生成用户语言文案。
pub fn validateLocationPattern(pattern: &LocationPattern) -> Result<(), LocationError> {
    validateProtocol(&pattern.protocol)?;
    validateHost(&pattern.host)?;
    parsePortRanges(&pattern.port)?;
    validatePath(&pattern.path)?;
    Ok(())
}

/// 按统一 Location 语义匹配已解析目标；无效规则或候选返回稳定错误类型。
pub fn locationMatches(
    pattern: &LocationPattern,
    candidate: &ResolvedLocation,
    options: LocationMatchOptions,
) -> Result<bool, LocationError> {
    validateLocationPattern(pattern)?;
    validateCandidate(candidate)?;
    if !matchesProtocol(&pattern.protocol, &candidate.protocol) {
        return Ok(false);
    }
    if !matchesHost(&pattern.host, &candidate.host, options.caseSensitiveHost) {
        return Ok(false);
    }
    if !matchesPort(&pattern.port, candidate.port)? {
        return Ok(false);
    }
    if !matchesPath(&pattern.path, &candidate.path, options.normalizePath) {
        return Ok(false);
    }
    Ok(matchesQuery(pattern.query.as_deref(), &candidate.query))
}

/// 限定协议字段为产品支持的稳定集合，避免拼写错误静默变成永不命中的规则。
fn validateProtocol(protocol: &str) -> Result<(), LocationError> {
    if protocol.is_empty()
        || matches!(
            protocol.to_ascii_lowercase().as_str(),
            "*" | "http" | "https" | "ws" | "wss" | "socks" | "tcp" | "tls" | "udp"
        )
    {
        return Ok(());
    }
    Err(LocationError::InvalidProtocol)
}

/// 拒绝把 URL、端口或空白混入 host；IPv6 允许带或不带方括号。
fn validateHost(host: &str) -> Result<(), LocationError> {
    if host.is_empty() || host == "*" {
        return Ok(());
    }
    if host.chars().any(char::is_whitespace)
        || host.contains('/')
        || host.contains('#')
        || containsQuerySyntax(host)
    {
        return Err(LocationError::InvalidHost);
    }
    let hasOpeningBracket = host.starts_with('[');
    let hasClosingBracket = host.ends_with(']');
    if hasOpeningBracket != hasClosingBracket {
        return Err(LocationError::InvalidHost);
    }
    let normalizedHost = stripIpv6Brackets(host);
    if normalizedHost.is_empty() {
        return Err(LocationError::InvalidHost);
    }
    if hasOpeningBracket && normalizedHost.parse::<std::net::Ipv6Addr>().is_err() {
        return Err(LocationError::InvalidHost);
    }
    if normalizedHost.contains(':') && normalizedHost.parse::<IpAddr>().is_err() {
        return Err(LocationError::InvalidHost);
    }
    Ok(())
}

/// 区分单字符 host 通配符与误粘贴的 URL query，后者通常在问号后携带赋值或分隔符。
fn containsQuerySyntax(host: &str) -> bool {
    host.split_once('?')
        .is_some_and(|(_, suffix)| suffix.contains('=') || suffix.contains('&'))
}

/// 确保规则路径可以与 HTTP origin-form 路径稳定比较。
fn validatePath(path: &str) -> Result<(), LocationError> {
    if path.is_empty() || path == "*" || path.starts_with('/') {
        return Ok(());
    }
    Err(LocationError::InvalidPath)
}

/// 验证数据面传入的目标已完成协议、主机、端口和路径解析。
fn validateCandidate(candidate: &ResolvedLocation) -> Result<(), LocationError> {
    if candidate.protocol.is_empty()
        || candidate.host.is_empty()
        || candidate.port == 0
        || (!candidate.path.is_empty() && !candidate.path.starts_with('/'))
        || candidate.protocol == "*"
        || validateProtocol(&candidate.protocol).is_err()
        || validateHost(&candidate.host).is_err()
        || candidate.host.contains('*')
        || candidate.host.contains('?')
    {
        return Err(LocationError::InvalidCandidate);
    }
    Ok(())
}

/// 解析逗号列表和闭区间；空或星号使用空集合表示任意端口。
fn parsePortRanges(portExpression: &str) -> Result<Vec<(u16, u16)>, LocationError> {
    let trimmedExpression = portExpression.trim();
    if trimmedExpression.is_empty() || trimmedExpression == "*" {
        return Ok(Vec::new());
    }
    let mut ranges = Vec::new();
    for segment in trimmedExpression.split(',') {
        let segment = segment.trim();
        if segment.is_empty() {
            return Err(LocationError::InvalidPort);
        }
        let mut bounds = segment.split('-');
        let start = parsePort(bounds.next().unwrap_or_default())?;
        let end = match bounds.next() {
            Some(value) => parsePort(value)?,
            None => start,
        };
        if bounds.next().is_some() || start > end {
            return Err(LocationError::InvalidPort);
        }
        ranges.push((start, end));
    }
    Ok(ranges)
}

/// 解析单个非零端口；u16 溢出和保留的零端口均使用同一稳定错误。
fn parsePort(value: &str) -> Result<u16, LocationError> {
    let port = value
        .trim()
        .parse::<u16>()
        .map_err(|_| LocationError::InvalidPort)?;
    if port == 0 {
        return Err(LocationError::InvalidPort);
    }
    Ok(port)
}

/// 匹配协议；规则中的空值与星号覆盖所有受支持协议。
fn matchesProtocol(pattern: &str, candidate: &str) -> bool {
    pattern.is_empty() || pattern == "*" || pattern.eq_ignore_ascii_case(candidate)
}

/// 匹配主机；默认先统一大小写并剥离 IPv6 方括号，再运行线性通配算法。
fn matchesHost(pattern: &str, candidate: &str, caseSensitive: bool) -> bool {
    if pattern.is_empty() || pattern == "*" {
        return true;
    }
    let pattern = stripIpv6Brackets(pattern);
    let candidate = stripIpv6Brackets(candidate);
    if caseSensitive {
        return wildcardMatches(pattern, candidate);
    }
    wildcardMatches(&pattern.to_lowercase(), &candidate.to_lowercase())
}

/// 匹配端口列表或范围；规则已校验时只会返回成功结果。
fn matchesPort(portExpression: &str, candidate: u16) -> Result<bool, LocationError> {
    let ranges = parsePortRanges(portExpression)?;
    Ok(ranges.is_empty()
        || ranges
            .iter()
            .any(|(start, end)| (*start..=*end).contains(&candidate)))
}

/// 匹配路径；无通配符时按目录边界做前缀匹配，避免 `/api` 误命中 `/apix`。
fn matchesPath(pattern: &str, candidate: &str, normalizePath: bool) -> bool {
    if pattern.is_empty() || pattern == "*" {
        return true;
    }
    let pattern = normalizedPath(pattern, normalizePath);
    let candidate = normalizedPath(candidate, normalizePath);
    if pattern.contains('*') || pattern.contains('?') {
        return wildcardMatches(pattern, candidate);
    }
    candidate == pattern
        || candidate
            .strip_prefix(pattern)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

/// 匹配可选查询；普通文本按子串语义，显式通配符按完整查询语义。
fn matchesQuery(pattern: Option<&str>, candidate: &str) -> bool {
    let Some(pattern) = pattern else {
        return true;
    };
    if pattern.is_empty() || pattern == "*" {
        return true;
    }
    if pattern.contains('*') || pattern.contains('?') {
        return wildcardMatches(pattern, candidate);
    }
    candidate.contains(pattern)
}

/// 在启用路径规范化时忽略非根路径末尾的重复斜杠。
fn normalizedPath(path: &str, normalize: bool) -> &str {
    if !normalize || path == "/" {
        return path;
    }
    let normalized = path.trim_end_matches('/');
    if normalized.is_empty() {
        "/"
    } else {
        normalized
    }
}

/// 剥离完整 IPv6 字面量两侧方括号；普通 host 保持原样。
fn stripIpv6Brackets(host: &str) -> &str {
    host.strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .unwrap_or(host)
}

/// 用单次回溯星号位置的贪心算法匹配 `*` 与 `?`，复杂度为线性时间和线性字符索引空间。
fn wildcardMatches(pattern: &str, candidate: &str) -> bool {
    let patternCharacters: Vec<char> = pattern.chars().collect();
    let candidateCharacters: Vec<char> = candidate.chars().collect();
    let mut patternIndex = 0_usize;
    let mut candidateIndex = 0_usize;
    let mut latestStar = None;
    let mut starCandidateIndex = 0_usize;

    while candidateIndex < candidateCharacters.len() {
        if patternIndex < patternCharacters.len()
            && (patternCharacters[patternIndex] == '?'
                || patternCharacters[patternIndex] == candidateCharacters[candidateIndex])
        {
            patternIndex += 1;
            candidateIndex += 1;
            continue;
        }
        if patternIndex < patternCharacters.len() && patternCharacters[patternIndex] == '*' {
            latestStar = Some(patternIndex);
            patternIndex += 1;
            starCandidateIndex = candidateIndex;
            continue;
        }
        let Some(starIndex) = latestStar else {
            return false;
        };
        patternIndex = starIndex + 1;
        starCandidateIndex += 1;
        candidateIndex = starCandidateIndex;
    }
    while patternIndex < patternCharacters.len() && patternCharacters[patternIndex] == '*' {
        patternIndex += 1;
    }
    patternIndex == patternCharacters.len()
}
