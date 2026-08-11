# 33 移动设备抓包

## Charles 对照

Charles 引导用户将手机 HTTP 代理指向电脑 IP，并安装/信任 Charles CA，从而抓取 App HTTPS 流量。

## 目标

- P1：产品内帮助页（中文）完整描述 Android / iOS 步骤。
- 一键显示本机局域网 IP + HTTP 代理端口 + CA 下载二维码/链接。
- CA 下载仅在用户确认后通过受控 HTTP 提供（可临时绑定局域网或通过已设代理下载）。
- 文档说明证书固定（pinning）与系统限制（iOS 完整信任开关等）。

## 非目标

- 不做 USB 逆向或越狱/root 专用框架。
- 不绕过 SSL pinning。

## 领域模型

```typescript
interface MobileCaptureHelpModel {
  lanAddresses: string[];
  httpProxyPort: number;
  socksPort: number | null;
  caDownloadPath: string; // /api/v1/ssl/ca/export?format=cer
  /** 可选：临时在局域网暴露 CA 下载的开关状态 */
  lanCaDownloadEnabled: boolean;
  qrContent: string; // 代理 pac 或说明 URL
}

interface MobileCaptureConfiguration {
  /** 允许从非回环下载 CA 公钥（仍不暴露私钥） */
  allowLanCaDownload: boolean;
  lanCaDownloadToken: string; // 随机 token 查询参数
}
```

## 行为

### 网络前提

1. 电脑与手机同一局域网；防火墙放行 HTTP 代理端口。
2. HTTP 代理 `listenHost` 需为 `0.0.0.0` 或局域网 IP（设置中明确安全提示）。
3. ACL 需允许手机 IP（[14](14-accessControlAndUpstream.md)）。

### Android

1. WLAN 高级 → 代理手动 → 主机/端口。
2. 浏览器打开 CA 下载链接 → 安装证书。
3. Android 7+ 用户证书对 App 默认不信任：说明仅对浏览器或用户可改 network security config 的调试包有效。

### iOS

1. WLAN 代理手动。
2. 安装描述文件/证书 → 设置 → 通用 → 关于 → 证书信任设置 → 启用完全信任。

### CA 下载安全

- 默认仅回环导出；`allowLanCaDownload` 开启后路径带 token，仅公钥。
- 关闭开关立即失效 token。

## 控制 API

```http
GET /api/v1/help/mobileCapture
PUT /api/v1/help/mobileCapture
GET /api/v1/ssl/ca/export?format=cer&token=
```

## UI 要点

- 「移动设备」向导页：步骤清单、复制代理、显示二维码、导出 CA。
- 安全警示横幅：监听 0.0.0.0 的风险。
- 链接 SSL 主机表配置。

## UI 操作指南

### 界面位置

L3 向导：**帮助 → 移动设备抓包…**（步骤 + 二维码 + 证书链接）。  
监听地址仍在 **代理 → 代理设置…**。

### 如何打开

帮助菜单向导对话框（可多步，仍属 L3 向导型）。

### 操作步骤

1. 代理设置中绑定局域网并配 ACL。
2. 打开移动设备向导 → 按步骤设手机代理、装证书。
3. SSL 代理设置中加主机。
4. 回 L2 看手机流量。

### 预期行为

向导不替换连接会话；可边开向导边在后台看是否出事务（向导关闭前 L2 被遮罩——可接受；或向导支持「置后」P2）。


## 验收标准

- [ ] 帮助页含 Android/iOS 关键步骤与 pinning 限制说明。
- [ ] 显示至少一个非回环 IPv4（有网卡时）与端口。
- [ ] LAN CA 下载默认关；开启后仅公钥可下载。
- [ ] 手机经代理访问 HTTP 站点事务可见（集成手工）。

## 交叉链接

- [14](14-accessControlAndUpstream.md)
