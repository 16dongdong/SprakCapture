# 10 SSL MITM 与证书

## Charles 对照

Charles SSL Proxying：维护启用列表（Location）；对匹配的 CONNECT 目标生成叶证书，用动态 CA 签名，客户端需信任 Charles CA。提供证书导出与移动端安装说明。

## 目标

- 安装目录 `data/certs` 维护根 CA（首次启动自动生成或向导生成）。
- SSL 主机表（Location 列表）控制解密范围；默认空=不解密。
- 叶证书按主机名签发并缓存；支持 SANs。
- Windows 导出 `.cer`/`.pem`；引导安装到当前用户「受信任的根证书颁发机构」。
- 解密后 HTTP/1.0、HTTP/1.1 与 HTTP/2 解析进入工具流水线与 Transaction。
- 可按 HTTPS Location 导入上游客户端身份，支持 PKCS#12/PFX、PEM 与 DER。

## 非目标

- 不破解固定证书 pinning（可文档说明 bypass 限制）。
- 第一版不做透明 MITM 与 UDP QUIC 解密。
- 不上传私钥到任何网络服务。

## 领域模型

```typescript
interface SslMitmConfiguration {
  enabled: boolean;
  includeLocations: Location[];
  excludeLocations: Location[]; // 排除优先于 include
  maxCachedCertificates: number;
  useClientSni: boolean;
}

interface CertificateAuthorityInfo {
  installed: boolean; // 尽力检测当前用户是否已信任
  subject: string;
  validFromMilliseconds: number;
  validToMilliseconds: number;
  fingerprintSha256: string;
  pemPath: string;
}

interface SslPublicState {
  enabled: boolean;
  includeLocations: Location[];
  excludeLocations: Location[];
  ca: CertificateAuthorityInfo | null;
  cachedLeafCount: number;
  handshakeSuccessTotal: number;
  handshakeFailureTotal: number;
  clientCertificates: ClientCertificateInfo[];
  supportedHttpVersions: string[];
}
```

### 证书文件布局（安装目录 `data`）

```text
{userData}/certs/
  rootCA.pem
  rootCA.key          # ACL 仅当前用户
  leaves/             # 可选缓存
  clientCertificates/ # 规范化证书链与私钥，继承用户证书目录 ACL
  clientCertificates.json
```

## 行为

1. CONNECT 成功后查 SNI/目标 host：exclude 命中 → 透传；include 命中且 enabled → MITM。
2. MITM：与客户端 TLS 握手（叶证），与服务器 TLS 握手（系统根或 webpki 根）。
3. 握手失败：事务标记 failed，`errorMessage` 中文原因；默认不回退半解密。
4. 双向字节流使用自动 HTTP/1.x/2 连接解析器；TLS ALPN 优先 `h2` 并回退 `http/1.1`。
5. 生成 CA：RSA 2048 或 ECDSA P-256；有效期 10 年；叶证 1 年。
6. 上游请求按 Location 原子选择客户端身份；不同身份使用隔离连接池，避免跨主机复用证书。

## 控制 API

| 方法 | 路径 | 作用 |
|---|---|---|
| `GET` | `/api/v1/ssl` | SSL 状态 |
| `PUT` | `/api/v1/ssl` | 更新开关与主机表 |
| `POST` | `/api/v1/ssl/ca/generate` | 重新生成 CA（需确认） |
| `GET` | `/api/v1/ssl/ca/export?format=pem\|cer` | 下载公钥证书 |
| `POST` | `/api/v1/ssl/client-certificates` | 导入 PKCS#12/PFX、PEM 或 DER 客户端身份 |
| `PUT` | `/api/v1/ssl/client-certificates/{id}` | 更新名称、启停与 Location |
| `DELETE` | `/api/v1/ssl/client-certificates/{id}` | 删除客户端身份材料 |
| `POST` | `/api/v1/ssl/ca/trustWindows` | 可选：安装到当前用户（Desktop） |

## UI 要点

- SSL 设置页：总开关、include/exclude 表、CA 指纹、导出按钮、Windows 信任按钮。
- 帮助页嵌入移动端步骤链接 → [33](33-mobileDeviceCapture.md)。
- 结构报文标志显示「已解密」。

## UI 操作指南

### 界面位置

| 配置 | L3：**代理 → SSL 代理设置…** |
| 安装证书 | 同对话框按钮；或 **帮助 → 安装根证书…** 向导 |
| 单主机开关 | L2 结构树右键「启用 SSL 代理」（即时） |

### 如何打开

**代理 → SSL 代理设置…**

### 操作步骤

1. 打开 SSL 代理设置对话框。
2. 勾选启用 SSL 代理；主机表添加 `*.example.com` 或 `*`（`*` 需 L4 确认）。
3. **导出/安装根证书**（桌面向导可再叠 L4/向导步）。
4. 确定后访问 HTTPS；L2 见明文事务。

### 预期行为

证书私钥永不在对话框展示；再生根证书走 L4 强确认。


## 验收标准

- [ ] 空 include 时 HTTPS 仅隧道元数据或透传，无明文 URL path。
- [ ] 添加 `example.com` 后可在 Contents 看 JSON。
- [ ] 导出的 PEM 可被系统/浏览器信任流程使用。
- [ ] 私钥文件不出现在仓库与日志。
- [ ] 重新生成 CA 后旧叶缓存清空。

## 交叉链接

- [09](09-httpHttpsProxy.md) · [03](03-locationMatching.md) · [33](33-mobileDeviceCapture.md)
