# Monocypher 固定来源

- 官方包：`https://monocypher.org/download/monocypher-4.0.3.tar.gz`
- 版本：`4.0.3`
- SHA-512、双 ABI 归档哈希：`../sourceLock.json`
- 许可证：BSD-2-Clause，完整文本见 `LICENCE.md`

仓库保留稳定 API 头和 Android 静态归档；官方 2988 行单文件源码由 `../rebuildPrebuilt.ps1` 在 D 盘临时目录下载、验签、编译并清理。业务层只使用官方 XChaCha20-Poly1305 AEAD 与 `crypto_wipe`，没有自定义密码算法。
