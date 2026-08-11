# 39 Agent 系统设计

> **状态：设计预留 · 延后实现**  
> 待 Charles 对等主路径（路线图 **M1–M6**）及基础流水线稳定后再启动。  
> 采集 Agent 依赖事务汇入模型；分析 Agent 依赖检查器与规则对话框草稿能力。  
> 当前阶段 **不排期、不实现**。插件与模块平台已独立进入重构，不再与 Agent 绑定排期。
>  
> **与 MCP 的关系（已定）：** 日后做 Agent 时，**操作 Sprak Capture 一律优先走 MCP**（[40](40-mcpSystem.md)），
> 操作手册使用 **`Server/Skill`**（[41](41-skillSystem.md)）。因此 MCP/Skill 必须在主路径阶段同步建好，  
> 避免 Agent 再实现第二套控制客户端。

## 1. 命名与范围

「Agent」在网络工具里容易混淆。Sprak Capture **Agent 系统** 拆成两个正交子系统：

| 子系统 | 中文名 | 一句话 |
|---|---|---|
| **Capture Agent** | 采集 Agent | 跑在远端或旁路的轻量节点，把流量/事务送进本机工作台 |
| **Analysis Agent** | 分析 Agent | 在工作台侧对事务做自动分析、建议规则、生成报告；可含 LLM 工具调用 |

二者都 **不是** HTTP `User-Agent` 头。文档与 UI 文案使用全称或「采集 Agent / 分析 Agent」。

完整插件与模块平台（[38](38-pluginSystem.md)）扩展 **本机 host 能力**；Agent 扩展 **部署拓扑** 与 **智能闭环**。
**Analysis Agent / 自动化编排** 调用改包、录制、导出等能力时 → **MCP tools**；Skill 路径 → `Server/Skill/`。

## 2. 目标

### 2.1 采集 Agent

2. 将事务以受控协议汇入中心 `proxyService` 的 `RecordingSession`。  
3. 支持只读汇入 vs 经中心代理转发（模式可选）。  
4. 鉴权、限流、可撤销令牌；默认不暴露公网裸监听。

### 2.2 分析 Agent

1. 对选中事务/会话自动摘要、异常检测、建议 Location 与 Rewrite。  
2. 可选接入 LLM（用户自备端点与密钥，密钥不进 snapshot）。  
3. 以 **建议** 为主：写入规则前需用户确认（或明确的自动应用策略）。  
4. 与插件命令、导出、重复测试形成闭环。

## 3. 非目标

- 不做商业 APM SaaS 多租户后端。  
- 不做无用户同意的隐蔽流量外传。  
- 第一期不做内核级强装驱动抓包（可后续研究）。  
- 分析 Agent 不替代检查器人工研判；默认不自动对生产主机启用危险 mutate。  
- 不在 Agent 通道上传输插件任意机器码（采集协议只传事务/控制消息）。

## 4. 业界对照（摘要）

| 模式 | 做法 | Sprak Capture 映射 |
|---|---|---|
| 远程设备代理 | 手机改代理 + 装 CA 指向主机 | 已有方向 [33](33-mobileDeviceCapture.md)；Agent 是补充不是替换 |
| 旁路/镜像 | span、容器 sidecar 出流量 | Capture Agent `mirror`/`export` 模式 |
| CI 脚本 + mitmproxy | 管道里起代理 | Capture Agent headless + 中心汇聚 |
| LLM 助研 | 复制 HAR 问 ChatGPT | Analysis Agent 本地工具调用 + 可选模型 |

调研详见 [37](37-pluginAndAgentResearch.md)。

## 5. 总架构

```text
┌──────────────────┐     mTLS/Token      ┌─────────────────────────────┐
│ Capture Agent(s) │ ──────────────────► │ proxyService                │
│  · proxy 模式    │   Agent 接入协议     │  Agent Gateway              │
│  · export 模式   │                      │  ├─ 鉴权 / 限流 / 注册表     │
└──────────────────┘                      │  ├─ 事务汇入 capture-core   │
                                          │  ├─ 可选：要求走中心出站     │
┌──────────────────┐                      │  └─ Analysis Agent Runtime  │
│ Analysis Agent   │ ◄── 读事务/下命令 ──│       ├─ 规则引擎            │
│ (进程内或旁路)   │ ──► 建议/补丁 ─────►│       └─ LLM 工具调用(可选) │
└──────────────────┘                      └─────────────┬───────────────┘
                                                        │ 既有 Control API
                                                        ▼
                                                   Web / Desktop UI
```

## 6. 采集 Agent（Capture Agent）

### 6.1 部署形态

| 形态 | 说明 |
|---|---|
| **独立二进制** | `capture-agent`，Linux/macOS/Windows；CI 友好 |
| **容器 sidecar** | 与业务容器共享 network namespace 或显式代理环境变量 |
| **开发机轻量** | 同事开 Agent 指到主开发者工作台（需 token） |

