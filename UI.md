# Share Frontend UI 实施计划

## 目标

为新引入的 Token Share / Tunnel 能力补齐完整前端，不以“最小可用”为目标，而是一次性规划为可上线、可维护、可扩展的完整实现。范围包括：

- Share 管理中心
- Share 创建/查看/控制
- Tunnel 全局配置
- 连接信息与调用示例
- 状态展示、轮询、反馈、异常处理
- 国际化
- 测试与验收

本计划只描述实施方案，不包含代码实现。

## 现状判断

当前仓库已有：

- 后端 Tauri commands 与 Rust 服务层
- 前端 API 封装：[src/lib/api/share.ts](./src/lib/api/share.ts)
- 单页多视图壳层：[src/App.tsx](./src/App.tsx)
- Settings 内部 tab 架构：[src/components/settings/SettingsPage.tsx](./src/components/settings/SettingsPage.tsx)
- React Query 查询模式：[src/lib/query/queries.ts](./src/lib/query/queries.ts)

当前缺失：

- `share` 的 query/mutation 层
- `share` 组件目录
- Share 入口与视图
- Tunnel 配置 UI
- i18n 文案
- 任何 Share 前端交互

## 产品定位

Share 功能应被视为“系统能力 + 业务面板”的组合，不是 provider 编辑页的一部分。

最终交互定位：

- 主导航中存在一级入口：`Share`
- 该页面独立于 `Settings`
- 页面内部自带 Tunnel 配置、Share 列表、创建入口、详情与连接信息

原因：

- Share 有独立生命周期、状态、配额、隧道、连接信息
- 仅放进 Settings 会弱化其业务属性
- 后续很可能扩展统计、模板、审计、分享历史，独立页面更合理

## 顶层信息架构

### App 顶层视图

在 [src/App.tsx](./src/App.tsx) 中新增一级 `View`：

- `shares`

需要同步修改：

- `View` 联合类型
- `VALID_VIEWS`
- header 标题映射
- 左侧或顶部导航按钮
- `setCurrentView(...)` 的跳转逻辑

### 页面结构

Share 页面采用三段式布局：

1. 页面头部
2. Tunnel 配置区
3. Share 管理区

建议结构：

- `SharePage`
  - `ShareHero`
  - `TunnelConfigPanel`
  - `ShareToolbar`
  - `ShareStatsBar`
  - `ShareList`
  - `CreateShareDialog`
  - `ShareDetailDrawer`
  - `ShareConnectDialog`

## 页面交互模型

### 1. Share 页面头部

职责：

- 说明 Share 功能用途
- 提供“新建分享”主操作
- 提示 Tunnel 是否已配置
- 显示全局统计概览

展示内容：

- 标题：`Share`
- 副标题：说明“通过带配额和到期控制的 token 对外共享代理能力”
- 主按钮：`Create Share`
- 次级状态：
  - Tunnel configured / not configured
  - Active shares 数量
  - Running tunnels 数量

### 2. Tunnel 配置区

功能定位：

- 系统级配置，只配置一次
- 所有 share 共用

字段：

- `serverUrl`
- `sshUrl`
- `tunnelUrl`
- `secretKey`
- `useLocalhost`

交互：

- 编辑
- 保存
- 重置为当前已保存值
- 敏感字段显示/隐藏

增强项：

- “配置状态”摘要
- “配置说明”折叠卡片
- “端口映射说明 / 本地地址说明”

注意：

- 第一版不必须做联机探测，但 UI 要为未来 `Test Connection` 预留按钮位
- 未配置 Tunnel 时：
  - 可以创建 share
  - 但不能启动 tunnel
  - 相关按钮给出明确阻断提示

### 3. Share Toolbar

职责：

- 新建
- 搜索
- 筛选
- 排序

字段：

- 搜索：按 `name / subdomain / providerId`
- 筛选：
  - `status`
  - `appType`
  - `tunnel state`
- 排序：
  - createdAt desc
  - expiresAt asc
  - tokensUsed desc
  - name asc

