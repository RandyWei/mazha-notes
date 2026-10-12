# 码札 (Mazha) — 规格说明书 (SPEC)

> 版本：0.1（初稿）
> 日期：2026-10-12
> 状态：规划中，标注 `[待确认]` 的条目需与产品负责人确认后再锁定

---

## 0. 文档说明与规范驱动开发原则

1. 本 SPEC 是本项目的**单一事实来源**（Single Source of Truth）。
2. 任何接口、数据结构、架构的变更，必须**先更新本 SPEC，再改代码**。
3. 标注 `[待确认]` 的条目在实现前必须先确认；确认后删除标记并记入第 14 节决策记录。
4. 实现必须严格遵循第 6 节的接口契约与第 8 节的技术选型。

---

## 1. 项目概览

| 项目 | 详情 |
|------|------|
| 产品名称 | **码札** / Mazha |
| 名称来源 | 原拟「码匠」（编码之工匠），因被占用改为「码札」，札有书信、笔记之意 |
| 仓库目录 | `mazha-notes-v2/`（本目录，全新重写，不继承 mazha-notes v1） |
| 包 / Bundle ID | `icu.bughub.app.mazha` |
| Rust crate 前缀 | `mazha-` |
| 产品定位 | 个人笔记/知识库 + 开发者文档 + 通用写作 + 团队协作文档 |
| 编辑器形态 | **WYSIWYG 所见即所得**（Typora 式，无预览窗、无同步循环） |
| 编辑器内核 | **源文本为真相**（rope 存原文 + 增量解析 + 显示/源偏移映射） |
| Markdown 方言 | **GFM + 知识库扩展**（wiki-link / callout / front-matter / 标签） |
| 文档存储 | **本地文件 + 文件夹**（一文档一 `.md`），用户自选目录 |
| 多端同步 | **计划云同步**；底层现在就引入 **CRDT（yrs）**，CRDT 内保存 Markdown 源文本 |
| 扩展机制 | **WASM 插件运行时**（连接自部署/自定义存储、第三方能力） |
| 开源许可 | **AGPL-3.0**（代码开源；云服务/高级功能可收费） |
| 仓库结构 | **Monorepo**（Cargo workspace + `platforms/` 平台目录） |
| 共享核心架构 | **纯 Rust 共享核心 + 各平台独立 UI 层** |
| 开发投入 | 个人业余时间开发，多端**串行推进** |

---

## 2. 产品定位与目标用户

产品同时服务四类场景（按优先级）：

| 场景 | 典型用户 | 关键诉求 |
|------|----------|----------|
| 个人笔记 / 知识库 | 知识工作者、学生 | 本地文件、双链/标签、快速检索、模板 |
| 开发者文档工具 | 开发者 | 代码块高亮、公式、Mermaid、Git 协作 |
| 通用写作工具 | 写作者 | 纯净排版、导出、主题、专注/打字机模式 |
| 团队协作文档 | 小团队/企业 | 云同步、多端一致性、权限、（远期）实时协作 |

**核心差异点**：单文件原生 Markdown（不被数据库绑架）+ 原生桌面性能（非 Electron/WebView）+ 纯 Rust 技术栈。

---

## 3. 平台矩阵与优先级

多平台**不追求同一套代码，追求功能效果一致**。共享 Rust 核心，UI 层各平台实现。

| 平台 | 优先级 | UI 技术栈 | 核心接入方式 | 状态 |
|------|--------|-----------|--------------|------|
| macOS | **P0** | GPUI + gpui-kit | Rc 同进程 | MVP |
| Windows | **P0** | GPUI + gpui-kit | Rc 同进程 | MVP |
| Web（现代浏览器） | P1 | `[待确认]` GPUI-wasm 或 TS 前端 | wasm | Phase 7 |
| iOS | P1 | SwiftUI | UniFFI | Phase 6 |
| Android | P1 | Jetpack Compose | UniFFI | Phase 6 |
| Linux | P2 | GPUI + gpui-kit | Rc 同进程 | Phase 5+ |
| HarmonyOS | P3 | ArkUI (ArkTS) | NAPI | Phase 8（仅规划） |

