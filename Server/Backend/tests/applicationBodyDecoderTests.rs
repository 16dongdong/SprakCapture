#![allow(non_snake_case)]

use aes::{
    Aes128,
    cipher::{BlockCipherEncrypt, KeyInit},
};
use capture_core::MessageSide;
use flate2::{
    Compression,
    write::{DeflateEncoder, GzEncoder, ZlibEncoder},
};
use std::io::Write;

#[path = "../src/controlApi/applicationBodyDecoder.rs"]
mod applicationBodyDecoder;

const BLOCK_BYTES: usize = 16;
const PROTOCOL_KEY: &[u8; BLOCK_BYTES] = b"e82ckenh8dichen8";

/// 构造确定性的 AES-128-ECB/PKCS#7 测试向量；测试只验证识别器，不依赖网络或录制状态。
fn encryptVector(plaintext: &[u8]) -> Vec<u8> {
    let paddingBytes = BLOCK_BYTES - plaintext.len() % BLOCK_BYTES;
    let mut encrypted = plaintext.to_vec();
    encrypted.extend(std::iter::repeat_n(paddingBytes as u8, paddingBytes));
    let cipher = Aes128::new(PROTOCOL_KEY.into());
    for block in encrypted.chunks_exact_mut(BLOCK_BYTES) {
        let blockArray: &mut [u8; BLOCK_BYTES] = block.try_into().expect("完整 AES 分组");
        cipher.encrypt_block(blockArray.into());
    }
    encrypted
}

/// 验证直接密文响应可被自动识别为 JSON，同时算法元信息保持稳定。
#[test]
fn decryptsDirectJsonResponse() {
    let expected = br#"{"code":200,"data":{"name":"test"}}"#;
    let encrypted = encryptVector(expected);
    let decoded = applicationBodyDecoder::decodeApplicationBody(
        MessageSide::Response,
        "application/json;charset=UTF-8",
        "identity",
        &encrypted,
    )
    .expect("合法应用层密文必须自动解码");

    assert_eq!(decoded.algorithm, "aes128EcbPkcs7Json");
    assert_eq!(decoded.contentType, "application/json;charset=UTF-8");
    assert_eq!(decoded.bytes, expected);
}

/// 验证表单请求只展示帧内 JSON，不把路径、分隔符和摘要混进参数视图。
#[test]
fn decryptsFramedFormRequest() {
    let expected = br#"{"keyword":"test","limit":30}"#;
    let mut plaintext = b"/api/search/pc/complex/page/v3-36cd479b6b5-".to_vec();
    plaintext.extend_from_slice(expected);
    plaintext.extend_from_slice(b"-36cd479b6b5-0123456789abcdef0123456789abcdef");
    let formBody = format!("params={}", hex::encode_upper(encryptVector(&plaintext)));
    let decoded = applicationBodyDecoder::decodeApplicationBody(
        MessageSide::Request,
        "application/x-www-form-urlencoded",
        "",
        formBody.as_bytes(),
    )
    .expect("合法表单密文必须自动解码");

    assert_eq!(decoded.bytes, expected);
}

/// 验证随机分组、非法填充和伪 JSON 都不会产生派生正文，原始查看路径因此保持唯一真源。
#[test]
fn rejectsUnknownOrDamagedBodies() {
    assert!(
        applicationBodyDecoder::decodeApplicationBody(
            MessageSide::Response,
            "application/json",
            "",
            &[0_u8; BLOCK_BYTES],
        )
        .is_none()
    );
    let encryptedText = encryptVector(b"this is not json");
    assert!(
        applicationBodyDecoder::decodeApplicationBody(
            MessageSide::Response,
            "application/json",
            "identity",
            &encryptedText,
        )
        .is_none()
    );
    let encryptedJson = encryptVector(br#"{"code":200}"#);
    assert!(
        applicationBodyDecoder::decodeApplicationBody(
            MessageSide::Response,
            "image/png",
            "",
            &encryptedJson,
        )
        .is_none()
    );
}

/// 使用指定写入编码器生成确定性压缩向量；写入或收尾失败会让测试立即失败。
fn compressVector(mut encoder: impl Write, plaintext: &[u8]) {
    encoder.write_all(plaintext).expect("压缩向量必须完整写入");
}

/// 验证常见公开 HTTP 内容编码均可恢复为原始正文，且不会改写录制字节。
#[test]
fn decodesStandardHttpContentCodings() {
    let expected = br#"{"code":200,"items":[1,2,3]}"#;

    let mut gzipEncoder = GzEncoder::new(Vec::new(), Compression::default());
    compressVector(&mut gzipEncoder, expected);
    let gzip = gzipEncoder.finish().expect("gzip 向量必须收尾");

    let mut zlibEncoder = ZlibEncoder::new(Vec::new(), Compression::default());
    compressVector(&mut zlibEncoder, expected);
    let zlib = zlibEncoder.finish().expect("zlib 向量必须收尾");

    let mut rawEncoder = DeflateEncoder::new(Vec::new(), Compression::default());
    compressVector(&mut rawEncoder, expected);
    let rawDeflate = rawEncoder.finish().expect("裸 deflate 向量必须收尾");

    let brotli = {
        let mut output = Vec::new();
        let mut writer = brotli::CompressorWriter::new(&mut output, 4096, 5, 22);
        compressVector(&mut writer, expected);
        drop(writer);
        output
    };
    let zstd = zstd::stream::encode_all(expected.as_slice(), 3).expect("zstd 向量必须生成");

    for (coding, encoded) in [
        ("gzip", gzip),
        ("deflate", zlib),
        ("deflate", rawDeflate),
        ("br", brotli),
        ("zstd", zstd),
    ] {
        let decoded = applicationBodyDecoder::decodeApplicationBody(
            MessageSide::Response,
            "application/json;charset=UTF-8",
            coding,
            &encoded,
        )
        .expect("公开内容编码必须自动恢复");
        assert_eq!(decoded.bytes, expected, "编码 {coding} 的恢复结果错误");
        assert_eq!(decoded.algorithm, coding);
    }
}

/// 验证组合内容编码严格逆序解除；未知或损坏编码不得返回中间层半成品。
#[test]
fn decodesStackedCodingsAndRejectsIncompleteChains() {
    let expected = br#"{"stream":true}"#;
    let gzip = {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        compressVector(&mut encoder, expected);
        encoder.finish().expect("gzip 向量必须收尾")
    };
    let brotli = {
        let mut output = Vec::new();
        let mut writer = brotli::CompressorWriter::new(&mut output, 4096, 5, 22);
        compressVector(&mut writer, &gzip);
        drop(writer);
        output
    };
    let decoded = applicationBodyDecoder::decodeApplicationBody(
        MessageSide::Response,
        "application/json",
        "gzip, br",
        &brotli,
    )
    .expect("组合编码必须按逆序恢复");
    assert_eq!(decoded.bytes, expected);
    assert_eq!(decoded.algorithm, "gzip + br");

    assert!(
        applicationBodyDecoder::decodeApplicationBody(
            MessageSide::Response,
            "application/json",
            "gzip, private-cipher",
            &brotli,
        )
        .is_none()
    );
    assert!(
        applicationBodyDecoder::decodeApplicationBody(
            MessageSide::Response,
            "application/json",
            "gzip",
            b"damaged",
        )
        .is_none()
    );
}