### 4. Share 列表

展示方式：

- 卡片列表，不建议简单 table
- 每卡片分为摘要区、状态区、操作区、扩展信息区

每个 ShareCard 展示：

- 名称
- appType badge
- status badge
- provider 说明
- tokenLimit / tokensUsed
- requestsCount
- expiresAt
- tunnelUrl
- subdomain
- lastUsedAt
- tunnel health

视觉建议：

- `active` 用绿色
- `paused` 用黄色
- `expired` 用灰色
- `exhausted` 用红色

卡片主操作：

- `Connect Info`
- `Start Tunnel`
- `Stop Tunnel`
- `Pause`
- `Resume`
- `Delete`
- `Details`

卡片次级信息：

- 创建时间
- provider 来源说明
  - 绑定 provider
  - 当前 provider
- 配额进度条

### 5. Share 详情抽屉

建议使用右侧 Drawer / Full-height panel，而不是 modal。

内容：

- 基础信息
  - name
  - id
  - shareToken 掩码
  - appType
  - providerId
  - status
- 配额信息
  - tokenLimit
  - tokensUsed
  - requestsCount
  - usage ratio
- 生命周期
  - createdAt
  - expiresAt
  - lastUsedAt
- Tunnel
  - subdomain
  - tunnelUrl
  - tunnel health
  - remotePort
- 配置快照
  - `settingsConfig` 只读 JSON 查看器

抽屉操作：

- `Open Connect Info`
- `Start/Stop Tunnel`
- `Pause/Resume`
- `Delete`

### 6. Connect Info 弹窗

内容：

- `tunnelUrl`
- `shareToken`
- `subdomain`
- `curl` 示例
- 使用说明

需要按钮：

- `Copy Token`
- `Copy URL`
- `Copy Curl`

示例：

```bash
curl -H "X-Share-Token: <token>" "<tunnelUrl>"
```

扩展展示：

- appType 限定提示
- 配额和过期提醒

### 7. Create Share Dialog

创建表单不能只做“字段直出”，需要尽量智能预填。

表单区块：

1. 基础信息
2. Provider 来源
3. 认证与配置
4. 配额与有效期
5. 高级选项

字段设计：

- `name`
- `appType`
- `providerId`
- `apiKey`
- `settingsConfig`
- `tokenLimit`
- `expiresInSecs`

建议交互：

- `appType` 下拉只显示当前后端代理实际支持的应用
  - `claude`
  - `codex`
  - `gemini`
- `providerId` 支持两种模式
  - 绑定指定 provider
  - 跟随当前 provider
- `apiKey`
  - 从 provider 自动提取并预填
  - 默认掩码显示，支持 reveal
- `settingsConfig`
  - 默认从 provider 当前 `settingsConfig` 预填
  - 使用现有 `JsonEditor`
- `tokenLimit`
  - number input
  - 提供常用预设 chips
- `expiresInSecs`
  - 提供预设：
    - 1 hour
    - 6 hours
    - 1 day
    - 7 days
    - 30 days
    - custom

创建前校验：

- `name` 必填
- `appType` 必填
- `apiKey` 必填
- `tokenLimit > 0`
- `expiresInSecs > 0`
- `settingsConfig` 必须可解析为 JSON

创建成功后：

- 关闭对话框
- toast success
- 刷新列表
- 自动打开该 share 的 `Connect Info` 或 `Detail Drawer`

## 领域行为设计

### appType 限制

前端必须只暴露实际可用的 share appType。

当前以现有后端代理入口为准：

- `claude`
- `codex`
- `gemini`

不要在 Share UI 中允许：

- `opencode`
- `openclaw`

除非后端后续真正补齐代理入口。

### provider 预填策略

当用户选择 `appType + providerId` 时：

- 从 `useProvidersQuery(appType)` 获取 provider
- 自动尝试提取 API key
- 自动将 `settingsConfig` 序列化填入编辑器
- 默认 name 使用：
  - `${provider.name} Share`

需要新增工具函数：