**MVP 平台 = macOS + Windows**。其余平台在 SPEC 中规划，不进入 MVP 验收。

---

## 4. 总体架构

### 4.1 设计原则

1. **核心与 UI 分离**：`mazha-core` 及其以下不依赖任何 GUI 框架，可在无 UI 环境下单测。
2. **源文本为唯一真相**：文件内容、CRDT 内容、渲染输入均源自同一份 Markdown 源文本。
3. **平台 UI 只做两件事**：把核心的"投影结果"画出来；把用户输入转成核心的"编辑命令"。
4. **接口先行**：核心对外契约稳定后，各平台可并行（未来）实现。
5. **可测试**：光标/偏移映射等脏逻辑必须能在纯 Rust 单测中覆盖。

### 4.2 分层架构

```
┌───────────────────────────────────────────────────────────────────┐
│                          UI 层（各平台独立）                        │
│  ┌──────────────┐ ┌──────────────┐ ┌──────────────┐ ┌───────────┐ │
│  │ desktop      │ │ ios (SwiftUI)│ │ android      │ │ web       │ │
│  │ GPUI+gpui-kit│ │              │ │ (Compose)    │ │ (WASM)    │ │
│  └──────┬───────┘ └──────┬───────┘ └──────┬───────┘ └─────┬─────┘ │
└─────────┼────────────────┼────────────────┼───────────────┼───────┘
          │ Rc             │ UniFFI         │ UniFFI        │ wasm-bindgen
          ▼                ▼                ▼               ▼
┌───────────────────────────────────────────────────────────────────┐
│                      桥接层（平台绑定）                             │
│   mazha-ffi (UniFFI, iOS/Android/鸿蒙)   ·   mazha-wasm (Web)      │
└───────────────────────────────────┬───────────────────────────────┘
                                    ▼
┌───────────────────────────────────────────────────────────────────┐
│                        共享核心（纯 Rust）                          │
│                                                                   │
│  mazha-core ── 文档模型 / 编辑命令 / 撤销 / 光标 / 偏移映射          │
│      ├── mazha-md      ── Markdown 解析 / 方言 / 增量解析            │
│      ├── mazha-crdt    ── yrs 封装 / 同步协议 / 合并                 │
│      ├── mazha-store   ── 存储抽象 / 本地文件 Provider               │
│      ├── mazha-export  ── HTML / PDF / (DOCX / EPUB) 导出           │
│      └── mazha-plugin  ── WASM 插件宿主 / 存储插件桥                 │
└───────────────────────────────────────────────────────────────────┘
```

### 4.3 数据真相链

```
  磁盘 .md 文件  ◀──(读写)──▶  Document(source text, rope)
        ▲                            ▲              │
        │                    (CRDT)  │              │ 增量解析
        │                            │              ▼
   StorageProvider                 Y.Text      解析结果 spans/blocks
   ├─ 本地文件系统                  (yrs)             │
   ├─ 自部署/云（插件/内建）                          ▼
   └─ 用户自定义存储                            显示投影(display)
                                          （隐藏语法标记、样式、布局）
                                                     │
                                      UI 层只渲染投影 + 回传编辑命令
```

**关键约定**：
- 本地文件是**存储真相**；CRDT 的 `Y.Text` 保存同一份**源文本**；二者通过同步引擎保持一致。
- WYSIWYG 只是源文本的**投影**，任何编辑都先把输入翻译成"对源文本的编辑命令"，再重投影。
- 因此：保存无损（源文本原样），协同可行（Y.Text），渲染可控（投影可重建）。

### 4.4 仓库结构（Monorepo）