主工作台仍是 Desktop/`proxyService`；Agent **无** 完整 UI（可有极简状态 HTTP）。

### 6.2 工作模式

| 模式 | 行为 |
|---|---|
| `export` | Agent 本地可终止 TLS 或仅 HTTP；将事务 **复制** 到中心；出站仍走 Agent 本机网络 |
| `proxy` | 客户端以 Agent 为代理；Agent 将请求 **经中心转发** 或本地出站后再上报元数据（可配） |
| `metadata-only` | 只报 URL/状态/耗时，不上报正文（隐私） |

默认推荐：`export` + 可选正文；生产默认 `metadata-only` 或采样。

### 6.3 接入协议（逻辑）

传输：优先 **WebSocket** 或 **HTTP/2 双向流** 到中心 `Agent Gateway`（与 UI 控制面端口分离或同端口不同 path）。

```text
中心默认：127.0.0.1:17890 控制面（UI）
Agent 建议：127.0.0.1:17891 或 path /api/v1/agent/* 且强制 Token
若绑定非回环：必须 TLS + Token，ACL 限制源 IP
```

**消息类型（草案）：**

| 方向 | 类型 | 内容 |
|---|---|---|
| C→S | `hello` | agentId、版本、能力、主机名 |
| S→C | `welcome` | 会话 id、下发配置（采样率、是否传正文） |
| C→S | `transaction` | 与 Transaction 元数据兼容的子集 + 可选 body 块 |
| C→S | `transactionBody` | 分块正文 |
| C→S | `heartbeat` | 存活 |
| S→C | `configUpdate` | 热更新采样/过滤 Location |
| S→C | `revoke` | 令牌作废，Agent 退出上报 |

兼容控制契约精神：字段 **camelCase**；大正文分块；中心汇入后分配正式 `transactionId`，并标记：

```typescript
interface TransactionAgentMark {
  source: "local" | "agent";
  agentId?: string;
  agentHostName?: string;
}
```

结构树可按 **Agent / 本机** 分区或过滤器展示。

### 6.4 鉴权与安全

| 机制 | 要求 |
|---|---|
| 接入 Token | 中心 UI 生成一次性/可轮转 token；只显示一次 |
| 权限 | 仅 `ingest`；不能调 start/stop 服务、不能读其他 Agent 数据 |
| 限流 | 每 Agent TPS / 日字节上限 |
| 过滤 | 中心下发 ignore Locations，Agent 侧预过滤省带宽 |
| 密钥 | Token 存 Agent 本地配置，权限收紧；不进 git |

### 6.5 与「手机改代理」的关系

| 场景 | 用哪种 |
|---|---|
| K8s 里服务、无 UI 机器、CI | **采集 Agent** |
| 要中心统一改包再出站 | Agent `proxy` 模式或流量仍直连中心 HTTP 代理 |

### 6.6 UI（对话框）

| 入口 | 内容 |
|---|---|
| **代理 → 采集 Agent…** | L3：启用 Gateway、生成 token、在线 Agent 列表、限流 |
| 连接会话过滤器 | 「来源：本机 / Agent:xxx」 |
| 概览卡片 | 在线 Agent 数、上报 QPS |

遵守 [35](35-uiShellAndNavigation.md)：配置进对话框，列表在 L2 过滤。

### 6.7 采集 Agent 分期

| 阶段 | 交付 |
|---|---|
| **CA0** | Gateway + token + `export` 元数据汇入 |
| **CA1** | 正文分块 + Location 过滤下发 |
| **CA2** | `proxy` 模式与 mTLS |
| **CA3** | 官方容器镜像与 Helm/compose 示例 |

## 7. 分析 Agent（Analysis Agent）

### 7.1 能力分层

| 层级 | 能力 | 依赖 |
|---|---|---|
| **L1 规则分析** | 无 LLM：状态码分布、重复路径、慢请求、敏感头检测 | 本地 |
| **L2 建议生成** | 生成「建议的忽略/屏蔽/Rewrite 草稿」 | 本地启发式 |
| **L3 LLM 分析** | 自然语言问会话、解释 Protobuf/JSON、生成测试描述 | 用户配置模型端点 |
| **L4 自动行动** | 经确认后调用插件命令 / 写规则 / 导出 | 明确策略开关 |

默认开启 L1；L3/L4 默认关。

### 7.2 运行位置

```text
方案 A（推荐默认）：proxyService 内嵌 Analysis Runtime
  - 与 snapshot 同权读取事务
  - 任务异步，不堵数据面

方案 B：独立 analysis 进程
  - 经 Control API 只读拉事务
  - 适合重 LLM 依赖隔离
```

数据面线程 **禁止** 同步等待 LLM。

### 7.3 工具调用（Function Calling）边界

分析 Agent 可调用的 **内部工具**（白名单）：