- `extractApiKeyFromProvider(provider, appType)`
- `serializeSettingsConfig(provider)`
- `buildDefaultShareName(provider)`

建议新文件：

- `src/utils/shareUtils.ts`

### 状态同步策略

列表查询：

- `listShares()` 为主数据源

隧道状态：

- 仅对有隧道或近期操作过的 share 进行单独轮询
- 避免全量高频调用 `getTunnelStatus`

推荐策略：

- 列表页展示时，卡片优先使用 `share.tunnelUrl / subdomain`
- 对“运行中”状态卡片每 8 秒轮询一次 `getTunnelStatus(share.id)`
- 页面失焦时可继续轮询，但降频到 15 秒

### 操作可用性规则

- `Start Tunnel`
  - 要求 Tunnel 配置完整
  - share 状态必须是 `active`
- `Stop Tunnel`
  - 仅对已启动 tunnel 的 share 可用
- `Pause`
  - 仅 `active`
- `Resume`
  - 仅 `paused`
- `Delete`
  - 所有状态均可
- `Connect Info`
  - 只要存在 share 即可查看

### 敏感信息策略

敏感字段：

- `apiKey`
- `shareToken`
- `secretKey`
- `settingsConfig` 中可能包含认证信息

UI 要求：

- 列表页不明文显示 token / apiKey
- 详情页 token 默认掩码，点 reveal 后展示
- `settingsConfig` 默认只读展示，但带敏感提示
- `secretKey` 在 tunnel 配置里默认密码输入框

## 技术架构

### 目录结构

新增：

- `src/components/share/SharePage.tsx`
- `src/components/share/ShareHero.tsx`
- `src/components/share/ShareToolbar.tsx`
- `src/components/share/ShareStatsBar.tsx`
- `src/components/share/ShareList.tsx`
- `src/components/share/ShareCard.tsx`
- `src/components/share/ShareStatusBadge.tsx`
- `src/components/share/ShareDetailDrawer.tsx`
- `src/components/share/ShareConnectDialog.tsx`
- `src/components/share/CreateShareDialog.tsx`
- `src/components/share/TunnelConfigPanel.tsx`
- `src/components/share/index.ts`

新增：

- `src/lib/query/share.ts`

新增：

- `src/utils/shareUtils.ts`

可选新增：

- `src/lib/schemas/share.ts`

### API 层调整

已有 [src/lib/api/share.ts](./src/lib/api/share.ts)，但还未纳入统一出口。

需要：

- 在 [src/lib/api/index.ts](./src/lib/api/index.ts) 导出 `shareApi`
- 统一 `share.ts` 的导出形式，建议改成对象导出

推荐结构：

- `export const shareApi = { ... }`
- 保留类型导出

这样可以和现有 `providersApi / settingsApi / proxyApi` 风格保持一致。

### Query 层设计

在 `src/lib/query/share.ts` 中定义：

- `shareKeys`
  - `all`
  - `lists`
  - `list()`
  - `detail(id)`
  - `tunnelStatus(id)`

查询 hooks：

- `useSharesQuery()`
- `useShareDetailQuery(shareId)`
- `useShareTunnelStatusQuery(shareId, enabled)`

mutation hooks：

- `useCreateShareMutation()`
- `useDeleteShareMutation()`
- `usePauseShareMutation()`
- `useResumeShareMutation()`
- `useStartShareTunnelMutation()`
- `useStopShareTunnelMutation()`
- `useConfigureTunnelMutation()`
- `useShareConnectInfoQuery(shareId, enabled)`

缓存失效策略：

- 创建/删除：失效 `list`
- 状态切换：失效 `list + detail(id)`
- tunnel 启停：失效 `list + detail(id) + tunnelStatus(id)`
- 配置保存：仅本地状态刷新，不必影响 share 列表

### 本地 UI 状态

`SharePage` 维护：

- `search`
- `statusFilter`
- `appFilter`
- `sortBy`
- `createOpen`
- `detailShareId`
- `connectShareId`
- `editingTunnelConfigDraft`