```
mazha-notes-v2/
├── SPEC.md
├── README.md
├── Cargo.toml                     # workspace 根
├── rust-toolchain.toml
├── crates/                        # 共享 Rust 核心
│   ├── mazha-core/
│   ├── mazha-md/
│   ├── mazha-crdt/
│   ├── mazha-store/
│   ├── mazha-export/
│   ├── mazha-plugin/
│   ├── mazha-ffi/                 # UniFFI 定义与绑定生成
│   └── mazha-wasm/                # Web 绑定（Phase 7，占位）
├── platforms/
│   ├── desktop/                   # GPUI + gpui-kit（macOS/Windows/Linux）
│   ├── ios/                       # SwiftUI（Phase 6）
│   ├── android/                   # Jetpack Compose（Phase 6）
│   ├── harmony/                   # ArkTS + NAPI（Phase 8）
│   └── web/                       # WASM 前端（Phase 7）
├── plugins/                       # 示例 / 内建 WASM 插件（存储 Provider 等）
└── docs/                          # 设计文档、ADR、开放问题记录
```

---

## 5. 核心模块规范

| crate | 职责 | 不依赖 | 关键类型/接口 |
|-------|------|--------|----------------|
| `mazha-core` | 文档模型、编辑命令、撤销栈、光标/选区、显示↔源偏移映射、投影生成 | 任何 GUI | `Document` `Edit` `Projection` `OffsetMap` `Selection` |
| `mazha-md` | Markdown 解析、GFM+知识库方言、增量解析（脏块重解析）、序列化 | GUI | `parse` `SyntaxSpan` `Block` `serialize` |
| `mazha-crdt` | 用 yrs 维护 `Y.Text`，同步协议、状态向量、合并、持久化更新 | GUI | `CrdtDoc` `SyncMessage` |
| `mazha-store` | 存储 Provider 抽象 + 本地文件系统实现 + 文件监听 | GUI | `StorageProvider` `FileSystemProvider` |
| `mazha-export` | 源文本 → HTML/PDF/(DOCX/EPUB) | GUI | `export_html` `export_pdf` |
| `mazha-plugin` | WASM 插件宿主、能力授权、存储插件桥 | GUI | `PluginHost` `Capability` |
| `mazha-ffi` | UniFFI 接口定义，供 Swift/Kotlin/ArkTS 调用 | GUI | 见 6.4 |
| `mazha-wasm` | Web 绑定（Phase 7） | GUI | 见 7.3 |

**依赖图**

```
platforms/desktop ─▶ mazha-core ─┬─▶ mazha-md
                                 ├─▶ mazha-crdt ─▶ yrs
                                 ├─▶ mazha-store
                                 ├─▶ mazha-export
                                 ├─▶ mazha-plugin ─▶ wasmtime/wasmi
                                 └─▶ (mazha-ffi / mazha-wasm 为平台绑定入口)
```

---

## 6. 接口契约（Spec Contracts）

> 下列为契约草案示意（非最终签名）。锁定前需评审。所有核心接口需提供纯 Rust 单测。

### 6.1 `mazha-core` 核心类型

