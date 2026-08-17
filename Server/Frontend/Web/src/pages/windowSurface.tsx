import type { ReactNode } from "react";

/**
 * 提供独立业务窗口共享的固定视口和正文网格。
 *
 * 运行上下文：设置、工具和插件窗口均在各自 React 路由中复用该外壳，具体页面只负责业务内容。
 * 参数：children 是独立窗口唯一的业务正文。
 * 失败语义：该纯布局组件不执行异步操作；子页面错误由各自状态边界呈现。
 */
export function WindowSurface({ children }: { children: ReactNode }) {
  return (
    <div className="windowSurface independentWindowSurface">
      <div className="windowSurfaceContent independentWindowContent">
        {children}
      </div>
    </div>
  );
}