不要把这些状态抬到 `App.tsx`，除非要做跨页面跳转。

## 组件职责定义

### SharePage

职责：

- 组合整个页面
- 读取 shares 数据
- 管理筛选、排序、弹窗、抽屉
- 注入 mutations

### ShareHero

职责：

- 页面标题
- 说明文字
- 创建按钮
- 配置状态摘要

### TunnelConfigPanel

职责：

- 编辑 TunnelConfig
- 保存配置
- 本地草稿和脏状态提示

建议使用：

- 局部 `useState`
- 保存时调用 `configureTunnel`

### ShareToolbar

职责：

- 搜索
- 筛选
- 排序

### ShareStatsBar

职责：

- 从 share 列表计算统计摘要

指标：

- total shares
- active shares
- running tunnels
- exhausted shares

### ShareList

职责：

- 纯展示容器
- 空态 / 加载态 / 错误态

### ShareCard

职责：

- 单个 share 展示与操作
- 不承担复杂数据查询

需要 props：

- `share`
- `tunnelStatus`
- `onOpenDetail`
- `onOpenConnect`
- `onPause`
- `onResume`
- `onDelete`
- `onStartTunnel`
- `onStopTunnel`

### ShareDetailDrawer

职责：

- 深度查看单个 share
- 汇总只读信息
- 提供关键操作

### ShareConnectDialog

职责：

- 展示连接信息
- 复制 token/url/curl

### CreateShareDialog

职责：

- 创建表单
- 与 providers 联动预填
- 校验

建议内部拆分：

- `BasicSection`
- `ProviderSection`
- `AuthSection`
- `QuotaSection`
- `ConfigSection`

第一轮可在同文件中完成，后续按复杂度再拆。

## 视觉与交互风格

遵循当前项目已有视觉语言，不重做设计系统。

要求：

- 延续 glass / rounded / muted surface 风格
- Share 页面比 Settings 更“业务面板化”
- 避免纯表格
- 强调状态与操作路径

建议：

- 顶部 hero 用较强信息层级
- ShareCard 使用中等密度卡片布局
- 配额用 progress bar
- Connect dialog 用 monospace 代码块显示 curl

## 空态 / 错误态 / 加载态

### 空态

场景：

- 没有任何 share

内容：

- 说明文案
- CTA：`Create your first share`

### 错误态

场景：

- `listShares` 失败
- `getTunnelStatus` 失败

处理：

- 列表失败：页面级错误卡片 + retry
- 隧道状态失败：卡片级 `unknown` 状态，不阻断整个页面

### 加载态

- 页面首次加载：Skeleton cards
- 创建/启动/停止按钮：局部 loading，禁止重复点击

## 国际化计划

在：

- `src/i18n/locales/zh.json`
- `src/i18n/locales/en.json`
- `src/i18n/locales/ja.json`

新增文案组：

- `share.title`
- `share.subtitle`
- `share.create`
- `share.empty`
- `share.search`
- `share.filter.status`
- `share.filter.app`
- `share.sort`
- `share.name`
- `share.appType`
- `share.provider`
- `share.apiKey`
- `share.settingsConfig`
- `share.tokenLimit`
- `share.tokensUsed`
- `share.requestsCount`
- `share.expiresAt`
- `share.createdAt`
- `share.lastUsedAt`
- `share.status`
- `share.startTunnel`
- `share.stopTunnel`
- `share.pause`
- `share.resume`
- `share.delete`
- `share.detail`
- `share.connectInfo`
- `share.copyToken`
- `share.copyUrl`
- `share.copyCurl`
- `share.tunnel.title`
- `share.tunnel.serverUrl`
- `share.tunnel.sshUrl`
- `share.tunnel.tunnelUrl`
- `share.tunnel.secretKey`
- `share.tunnel.useLocalhost`
- `share.tunnel.notConfigured`
- `share.tunnel.configSaved`
- `share.validation.invalidJson`
- `share.validation.required`

## 验证与测试计划