| 工具 | 作用 | 是否需确认 |
|---|---|---|
| `listTransactions` | 按过滤查元数据 | 否 |
| `getTransaction` | 取头/体（尊重敏感策略） | 否 |
| `proposeIgnoreLocation` | 生成忽略草稿 | 是（应用时） |
| `proposeRewriteRule` | 生成 Rewrite 草稿 | 是 |
| `proposeMapLocal` | 建议本地文件映射 | 是 |
| `runRepeat` | 触发重复 | 是 |
| `exportHar` | 导出 | 是 |

**禁止** 默认开放：任意 shell、任意 URL 外传全文、关闭 ACL、导出根 CA 私钥。

### 7.4 LLM 配置（用户数据目录）

```json
{
  "analysisAgent": {
    "enabled": false,
    "mode": "localOnly",
    "llm": {
      "enabled": false,
      "baseUrl": "",
      "model": "",
      "apiKeyEnv": "CAPTURE_LLM_API_KEY"
    },
    "autoApply": {
      "enabled": false,
      "allowedActions": []
    }
  }
}
```

- API Key **只** 来自环境变量或 OS 凭据库，**永不** 进入 GET snapshot。  
- 外发正文前：对话框确认范围（选中 / 过滤结果）与是否脱敏。

### 7.5 UI

| 入口 | 行为 |
|---|---|
| **工具 → 分析 Agent…** | L3：启用 L1/L3、模型配置（无回显 key）、自动应用策略 |
| 检查器 / 右键 **用分析 Agent 解释…** | 打开 L3 对话或结果面板（浮层，不替换 L2） |
| 结果 | 「建议卡片」→ **预览规则** → **应用到 Rewrite 对话框草稿** |
| 底栏 | 可选显示「分析任务进行中」 |

交互符合 [36](36-componentSpec.md)：建议流是对话框/侧面板，不是夺权全页。

### 7.6 与 MCP / Skill / 插件的关系

| 依赖 | 约定 |
|---|---|
| **MCP** | Analysis Agent 的默认工具面；启停、录制、事务、Map/Rewrite/断点等 **不直连拼 HTTP 散装**，走 MCP（或与 MCP 同构的内部 client） |
| **Skill** | 流程与提示来自 `Server/Skill`，与外部 AI 宿主共用同一套手册 |
| **插件与模块平台（38）** | 插件注册的命令通过统一命令贡献桥接为 MCP tool 后，Agent 才能稳定调用；插件不直连 Agent |

- 插件 **不** 自动获得 LLM 密钥访问。

### 7.7 分析 Agent 分期

| 阶段 | 交付 |
|---|---|
| **AA0** | L1 本地启发式报告（无 LLM）+ 结果对话框 |
| **AA1** | 建议 → 预填 Rewrite/Ignore 对话框草稿 |
| **AA2** | LLM 接入（用户端点）+ 工具白名单 |
| **AA3** | 会话级对话、批量回归断言生成 |

## 8. 控制面 API 增量（草案）

### 采集

```http
GET  /api/v1/agents/capture
POST /api/v1/agents/capture/tokens
DELETE /api/v1/agents/capture/tokens/{id}
POST /api/v1/agents/capture/gateway/start|stop
```

### 分析

```http
GET  /api/v1/agents/analysis/settings
PUT  /api/v1/agents/analysis/settings
POST /api/v1/agents/analysis/jobs
GET  /api/v1/agents/analysis/jobs/{id}
POST /api/v1/agents/analysis/jobs/{id}/apply  // 应用建议，需确认 token
```

事件：`captureAgents`、`analysisJob`。

## 9. 安全总则

1. 采集与 UI 控制面 **权限分离**（不同 token 范围）。  
2. 分析外发最小化；默认本地。  
3. 自动应用 mutate 类建议必须双开关（全局 + 动作级）。  
4. 审计：Agent 源写入 transaction 标记；分析应用写入 appliedTools / 配置 revision。  
5. 威胁模型写入帮助文档：恶意 Agent 令牌泄露 ≈ 向你灌垃圾事务，需限流与撤销。

## 10. 验收标准

### 采集 Agent

- [ ] 无 token 无法汇入  
- [ ] 汇入事务在 L2 可见且可过滤来源  
- [ ] 撤销 token 后旧连接断开  
- [ ] 正文超限被拒绝且中文错误  

### 分析 Agent

- [ ] L1 报告不产生外网请求  
- [ ] LLM 关闭时 L3 入口禁用  
- [ ] 建议应用必经用户确认（默认）  
- [ ] snapshot 永不包含 apiKey  

## 11. 交叉链接

- 调研 [37](37-pluginAndAgentResearch.md)  
- 插件 [38](38-pluginSystem.md)  
- 移动端抓包 [33](33-mobileDeviceCapture.md)  
- 事务模型 [05](05-transactionModel.md)  
- 平台架构 [02](02-platformArchitecture.md)