```rust
/// 文档的唯一标识
pub struct DocId(pub u64);

/// 存储位置（相对工作区，跨平台统一使用 / 分隔）
pub struct DocPath(pub String);

/// 一个编辑命令：对源文本的一次原子修改
pub enum Edit {
    Insert { at: usize, text: String },       // at = 源字节偏移（UTF-8 边界）
    Delete { range: Range<usize> },           // 源字节范围
    Replace { range: Range<usize>, text: String },
    ToggleInline { range: Range<usize>, mark: InlineMark }, // 包裹/取消 强调/代码/链接
    ToggleBlock  { block: usize, kind: BlockKind },         // 标题/列表/引用/代码块切换
}

/// 输入意图 → 编辑命令 的转换（缩进、列表续行、自动配对等）
pub trait IntentMapper {
    fn map(&self, intent: &Intent, doc: &Document) -> Vec<Edit>;
}

/// 渲染投影：UI 只消费它，不再自己解析 Markdown
pub struct Projection {
    pub lines: Vec<ProjectedLine>,   // 每逻辑行
}

pub struct ProjectedLine {
    pub source_range: Range<usize>,  // 本行在源文本中的字节范围
    pub block: BlockStyle,           // 段落/标题N/引用/代码块/列表项/表格行…
    pub runs: Vec<TextRun>,          // 带样式的文本片段（已隐藏语法标记）
    pub widgets: Vec<Widget>,        // 图片/公式/图表/表格/任务复选框等占位
    pub offset_map: OffsetMap,       // 显示位置 ↔ 源偏移
}

/// 显示偏移 ↔ 源偏移 双向映射（WYSIWYG 最关键的数据结构）
pub struct OffsetMap { /* 段内区间映射表 */ }

impl OffsetMap {
    pub fn source_to_display(&self, src: usize) -> DisplayPos;
    pub fn display_to_source(&self, disp: DisplayPos) -> usize;
}

/// 光标/选区均以源偏移表示
pub struct Selection { pub anchor: usize, pub head: usize }

/// 文档：唯一持有源文本真相
pub struct Document { /* rope + 解析缓存 + 撤销栈 */ }

impl Document {
    pub fn text(&self) -> String;                       // 全文拷贝；逐段读取用 slice()
    pub fn slice(&self, range: Range<usize>) -> Cow<str>; // 字节范围 → 文本（可能零拷贝）
    pub fn len(&self) -> usize;                         // 字节长度
    pub fn apply(&mut self, edit: Edit) -> Result<ChangeSummary>; // 非法偏移返回错误，不改文档
    pub fn undo(&mut self) -> Option<ChangeSummary>;
    pub fn redo(&mut self) -> Option<ChangeSummary>;
    pub fn projection(&self) -> Projection;             // Phase 2（投影层）
    pub fn snap_to_boundary(&self, byte: usize) -> usize; // 吸附到字符边界
}

/// 一次编辑的变更摘要（UI 据此决定重解析/重绘范围）
pub struct ChangeSummary {
    pub affected: Range<usize>,   // 新文档中发生变化的字节范围
    pub doc_len: usize,           // 应用后字节长度
}
```

> **实现状态**：`Edit`（Insert/Delete/Replace）、`Document`（rope + 边界校验 +
> undo/redo + 字节/字符边界导航）、`Selection`、`ChangeSummary` 已落地（Phase 1 最小内核，22
> 条单测，见 `crates/mazha-core`）。`ToggleInline`/`ToggleBlock`、`OffsetMap`/`Projection`、
> `DocId`/`DocPath` 为后续迭代项。

### 6.2 `StorageProvider`（存储抽象）

```rust
pub trait StorageProvider: Send + Sync {
    fn id(&self) -> &str;                                  // "local" | "mazha-cloud" | "plugin:xxx"
    fn capabilities(&self) -> StorageCaps;                 // 是否支持流式/版本/协作等
    async fn list(&self, dir: &str) -> Result<Vec<Entry>>;
    async fn read(&self, path: &str) -> Result<Vec<u8>>;
    async fn write(&self, path: &str, data: &[u8]) -> Result<()>;
    async fn rename(&self, from: &str, to: &str) -> Result<()>;
    async fn remove(&self, path: &str) -> Result<()>;
    /// 文件变更订阅（本地用文件监听，云用长连接/轮询）
    fn watch(&self, cb: Box<dyn Fn(ChangeEvent) + Send + Sync>) -> Result<Watcher>;
}
```

- 本地文件系统是**默认 Provider**。
- 云/自部署/用户自定义存储均以 Provider 形式接入，其中用户自定义走 **WASM 插件**（第 6.3 节）。
- 付费用户连接官方云服务，本质是 `mazha-cloud` Provider；是否收费与 Provider 接口无关。

### 6.3 插件 ABI（WASM）

- 运行时：桌面用 **wasmtime**（Component Model + WASI），移动端用 **wasmi**（无 JIT，适配 iOS/Android 限制）`[待确认]`。
- 接口用 **WIT + wit-bindgen** 定义，插件实现导出函数，宿主提供能力（host functions）。
- 能力需**逐项显式授权**（文件读写范围、网络域名、剪贴板等），默认全部拒绝。
- 首个插件类别：`storage-provider`（把插件当作一个 `StorageProvider`）。
- `[待确认]` 插件清单/签名/分发机制（是否上架官方插件市场）留待 Phase 5 单独 SPEC。

