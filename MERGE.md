# 上游合并记录

记录每次从上游 [farion1231/cc-switch](https://github.com/farion1231/cc-switch) 合并的详情。

---

## 2026-04-15

- **上游分支：** `main`
- **合并提交：** `c993972f`
- **共同祖先：** `449a1712`
- **合并提交数：** 16
- **主要变更：**
  - feat(stream-check): 刷新默认模型列表，检测 model-not-found 错误
  - fix(proxy): 按 RFC 7230 剥离逐跳响应头
  - fix(opencode): 使用 json5 解析器兼容尾逗号
  - perf: 虚拟化会话消息列表，折叠长消息以降低渲染开销
  - refactor: 将 "Local Proxy Takeover" 重命名为 "Local Routing"
  - refactor: 移除按供应商单独配置代理的功能
- **冲突解决：**
  - `src-tauri/src/services/stream_check.rs` — 保留我方新增的 Anthropic header 和 connection keep-alive
  - `src/i18n/locales/{en,ja,zh}.json` — 采用上游的 routing 重命名和端口 15721
