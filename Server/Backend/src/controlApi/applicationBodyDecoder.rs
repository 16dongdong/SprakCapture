//! 为正文查看器识别并还原公开内容编码与已知应用协议载荷。
//!
//! HTTP 传输层会保留录制时收到的原始字节，本模块只生成只读派生视图。标准
//! `Content-Encoding` 按声明顺序的逆序解码；应用私有协议只有在结构、填充和 JSON
//! 三重校验全部通过时才会命中，避免把普通二进制正文误判为明文。

use aes::{
    Aes128,
    cipher::{BlockCipherDecrypt, KeyInit},
};
use capture_core::MessageSide;
use flate2::read::{DeflateDecoder, GzDecoder, ZlibDecoder};
use serde_json::Value;
use std::{
    borrow::Cow,
    io::{Cursor, Read},
};

const BLOCK_BYTES: usize = 16;
const DECODED_BODY_LIMIT_BYTES: u64 = 256 * 1024 * 1024;
const FRAMED_JSON_DELIMITER: &[u8] = b"-36cd479b6b5-";
const FIXED_PROTOCOL_KEY: &[u8; BLOCK_BYTES] = b"e82ckenh8dichen8";

/// 描述自动识别器生成的只读正文；原始字节仍由 Capture 正文句柄持有。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct DecodedApplicationBody {
    pub(super) algorithm: String,
    pub(super) contentType: String,
    pub(super) bytes: Vec<u8>,
}

/// 自动还原标准 HTTP 内容编码，并在标准编码解除后识别已知应用协议正文。
///
/// 运行上下文：控制 API 已按需取得完整正文后同步调用。`contentEncoding` 可以包含
/// 多层逗号分隔编码；函数按 RFC 语义逆序解码。任何未知编码、损坏数据或超出查看器
/// 内存预算的输出都返回 `None`，调用方继续展示未经改写的原始录制字节。
pub(super) fn decodeApplicationBody(
    side: MessageSide,
    contentType: &str,
    contentEncoding: &str,
    bytes: &[u8],
) -> Option<DecodedApplicationBody> {
    let contentCodings = parseContentCodings(contentEncoding)?;
    let mut decodedBytes = bytes.to_vec();
    for coding in contentCodings.iter().rev() {
        decodedBytes = decodeContentCoding(coding, &decodedBytes)?;
    }

    if let Some(applicationBody) = decodeKnownApplicationProtocol(side, contentType, &decodedBytes)
    {
        let mut algorithms = contentCodings;
        algorithms.push(applicationBody.algorithm);
        return Some(DecodedApplicationBody {
            algorithm: algorithms.join(" + "),
            contentType: applicationBody.contentType,
            bytes: applicationBody.bytes,
        });
    }

    (!contentCodings.is_empty()).then(|| DecodedApplicationBody {
        algorithm: contentCodings.join(" + "),
        contentType: contentType.to_owned(),
        bytes: decodedBytes,
    })
}

/// 解析标准内容编码链；`identity` 不产生派生正文，未知编码拒绝部分解码。
///
/// 采用全有或全无语义是为了避免组合编码只解除一层后，把中间字节误当作最终正文。
fn parseContentCodings(contentEncoding: &str) -> Option<Vec<String>> {
    let mut codings = Vec::new();
    for token in contentEncoding
        .split(',')
        .map(str::trim)
        .filter(|token| !token.is_empty())
    {
        let coding = token.to_ascii_lowercase();
        match coding.as_str() {
            "identity" => {}
            "gzip" | "x-gzip" | "deflate" | "br" | "zstd" => codings.push(coding),
            _ => return None,
        }
    }
    Some(codings)
}

/// 解开一层公开 HTTP 内容编码，并对输出施加统一内存上限。
///
/// `deflate` 的规范形态是 zlib 包装，但历史服务器也会发送裸 DEFLATE；仅在规范解码
/// 失败时尝试裸流兼容。失败表示这一层正文损坏，不返回半截输出。
fn decodeContentCoding(coding: &str, bytes: &[u8]) -> Option<Vec<u8>> {
    match coding {
        "gzip" | "x-gzip" => readDecoded(GzDecoder::new(Cursor::new(bytes))),
        "deflate" => readDecoded(ZlibDecoder::new(Cursor::new(bytes)))
            .or_else(|| readDecoded(DeflateDecoder::new(Cursor::new(bytes)))),
        "br" => readDecoded(brotli::Decompressor::new(Cursor::new(bytes), 64 * 1024)),
        "zstd" => readDecoded(zstd::stream::read::Decoder::new(Cursor::new(bytes)).ok()?),
        _ => None,
    }
}

/// 完整读取解码器输出；超过预算或读取错误时丢弃派生结果，绝不截断正文。
fn readDecoded(mut reader: impl Read) -> Option<Vec<u8>> {
    let mut decoded = Vec::new();
    reader
        .by_ref()
        .take(DECODED_BODY_LIMIT_BYTES + 1)
        .read_to_end(&mut decoded)
        .ok()?;
    (decoded.len() as u64 <= DECODED_BODY_LIMIT_BYTES).then_some(decoded)
}