### 6.4 FFI（UniFFI）概览 — iOS / Android

以 `mazha-core` 暴露给 Swift/Kotlin 的最小面：

```rust
// mazha-ffi/src/mazha.udl（示意）
namespace mazha {};

interface Document {
    [Name=open] constructor(string path);
    string text();
    void apply_edit(Edit edit);       // Edit 为 UniFFI record/enum
    Projection projection();
    void undo();
    void redo();
};

interface Workspace {
    sequence<Entry> list(string dir);
    Document open_document(string path);
    void save(Document doc);
};
```

- HarmonyOS 单独走 **OHOS NAPI**（`napi-rs` 目标 OpenHarmony），ArkTS 侧封装 `[待确认]`。

### 6.5 同步协议

- 基于 yrs 的 **Yjs 同步协议**（状态向量 + 增量更新），保证与 Web 端 Yjs 生态**未来可互通**。
- 服务端/传输为**独立 SPEC**（Phase 5），本 SPEC 只约束：核心只产出/消费标准 Yjs 更新帧。
- 冲突语义：`Y.Text` 上是字符级合并；结构性并发编辑（如两人同时改同一表格）可能导致语义怪但**不丢数据**，后续用"块级锁定/提示"缓解 `[待确认]`。

---

## 7. 平台实现方案

### 7.1 桌面（macOS / Windows 优先，Linux 后）— GPUI + gpui-kit

- UI 框架：**GPUI**（Zed 自研，Apache-2.0）+ **gpui-kit 0.7**（组件/主题/Dock/表格/只读 Markdown 渲染）。
- GPUI 为 **git-only 依赖**，gpui-kit 锁定对应 GPUI 快照；升级需同步。可选 `gpui-fast` 保留渲染特性（`[待确认]`是否启用）。
- 编辑器实现要点（沿用现有开源实践 `zorite/gpui-editor`、`velotype`）：
  - 自写 `Element` + `shape_line` 绘制投影；实现 `EntityInputHandler`（键盘 + IME）。
  - 语法标记隐藏：光标不在该行时不绘制标记，光标进入/离开时切换显隐。
  - 块级 widget：表格（单元格网格 + 增删行列）、代码块（内嵌高亮）、图片、任务复选框。
- 已确认可复用的纯 Rust 组件：
  - **公式**：`ratex-*`（KaTeX 移植）+ `ratex-gpui`（渲染 + MathQuill 式结构编辑器）。
  - **图表**：`mermaid-rs-renderer`（纯 Rust → SVG/PNG，无浏览器依赖）。
  - **代码高亮**：tree-sitter（已确认；gpui-kit 内置 tree-sitter 集成，增量、多语言）。

### 7.2 移动端（iOS / Android）— Rust core + 原生 UI

- UI：**SwiftUI**（iOS）/ **Jetpack Compose**（Android）；通过 **UniFFI** 调 `mazha-core`。
- 交互适配：Typora 的"光标进入才显示标记"在触摸端需重新设计 → 定义**移动端编辑手势规范**（点触进入块编辑、长按选择、快捷工具栏）`[待确认]`。
- 移动端文件访问：iOS 走 **Files/iCloud 文档**，Android 走 **SAF（Storage Access Framework）**；均以 `StorageProvider` 适配。
- 移动端渲染：公式/图表复用 `ratex-*` / `mermaid-rs-renderer` 产出的图片/SVG，由平台原语绘制。

### 7.3 Web（Phase 7）— `[待确认]`：spike 后二选一

**前置动作**：进入 Phase 7 前做 **1 周技术验证 spike**（输入 + IME + 渲染标题/粗体）。

- **方案 A：GPUI 编译到 wasm**
  - 可行性：Zed 官方 `gpui_web`（wasm32-unknown-unknown + WebGPU），gpui-component 已有 `story-web` 全量 WASM 版。
  - 风险：强制 WebGPU；CJK 字体需打包；wasm 单线程不能用阻塞锁；富文本编辑在 wasm 上未大规模验证。
  - 收益：桌面/Web 同一套 Rust 渲染代码，一致性最高。