### 单元测试

新增测试文件建议：

- `tests/lib/query/share.test.ts`
- `tests/components/share/SharePage.test.tsx`
- `tests/components/share/CreateShareDialog.test.tsx`
- `tests/components/share/ShareCard.test.tsx`
- `tests/components/share/TunnelConfigPanel.test.tsx`

测试重点：

- query 缓存失效
- 创建表单校验
- provider 切换时自动预填
- status 对应按钮启用/禁用
- connect info 复制逻辑

### MSW / API mock

扩展现有测试状态：

- shares list
- create share
- pause/resume/delete
- start/stop tunnel
- get connect info
- configure tunnel

### 手工验收路径

1. 打开 Share 页面
2. 保存 Tunnel 配置
3. 创建指定 app 的 share
4. 自动出现于列表
5. 打开 Connect Info，复制 curl
6. 启动 tunnel，状态刷新为 running
7. 暂停 share，按钮切换
8. 恢复 share
9. 停止 tunnel
10. 删除 share

### 回归关注点

- App.tsx 新增 view 后，不影响原有 providers/settings/prompts/skills
- Settings 页面不应被破坏
- providers 查询仍按原逻辑工作
- i18n key 缺失不应导致空白页面

## 实施顺序

### 阶段 1：基础设施

1. 统一导出 `shareApi`
2. 建 `shareKeys`
3. 建 share query/mutation hooks
4. 建 `shareUtils.ts`
5. 建 `share` i18n 文案

### 阶段 2：页面骨架

1. `App.tsx` 新增 `shares` view
2. 新增导航入口
3. 新增 `SharePage`
4. 新增 `ShareHero`
5. 新增 `ShareToolbar`
6. 新增 `ShareStatsBar`

### 阶段 3：Tunnel 配置

1. `TunnelConfigPanel`
2. 表单状态与保存逻辑
3. 敏感字段显示/隐藏
4. 配置缺失提示接入全页

### 阶段 4：Share 列表能力

1. `ShareList`
2. `ShareCard`
3. `ShareStatusBadge`
4. 空态/错误态/加载态
5. tunnel status 轮询

### 阶段 5：创建与详情

1. `CreateShareDialog`
2. Provider 联动预填
3. JSON 编辑器接入
4. `ShareDetailDrawer`
5. `ShareConnectDialog`

### 阶段 6：完善交互

1. 复制按钮
2. 排序筛选搜索
3. 配额进度
4. toast 文案统一
5. 禁用态和解释文案

### 阶段 7：测试与验收

1. 单元测试
2. MSW mock
3. 手工验收
4. 回归检查

## 明确不做的事

以下内容不在本轮计划中：

- Share 使用量图表
- Share 审计日志页
- Share 编辑功能
- Tunnel 连接测试
- 多 tunnel provider 模板
- 路由级页面拆分

这些不是因为“不重要”，而是当前后端能力与页面复杂度下，先完成完整管理能力更优。

## 交付完成标准

当以下条件全部满足时，视为前端 Share 功能完成：

- App 中有独立 `Share` 入口
- 可查看 share 列表
- 可创建 share
- 可查看 connect info 并复制
- 可暂停/恢复/删除 share
- 可保存 tunnel 配置
- 可启动/停止 tunnel
- 页面有搜索、筛选、排序
- 有完整状态展示与错误反馈
- 有三语 i18n
- 有测试覆盖关键路径

## 逐文件任务清单

以下清单按“文件级职责”拆解，实施时可以逐个完成并提交。

### 1. [src/lib/api/share.ts](./src/lib/api/share.ts)

目标：

- 统一成与现有 API 模块一致的 `shareApi` 风格
- 保留类型定义

需要改动：

- 将零散函数改为：
  - `export const shareApi = { create, delete, pause, resume, list, getDetail, startTunnel, stopTunnel, getTunnelStatus, getConnectInfo, configureTunnel }`
- 导出类型：
  - `ShareRecord`
  - `CreateShareParams`
  - `TunnelInfo`
  - `TunnelConfig`
  - `ConnectInfo`

