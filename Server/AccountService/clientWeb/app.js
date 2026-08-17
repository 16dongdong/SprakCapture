"use strict";

const form = document.getElementById("downloadForm");
const usernameInput = document.getElementById("username");
const passwordInput = document.getElementById("password");
const applicationIdInput = document.getElementById("applicationId");
const applicationNameInput = document.getElementById("applicationName");
const applicationIconInput = document.getElementById("applicationIcon");
const downloadButton = document.getElementById("downloadButton");
const downloadStatus = document.getElementById("downloadStatus");
const maximumIconBytes = 1024 * 1024;

/** 从响应头读取安全文件名；缺失或包含路径字符时使用固定 APK 名称。 */
function responseFileName(response) {
  const disposition = response.headers.get("content-disposition") || "";
  const match = disposition.match(/filename="?([^";]+)"?/i);
  const candidate = match?.[1]?.trim() || "proxy-client.apk";
  return /^[A-Za-z0-9._-]+\.apk$/i.test(candidate) ? candidate : "proxy-client.apk";
}

/** 解析后端稳定错误体；非 JSON 响应保留 HTTP 状态，禁止把服务诊断或密码回显到页面。 */
async function responseError(response) {
  try {
    const payload = await response.json();
    if (typeof payload.message === "string" && payload.message) return payload.message;
  } catch (parseError) {
    // 上游错误响应未承诺一定为 JSON；只把语法错误映射为公开 HTTP 状态，其它异常继续上抛。
    if (!(parseError instanceof SyntaxError)) throw parseError;
  }
  return `生成安装包失败（HTTP ${response.status}）。`;
}

/** 读取可选图标并提取纯 Base64；文件越界在网络请求前直接拒绝。 */
async function selectedIconBase64() {
  const file = applicationIconInput.files?.[0];
  if (!file) return null;
  if (file.size === 0 || file.size > maximumIconBytes) {
    throw new Error("应用图标必须小于或等于 1 MiB。");
  }
  if (!["image/png", "image/jpeg", "image/webp"].includes(file.type)) {
    throw new Error("应用图标必须是 PNG、JPEG 或 WebP 图片。");
  }
  const encoded = await new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.addEventListener("load", () => resolve(reader.result), { once: true });
    reader.addEventListener("error", () => reject(new Error("读取应用图标失败。")), { once: true });
    reader.readAsDataURL(file);
  });
  if (typeof encoded !== "string" || !encoded.includes(",")) {
    throw new Error("读取应用图标失败。");
  }
  return encoded.slice(encoded.indexOf(",") + 1);
}

/** 仅把全空白可选值折叠为空；非空输入保持原样，由服务端严格拒绝首尾空白而不是静默改写。 */
function optionalText(input) {
  return input.value.trim() ? input.value : null;
}

/** 提交一次性凭据并下载同步生成的 APK；密码在请求结束后立即从 DOM 清除。 */
async function downloadClient(event) {
  event.preventDefault();
  downloadButton.disabled = true;
  downloadStatus.classList.remove("error");
  downloadStatus.textContent = "正在生成安装包，请保持页面打开……";
  try {
    const iconBase64 = await selectedIconBase64();
    const response = await fetch("/api/v1/clientPackages/download", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({
        username: usernameInput.value,
        password: passwordInput.value,
        applicationId: optionalText(applicationIdInput),
        applicationName: optionalText(applicationNameInput),
        iconBase64,
      }),
    });
    if (!response.ok) throw new Error(await responseError(response));
    const contentType = response.headers.get("content-type") || "";
    if (!contentType.toLowerCase().startsWith("application/vnd.android.package-archive")) {
      throw new Error("服务返回的文件类型不是 Android 安装包。");
    }
    const objectUrl = URL.createObjectURL(await response.blob());
    const anchor = document.createElement("a");
    anchor.href = objectUrl;
    anchor.download = responseFileName(response);
    anchor.click();
    window.setTimeout(() => URL.revokeObjectURL(objectUrl), 0);
    downloadStatus.textContent = "安装包已生成并开始下载。";
  } catch (error) {
    downloadStatus.classList.add("error");
    downloadStatus.textContent = error instanceof Error ? error.message : "生成安装包失败。";
  } finally {
    passwordInput.value = "";
    downloadButton.disabled = false;
  }
}

form.addEventListener("submit", downloadClient);