- **方案 B：TypeScript 前端 + `mazha-core`(wasm)**
  - Web 用 CodeMirror 6 / ProseMirror；核心经 `wasm-bindgen` 暴露。
  - 收益：稳定、无需 WebGPU。代价：WYSIWYG 渲染逻辑写两遍，一致性靠 SPEC 约束。

**默认倾向 A**；spike 不达标则回退 B。

### 7.4 HarmonyOS（Phase 8，仅规划）

- ArkUI(ArkTS) + **OHOS NAPI** 调 Rust 核心；鸿蒙优先用原生 ArkUI 以保证一致性。
- 编辑器控件与交互对齐移动端规范。

---

## 8. 技术选型与依赖

| 用途 | 选型 | 说明 |
|------|------|------|
| 桌面 UI | GPUI + gpui-kit 0.7 | Apache-2.0；GPUI 为 git 依赖 |
| 文本模型 | `ropey` | rope 结构，O(log n) 编辑 |
| Markdown 解析 | `pulldown-cmark`（GFM） | 带字节偏移，支持块级缓存；代码高亮用 tree-sitter |
| 增量解析 | 自研脏块重解析（基于 pulldown-cmark 偏移） | 见第 11 节 |
| CRDT | `yrs` | Yjs Rust 实现，与 Web Yjs 互通 |
| 移动端 FFI | `uniffi` | 生成 Swift/Kotlin 绑定 |
| 鸿蒙 FFI | `napi-rs` (@ohos) | `[待确认]` |
| 插件运行时 | `wasmtime`（桌面）/ `wasmi`（移动） | Component Model + WIT |
| 公式 | `ratex-parser/layout/render/svg` + `ratex-gpui` | KaTeX 移植 |
| 图表 | `mermaid-rs-renderer` | 纯 Rust → SVG/PNG |
| PDF 导出 | Typst | 纯 Rust；排版质量优先（已确认） |
| HTML 导出 | pulldown-cmark 渲染 + 模板 | 纯 Rust |
| DOCX/EPUB | `[待确认]` `docx-rs` / Zip+XHTML | 后续 Phase |
| 字体/中文字形 | `[待确认]` 系统字体 + 内置 CJK 子集 | Web/wasm 需内置 |

---

## 9. 功能范围

### 9.1 MVP（桌面首个可用版）

- [ ] 打开/保存 Markdown 文件；文件树；新建/重命名/删除/移动
- [ ] WYSIWYG 编辑：标题、粗体、斜体、删除线、行内代码、链接、引用、有序/无序/任务列表
- [ ] 基础块：代码块（高亮）、图片、表格（增删行列）、脚注
- [ ] 公式（`$…$` / `$$…$$`）与 Mermaid 图表
- [ ] 多标签页、命令面板、快捷键体系
- [ ] 导出 HTML / PDF（纯 Rust）

### 9.2 后续 Phase（已确认排除出 MVP）

- 大纲 / 目录导航、文档内搜索 → Phase 4
- 主题 / 专注模式 / 打字机模式、字体字号 → Phase 4
- CRDT + 云同步 + 存储 Provider + WASM 插件 → Phase 5
- 移动端（iOS/Android）→ Phase 6
- Web → Phase 7
- 鸿蒙 → Phase 8

### 9.3 明确范围外（除非另行决定）

- 实时多人协作光标（Phase 5 之后评估）
- 数据库型知识库（当前定位是本地文件）
- 非 Markdown 文档格式编辑

---

## 10. 里程碑与分期（个人业余）

> 估时按业余投入，单位为"周"；实际以功能验收为准，不以日期为准。