/// 识别仓库已知的应用协议载荷；未知私有加密不会被猜测或改写。
fn decodeKnownApplicationProtocol(
    side: MessageSide,
    contentType: &str,
    bytes: &[u8],
) -> Option<DecodedApplicationBody> {
    let encryptedBytes = match side {
        MessageSide::Request => Cow::Owned(encryptedFormParameter(contentType, bytes)?),
        MessageSide::Response if isPotentialEncryptedResponse(contentType) => Cow::Borrowed(bytes),
        MessageSide::Response => return None,
    };
    let plaintext = decryptAes128EcbPkcs7(encryptedBytes.as_ref())?;
    let jsonBytes = match side {
        MessageSide::Request => extractFramedJson(&plaintext)?,
        MessageSide::Response => validateJson(&plaintext)?,
    };
    Some(DecodedApplicationBody {
        algorithm: "aes128EcbPkcs7Json".to_owned(),
        contentType: "application/json;charset=UTF-8".to_owned(),
        bytes: jsonBytes.to_vec(),
    })
}

/// 从表单正文读取唯一的十六进制 `params` 字段；歧义输入返回 `None`。
fn encryptedFormParameter(contentType: &str, body: &[u8]) -> Option<Vec<u8>> {
    if !contentType
        .to_ascii_lowercase()
        .starts_with("application/x-www-form-urlencoded")
    {
        return None;
    }
    let mut values = body
        .split(|byte| *byte == b'&')
        .filter_map(|field| field.strip_prefix(b"params="));
    let encoded = values.next()?;
    if values.next().is_some() || encoded.is_empty() {
        return None;
    }
    hex::decode(encoded).ok()
}

/// 限定私有响应探测范围，避免对媒体等大型二进制正文执行无意义的 AES 扫描。
fn isPotentialEncryptedResponse(contentType: &str) -> bool {
    let mediaType = contentType
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    mediaType.is_empty()
        || mediaType == "application/json"
        || mediaType == "application/octet-stream"
        || mediaType.starts_with("text/")
}

/// 使用固定协议密钥完成 AES-128-ECB 解密，并严格校验 PKCS#7 填充。
fn decryptAes128EcbPkcs7(encryptedBytes: &[u8]) -> Option<Vec<u8>> {
    if encryptedBytes.is_empty() || !encryptedBytes.len().is_multiple_of(BLOCK_BYTES) {
        return None;
    }
    let cipher = Aes128::new(FIXED_PROTOCOL_KEY.into());
    let mut plaintext = encryptedBytes.to_vec();
    for block in plaintext.chunks_exact_mut(BLOCK_BYTES) {
        let blockArray: &mut [u8; BLOCK_BYTES] = block.try_into().ok()?;
        cipher.decrypt_block(blockArray.into());
    }
    let paddingBytes = *plaintext.last()? as usize;
    if paddingBytes == 0
        || paddingBytes > BLOCK_BYTES
        || paddingBytes > plaintext.len()
        || plaintext[plaintext.len() - paddingBytes..]
            .iter()
            .any(|byte| usize::from(*byte) != paddingBytes)
    {
        return None;
    }
    plaintext.truncate(plaintext.len() - paddingBytes);
    Some(plaintext)
}

/// 从路径、分隔符、JSON、分隔符、摘要组成的协议帧中提取 JSON 区域。
fn extractFramedJson(plaintext: &[u8]) -> Option<&[u8]> {
    let firstDelimiter = findBytes(plaintext, FRAMED_JSON_DELIMITER)?;
    let jsonStart = firstDelimiter.checked_add(FRAMED_JSON_DELIMITER.len())?;
    let secondRelative = findBytes(&plaintext[jsonStart..], FRAMED_JSON_DELIMITER)?;
    let jsonEnd = jsonStart.checked_add(secondRelative)?;
    let digestStart = jsonEnd.checked_add(FRAMED_JSON_DELIMITER.len())?;
    let digest = plaintext.get(digestStart..)?;
    if firstDelimiter == 0 || digest.len() != 32 || !digest.iter().all(u8::is_ascii_hexdigit) {
        return None;
    }
    validateJson(plaintext.get(jsonStart..jsonEnd)?)
}

/// 校验 UTF-8 JSON 且只接受对象或数组，避免普通标量偶然命中私有协议。
fn validateJson(bytes: &[u8]) -> Option<&[u8]> {
    let value = serde_json::from_slice::<Value>(bytes).ok()?;
    matches!(value, Value::Object(_) | Value::Array(_)).then_some(bytes)
}

/// 在线性时间内查找短协议分隔符，返回首个完整匹配的起始偏移。
fn findBytes(source: &[u8], pattern: &[u8]) -> Option<usize> {
    if pattern.is_empty() || pattern.len() > source.len() {
        return None;
    }
    source
        .windows(pattern.len())
        .position(|window| window == pattern)
}