验收点：

- 其他模块可以通过 `import { shareApi } from "@/lib/api"` 使用
- 类型仍可被单独 import

### 2. [src/lib/api/index.ts](./src/lib/api/index.ts)

目标：

- 将 share API 纳入统一出口

需要改动：

- 增加 `export { shareApi } from "./share"`
- 导出 share 相关类型

验收点：

- 页面层不再直接 import `./share`

### 3. 新增 `src/lib/query/share.ts`

目标：

- 建立 share 领域的 query key、query hooks、mutation hooks

需要包含：

- `shareKeys`
- `useSharesQuery`
- `useShareDetailQuery`
- `useShareTunnelStatusQuery`
- `useShareConnectInfoQuery`
- `useCreateShareMutation`
- `useDeleteShareMutation`
- `usePauseShareMutation`
- `useResumeShareMutation`
- `useStartShareTunnelMutation`
- `useStopShareTunnelMutation`
- `useConfigureTunnelMutation`

关键要求：

- mutation 内统一做 query invalidation
- 成功与失败都暴露 hook 状态，避免组件自己拼逻辑
- 轮询参数支持外部控制

验收点：

- 页面不直接手写 `queryClient.invalidateQueries`
- 所有 share 操作均有单独 hook

### 4. 新增 `src/utils/shareUtils.ts`

目标：

- 收敛 share 表单与展示中会重复用到的纯函数

建议函数：

- `getShareSupportedApps(): AppId[]`
- `extractApiKeyFromProvider(provider, appId): string`
- `serializeProviderSettings(provider): string`
- `parseShareSettingsConfig(raw): Record<string, unknown> | null`
- `buildDefaultShareName(provider): string`
- `formatShareStatus(status): string`
- `isShareActionAllowed(share, action, tunnelConfigured, tunnelStatus): boolean`
- `getShareUsageRatio(share): number`
- `buildShareCurlExample(connectInfo): string`

验收点：

- 组件中不再散落 provider 解析逻辑
- share 行为规则由工具函数或 hook 统一管理

### 5. 可选新增 `src/lib/schemas/share.ts`

目标：

- 用 zod 管理创建分享和 tunnel 配置表单校验

建议 schema：

- `createShareSchema`
- `tunnelConfigSchema`

验收点：

- 创建表单校验逻辑不写死在组件事件里

### 6. 新增 `src/components/share/index.ts`

目标：

- 统一导出 share 页面组件

建议导出：

- `SharePage`

### 7. 新增 `src/components/share/SharePage.tsx`

目标：

- 组合整页
- 管理本地 UI 状态
- 连接 query 与组件树

职责：

- 调用 `useSharesQuery`
- 持有：
  - `search`
  - `statusFilter`
  - `appFilter`
  - `sortBy`
  - `createOpen`
  - `detailShareId`
  - `connectShareId`
- 计算过滤和排序后的列表
- 计算统计摘要
- 渲染：
  - `ShareHero`
  - `TunnelConfigPanel`
  - `ShareToolbar`
  - `ShareStatsBar`
  - `ShareList`
  - dialogs / drawers

验收点：

- Share 页面只有一个数据入口和一个状态中心
- 子组件尽量是受控 props

### 8. 新增 `src/components/share/ShareHero.tsx`

目标：

- 页面头部展示

职责：

- 标题、副标题
- “Create Share” CTA
- tunnel 配置摘要
- 简要统计 chips

验收点：

- 页面进入后用户能立刻理解用途和下一步动作

### 9. 新增 `src/components/share/TunnelConfigPanel.tsx`

目标：

- 管理全局 Tunnel 配置

职责：

- 展示字段
- 编辑草稿
- 保存
- 重置
- 敏感字段显示/隐藏

建议 props：

- `initialConfig`
- `onSave`
- `isSaving`

实现建议：