| 阶段 | 内容 | 依赖 | 估时 |
|------|------|------|------|
| Phase 0 | 立项、SPEC、workspace 骨架、CI | — | 1–2 |
| Phase 1 | `mazha-core` 纯文本内核（rope/编辑/撤销/光标/选区）+ `mazha-md` 解析 + 单测 | Phase 0 | 4–6 |
| Phase 2 | 桌面 GPUI：只读投影渲染 + WYSIWYG 显隐标记 + IME | Phase 1 | 6–10 |
| Phase 3 | `OffsetMap` 完善（跨行选区/命中/IME）+ 块级 widget（表格/图片/代码/公式/图表） | Phase 2 | 6–10 |
| Phase 4 | 文件管理/多标签/命令面板/导出 + 大纲/搜索/主题 | Phase 3 | 4–6 |
| Phase 5 | `mazha-crdt`（yrs）+ `mazha-store` Provider + WASM 插件 + 云服务端（独立 SPEC） | Phase 4 | 8–12 |
| Phase 6 | 移动端 iOS/Android（UniFFI + 原生 UI + 交互规范） | Phase 5 | 8–12 |
| Phase 7 | Web：spike → 方案 A/B → 实现 | Phase 6 | 4–8 |
| Phase 8 | HarmonyOS（ArkUI + NAPI） | Phase 6 | 待评估 |

**关键路径**：Phase 1 → 2 → 3。这三段决定产品手感，是最大风险区。

---

## 11. 编辑器交互规范（Typora 式）

1. **无预览窗**：所有渲染发生在编辑区本身。
2. **语法标记隐藏**：光标不在所在行时隐藏 `**`、`#`、`>` 等标记；光标进入该行时恢复显示以便编辑。
3. **偏移映射**：任何显示位置都能映射回源字节偏移；所有命中测试、选区绘制、IME 定位都查 `OffsetMap`。
4. **增量解析**：按块（段落/列表/引用/代码块/表格）缓存解析结果，仅重解析受影响的块；文档尺寸扩大时保持流畅。
5. **块级 widget**：图片、公式、图表、表格在光标不在其中时呈现渲染态，进入时切换为可编辑态（表格为单元格网格）。
6. **输入意图转换**：列表续行、Tab 缩进、`>`/`-`/`#` 自动补全、选区包裹（`Cmd+B` 等）、自动配对括号，全部先转成 `Edit` 再应用。

---

## 12. 验收标准

### 12.1 编译/工程

- [ ] `cargo build --workspace` 通过；`cargo test` 核心 crate 全绿
- [ ] `cargo clippy -- -D warnings` 无告警；`cargo fmt --check` 通过
- [ ] 核心 crate 不依赖任何 GUI 框架（CI 校验 `cargo tree`）

### 12.2 功能（MVP）

- [ ] 打开一个含标题/列表/表格/代码/公式/图表的 `.md`，正确 WYSIWYG 渲染
- [ ] 编辑后保存，文件内容为无损 Markdown（往返 `serialize(parse(x)) == x` 对稳定块成立）
- [ ] 中文输入法（IME）在隐藏标记的行上候选框位置正确
- [ ] 表格可增删行列；代码块有语法高亮；公式与图表正确渲染
- [ ] 导出 HTML/PDF 内容与编辑区一致

### 12.3 跨平台一致性（长期）

- [ ] 同一 `.md` 在桌面/移动/Web 上渲染结构一致（允许主题/交互差异）
- [ ] 本地文件 ↔ CRDT ↔ 各端投影 数据不丢失

---

## 13. 风险与开放问题

| # | 风险/问题 | 影响 | 缓解/待定 |
|---|-----------|------|-----------|
| R1 | GPUI 为 git 依赖、API 变动快 | 自定义 Element 易受影响 | 锁定快照；隔离 `platforms/desktop` 的 GPUI 使用面 |
| R2 | `OffsetMap` 隐藏标记映射 | 光标/IME bug 密集 | Phase 1/3 用纯 Rust 单测覆盖；先做测试再堆功能 |
| R3 | wasm 下 GPUI 文本编辑成熟度未知 | 决定 Web 方案 A/B | Phase 7 前 1 周 spike（第 7.3 节） |
| R4 | `mermaid-rs-renderer` 早期，渲染质量未完全对齐 mermaid-cli | 图表观感 | 允许降级/回退；跟踪上游 |
| R5 | 移动端触摸交互与 Typora 键盘模型差异 | 一致性目标 | 单独定义移动端交互规范 `[待确认]` |
| R6 | CRDT 下结构性并发编辑语义 | 数据不丢但可能错乱 | 块级锁定/冲突提示策略 `[待确认]` |
| R7 | WASM 插件在 iOS/移动端运行时限制 | 插件可用性 | 移动端用 `wasmi` 无 JIT `[待确认]` |
| R8 | 纯 Rust PDF 导出的排版质量 | 导出观感 | `[待确认]` 引擎选型；必要时引入 Typst |
| R9 | 中文字体在 Web 端需内置 | 体积/一致 | 字体子集化 `[待确认]` |

