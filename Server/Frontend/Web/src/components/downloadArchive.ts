/**
 * 下载浏览器内生成的归档文件；对象 URL 只存活到同步点击结束，避免长时间占用正文内存。
 *
 * 运行上下文：导出对话框与事务右键菜单共用该入口。
 * 参数：archive 为服务端返回的二进制归档，fileName 为用户可见文件名。
 * 失败语义：浏览器拒绝创建或触发下载时异常直接向调用方传播，由操作入口显示错误。
 */
export function downloadArchive(archive: Blob, fileName: string): void {
  const objectUrl = URL.createObjectURL(archive);
  const anchor = document.createElement("a");
  anchor.href = objectUrl;
  anchor.download = fileName;
  document.body.append(anchor);
  try {
    anchor.click();
  } finally {
    anchor.remove();
    URL.revokeObjectURL(objectUrl);
  }
}