- 若后端尚无读取 tunnel config 的接口，则第一轮可以：
  - 页面加载时展示空草稿
  - 保存后维持当前内存草稿
  - 在 `UI.md` 的实施备注中记录后端后续最好补 `get_tunnel_config`

验收点：

- 用户能单独配置 tunnel，不依赖创建 share

### 10. 新增 `src/components/share/ShareToolbar.tsx`

目标：

- 提供列表控制能力

职责：

- 搜索输入框
- status 筛选
- appType 筛选
- sort 选择器

建议 props：

- `search`
- `onSearchChange`
- `statusFilter`
- `onStatusFilterChange`
- `appFilter`
- `onAppFilterChange`
- `sortBy`
- `onSortByChange`

### 11. 新增 `src/components/share/ShareStatsBar.tsx`

目标：

- 以概览方式展示全局 share 数据

建议统计：

- Total Shares
- Active
- Running Tunnels
- Exhausted
- Expiring Soon

验收点：

- 数据来自 share 列表实时计算，而不是写死

### 12. 新增 `src/components/share/ShareList.tsx`

目标：

- 负责空态、错误态、加载态和列表映射

职责：

- 渲染 skeleton
- 渲染 empty state
- 渲染 error state
- 渲染 `ShareCard[]`

建议 props：

- `shares`
- `isLoading`
- `error`
- `renderTunnelStatus`
- 各类 action handlers

### 13. 新增 `src/components/share/ShareCard.tsx`

目标：

- 单条 share 的可读摘要和操作

建议内容区块：

- 顶部：标题 + 状态
- 中部：app/provider/到期/请求次数
- 配额：progress
- tunnel：url/subdomain/health
- 操作栏：详情、连接信息、启停、暂停恢复、删除

需要特别处理：

- 不展示明文 `shareToken`
- `settingsConfig` 不在卡片展示
- `apiKey` 不展示

### 14. 新增 `src/components/share/ShareStatusBadge.tsx`

目标：

- 统一 share 状态视觉映射

建议输入：

- `status`
- `kind?: "share" | "tunnel"`

输出：

- 颜色、文案、图标

### 15. 新增 `src/components/share/ShareDetailDrawer.tsx`

目标：

- 完整查看 share 信息

职责：

- 详情只读展示
- reveal token
- 只读 JSON viewer
- 关键操作复用

建议 props：

- `share`
- `tunnelStatus`
- `open`
- `onOpenChange`
- 各 action handlers

### 16. 新增 `src/components/share/ShareConnectDialog.tsx`

目标：

- 专门面向“复制/使用”的轻量弹窗

职责：

- 请求 connect info
- 显示 url、token、subdomain
- 生成 curl
- copy actions

验收点：

- 用户无需进入详情抽屉就能直接复制接入信息

### 17. 新增 `src/components/share/CreateShareDialog.tsx`

目标：

- 承载创建分享的完整表单

职责：

- app/provider 联动
- 自动预填 apiKey/settingsConfig
- JSON 校验
- 提交 mutation

建议 props：

- `open`
- `onOpenChange`
- `defaultApp?: AppId`

内部状态建议：

- 使用 `react-hook-form`
- 如引入 schema，则配合 `zodResolver`

验收点：

- 用户不需要手动复制 provider 全部配置即可快速创建

### 18. [src/App.tsx](./src/App.tsx)

目标：

- 把 Share 页面接入现有多视图壳层

需要改动：

- `View` 增加 `shares`
- `VALID_VIEWS` 增加 `shares`
- 头部标题映射增加 share
- 导航入口增加 share
- 内容渲染分支中挂载 `SharePage`

验收点：

- Share 可以像 providers/settings 一样正常切换
- 不破坏现有 view 逻辑

### 19. `src/i18n/locales/*.json`

目标：

- 补齐 share 文案三语

要求：

- 所有新组件不得写死中文
- share 相关 toast 也必须走 i18n

### 20. 测试文件

新增建议：

- `tests/components/share/SharePage.test.tsx`
- `tests/components/share/CreateShareDialog.test.tsx`
- `tests/components/share/ShareCard.test.tsx`
- `tests/components/share/TunnelConfigPanel.test.tsx`
- `tests/lib/query/share.test.ts`

