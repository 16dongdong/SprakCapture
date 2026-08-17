use argon2::Argon2;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, Mac};
use rand::{RngCore, rng};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::{AccountServiceError, Result};

const saltBytes: usize = 16;
const hashBytes: usize = 32;
const apiKeyPrefix: &str = "sak_v1";
const apiKeySaltVersionPrefix: &str = "v2.";

type HmacSha256 = Hmac<Sha256>;

/// 保存密码摘要和独立盐；摘要不包含可用于直接认证的明文材料。
pub struct PasswordDigest {
    pub encodedHash: String,
    pub encodedSalt: String,
}

/// API Key 派生只接收存储层内部材料；领域对象避免参数顺序错误，并把不可逆密码摘要限制在一次调用内。
pub struct ApiKeyDerivation<'a> {
    pub username: &'a str,
    pub passwordHash: &'a str,
    pub credentialRevision: i64,
    pub databaseInstanceId: &'a str,
    pub encodedSalt: &'a str,
    pub keyId: &'a str,
}

/// 生成随机盐并执行 Argon2id；空密码由调用边界拒绝，避免与任意密码模式混淆。
pub fn hashPassword(password: &str) -> Result<PasswordDigest> {
    let mut salt = [0_u8; saltBytes];
    rng().fill_bytes(&mut salt);
    let mut hash = [0_u8; hashBytes];
    Argon2::default()
        .hash_password_into(password.as_bytes(), &salt, &mut hash)
        .map_err(|_| AccountServiceError::Credential)?;
    Ok(PasswordDigest {
        encodedHash: URL_SAFE_NO_PAD.encode(hash),
        encodedSalt: URL_SAFE_NO_PAD.encode(salt),
    })
}

/// 重新计算固定长度摘要并恒定时间比较；损坏的持久化材料按认证失败处理。
pub fn verifyPassword(password: &str, encodedHash: &str, encodedSalt: &str) -> bool {
    let Ok(expectedHash) = URL_SAFE_NO_PAD.decode(encodedHash) else {
        return false;
    };
    let Ok(salt) = URL_SAFE_NO_PAD.decode(encodedSalt) else {
        return false;
    };
    let mut actualHash = [0_u8; hashBytes];
    if Argon2::default()
        .hash_password_into(password.as_bytes(), &salt, &mut actualHash)
        .is_err()
    {
        return false;
    }
    bool::from(expectedHash.as_slice().ct_eq(actualHash.as_slice()))
}

/// 生成 API Key 派生参数；Key ID 可公开展示，盐只能保存在账号数据库中。
pub fn newApiKeyMaterial() -> (String, String) {
    let mut salt = [0_u8; saltBytes];
    let mut keyId = [0_u8; 6];
    rng().fill_bytes(&mut salt);
    rng().fill_bytes(&mut keyId);
    (
        format!("{apiKeySaltVersionPrefix}{}", URL_SAFE_NO_PAD.encode(salt)),
        URL_SAFE_NO_PAD.encode(keyId),
    )
}

/// 判断持久化盐是否属于当前“密码摘要派生”版本；无标记值只可能来自旧数据库。
pub fn apiKeyMaterialIsCurrent(encodedSalt: &str) -> bool {
    encodedSalt.starts_with(apiKeySaltVersionPrefix)
}

/// 给已验证可解码的旧盐附加版本标记；标记不参与密码学输入，因此兼容迁移不会改变随机盐本身。
pub fn upgradeApiKeySalt(encodedSalt: &str) -> String {
    format!("{apiKeySaltVersionPrefix}{encodedSalt}")
}

/// 从不可逆密码摘要确定性派生 API Key；账号服务可在不保存或重收明文密码的前提下恢复当前 Key。
///
/// `passwordHash` 已由 Argon2id 和独立盐生成，只能停留在数据库边界。这里再使用 API Key 专用盐做
/// 二次 Argon2id，避免密码认证摘要被直接当作 Bearer 凭据；任何持久化材料损坏均返回凭据错误。
pub fn deriveApiKey(derivation: ApiKeyDerivation<'_>) -> Result<String> {
    let encodedSalt = derivation
        .encodedSalt
        .strip_prefix(apiKeySaltVersionPrefix)
        .unwrap_or(derivation.encodedSalt);
    let salt = URL_SAFE_NO_PAD
        .decode(encodedSalt)
        .map_err(|_| AccountServiceError::Credential)?;
    let mut credentialMaterial = [0_u8; hashBytes];
    Argon2::default()
        .hash_password_into(
            derivation.passwordHash.as_bytes(),
            &salt,
            &mut credentialMaterial,
        )
        .map_err(|_| AccountServiceError::Credential)?;
    let mut mac = HmacSha256::new_from_slice(&credentialMaterial)
        .map_err(|_| AccountServiceError::Credential)?;
    mac.update(derivation.username.as_bytes());
    mac.update(&derivation.credentialRevision.to_be_bytes());
    mac.update(derivation.databaseInstanceId.as_bytes());
    let secret = URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes());
    Ok(format!("{apiKeyPrefix}_{}_{secret}", derivation.keyId))
}

/// 只保存完整 API Key 的 SHA-256 摘要，外部 Bearer 校验不需要管理密码参与。
pub fn hashApiKey(apiKey: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(apiKey.as_bytes()))
}

/// 使用恒定时间比较 Bearer Key 摘要，长度或编码损坏统一返回失败。
pub fn verifyApiKey(apiKey: &str, expectedHash: &str) -> bool {
    let actual = Sha256::digest(apiKey.as_bytes());
    let Ok(expected) = URL_SAFE_NO_PAD.decode(expectedHash) else {
        return false;
    };
    bool::from(expected.as_slice().ct_eq(actual.as_slice()))
}