**待确认清单**：第 7.1（gpui-fast 是否启用）、7.3（Web 方案）、7.4 与 8（DOCX/EPUB 引擎、中文字体子集）、6.3（插件分发与清单）、6.5（并发编辑语义）、9（移动端交互规范）。

---

## 14. 决策记录（Decision Log）

| # | 决策 | 结论 | 来源 |
|---|------|------|------|
| D1 | 项目性质 | 全新重写，不继承 v1 | 确认 |
| D2 | 平台优先级 | 桌面(macOS+Windows) P0；Web/iOS/Android P1；Linux P2；鸿蒙 P3 | 确认 |
| D3 | 架构 | 纯 Rust 共享核心 + 各平台独立 UI | 确认 |
| D4 | 编辑器内核 | 源文本为真相（rope + 偏移映射） | 确认 |
| D5 | Markdown 方言 | GFM + 知识库扩展 | 确认 |
| D6 | 存储 | 本地文件+文件夹（Typora 式） | 确认 |
| D7 | 同步数据模型 | 现在就引入 CRDT；CRDT 内保存源文本 | 确认 |
| D8 | CRDT 库 | yrs | 确认 |
| D9 | 移动端 UI | 各平台原生（SwiftUI / Compose / ArkUI） | 确认 |
| D10 | FFI | UniFFI + 鸿蒙 NAPI | 确认 |
| D11 | 插件运行时 | WASM | 确认 |
| D12 | 导出 | 纯 Rust 自实现 | 确认 |
| D13 | 许可证 | AGPL-3.0（代码开源 + 云服务收费） | 确认 |
| D14 | 仓库结构 | Monorepo | 确认 |
| D15 | 命名 | 码札 / Mazha / `icu.bughub.app.mazha` / `mazha-` | 确认 |
| D16 | Web 路线 | 延后决定，Phase 7 前 spike | 确认 |
| D17 | 开发投入 | 个人业余，多端串行 | 确认 |
| D18 | 仓库初始化 | git init + 创建远程仓库 | 确认 |
| D19 | 工具链 | Rust edition 2024，最新 stable | 确认 |
| D20 | 代码高亮 | tree-sitter（gpui-kit 内置） | 确认 |
| D21 | PDF 导出引擎 | Typst | 确认 |
| D22 | 仓库可见性 | GitHub 私有，发布后转公开（AGPL-3.0） | 确认 |

---

## 15. 变更记录

| 版本 | 日期 | 变更内容 |
|------|------|----------|
| 0.1 | 2026-10-12 | 初始规范（规划版）：完成平台矩阵、总体架构、核心模块与契约草案、里程碑与决策记录 |
| 0.1.1 | 2026-10-12 | Phase 1 启动：落地 `mazha-core` 最小可用内核（Document/Edit/Selection/ChangeSummary + undo/redo + 边界校验），契约 §6.1 同步实现态签名；接入 ropey 1.6 + thiserror 2；新增约定 AGENTS.md（规范驱动 + 必备注释 + CI 命令） |

---

> **规范驱动开发原则**：
> 1. 所有实现必须严格遵循本 SPEC 定义的契约。
> 2. 任何接口/架构变更需先更新 SPEC 再实现。
> 3. SPEC 是单一事实来源；`[待确认]` 项确认后移入第 14 节。