## 实施顺序 Checklist

以下 checklist 以“可以逐项勾掉”为目标。

### A. 基础设施

- [ ] 重构 `src/lib/api/share.ts` 为 `shareApi`
- [ ] 在 `src/lib/api/index.ts` 导出 `shareApi`
- [ ] 新建 `src/lib/query/share.ts`
- [ ] 新建 `src/utils/shareUtils.ts`
- [ ] 视需要新建 `src/lib/schemas/share.ts`

### B. 页面接入

- [ ] 在 `src/App.tsx` 新增 `shares` view
- [ ] 增加 share 导航入口
- [ ] 增加 share 标题与空态映射
- [ ] 新建 `src/components/share/index.ts`
- [ ] 新建 `src/components/share/SharePage.tsx`

### C. 页面骨架

- [ ] 实现 `ShareHero`
- [ ] 实现 `ShareToolbar`
- [ ] 实现 `ShareStatsBar`
- [ ] 在 `SharePage` 中接入列表查询
- [ ] 完成过滤、搜索、排序本地状态

### D. Tunnel 配置

- [ ] 实现 `TunnelConfigPanel`
- [ ] 支持保存 tunnel config
- [ ] 支持敏感字段 reveal/hide
- [ ] 支持“未配置 tunnel”状态提示

### E. 列表与状态

- [ ] 实现 `ShareStatusBadge`
- [ ] 实现 `ShareList`
- [ ] 实现 `ShareCard`
- [ ] 完成空态、错误态、加载态
- [ ] 接入 tunnel status 轮询
- [ ] 完成 action 启用/禁用规则

### F. 创建与详情

- [ ] 实现 `CreateShareDialog`
- [ ] 接入 provider 自动预填
- [ ] 接入 `JsonEditor`
- [ ] 完成表单校验
- [ ] 实现 `ShareDetailDrawer`
- [ ] 实现 `ShareConnectDialog`
- [ ] 接入复制 token/url/curl

### G. 交互完善

- [ ] 创建成功后自动定位新建条目
- [ ] 创建成功后打开 connect info 或 detail
- [ ] 删除前二次确认
- [ ] pause/resume/start/stop 的按钮 loading 态
- [ ] toast 文案统一

### H. 国际化

- [ ] 补齐 `zh.json`
- [ ] 补齐 `en.json`
- [ ] 补齐 `ja.json`
- [ ] 替换所有 share 页面硬编码文案

### I. 测试

- [ ] 补 query 测试
- [ ] 补创建表单测试
- [ ] 补卡片状态逻辑测试
- [ ] 补 tunnel 配置面板测试
- [ ] 补页面集成测试

### J. 验收

- [ ] 可以进入 Share 页面
- [ ] 可以创建 share
- [ ] 可以查看 connect info
- [ ] 可以暂停/恢复/删除
- [ ] 可以启动/停止 tunnel
- [ ] 可以搜索和筛选
- [ ] 可以显示正确的状态和配额
- [ ] 可以通过测试和手工回归

## 实施备注

### 备注 1：Tunnel 配置读取能力

当前后端只明确暴露了 `configure_tunnel`，没有看到对应的 `get_tunnel_config` 前端 API。

如果后端确实缺失读取接口，则前端有两个选项：

- 方案 A：本轮补后端读取接口
- 方案 B：本轮前端仅维护本次会话内草稿状态

建议采用方案 A，因为“一步到位”的前端不应在页面刷新后丢失配置展示。

### 备注 2：appType 可用范围

虽然 `CreateShareParams.appType` 是 string，但 UI 不应把所有 `AppId` 暴露给用户。

本轮必须限制为：

- `claude`
- `codex`
- `gemini`

### 备注 3：后续扩展预留

本轮组件命名和目录已经为以下能力预留扩展位：

- share 编辑
- share 使用统计
- tunnel 连接测试
- 审计与日志
- 批量操作
