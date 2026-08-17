# 图标与视觉资源

## 控件图标

Web 控件统一使用 [`lucide-react`](https://lucide.dev/) 提供的线性 SVG 图标。项目通过
`package.json` 和 `pnpm-lock.yaml` 锁定依赖版本，不复制第三方图标源码，也不混用另一套
图标规范。Lucide 使用 ISC 许可证，`Web/public/lucideLicense.txt` 会随 Web 生产产物和
Desktop 安装包一并发布。

## 应用图标

`Frontend/Desktop/src-tauri/icons/` 中的蓝底环形标记是项目自有资源，只用于
桌面窗口与安装包。位图文件按二进制跟踪，构建脚本不得重新压缩或改写源文件。

## 字体

界面不打包第三方字体文件。CSS 依次使用系统提供的 `SF Pro Text`、
`Segoe UI Variable`、`Segoe UI` 与 `Microsoft YaHei UI`，缺失时回落到系统
无衬线字体族。
