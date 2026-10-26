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
| Markdown 方言 | **GFM + 知识库扩展**（wiki-link / callout / front-matter / 标签 / 公式 `$…$` `$$…$$`） |
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
│      ├── mazha-export  ── HTML / PDF / DOCX / EPUB 导出              │
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
| `mazha-core` | 文档模型、编辑命令、撤销栈、光标/选区、显示↔源偏移映射、投影生成、表格结构编辑、代码块语法高亮、查找/替换 | 任何 GUI | `Document` `Edit` `Projection` `OffsetMap` `Selection` `edit_table` `Highlighter` `find_all` `replace_all` |
| `mazha-md` | Markdown 解析、GFM+知识库方言、脚注（引用 + 定义）、公式（行内 `$…$` + 块级 `$$…$$`）、增量解析（脏块重解析）、序列化 | GUI | `parse` `SyntaxSpan` `Block` `serialize` |
| `mazha-crdt` | 用 yrs 维护 `Y.Text`，同步协议、状态向量、合并、持久化更新 | GUI | `CrdtDoc` `SyncMessage` |
| `mazha-store` | 存储 Provider 抽象 + 本地文件系统实现 + 文件监听 | GUI | `StorageProvider` `FileSystemProvider` |
| `mazha-export` | 源文本 → HTML / PDF / DOCX / EPUB | GUI | `to_html` / `to_pdf` / `to_docx` / `to_epub` |
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
                                 ├─▶ syntect（代码高亮，纯 Rust fancy-regex）
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
    ToggleInline { range: Range<usize>, mark: InlineMark },   // 包裹/取消 强调/粗体/删除线/代码
    ToggleBlock  { range: Range<usize>, target: BlockTarget }, // 标题/引用/列表/段落
}

/// 行内强调种类 / 块级目标
pub enum InlineMark { Emphasis, Strong, Strikethrough, Code }
pub enum BlockTarget { Paragraph, Heading(u8), BlockQuote, List { ordered: bool } }

/// 表格结构编辑（增删行列）：定位光标所在表格块，重写整块并产出一次 Edit
pub enum TableOp {
    InsertRowAbove, InsertRowBelow,
    InsertColumnLeft, InsertColumnRight,
    DeleteRow, DeleteColumn,
}
fn edit_table(source: &str, caret: usize, op: TableOp) -> Option<Edit>;

/// 代码块语法高亮（纯逻辑、无 GUI）：一段代码 → 每行的带前景色片段
pub struct Highlighter { /* 内置 SyntaxSet + Theme */ }
pub struct ColoredRun { pub range: Range<usize>, pub color: (u8, u8, u8) } // range 相对行首
impl Highlighter {
    pub fn with_theme(theme: &str) -> Self;                              // 内置主题名；未知则回退
    pub fn highlight(&self, code: &str, lang: Option<&str>) -> Vec<Vec<ColoredRun>>;
}

/// 输入意图 → 编辑命令 的转换（缩进、列表续行、自动配对等）
pub trait IntentMapper {
    fn map(&self, intent: &Intent, doc: &Document) -> Vec<Edit>;
}

/// 渲染投影：UI 只消费它，不再自己解析 Markdown（按源文本逻辑行 + 末尾空行）
pub struct Projection {
    pub lines: Vec<ProjectedLine>,
    pub widgets: Vec<BlockWidget>,   // 块级 widget（代码块/表格/分割线…），按源序排列
}

pub struct ProjectedLine {
    pub source_range: Range<usize>,  // 本行在源文本中的字节范围（不含行尾换行）
    pub block: BlockStyle,           // 块样式（标题级别/引用深度/列表/任务/代码块/分割线）
    pub runs: Vec<TextRun>,          // 带样式的可见文本片段（已隐藏语法标记）
    pub widgets: Vec<Widget>,        // 图片/公式/图表等占位
    pub offset_map: OffsetMap,       // 显示位置 ↔ 源偏移
}

impl ProjectedLine {
    pub fn display_text(&self) -> String;              // 可见片段拼接
    pub fn display_byte_len(&self) -> usize;           // 显示文本字节长度
    /// 显示列（字符索引）↔ 显示文本字节偏移（软换行布局用；越界钳制、字节落字符内上取整）
    pub fn display_byte_of_col(&self, col: usize) -> usize;
    pub fn display_col_of_byte(&self, byte: usize) -> usize;
}

/// 块样式（容器样式可叠加，如"引用中的列表项"）
pub struct BlockStyle {
    pub quote_depth: u8,
    pub heading: Option<u8>,         // 1..=6
    pub bullet: Option<Bullet>,      // 列表项符号（有序/无序 + 序号）
    pub task: Option<bool>,          // 任务项状态
    pub code_block: bool,
    pub code_lang: Option<String>,
    pub thematic_break: bool,
}

pub struct TextRun {
    pub source_range: Range<usize>,  // 连续可见文本的源字节范围
    pub display_start: usize,        // 首字符的显示字符列
    pub style: RunStyle,             // Normal/Emphasis/Strong/Strikethrough/InlineCode/LinkText/Math
    pub text: String,                // 可见文本
}

pub struct Widget {                  // 内联占位（源文本隐藏，UI 绘制）
    pub source_range: Range<usize>,
    pub display_col: usize,
    pub kind: WidgetKind,            // Image { alt } …
}

/// 块级 widget：整块由 UI 按"渲染态/编辑态"整体绘制（SPEC §11.5）。
/// 光标不在其任一逻辑行时呈渲染态（隐藏围栏/分隔等标记），光标进入则切回编辑态（显示原文）。
pub struct BlockWidget {
    pub source_range: Range<usize>,  // 覆盖整块的源字节范围（含围栏/分隔行）
    pub line_range: Range<usize>,    // 覆盖的逻辑行（0 起，半开）
    pub kind: BlockWidgetKind,
}

pub enum BlockWidgetKind {
    Code { lang: Option<String>, fenced: bool },  // 代码块（围栏/缩进）
    Table { align: Vec<Align> },                  // GFM 表格（列对齐）
    Rule,                                         // 分割线（单行）
    Math,                                         // 显示公式块（`$$…$$`，D34）
    Chart { lang: String },                       // 图表块（mermaid 等，D34）
}

/// 显示偏移 ↔ 源偏移 双向映射（WYSIWYG 最关键的数据结构）
///
/// 模型：一行文本划分为若干**可见区间**（连续可见字符的源字节范围），相邻区间之间是被
/// 隐藏的语法标记（宽度 0）。光标只能在源文本的**字符边界**上落点，绝不会插进隐藏标记。
/// 纯文本区间内 1 显示字符 = 1 源字符，故用"可见字符起始边界"数组即可精确双向映射。
pub struct OffsetMap { /* 可见字符的 src_marks 与等长 disp_marks */ }

impl OffsetMap {
    /// 由"可见源区间"构造（隐藏区间=补集）。要求区间有序、不重叠、字符边界对齐，
    /// 相邻区间自动合并。
    pub fn from_visible_runs(source: &str, visible_runs: &[Range<usize>]) -> MapResult<Self>;
    /// 源字节偏移 → 显示位置；落进隐藏标记时吸附到标记前（最近显示边界）。
    pub fn source_to_display(&self, src: usize) -> DisplayPos;
    /// 显示位置 → 源字节偏移；交叉点取源偏移更小者（=隐藏标记起点，稳定落点）。
    pub fn display_to_source(&self, disp: DisplayPos) -> usize;
    /// 整行显示宽度（可见字符总数）。
    pub fn display_len(&self) -> usize;
}

/// 文档坐标：逻辑行号 + 行内**显示列**（可见字符索引，非源字节）。
pub struct DocPos { pub line: usize, pub col: usize }

/// 跨行选区落在某一逻辑行上的显示列区间（绘制选区高亮用）。
pub struct LineDisplayRange { pub line: usize, pub cols: Range<usize> }

impl Projection {
    /// 源字节偏移 → 文档坐标（行号 + 显示列）。落进隐藏标记吸附到标记起点；
    /// 行尾边界归本行（与"删行尾换行"类编辑对齐）。
    pub fn source_to_doc_pos(&self, src: usize) -> DocPos;
    /// 文档坐标 → 源字节偏移。列越界钳到行尾，行越界钳到末行。
    pub fn doc_pos_to_source(&self, pos: DocPos) -> usize;
    /// 源字节偏移所在逻辑行号（行尾边界归前一行）。
    pub fn line_of_source(&self, src: usize) -> usize;
    /// 某一逻辑行的显示宽度（可见字符数）；行号越界钳到末行。
    pub fn display_len_at(&self, line: usize) -> usize;
    /// 选区覆盖到的每行显示列区间（跨行选区绘制用）。空选区返回空表。
    pub fn selection_display_ranges(&self, sel: Selection) -> Vec<LineDisplayRange>;
    /// 全部块级 widget（按源序）。
    pub fn block_widgets(&self) -> &[BlockWidget];
    /// 源字节偏移所在的块级 widget（无则 None；用于"渲染态/编辑态"判定）。
    pub fn block_widget_at(&self, src: usize) -> Option<&BlockWidget>;
    /// 某一逻辑行所属的块级 widget（无则 None）。
    pub fn block_widget_on_line(&self, line: usize) -> Option<&BlockWidget>;
}

/// 显示位置：显示文本中的字符索引（newtype）。
pub struct DisplayPos(pub usize);

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

> **实现状态**：`Edit`（Insert/Delete/Replace + ToggleInline/ToggleBlock，切换命令经
> `commands` 模块解析为源级编辑）、`Document`（rope + 边界校验 + undo/redo +
> 字节/字符边界导航 + `projection()` 全量重建）、`Selection`、`ChangeSummary`、
> `OffsetMap`（src↔display 双向映射 + 稳定落点规则）、`Projection`（按逻辑行投影：
> `BlockStyle`/`TextRun`/`Widget` + 每行 `OffsetMap`；**文档级跨行坐标映射**
> `source_to_doc_pos`/`doc_pos_to_source`/`line_of_source`/`display_len_at`/
> `selection_display_ranges`，覆盖跨行选区/命中/IME）、**块级 widget**（`BlockWidget`/
> `BlockWidgetKind`：代码块/表格/分割线，含源范围与逻辑行范围，供 UI 做渲染态/编辑态切换）、
> `IntentMapper`（最小实现：
> 回车列表续行/退出列表 + 切换转发）已落地（Phase 1，见 `crates/mazha-core`）。
> **桌面端已接入**该文档级 API（`platforms/desktop` 的行归属 `line_of_source`、上下移动
> 保留显示列 `source_to_doc_pos`/`doc_pos_to_source`、选区高亮 `selection_display_ranges`、
> IME 候选框定位均改走 `Projection`，原先内联的 `line_index`/选区换算与 `OffsetMap`
> 二次换算已移除）。
> `DocId`（待定）、`Projection` 的**增量重建**、
> 更丰富的意图（缩进/自动配对/删除键合并）为后续迭代项。
> `Workspace<S: StorageProvider>`（`open`/`save`/`list`，串联存储与文档）已落地。

### 6.2 `StorageProvider`（存储抽象）

```rust
/// 工作区内相对路径（`/` 分隔、禁 `..`/`.`/空段/绝对前缀/反斜杠）
pub struct DocPath(/* normalized */);

pub trait StorageProvider: Send + Sync {
    fn id(&self) -> &'static str;                          // "local" | "mazha-cloud" | "plugin:xxx"
    fn capabilities(&self) -> StorageCaps;                 // writable/streaming/versioning/collaboration
    fn list(&self, dir: &DocPath) -> Result<Vec<Entry>>;   // 不递归，目录在前
    fn read(&self, path: &DocPath) -> Result<Vec<u8>>;
    fn write(&self, path: &DocPath, data: &[u8]) -> Result<()>;   // 覆盖并自动建父目录
    fn rename(&self, from: &DocPath, to: &DocPath) -> Result<()>;
    fn remove(&self, path: &DocPath) -> Result<()>;        // 目录递归删除
}
```

- 默认实现 `FileSystemProvider`（本地文件系统）；`DocPath` 定义在 `mazha-store`，
  由 `mazha-core` 复用（依赖方向 `core → store`，避免循环）。
- 云/自部署/用户自定义存储均以 Provider 形式接入，其中用户自定义走 **WASM 插件**（第 6.3 节）。
- 付费用户连接官方云服务，本质是 `mazha-cloud` Provider；是否收费与 Provider 接口无关。
- **MVP 采用同步接口**（决策 D24，第 14 节）：本地文件系统是唯一 Provider，同步 IO
  足够且免去异步运行时依赖。异步/流式能力（以及 `watch` 文件监听）留待 Phase 5
  接入云端 Provider 时评估。

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

- UI 框架：**GPUI**（Zed 自研，Apache-2.0；现以 `gpui-pre` 家族发布在 crates.io）+ **gpui-kit 0.7**（组件/主题/Dock/表格/只读 Markdown 渲染）。
- GPUI 版本由 gpui-kit 锁定（`gpui-pre` 0.3.x 快照），应用只声明 `gpui-kit`，升级需同步；可选 `gpui-fast` 保留渲染特性（`[待确认]`是否启用）。
- 编辑器实现要点（沿用现有开源实践 `zorite/gpui-editor`、`velotype`）：
  - 自写 `Element` + `shape_text`（软换行）绘制投影；实现 `EntityInputHandler`（键盘 + IME）。
  - **软换行**（D33）：每条投影行用 `shape_text`（带 `wrap_width`）折成多个视觉行；布局按视觉行累计（`row_starts`），命中/光标/选区/IME/块级装饰均以视觉行为单位，光标与选区仍以逻辑行 + 显示字符列表示，经 `ProjectedLine::display_byte_of_col`（↔ `display_col_of_byte`）与字形字节索引互换。
  - 语法标记隐藏：光标不在该行时不绘制标记，光标进入/离开时切换显隐。
  - 块级 widget：表格（单元格网格 + 增删行列）、代码块（内嵌高亮）、图片、任务复选框。
  - **系统剪贴板**（0.1.47，SPEC §11.7）：`Cmd+C/X/V` 走 `gpui` 的 `write_to_clipboard`/`read_from_clipboard`（纯文本），多光标编辑按位置倒序应用以保证偏移正确。
- 已确认可复用的纯 Rust 组件：
  - **公式**：`ratex-*`（KaTeX 移植）+ `ratex-gpui`（渲染 + MathQuill 式结构编辑器）。
  - **图表**：`mermaid-rs-renderer`（纯 Rust → SVG/PNG，无浏览器依赖）。
  - **代码高亮**：**syntect**（纯 Rust fancy-regex，Sublime 语法；SPEC D32）——核心 `mazha-core::highlight` 输出着色区间，桌面端分屏着色。（tree-sitter 亦可用，但为免 onig/交叉编译负担改用 syntect。）

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
| Markdown 解析 | 自研行扫描（`mazha-md`），覆盖 MVP 方言子集 | 精确字节偏移、便于脏块增量解析；`pulldown-cmark` 仅作测试对照（dev 依赖） |
| 增量解析 | 自研脏块重解析（基于块级字节范围） | 见第 11 节 |
| CRDT | `yrs` | Yjs Rust 实现，与 Web Yjs 互通 |
| 移动端 FFI | `uniffi` | 生成 Swift/Kotlin 绑定 |
| 鸿蒙 FFI | `napi-rs` (@ohos) | `[待确认]` |
| 插件运行时 | `wasmtime`（桌面）/ `wasmi`（移动） | Component Model + WIT |
| 公式 | `ratex-parser/layout/render/svg` + `ratex-gpui` | KaTeX 移植 |
| 图表 | `mermaid-rs-renderer` | 纯 Rust → SVG/PNG |
| PDF 导出 | Typst 0.15（`typst` / `typst-layout` / `typst-pdf` / `typst-kit`）→ [`mazha-export::to_typst`] + [`mazha-export::to_pdf`] | 纯 Rust；排版质量优先（D21，接入方式见 D25）：`to_typst` 产出标记，`ExportWorld` 实现 `typst::World` 后 `compile` + `typst_pdf::pdf` 出字节；字体走 `typst-kit`（内置 + 系统扫描）；`PdfDocument{bytes, warnings, pages}`（`pages` 为排出页数）；行内代码内容含反引号时改用 `#raw("…")`（反引号围栏会与内容粘连成更长串致 unclosed raw text） |
| HTML 导出 | 基于 `mazha-md` 块/片段模型自研渲染（[`mazha-export::to_html`]） | 纯 Rust；与编辑器同源（D23 后 `pulldown-cmark` 仅 dev 对照） |
| DOCX 导出 | `docx-rs` 1.1（[`mazha-export::to_docx`]） | 纯 Rust；基于 `mazha-md` 块/片段模型直接生成 OOXML（标题用直接格式、行内 粗/斜/删除线/行内代码、链接「文字 + (URL)」、图片占位、GFM 表格、列表/任务/引用/提示块/代码块/分割线），决策 D26 |
| EPUB 导出 | Zip + XHTML（复用 `to_html` 同源渲染，[`mazha-export::to_epub`]） | 纯 Rust；EPUB 3 容器（`mimetype` 首条目且不压缩 / `container.xml` / `content.opf` / `nav.xhtml` / `toc.ncx` / XHTML 正文），正文走 XHTML 方言（空元素自闭合、布尔属性带值），决策 D27 |
| 导出主题/模板 | 自研 [`mazha-export::Theme`]（HTML/EPUB 的 CSS）与 [`mazha-export::Template`]（PDF/Typst 的版式前置） | 纯 Rust；内置主题 `light`/`dark`/`sepia`、模板 `plain`/`article`，均确定性常量，可 `custom`；`Template::article` 不覆盖字体族以保留 CJK 回退，决策 D28 |
| 字体/中文字形 | `[待确认]` 系统字体 + 内置 CJK 子集 | Web/wasm 需内置 |

---

## 9. 功能范围

### 9.1 MVP（桌面首个可用版）

- [ ] 打开/保存 Markdown 文件；文件树；新建/重命名/删除/移动
- [ ] WYSIWYG 编辑：标题、粗体、斜体、删除线、行内代码、链接、引用、有序/无序/任务列表
- [ ] 基础块：代码块（高亮）、图片、表格（增删行列）、脚注（**解析与模型**已实现 0.1.52：行内引用 `[^label]` + 块级定义 `[^label]: body`；跳转/自动编号/文末归集待渲染层）
- [ ] 公式（`$…$` / `$$…$$`）与 Mermaid 图表（**解析与模型**已实现 0.1.51；所见即所得排版待 ratex / mermaid-rs 接入）
- [ ] 多标签页、命令面板、快捷键体系
- [ ] 导出 HTML / PDF（纯 Rust）

### 9.2 后续 Phase（已确认排除出 MVP）

- 大纲 / 目录导航（**已实现 0.1.53**：投影 `block.heading` → 侧栏标题列表 + 点击跳转，D39）→ Phase 4
- 文档内搜索（**当前文档**查找/替换已实现 0.1.49 `Cmd+F`；**全工作区**跨文件搜索 `Cmd+Shift+F` 列为低优先规划，见 D40）→ Phase 4
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
2. **语法标记隐藏**：默认隐藏 `**`、`#`、`>` 等标记；光标落入某个 `[文字](目标)` 链接内时，**仅该链接**临时恢复原文（含 URL，便于继续编辑，D42），光标移开后复原。整行原文显示会改变整行显示坐标，需同时切换命中/选区/IME 映射，留后续。
3. **偏移映射**：任何显示位置都能映射回源字节偏移；所有命中测试、选区绘制、IME 定位都查 `OffsetMap`。
4. **增量解析**：按块（段落/列表/引用/代码块/表格）缓存解析结果，仅重解析受影响的块；文档尺寸扩大时保持流畅。
5. **块级 widget**：图片、公式、图表、表格在光标不在其中时呈现渲染态，进入时切换为可编辑态（表格为单元格网格）。
6. **输入意图转换**：列表续行、Tab 缩进、`>`/`-`/`#` 自动补全、选区包裹（`Cmd+B` 等）、自动配对括号，全部先转成 `Edit` 再应用。
7. **系统剪贴板**：`Cmd+C`/`Cmd+X`/`Cmd+V` 复制/剪切/粘贴纯文本（`Cmd+A` 全选已有的选区集合）。多光标时：复制把所有选区按文档顺序以换行拼接；剪切/粘贴按光标**位置倒序**逐个应用，避免靠前编辑使靠后光标偏移失效；剪贴板仅取纯文本（忽略图片/文件），空选区不覆盖剪贴板。
8. **查找/替换**：`Cmd+F` 展开查找条（查询框 + 替换框 + 匹配计数 + 大小写开关）。`Enter`/`Shift+Enter` 跳上/下一个匹配、`Tab` 切换查找/替换框、`Cmd+Enter` 替换当前、`Cmd+Shift+Enter` 替换全部、`Cmd+Alt+C` 切换大小写敏感、`Esc` 关闭。查找/替换逻辑是核心纯函数 `mazha_core::{find_all, replace_all, replace_all_edit}`（大小写不敏感按字符 `to_lowercase` 比较，支持非 ASCII；不支持正则捕获组），替换全量为单次全文 `Edit::Replace`（一条撤销记录）。桌面查找框为静态文本输入（无 IME），满足 MVP。
9. **公式/图表占位**（D34）：`$$…$$` 显示公式块与 mermaid 围栏块各归类为一个**块级 widget**（`BlockWidgetKind::{Math, Chart}`），光标不在块内时按块画占位面板（描边 + 「公式」/「图表」标签），光标进入切回编辑态（显示 TeX/mermaid 源文本，与代码块同规则）；行内 `$…$` 的 `$` 标记隐藏、公式体作为可见文本并用 `RunStyle::Math`（斜体 + 主题色）区分。所见即所得的数学排版与图表 SVG 属渲染层（`ratex-*` / `mermaid-rs-renderer`），不在本阶段范围（风险 R4）。
10. **脚注**（D35）：行内引用 `[^label]` 与块级定义 `[^label]: body` 都只隐藏语法标记（`[^`、`]`、`]:`），label 与正文保留为**可见文本**（避免"丢字"）；引用与定义标签用 `RunStyle::Footnote`（主题色）区分，定义块**不**产生块级 widget（它是普通可见文本块）。导出按可见文本处理（HTML `<sup class="fnref">` + `<div class="footnote">`，Typst `#super[…]`，DOCX 纯文本）；点击引用跳转定义、自动编号、文末归集属渲染层（后续）。

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
- [ ] 表格可增删行列（核心 `edit_table` + 桌面快捷键已实现，0.1.44）；代码块有语法高亮（核心 `Highlighter` + 桌面接入已实现，0.1.45）；软换行（核心显示列↔字节换算 + 桌面按视觉行布局已实现，0.1.46）；系统剪贴板复制/剪切/粘贴（桌面已实现，0.1.47）；查找/替换（核心 `find_all`/`replace_all` + 桌面 `Cmd+F` 面板已实现，0.1.49）；公式与图表**已解析并入块级 widget 模型**（核心 `BlockWidgetKind::{Math,Chart}` + 桌面板与 `RunStyle::Math` 已实现，0.1.51），真实排版待 ratex / mermaid-rs；脚注**已解析并入模型**（行内 `[^label]` + 块级 `[^label]: body`，核心 `RunStyle::Footnote` + HTML/Typst/DOCX 导出已实现，0.1.52），跳转/编号/文末归集待渲染层
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
| R8 | 纯 Rust PDF 导出的排版质量 | 导出观感 | 已选 Typst 0.15（D21/D25，接入已通）；已补**真实文档回归**（`tests/pdf_real_doc.rs`：中文技术报告多页排版 + 逐字节可复现 + 标记转义扫描），修复行内代码含反引号的 raw 围栏缺陷；残留：字体子集化（Web，Phase 7）、极端字体的字形覆盖需人工抽检 |
| R9 | 中文字体在 Web 端需内置 | 体积/一致 | 字体子集化 `[待确认]` |
| R10 | `platforms/*` 是独立 workspace，**不在** `cargo --workspace` 门禁内 | 桌面端编译错误可能漏检（已发生：多光标/PDF 导出提交未过 `cargo build`，桌面端一度无法编译） | 桌面门禁已写入 `HANDOFF.md`（触碰 `platforms/*` 必须单独 `fmt`/`clippy -D warnings`/`build`）；`platforms/desktop/Cargo.lock` 需与依赖变更同步（含 `zip 0.5` 等 yanked 版本要保留锁定）；后续接入 CI |

**待确认清单**：第 7.1（gpui-fast 是否启用）、7.3（Web 方案）、7.4 与 8（中文字体子集）、6.3（插件分发与清单）、6.5（并发编辑语义）、9（移动端交互规范）。

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
| D23 | Markdown 解析实现 | 自研行扫描（块级）+ 自研行内扫描，精确字节偏移；`pulldown-cmark` 降为 dev 依赖（测试对照/后续 HTML 导出参考） | 确认（见 `docs/ADR/ADR-001`） |
| D24 | 存储接口同步/异步 | MVP 采用**同步** `StorageProvider`（本地文件系统为唯一 Provider）；异步/流式与文件监听推迟到 Phase 5 | 确认 |
| D25 | PDF 编译接入方式 | Typst 0.15 走 `to_typst` 标记 + 自实现 `ExportWorld`（`typst::World`）：虚拟主文件、资源根 `base_dir`（`Project` 根才允许相对路径，拒绝包根）、字体经 `typst-kit`（内置 + 可选系统扫描），`typst::compile` → `typst_pdf::pdf`；workspace `rust-version` 随 typst MSRV 提升至 **1.92** | 确认（见 SPEC §8） |
| D26 | DOCX 导出引擎 | `docx-rs` 1.1（纯 Rust）：直接基于 `mazha-md` 块/片段模型生成 OOXML，不经 HTML 中转；标题用直接格式（加粗 + 字号）不依赖样式表，链接渲染为「文字 + (URL)」（docx-rs 的 `Relationship` 不支持 `TargetMode="External"`，无法生成可点击外链），图片仅输出占位文本；文本转义交由 `strong-xml`，一律 `xml:space="preserve"` 保空白 | 确认（见 SPEC §8） |
| D27 | EPUB 导出引擎 | Zip + XHTML（纯 Rust，`zip` 0.5，与 docx-rs 复用同版本避免重复依赖）：正文复用 HTML 渲染的 **XHTML 方言**（`to_xhtml_body`，空元素自闭合 `<hr/>`/`<br/>`/`<img .../>`/`<input .../>`、布尔属性写 `disabled="disabled"`），打包为 EPUB 3（`mimetype` 首条目且 `Stored` 不压缩、`META-INF/container.xml`、`OEBPS/content.opf`、`nav.xhtml`、`toc.ncx` 兼容回退、`style.css` 复用 `DEFAULT_CSS`、单一 `text.xhtml`）；`dc:identifier` 缺省时用 FNV-1a 内容哈希派生（避免随机/`uuid` 依赖），`dcterms:modified` 用固定时间戳（输出确定） | 确认（见 SPEC §8） |
| D28 | 导出主题/模板 | 引入 `Theme`（HTML/EPUB 的 CSS）与 `Template`（PDF/Typst 版式前置）两个外观预设；内置 `Theme::{light,dark,sepia}`、`Template::{plain,article}`，均为确定性常量（同输入同输出），支持 `custom`；`to_html_themed`/`to_epub_themed`/`to_pdf_with`/`to_typst_with_template` 接入；`Template::article` 只设页面/字号/段距，**不设字体族**以保留 Typst 的 CJK 字体回退 | 确认（见 SPEC §8、D12/D27） |
| D29 | 桌面端坐标/选区实现 | `platforms/desktop` 不再自行维护行归属与偏移换算：行归属用 `Projection::line_of_source`、上下移动按 `DocPos` 保留**显示列**（修正 CJK 字节相对偏移缺陷）、选区高亮用 `selection_display_ranges` 折成每行显示列、IME 候选框用 `source_to_doc_pos` 定位；行内命中仍用缓存 `BlockGeom` 的 `OffsetMap`（只读路径无法借 `&mut` 取投影）。删除桌面端内联 `line_index`/`ranges_overlap`/`apply_edit_all` 等死代码与借用冲突（诸如 `cursors_mut(&mut self)` 触及整 `self` 导致的连锁借用错误） | 确认（见 SPEC §6.1/§11.3） |
| D30 | 块级 widget 建模位置 | 块级 widget（代码块/表格/分割线…）作为**核心投影**的一部分：`Projection.widgets: Vec<BlockWidget>` + `block_widget_at`/`block_widget_on_line` 查询；每个 widget 记 `source_range`（整块）+ `line_range`（逻辑行）。核心只给"分组与范围"，具体绘制（面板/网格/高亮）留在 UI；"光标进入才可编辑"由 UI 用 `block_widget_on_line(光标行)` 判定。理由：分组/范围是可无 UI 单测的纯逻辑，且被多端（桌面/移动/Web）复用；公式/图表待解析器支持后再加 `BlockWidgetKind` 变体 | 确认（见 SPEC §6.1/§11.5） |
| D31 | 表格增删行列的实现位置 | 表格结构编辑（增删行列）作为**核心纯函数** `edit_table(source, caret, TableOp) -> Option<Edit>`（`mazha-core::table`），而非 UI 直接改字符串：核心用解析器定位光标所在 `BlockKind::Table` 块，把该区域解析成 `TableModel{header, align, rows}`，套用结构变更后**规范化重写整块**（带首尾竖线、按对齐重写分隔行），产出单次 `Edit::Replace`。理由：① 结构变更（行/列索引、转义 `\|`、对齐保留）是可无 UI 单测的纯逻辑，先测试后实现；② 走 `Edit` 通道才能正确纳入 undo/redo 与投影重建；③ 规范化重写牺牲原表格的空白风格，换取实现简单与结果可预测（后续如需保留风格可改增量编辑）。光标定位用整块偏移（非字符级），"删唯一列 / 在表头删行"等不产生变化时返回 `None` | 确认（见 SPEC §6.1/§11.5） |
| D32 | 代码块语法高亮的引擎与位置 | 用 **syntect**（Sublime 语法定义）做语法高亮，作为**核心纯模块** `mazha-core::highlight`（不依赖 GUI）：`Highlighter::with_theme(name)` 持有内置 `SyntaxSet`（大、需复用），`highlight(code, lang) -> Vec<Vec<ColoredRun>>` 逐行返回带前景色片段（区间相对行首），语言名按扩展名/词元/语法名匹配、未命中回退纯文本。引擎用**纯 Rust 的 fancy-regex**（`default-features=false, features=["default-fancy","default-syntaxes","default-themes"]`），避免 `onig` 的 C 依赖与交叉编译/移动端工具链风险。理由：① 输出（源文本→着色区间）是可无 UI 单测的纯逻辑，多端复用；② 不把 `syntect` 类型泄进公共 API（对外只用 `(u8,u8,u8)`）；③ UI 按显示文本切分着色（代码块行显示文本 == 源码行内容），无需再做源↔显示换算。取舍：syntect 会显著增大 `mazha-core` 体积（含语法/主题 dump），后续如体积敏感可下沉到独立 crate 或按特性裁剪 | 确认（见 SPEC §6.1/§11.5） |
| D33 | 软换行的实现层次与坐标模型 | **软换行放在 UI 层**（`platforms/desktop`），不改变核心的"逻辑行 + 显示字符列"坐标模型：每条投影行用 gpui `shape_text`（带 `wrap_width = 内容宽度`）折成一条 `WrappedLine`（内含多个换行边界 = 视觉行）。布局几何按视觉行累计（`DocLayout::row_starts`），命中测试、光标竖线、选区高亮、IME 候选框、块级 widget 装饰的纵向范围都以**视觉行**为单位；光标与选区仍以逻辑行 + 显示**字符列**表示（`DocPos`/`LineDisplayRange` 不变）。显示字符列 ↔ 字形字节索引的换算由核心 `ProjectedLine::display_byte_of_col`/`display_col_of_byte`（纯逻辑、可单测）承担——形状化与换行边界以显示文本的 UTF-8 字节为单位，而投影/偏移映射以字符列为单位（引用 `"> "` 等投影外前缀以 `prefix_bytes` 计入）。理由：① 换行依赖像素宽度（字体度量），属 UI 关注点，核心保持无 GUI、可单测；② 复用 gpui 的换行/命中（`WrappedLine::position_for_index`/`closest_index_for_position`），避免自研断行；③ 顺手修正桌面端先前"以字符列当字节索引"喂 `x_for_index` 的 CJK 隐患。取舍与限制：上下移动已按**视觉行**行走（0.1.48 `visual_vertical_target`：绝对 x 跨块携带、目标行窄则钳到行内、越界不动）；不实现横向滚动/超长不可断行（如超长 URL）溢出（溢出即被裁切）；块级装饰纵向范围随换行伸展 | 确认（见 SPEC §7.1/§11.5） |
| D34 | 公式/图表的解析与建模 | **解析放 `mazha-md`，归类放 `mazha-core`，真排版留渲染层**。① 块级显示公式 `$$…$$` 新增 `BlockKind::MathBlock`（行首缩进 ≤3 开启；同行内闭合即单行块，否则扫描到仅含 `$$` 的行；未闭合延伸到文档末尾，与未闭合围栏同规则，增量裁切点校验 `is_unclosed_fence` 同样覆盖），`$$` 记 `SpanRole::MathMarker`（隐藏）、公式体记 `SpanRole::Math`（可见）。② 行内公式 `$…$` 由行内扫描产出 `SpanRole::{MathMarker, Math}`，规则为：开 `$` 右侧非空白且左侧非字母数字（或行首）、闭 `$` 左侧非空白且右侧非数字（避免 `$5`/`$10` 误判）、内容非空且不跨行、`$$` 在行内不构成行内公式；转义 `\$` 仍走既有转义分支。③ **mermaid 不改解析器**（` ```mermaid ` 本就是 `CodeBlock{lang}`），由核心投影归类为 `BlockWidgetKind::Chart { lang }`；`BlockKind::MathBlock` 归类为 `BlockWidgetKind::Math`（D30 的"待解析器支持后再加变体"至此补齐）。④ 行内公式体用 `RunStyle::Math`（斜体 + 主题色）区分。⑤ 导出：HTML `<span class="math">` / `<div class="math">`；Typst/DOCX 按字面量输出（`#raw` / 纯文本，遵循 R8"行内代码用 `#raw`"的先例——**不**塞进 Typst `$…$` 数学模式，以免 `\frac` 等 LaTeX 命令令 `typst::compile` 报错而让 PDF 导出失败）。取舍：所见即所得的数学排版（`ratex-*`）与图表 SVG（`mermaid-rs-renderer`）属渲染层、风险 R4，本阶段只做占位面板；行内公式暂不作为 `SpanKind::Widget`（否则源文本整段被隐藏而 UI 尚无公式渲染，会凭空"丢字"），保留可见文本 | 确认（见 SPEC §6.1/§11.5/§11.9） |
| D36 | Phase 4 文件管理（打开/保存 + 文件树侧栏） | **接入 `mazha-core::Workspace<S>`**：桌面端用 `Workspace<FileSystemProvider>`（本地文件系统）管理打开/保存/列目录。**工作区根**：优先用命令行传入的目录或文件所在目录；首次无上下文时弹出 `rfd` 选择文件夹设为工作区根。**文件树侧栏**：左侧可收起的树形列表，用 `Workspace::list`/`Workspace::list_markdown` 展示（按目录展开），仅列 `.md`/`.markdown`，目录在前文件在后、按名称排序。**打开/保存**：`Cmd+O` 打开 Markdown 用 `Workspace::open` 载入；`Cmd+S` 已有路径用 `Workspace::save` 覆盖，否则走 `Cmd+Shift+S` 另存为；`Cmd+Shift+S` 始终另存为。**脏状态**：对比快照标记未保存，标题栏显示 `*`，保存成功后清除。**错误处理**：`WorkspaceError::NotUtf8` 与 IO 错误给出提示。**兼容**：命令行传入单个 `.md` 仍直接打开（工作区根设为其父目录），无参数且未选工作区时保留内置示例。多标签页/命令面板/重命名删除新建留后续增量。 | 确认（Phase 4 首增量，见 §9.1/§10） |

---

| D36 | Phase 4 文件管理（打开/保存 + 文件树侧栏） | **接入 `mazha-core::Workspace<S>`**：桌面端用 `Workspace<FileSystemProvider>`（本地文件系统）管理打开/保存/列目录。**工作区根**：优先用命令行传入的目录或文件所在目录；首次无上下文时弹出 `rfd` 选择文件夹设为工作区根。**文件树侧栏**：左侧可收起的树形列表，用 `Workspace::list`/`Workspace::list_markdown` 展示（按目录展开），仅列 `.md`/`.markdown`，目录在前文件在后、按名称排序。**打开/保存**：`Cmd+O` 打开 Markdown 用 `Workspace::open` 载入；`Cmd+S` 已有路径用 `Workspace::save` 覆盖，否则走 `Cmd+Shift+S` 另存为；`Cmd+Shift+S` 始终另存为。**脏状态**：对比快照标记未保存，标题栏显示 `*`，保存成功后清除。**错误处理**：`WorkspaceError::NotUtf8` 与 IO 错误给出提示。**兼容**：命令行传入单个 `.md` 仍直接打开（工作区根设为其父目录），无参数且未选工作区时保留内置示例。多标签页/命令面板/重命名删除新建留后续增量。 | 确认（Phase 4 首增量，见 §9.1/§10） |

| D37 | Phase 4 多标签页 | **模型**：`DocView` 改为多文档（`Vec<Tab>`，每个 Tab 含 `path`/`doc`/`cursors`/`primary`/`find`/`saved_text`/`layout` 等状态），当前活动索引 `active`。**切换**：`Cmd+1..9` 切到第 N 个、`Cmd+0` 切到最后、`Cmd+[`/`]` 前后切换、`Cmd+Shift+[`/`]` 移动标签、`Cmd+W` 关闭当前（不关闭最后一个）、`Cmd+Shift+W` 关闭所有。**打开逻辑**：新文件/打开文件都新建 Tab 并设为活动；同一 `PathBuf` 若已存在则直接切换到该 Tab 而非重复打开。**保存**：`Cmd+S`/`Cmd+Shift+S` 作用于当前活动 Tab。**标题栏/状态栏**：显示当前 Tab 文件名与脏标记（`*`）、全路径。**内存**：关闭 Tab 时释放其状态（DocLayout/高亮缓存可丢弃），布局与投影复用现有逻辑。**兼容**：单文件命令行启动时仍初始化单个 Tab。 | 确认（Phase 4 第二增量） |

| D38 | Phase 4 命令面板 + 快捷键体系 | **命令面板**：`Cmd+P` 打开浮层，输入过滤命令与文件两类条目（前导 `>` 仅命令，无前导同时列出文件；`@`/`#` 留后续）。**命令集**：封装为 `Vec<Command>`（id/标题/快捷键/执行闭包），覆盖新建/打开/保存/另存为、切换/关闭标签、导出、查找、切换文件树。**匹配**：子串大小写不敏感 + 简单打分（前缀 > 词首 > 子串）。**交互**：`↑`/`↓` 选择、`Enter` 执行、`Esc` 关闭；输入框复用桌面静态文本处理（无 IME）。**快捷键体系**：`handle_key` 分派集中到命令 id，命令面板与既有快捷键共用同一命令表，避免两处维护。**范围**：MVP 只做命令面板 + 快捷键收敛，模糊匹配打分从简；文档内搜索/大纲留后续增量。 | 确认（Phase 4 第三增量） |
| D39 | Phase 4 大纲 / 目录导航 | **数据来源**：复用投影，无需新解析——扫描当前活动 Tab 的 `Projection::lines`，凡 `ProjectedLine.block.heading == Some(level)` 者即标题行；标题文本取该行 `display_text()`（已隐藏 `#` 标记、去首尾空白），跳转目标取 `source_range.start`（标题行源字节偏移）。**层级**：按 `level`（1..=6）缩进（每级 +8px），只平铺不折叠。**交互**：侧栏「大纲」分区列出标题，点击调用 `goto_source(offset)` 把光标移到标题行并聚焦编辑区；`Cmd+Shift+O` 或命令面板「切换大纲」展开/收起。**范围**：只做当前文档大纲 + 点击跳转；标题搜索过滤、折叠展开、跟随滚动高亮留后续。 | 确认（Phase 4 第四增量） |
| D40 | Phase 4 文档内搜索（规划，低优先） | **当前文档**查找/替换已实现（0.1.49，`Cmd+F` 查找条 + `mazha-core::find`）。**全工作区**跨文件搜索（`Cmd+Shift+F`：遍历工作区 `.md`，按文件分组列出命中行 + 上下文片段，点击打开并定位）确认为**低优先规划项**，暂不实现；后续若做，复用 `Workspace::list_markdown` + `mazha-core::find::find_all`，UI 侧加搜索面板。 | 确认（低优先，暂不排期） |
| D41 | Phase 4 桌面原生对话框必须用**异步** API（`AsyncFileDialog` + `cx.spawn`） | **约束**：macOS 原生文件对话框若用 rfd **同步** `FileDialog`，会走 `NSApp runModal` **嵌套泵主 runloop**；其间系统重查键盘布局会回调 gpui 的 `platform.on_keyboard_layout_change` → `app.borrow_mut()`，而此时 gpui 仍持有 app 的 `RefCell`，导致 `RefCell already borrowed` panic（`app.rs`）。**`window.defer` 无效**：defer 的闭包仍在同一次 gpui 借用内执行，同步对话框照样嵌套泵 runloop。**做法**：改用 rfd **异步** `AsyncFileDialog`（macOS 走 `beginSheetModalForWindow`，非阻塞、不嵌套泵 runloop），在 gpui 的 `cx.spawn(async move \|this, cx\| { … .await })` 里驱动，完成后经 `this.update(cx, …)` 回写；`DocView` 的 `open_file_dialog`/`save_as`/`move_current_file_dialog`/`export_pdf` 均如此。 | 确认（修复 Cmd+O panic） |
| D42 | 桌面端链接局部展开 + Cmd+点击打开 | **局部展开**：光标落入 `[文字](目标)` 时，UI 只为该逻辑行构造一份展开副本（`mazha-md::parse_inline` 分组 → 把 `[` / `](` / URL / `)` 作为普通 run 插回，重建该行 `OffsetMap`），其余行仍用投影；不采用整行原文（SPEC §11.2 全量版），因为整行显示坐标会变，命中/选区/IME/上下移动都要整行分叉。展开行的 `OffsetMap` 以**行首为基准**，`BlockGeom.revealed` 标记后在 `source_of_col` 加回行起点，光标/IME 列统一走 `BlockGeom::col_of_source`。**Cmd+点击**：同一套链接分组取目标 URL（剥掉可选 `<...>`），`App::open_url` 交给系统默认应用，不改光标。 | 确认（Phase 4 UI 优化） |
| D43 | 桌面端中文斜体用**渲染器级合成倾斜**（而非字体回退） | **问题**：macOS 中文字体（PingFang SC / Hiragino / Songti）没有 italic 字面；给斜体 run 配置 CJK `font.fallbacks` 后，CoreText 的级联表里这些字体仍是 Regular（`kCTFontSlantTrait` 不置位），中文保持直立，只有拉丁字母倾斜。**决策**：不改字体、不改核心投影，在 UI 渲染层自绘——斜体 run 中的 CJK 片段用**透明占位 run** 保留排版（advance/换行/光标/选区/IME 全部复用 gpui 布局），同时收集该段字节区间；paint 时用 CoreText 取字形轮廓，按 `x' = x + tan(12°)·y` 剪切后 `paint_path` 填充。**取舍**：12° 与 Chromium/VS Code 的合成斜体视觉接近；只处理 `Emphasis`/`Math`；透明占位而非删除 run，避免破坏字节↔列映射；字体对象按族名+字号缓存，paint 每字形只做轮廓几何。**验证限制**：本机无屏幕录制权限（截图全黑），观感需实机验收；单测覆盖 `split_segments`、透明 run + span 产出、CoreText 可取 CJK 轮廓。**已知不足（待改进）**：自绘倾斜与原生 italic 字面在字符间距/抗锯齿上仍有差异（透明占位 run 的 advance 与轮廓起点可能差 <1px），且 paint 期逐字取轮廓未缓存；后续按"轮廓缓存 + advance 校正"优化。详见 `platforms/desktop/src/oblique.rs`。 | 确认（Phase 4 UI 优化） |
| D44 | 桌面编辑器内 Tab/Shift+Tab 必须用**具体 key context 的 action**接管 | **问题**：`gpui-base` 的 Root 插件在 `"Root"` 上下文绑定了 `tab`/`shift-tab`（焦点切换）；gpui 的按键派发在**命中 key binding 后直接执行 action、不再调用 `on_key_down`**（`window.rs` 中 binding 分支早退），因此 `#doc` 上的 `on_key_down` 永远收不到 Tab，`insert_tab` 成了死代码。**决策**：在 `mazha-doc` 上下文绑定更具体的 `tab`→`InsertTab`、`shift-tab`→`Outdent`，用 `on_action` 处理并 `stop_propagation`，避免落到 Root 的焦点切换。**行为**：无选区时 Tab 在光标处插入 4 空格；有选区时对选中的每一行整体缩进 4 格；Shift+Tab 去掉每行最多 4 个前导空格或一个 `\t`。多光标先合并重叠行区间、**从后往前**替换，并按每行前缀增删量重映射光标/锚点（落在被删空格内夹到行首）。查找条打开时 Tab 仍切换查找/替换焦点；命令面板/重命名打开时不缩进。 | 确认（Phase 4 UI 优化） |
| D45 | 桌面回车走核心 `IntentMapper`；空列表项样式由**容器自身**写入 | **问题**：桌面 `insert_newline` 一直直接插入 `"\n"`，核心 `Intent::Newline`（续列表、有序序号 +1、任务补框、空项退出列表）从未被调用；即使调用，空列表项（`- [ ] ` 无子块）在投影里也没有样式——`assign_style` 只把样式写给叶子块，`ListItem` 作为容器且 `children` 为空时不写任何行，导致"回车后新项的图标不显示，直到输入文字"。**决策**：① 桌面回车改为 `DefaultIntentMapper.map(Intent::Newline)`，有选区时仍按覆盖为新行处理；多光标从后往前应用；② `assign_style` **始终**给 `ListItem` 的标记行写继承的 `bullet/task/quote_depth`（仅判空不够：新项后跟缩进内容时子块非空，但子块不覆盖标记行）；③ `list_prefix` 识别缩进并保留到续行标记（嵌套项回车不再跳回顶层）。**行为**：列表/任务项回车立刻出现下一项的项目符号/复选框；在新空项上再次回车删除标记退出列表（即"两次回车结束列表"）；有序列表自动递增。 | 确认（Phase 4 UI 优化） |
| D46 | 表格观感（观感优先，不做列重排） | **问题**：表格只有浅底+外框，表头与表体无区别；隐藏的分隔行仍占一整行空白；单元格内容被 trim，渲染成 `\|a\|b\|` 很挤。**决策**：① 投影层给表格行标注 `TableLine::{Header, Delimiter}`，表头行在 UI 加粗+底色；② 分隔行在**非光标行**时高度归零，表头行加高约 12px 提供上下内边距（光标进入分隔行时恢复高度）；③ 保留单元格源码两端空格作为显示内边距，导出 HTML/DOCX 时裁掉；④ 表体分隔线从表体首行开始画。**不做**：按内容重排列宽/竖向网格线（当前按源码字符排版，重排会改动源文本或需要新的布局模型）、单元格 Tab 导航（留作编辑体验项）。 | 确认（Phase 4 UI 优化） |

| D47 | 表格按内容列宽 + 竖向网格线 + 单元格 Tab 导航（D46 的"不做"项升级为做） | **问题**：D46 后表格仍按源码字符排版，列宽随字符数变化、中文/英文混排对不齐，也没有竖向列边界；Tab 在单元格里只会插入 4 个空格，无法在单元格间移动。**决策**：① 核心投影给表体行也标 `TableLine::Body`，`ProjectedLine` 新增 `table_cells: Option<Vec<Range<usize>>>`（显示文本内、已裁掉竖线与两端空白的单元格字节范围；用 `SpanRole::TablePipe` 而不是字符 `\|` 判断，转义竖线不切列）；② 核心新增 `table::table_cells` / `cell_index_at`，与 `split_cells` 共用边界规则，供 UI 导航；③ 桌面端**整表测量**：逐单元格 `shape_line` 取自然宽，列宽 = 该列最大自然宽 + 2×10px（下限 40px），总宽 + 缩进 ≤ 可用宽度才启用网格；否则整表回退 D46 的流式排版（不压缩、不裁字、不换行）。列对齐沿用分隔行的 `Align`（左/中/右）；④ 网格模式逐单元格绘制文本、光标、选区与点击命中，表头行高 +12px 且文字垂直居中；竖向网格线画在内部列边界，表格装饰在编辑态也保留；⑤ Tab/Shift+Tab 在单光标无选区时按单元格跳转（同一行 next/prev，跨行跳过分隔行，到表格边界吞掉按键），**不**在末格自动加行/列。**限制**：含中文合成斜体（`oblique_spans` 非空）的表格整表回退流式；单元格不换行；表格行不做行内格式局部展开（展开会改变单元格字节范围）。 | 确认（Phase 4 UI 优化） |

| D48 | 表格内左右方向键按**显示文本**移动，跳过隐形标记与单元格分隔 | **问题**：0.1.68 网格化后，方向键仍按源码字符逐字节走——会依次落在隐藏的竖线、单元格两端填充空格和 `**`/`*` 等行内标记上；这些位置在网格里没有可见位移，表现为"连着按几次后需要按两次才动一下"。**决策**：表格行内左右移动改走核心 `table::cell_step(display, cells, offset, forward)`：在**显示文本**内逐字符走（隐藏标记本来就不在显示文本里），单元格末尾一步跳到下一格首字符、下一格首字符反向一步回到上一格末尾；本行到边界时跨到相邻表格行（跳过分隔行），表格边界移到相邻逻辑行行首/行尾；非表格行仍用普通字符移动。**取舍**：每条表格行移动时 `display_text()` 会分配一次字符串（表格规模小，可接受）；点击仍可落在竖线/填充附近的最近单元格，不额外改写源文本。 | 确认（Phase 4 UI 优化） |

| D49 | 表格命中测试用**单元格距离**选最近格，不能拿格内相对字节当距离 | **问题**：0.1.68 的 `table_cell_of_col` 把 `(cell, local)` 存进 `best`，比较时误把 `local` 当 `best_dist`。多字单元格末尾的 `local`（如 6）大于下一格到边界的距离（如 3），于是 caret 源位置虽在本格末尾（输入仍落本格），视觉 x 却跳到下一格首字符；再按右键才真正切换，表现为"多字单元格每到格尾就像被吞一次"，单字单元格 `local` 与下一格距离恰好相等而不复现。**决策**：抽出纯函数 `nearest_cell(cells, byte) -> (序号, relative)`，显式记录 `(index, local, dist)`，只在距离更小（相等保留靠前格）时替换；`table_cell_of_col` 只负责取几何。新增多字格尾/间隙/单字格尾回归测试。 | 确认（Phase 4 UI 优化） |

| D50 | 从表格外横移进入表格时，第一下就落到**单元格内** | **问题**：普通字符左右移动到表格行时，会先落在行首（首个 `|` 前）或行尾（末个 `|` 后）。网格渲染把行首/行尾都画在首/末单元格边界上，视觉看不出还隔着竖线；此时输入文字会落到竖线外，Markdown 把新增文字解析成新单元格（左侧多一格 / 右侧多一格）。**决策**：`move_left/right` 先走 `table_arrow_step`；未接管时用普通字符移动算出 raw target，再经 `table_entry_snap` 把表格行首吸附到首格显示文本起点、行尾吸附到末格显示文本末尾。只改横向移动，点击/Home/End 等既有行为不变；表格前若有空行，空行仍是独立一行，需再按一次才进表（正常行导航）。 | 确认（Phase 4 UI 优化） |

| D51 | 代码块右侧内边距与编辑区边界对齐，长行不得越出面板 | **问题**：0.1.60 给代码块加左右 padding 时，`wrap_width` 用了 `+24px`，而文本本身又右移 `12px`，导致代码文本右缘实际到 `content_right + 24px`；同时块级装饰统一用 `content_right + 16px` 作右缘，代码面板/表格框/分割线都顶出正文右边距，窗口边缘看起来被切掉。**决策**：新增 `CODE_TEXT_INSET = 12px`、`CODE_RIGHT_INSET = 20px` 常量与 `code_block_wrap_width`：面板右缘固定到 `content_width - BASE`（与正文右边距一致），代码文本右缘 = 面板右缘 − 20px，左右内边距对称；块级装饰右缘统一改为 `content_right`（不再 `+16px`）。新增几何单测断言长行右缘不越过面板。 | 确认（Phase 4 UI 优化） |

| D52 | 引用块底色限制在正文文本列内，左右各留 4px | **问题**：0.1.58 的引用底色相对**整个元素边界**左右各 4px，而正文文本列本身还有 `BASE=16px` 左右边距（块缩进从 `BASE` 起算）。结果底色左缘比正文左缘还靠左 12px、右缘超出正文右边距 12px；代码块边界修正后引用块更显得左右"探出"。**决策**：底色改为 `正文左缘 + BASE + 4px` 到 `正文右缘 − BASE − 4px`（即元素内缩 `BASE+4px`），左侧 3px 竖线跟随底色左缘；引用文字右侧额外收 `QUOTE_TEXT_RIGHT_INSET=16px`，使长行右缘距底色右缘 12px（与左侧留白对称）。嵌套引用仍共用同一条底色带，靠文字缩进体现层级。新增引用块几何回归测试，并用窗口截图核对底色从旧 `x=522..3131` 收到 `x=554..3099`（正文列内）。 | 确认（Phase 4 UI 优化） |

## 15. 变更记录

| 版本 | 日期 | 变更内容 |
|------|------|----------|
| 0.1.73 | 2026-10-24 | **修复引用块左右越界**（Phase 4，D52）：引用底色原相对整个元素左右各 4px，未计入正文 `BASE=16px` 边距，导致左缘比正文左缘靠左 12px、右缘超出正文右边距 12px。改为底色在正文文本列内左右各缩 4px，引用文字右侧再留 16px（长行距底色右缘 12px，与左侧对称）；竖线随底色左缘。新增引用块几何回归测试，截图核对底色收进正文列；core 196 / 总计 495 / 桌面 18 全绿 |
| 0.1.72 | 2026-10-24 | **修复代码块右侧越界**（Phase 4，D51）：代码块长行 `wrap_width` 误用 `+24px`，叠加文本 `+12px` 右移后面板内文字可超出编辑区 20px；块级装饰右缘又统一 `+16px`，代码面板/表格框/分割线顶出正文右边距。新增 `CODE_TEXT_INSET`/`CODE_RIGHT_INSET` 与 `code_block_wrap_width`，代码文本在面板内左右各留 20px、面板右缘与正文右边距对齐，块级装饰右缘去掉 `+16px`。新增几何回归测试；core 196 / 总计 495 / 桌面 17 全绿 |
| 0.1.71 | 2026-10-24 | **修复从表格外横移进入时首下按键落在竖线外**（Phase 4，D50）：普通字符移动进入表格行会先落在行首/行尾（首末竖线之外），网格视觉却像已在单元格内，此时输入会被 Markdown 解析成新单元格。`move_left/right` 新增 `horizontal_step`：表格内仍走 `table_arrow_step`；普通移动后由 `table_entry_snap` 将表头行首吸附到首格文本起点、末行行尾吸附到末格文本末尾。新增外部横移进表回归测试；core 196 / 总计 495 / 桌面 16 全绿 |
| 0.1.70 | 2026-10-24 | **修复表格光标视觉提前跳格**（Phase 4，D49）：0.1.69 已让方向键按显示文本移动，但 `table_cell_of_col` 用格内相对字节 `local` 冒充最近距离，多字单元格末尾 `local` 大于下一格间隙距离，导致源光标还在本格末尾（输入也落本格）时视觉已跳到下一格首字符，需再按一次才真正切换。抽出 `nearest_cell` 显式记录距离并只在更近时替换（相等保留靠前格）；新增多字格尾/间隙/单字格尾回归测试。core 196 / 总计 495 / 桌面 15 全绿 |
| 0.1.69 | 2026-10-24 | **表格内左右方向键跳过隐形字符**（Phase 4，D48）：0.1.68 网格表格隐藏竖线与单元格填充空格后，方向键仍按源码逐字符移动，会落在这些不可见位置，连续操作时出现"按一次没反应、要按两次"。核心新增 `table::cell_step(display, cells, offset, forward)`：在**显示文本**内逐字符移动（行内标记已隐藏，不会卡在 `**`/`*` 上），单元格末尾一步跳下一格首字符；桌面 `move_left`/`move_right` 在表格行改走该函数，到行边界跳过隐藏分隔行、到表格边界移到相邻逻辑行，非表格行行为不变。core 新增 2 条 `cell_step` 单测（含空单元格跨越），桌面新增 1 条方向键跨行/跳格测试；core 196 / 总计 495 / 桌面 14 全绿 |
| 0.1.68 | 2026-10-24 | **表格按内容列宽/竖向网格线 + 单元格 Tab 导航**（Phase 4，D47）：核心投影表体行新增 `TableLine::Body`，`ProjectedLine` 新增 `table_cells`（显示文本内、去掉竖线与两端空白的单元格范围，按 `TablePipe` 片段切分、转义 `\|` 不分列）；新增 `table::table_cells`/`cell_index_at`。桌面端对整表逐单元格测量（表头与数据行共用列宽，列宽取该列最大自然宽 + 2×10px、下限 40px），总宽放得下才启用网格：单元格独立形状化/绘制，点击命中、光标、选区、视觉行移动与 IME 边界全部走单元格几何；表头文字垂直居中，内部列边界画竖向网格线，编辑态也保留表格框线；列内文本按分隔行 `:--`/`:-:`/`--:` 左/中/右对齐。Tab/Shift+Tab 在单光标无选区时按单元格跳转，跨行跳过分隔行，表格边界吞键且不在末格自动加行；含中文合成斜体或超宽的表格回退原有流式排版；表格行不做行内局部展开。core 新增 4 条单测（2 投影 + 2 表格导航），core 194 / 总计 493 / 桌面 13 全绿 |
| 0.1.67 | 2026-10-23 | **表格观感优化**（Phase 4，D46）：表头行整行加粗并铺浅色底；表头后的分隔行（`| --- |`）不再占一整行——高度归零，改由表头行自身加高提供上下内边距（表头文字垂直居中），光标进入分隔行时恢复原高度以免出现看不见的光标；表体行分隔线从表体首行开始画，避免表头下两条线夹着空行；单元格两端空格保留为 WYSIWYG 内边距（此前投影 trim 掉，渲染成 `|a|b|`），导出 HTML/DOCX 时用 `trim_cell_spans` 裁掉。核心 `BlockStyle` 新增 `table_line: Option<TableLine>`（Header/Delimiter）。投影新增 1 条单测；累计测试数修正（0.1.63 起少计 1）。core 190 / 总计 489 / 桌面 13 全绿 |
| 0.1.66 | 2026-10-23 | **带嵌套内容的新列表项也即时显示标记**（Phase 4，D45 补充）：0.1.63 只在 `ListItem` **无子块**时由容器写样式；当新空项后面紧跟缩进内容（嵌套列表/续行）时子块让 `children` 非空，标记行没有叶子块覆盖、退回默认段落样式，序号/项目符号仍要等输入文字才出现（自带的示例文档正是"2. 有序项二"后接一段嵌套无序列表，故从代码启动看得到问题）。现改为**始终**给列表项的标记行写继承样式，子块再覆盖各自行。新增 2 条单测（投影层空有序项后接嵌套内容、文档层回车后 `3. ` 立即带 bullet），并用截图实测新建 `1. a\n2. b\n   - nested` 回车后第三行序号即时渲染。根 488 全绿 |
| 0.1.65 | 2026-10-23 | **有序列表标记兼容 `1)` 与行尾标记**（Phase 4，D45 补充）：`list_prefix` 与 `mazha-md` 扫描器对齐——有序标记接受 `.` / `)`（此前只认 `1. `，`1) a` 回车只插普通换行、不续行也不显示新项编号），无序标记允许 `-`/`*`/`+` 后无空格（行尾也算标记），并统一跳过标记后的所有空白后再识别任务框；空项判断改用 `get` 防越界。新增 2 条单测（`1) a`→`2) `、`9. a`→`10. `）。根 486 全绿 |
| 0.1.64 | 2026-10-23 | **引用内列表回车续行**（Phase 4，D45 补充）：`newline_edit` 先剥离行首引用前缀（支持 `> ` 与嵌套 `> > `，最多 3 个前导空格）再识别列表标记，续行时补回相同的引用层级——此前 `> - a` 回车只插普通换行，引用里的列表无法续行、新项也不会有项目符号/复选框；在引用内的空列表项上回车只删标记与缩进、保留 `> `，不会把整段引用退掉。新增 3 条单测（引用列表、嵌套引用任务、引用空项）。根 484 全绿 |
| 0.1.63 | 2026-10-23 | **列表/任务回车续行 + 新项图标即时显示**（Phase 4，D45）：桌面 `insert_newline` 此前只插 `"\n"`，未走核心 `Intent::Newline` —— 列表/任务项回车不会续标记，空项也不会退出列表。现改为 `DefaultIntentMapper.map(Intent::Newline)`：无选区时续行并补回标记（无序 `- `、有序序号 +1、任务 `[ ] `），空项再回车删除标记退出列表（"两次回车结束列表"）；有选区按覆盖为新行；多光标从后往前应用避免偏移串位。同时修复投影：**空列表项没有叶子子块**时 `assign_style` 不写样式，导致新行项目符号/复选框不显示（正是"提前显示前缀图标"的缺失点）——`ListItem` 为空时用继承上下文写自身行范围；`list_prefix` 现在识别并保留前导缩进，嵌套项回车不再跳回顶层。新增 4 条单测（嵌套无序/嵌套任务续行、空嵌套项退出、空任务项保留 checkbox）；CGEvent 实测 `- [x] done` 回车得到 `- [ ] `（带框），再回车退出列表。根 481 / 桌面 13 全绿 |
| 0.1.62 | 2026-10-23 | **桌面端：代码块支持 Tab / Shift+Tab**（Phase 4，D44）：此前编辑器内 Tab 完全无效——`gpui-base` 的 Root 上下文绑定了全局 `tab`，gpui 命中绑定后不再调用 `on_key_down`，`insert_tab` 从未被执行。现于 `mazha-doc` 上下文注册更具体的 `InsertTab`/`Outdent` action：**Tab** 无选区时插入 4 空格，有选区时把选中行整体缩进 4 格；**Shift+Tab** 去掉每行最多 4 个前导空格或一个 `\t`。多光标合并重叠行区间后从后往前应用编辑，并重映射光标/锚点；查找条打开时 Tab 仍切换查找/替换焦点。已用 CGEvent 自动按键验证：代码行 Tab 插入 4 空格、Shift+Tab 撤销、多行选区整体缩进/反缩进均正确；桌面 fmt/clippy/test/build 全绿 |
| 0.1.61 | 2026-10-23 | **桌面端修复：代码块光标与文本错位**：0.1.60 给代码块文本加了 12px 左内边距（`text_x = line_x + 12px`），但光标竖线与选区矩形仍按未偏移的 `line_x` 计算——表现为光标压在左侧字符上（重叠），且行末光标停在最后一个字**之前** 12px，看起来"到不了行尾"。改为在块绘制开头统一计算 `text_x`，选区（`text_x + x0`）、光标（`text_x + p.x`）、文本、合成斜体全部使用同一原点。桌面 fmt/clippy/test/build 全绿 |
| 0.1.60 | 2026-10-23 | **桌面端三项渲染优化：中文斜体真正倾斜 / 行内代码可见底色 / 代码块浅底 + Typora 式就地显源码**（Phase 4，D43）：① **中文斜体合成倾斜**：实测 macOS 中文字体（PingFang SC 等）没有斜体字面，且给 run 挂 CJK `font.fallbacks` 并**不能**让 CoreText 合成倾斜（`kCTFontSlantTrait` 仍解析为 Regular，`NSFontManager` 对 CJK 也是空操作）。改为**渲染器级合成**：新增 `platforms/desktop/src/oblique.rs`——`split_segments` 按字符把斜体文本切成 CJK/非 CJK 段，`primary_covers` 用 CoreText 解析主字体族名（`.SystemUIFont`→`.AppleSystemUIFont`，按族名校验，避免不存在的名字静默退化为 Helvetica）判断主字体是否已覆盖该字符；`glyph_path` 用 `CTFontCreatePathForGlyph` 取字形轮廓，按 `x' = x + tan(12°)·y` 做剪切并生成绝对坐标的 gpui `Path`，字体对象按族名+字号缓存。`to_gpui_runs` 对 `Emphasis`/`Math` 的 CJK 片段生成**透明占位 run**（`a=0`，保留 advance/光标/选区/换行）并收集 `ObliqueSpan{range,color}`；`BlockGeom` 增 `oblique_spans`/`font_size`；paint 阶段文本绘制后按视觉行 `position_for_index` + `ascent/descent` 求基线，`window.paint_path(path, color)` 填充倾斜字形。② **行内代码底色可见**：`Palette` 新增 `code_bg`（foreground + alpha 0.08），作为 `RunStyle::InlineCode` 的 `TextRun.background_color` 交给 gpui——macOS 端 `WrappedLine::paint` 会在各 run 的视觉行范围内 `paint_quad`，实际可见（此前底色不可见是色值过淡/未设）。③ **代码块浅底 + 左右 padding + 就地显源码**：新增 `Palette.code_block_bg`（foreground + alpha 0.06，浅色主题约 #f0f0f0），代码块面板整块铺底（编辑态也铺）；`block_indent` 对代码块加 `base*1.5`、`wrap_width` +24px、文本 `text_x` +12px，使文字不贴面板边缘。光标进入**围栏**代码块时，该块所有围栏行就地显出 ``` 源码（等宽 + muted）：`BlockGeom` 新增 `fence_src`，`byte_of_col`/`col_of_byte`/`source_of_col`/`col_of_source` 在围栏行改走"字符列 ↔ 行内 UTF-8 字节"1:1 映射（`char_col_to_byte`），prepaint 扫描光标所在连续 `code_block` 行、按 `source_range` 切源码并以 ` ``` `/`~~~` 前缀筛出围栏行，移开光标即复原。桌面端新增 `oblique` 模块单测与坐标映射单测（累计 13 条，含 CJK 斜体产出透明 run + span、CoreText 可取 CJK 轮廓、围栏行字符列→字节映射）；桌面 fmt/clippy(含 `--all-targets`, `-D warnings`)/test/build 与根 workspace 门禁全绿 |
| 0.1.59 | 2026-10-21 | **桌面端：行内格式局部展开推广到粗体/斜体/删除线/行内代码**（Phase 4，D42 同机制）：把 0.1.56 的"链接局部展开"从"只处理链接"推广为**通用行内格式组展开**——UI 侧 `inline_reveals(line_src)` 解析一行内的两类可展开组：① 链接组（`[`/`](`/URL/`)`，URL 为空不成组）；② 同类定界标记成对组（`SpanKind::Marker` 且 role 为 `Strong`/`Emphasis`/`Strikethrough`/`InlineCode`，按出现顺序两两配对，标记已被用过的不再参与配对）。`reveal_inline(line, line_src, reveal)` 只把**当前光标所在组**的标记还原为可见原文并重建该行 `OffsetMap`（行首为基准），同行其它组仍隐藏；守卫沿用 0.1.56 两条（标记必须真被投影隐藏、该行须有可见片段）。光标离开即复原。桌面端新增 1 条单测（粗体/斜体/删除线/行内代码四类均可展开且展开后等于原文）；桌面 fmt/clippy(含 `--all-targets`)/test/build 与根 workspace 门禁全绿 |
| 0.1.58 | 2026-10-21 | **桌面端微调：引用块更浅并加内边距**：`quote_bg` 由 foreground+alpha 0.06 收至 0.04，底条上下各 +2px（`row_top - pad_y`，高度 +4px），左右仍留 4px，左侧 3px border 竖线随底条同步；**上下 padding 只加在最外层引用（`quote_depth == 1`），嵌套引用不加**；**各层引用都铺底色 + 画左侧竖线**（嵌套层不另画竖线，见 0.1.58 后续修正）。实机截图确认引用行底色 #f5f5f5（正文 #ffffff）、竖线清晰。 |
| 0.1.57 | 2026-10-21 | **桌面端修复：引用块底色在浅色主题下不可见**（0.1.56 遗留）：`Palette.quote_bg` 原由 `theme.muted` + alpha 0.18 得到，但浅色主题的 muted 本身接近白色（实测约 #f3f3f3，叠在白底上只差约 2/255，肉眼不可辨）。改为由 **`theme.foreground` + alpha 0.06** 混合（浅色约 #f0f0f0、深色约 #232527，均与背景拉开可辨差距），并保留左侧 3px `palette.border` 竖条作为边界。已用窗口截图逐像素核对：引用行整行 240/240/240，正文区 255/255/255，竖条 229/229/229。桌面 fmt/clippy/test/build 全绿 |
| 0.1.56 | 2026-10-21 | **桌面端 UI 优化：链接局部展开 + Cmd+点击 + 配色**（Phase 4，D42）：① **链接局部展开**：光标落入 `[文字](目标)` 时，仅该链接恢复原文（`[`、`](`、URL、`)` 重新可见，便于继续编辑），移开后复原；实现为 UI 侧用 `mazha-md::parse_inline` 分组（`inline_links`）→ `reveal_link_line` 把标记作为普通 run 插回并重建该行 `OffsetMap`（以行首为基准，`BlockGeom::revealed` 标记后 `source_of_col` 加回行起点）；光标/选区/IME/上下移动列统一走新的 `BlockGeom::col_of_source`，避免"显示字节 == 源字节"的错误假设（同行其它标记仍隐藏）。这不改变核心投影，§11.2 整行原文仍留后续。② **Cmd+点击打开链接**：`DocView::link_at` 复用同一套分组取 URL（剥掉可选 `<...>`），`App::open_url` 交系统默认应用，不移动光标。③ **行内代码配色**：文字由主题 accent 改为 `foreground`（accent 在浅色底下对比度偏低）；④ **引用块浅底**：新增 `Palette.quote_bg`（muted，alpha 0.10），每个引用逻辑行整行铺底。展开带两道守卫（标记必须真被投影隐藏，避免代码块整行可见文本被重复插入；行内无可见片段时不展开，避免 Front-matter/公式行只剩标记碎片）；桌面端新增 `mazha-md` 依赖与 5 条纯 Rust 单测（链接分组/空目标/多链接/展开后偏移可逆/代码块不展开）；桌面 fmt/clippy(含 `--all-targets`)/test/build 与根 workspace 门禁全绿 |
| 0.1.55 | 2026-10-21 | **桌面端修复：滚动 / 光标跟随 / IME 组合**（纯 UI 层，核心无改动）：① **滚动结构**：原 `Scrollable` + `vertical_scrollbar` 把滚动条放在滚动内容内，滚动条随内容位移、拖动落点错位；改为 `DocView` 自持 `ScrollHandle`（`track_scroll`，因 `use_keyed_state` 的全局 id 含元素栈上下文，无法与 `Scrollable` 内部句柄共享），滚动条改为滚动容器**外层兄弟**叠加 `Scrollbar::vertical`。② **滚动跟随**：`set_offset` 只改句柄不触发重绘，方向键移动光标后画面停在旧偏移；改为改完偏移即 `cx.notify()`。③ **命中映射**：空行/纯隐藏语法行（图片行、围栏行）的 `OffsetMap` 无可落点，`display_to_source` 退回 0（文首），点击空行或上下移动经过空行会跳文首；新增 `BlockGeom::source_of_col`（显示为空 → 行首，非空钳进本行 `source_range`）。④ **IME**：`EntityInputHandler` 的区间参数/返回值为 **UTF-16** 码元下标，此前 `text_for_range`/`replace_text_in_range`/`replace_and_mark_text_in_range`/`marked_text_range` 误当 UTF-8 字节；同时 `setMarkedText:`/`insertText:` 更新或提交组合时常传 `None`（NSNotFound），此前退回到选区/光标导致每帧组合串再插一份（拼音字母逐个上屏）；新增 `ime_target`（UTF-16 区间 → marked 区间 → 选区/光标）并移除按键处理里的"可打印字符直插"兜底（字符输入完全交给 IME）。桌面端 fmt/clippy/test 与根 workspace 门禁全绿 |
| 0.1.54 | 2026-10-20 | **桌面端修复：布局 + 光标 + 原生对话框 panic**（Phase 4，SPEC §14 D41）：① **布局**：`#doc` 主列此前只含标签栏，信息条/查找条/命令浮层/编辑区（`DocElement`）被错误地挂在 `#app` 行容器下，导致编辑区被横向挤压、标签栏占据中部大块空间；改为 `#doc`（纵向 flex）依次包含标签栏 → 信息条 → 查找/命令浮层 → 编辑区，并把按键/鼠标处理与滚动（`overflow_y_scrollbar`）移到 `#doc`。② **光标**：竖线颜色由 `palette.accent`（主题强调色，偏浅）改为 `palette.foreground`（正文前景色，对比度最高），线宽 1.5→2px。③ **panic**：`Cmd+O` 等同步打开原生对话框会与 gpui 键盘布局回调争用 `RefCell`（`app.rs` `RefCell already borrowed`）；`open_file_dialog`/`save_as`/`move_current_file_dialog`/`export_pdf` 改为用**异步** `AsyncFileDialog` + `cx.spawn`（**非** `window.defer`，defer 仍同借用内执行无效，D41）。桌面端 fmt/clippy/build 全绿 |
| 0.1.53 | 2026-10-20 | **桌面大纲 / 目录导航**（Phase 4，SPEC §9.2/§14 D39）：纯 UI 增量，核心无改动。桌面端新增 `DocView.outline_open` 与 `outline_items()`（扫描 `Projection::lines` 中 `block.heading` 非空的行，取 `display_text()` 作标题、`source_range.start` 作跳转目标）、`goto_source(offset)`（光标移到指定源偏移）；侧栏新增「大纲」分区，标题按 `level` 缩进（每级 +8px）、点击跳转；`Cmd+Shift+O` 与命令面板「切换大纲」切换显隐。桌面端 fmt/clippy/build 全绿（本环境无法可视化验证） |
| 0.1 | 2026-10-12 | 初始规范（规划版）：完成平台矩阵、总体架构、核心模块与契约草案、里程碑与决策记录 |
| 0.1.1 | 2026-10-12 | Phase 1 启动：落地 `mazha-core` 最小可用内核（Document/Edit/Selection/ChangeSummary + undo/redo + 边界校验），契约 §6.1 同步实现态签名；接入 ropey 1.6 + thiserror 2；新增约定 AGENTS.md（规范驱动 + 必备注释 + CI 命令） |
| 0.1.2 | 2026-10-12 | 内核迭代：落地 `OffsetMap`（可见区间模型 + src↔display 双向映射 + 交叉点稳定落点规则），契约 §6.1 补充实现细节；12 条新单测 |
| 0.1.3 | 2026-10-12 | `mazha-md` 起步：行扫描块级解析（ATX/Setext 标题、段落、分割线、围栏代码、引用、有序/无序/任务列表）+ `SyntaxSpan`/`visible_runs`；决策 D23（自研行扫描，pulldown-cmark 降为 dev 依赖）与 §8 选型更新；16 条单测 |
| 0.1.4 | 2026-10-12 | `mazha-md` 行内解析：强调/粗体/删除线（递归嵌套 + 颜色继承）、行内代码（等长反引号 + 空格修剪）、链接（URL 隐藏）、图片（Widget 占位）、转义、自动链接；接入段落/标题叶子块；硬换行标记；13 条单测（累计 29） |
| 0.1.5 | 2026-10-12 | 落地 `Projection`（`mazha-core` 依赖 `mazha-md`）：按逻辑行投影出 `BlockStyle`/`TextRun`/`Widget` + 每行 `OffsetMap`，`Document::projection()` 全量重建；13 条单测（累计 47）。确认决策 D23 并新增 `docs/ADR/ADR-001`（自研解析 vs pulldown-cmark 对比）；CI/AGENTS 的 clippy 升级为 `--all-targets` |
| 0.1.6 | 2026-10-12 | 编辑命令：新增 `Edit::ToggleInline`/`ToggleBlock` + `InlineMark`/`BlockTarget`；`commands` 模块（行内包裹/解包裹、块前缀逐行增删/剥离）；`Document::apply` 解析切换命令并纳入 undo/redo；`IntentMapper` 最小实现（回车列表续行/递增序号/空项退出、切换转发）；22 条单测（累计 69）；SPEC §6.1 契约同步 |
| 0.1.7 | 2026-10-12 | 落地 `mazha-store`：`StorageProvider`（同步，决策 D24）/`FileSystemProvider`/`DocPath`（校验+根路径）/`Entry`/`StorageCaps`/`StoreError`；12 条单测（累计 81）；SPEC §6.2 契约改为同步接口并记 D24 |
| 0.1.8 | 2026-10-12 | `mazha-core` 依赖 `mazha-store`：落地 `Workspace<S: StorageProvider>`（`open`/`save`/`list`，UTF-8 校验、存储错误透传）；重导出 `DocPath`/`StorageProvider`；3 条单测（累计 84） |
| 0.1.9 | 2026-10-12 | 落地 `mazha-export::to_html`：基于 `mazha-md` 块/片段模型渲染 HTML（标题/段落/引用/列表/任务表/围栏代码/分割线 + 行内强调/粗体/删除线/代码/链接/自动链接/图片/换行，HTML 转义）；11 条单测（累计 124）；SPEC §8 HTML 导出选型改为自研同源渲染 |
| 0.1.10 | 2026-10-12 | `mazha-crdt` 基于 `yrs` 封装：`CrdtDoc`（`Y.Text` 根 `content`）、字符索引（UTF-8 字节转换）、`insert/delete/replace`、状态向量+增量更新（Yjs v1）、`apply_update`、并发收敛测试；6 条单测（累计 130） |
| 0.1.11 | 2026-10-12 | `mazha-md` 新增 `serialize_source`/`to_md`（极简 AST→Markdown MVP）；补充 6 条回归测试（段落/标题/列表/代码块/引用/任务/行内/链接），修复链接序列化丢失 URL 的问题（`[text](url)` 整体重建）；累计 37 条（总计 141） |
| 0.1.12 | 2026-10-12 | `mazha-export` 新增 `to_html_document`（完整 HTML5 外壳 + 默认 CSS + title 转义）；3 条新单测（累计 14，总计 144） |
| 0.1.13 | 2026-10-12 | `mazha-core` 依赖 `mazha-crdt`：新增 `crdt` 桥接模块（`document_to_crdt`/`crdt_to_document`/`apply_edit_to_crdt`，Edit 字节偏移 ↔ CrdtDoc 字符索引安全转换，CJK 安全）；`Document::apply_and_sync`（编辑+同步原子操作）；1 条集成单测（core 77，总计 149） |
| 0.1.14 | 2026-10-12 | `Document::apply_and_sync` 集成测试补充（CRDT 往返 + 投影行数一致）；修 clippy（未用导入、`# Panics` 文档）；core 78，总计 150 |
| 0.1.15 | 2026-10-12 | 协同闭环：`CrdtDoc::fork`（同状态多端副本）、`mazha_core::apply_remote`（远端更新帧 → `CrdtDoc` 并入 → `Document` 整体重建，返回是否变化）；双端收敛/非法帧不改文档测试；core 80 + crdt 8（总计 154） |
| 0.1.16 | 2026-10-12 | CRDT 持久化：`mazha_core::{save_crdt,load_crdt}`（更新帧经 `StorageProvider` 落盘/读回，`CrdtSyncError` 统一 Store/CRDT 错误）；3 条测试（存取往返+继续编辑收敛、缺文件、损坏帧）；core 83（总计 157） |
| 0.1.17 | 2026-10-12 | `mazha-md::reparse` 增量重解析：按旧块森林定位受影响块区间（前后各放宽一块）→ 局部重解析 + 未受影响块 range 平移 `delta`；短文档/高命中比例/区间非法时退化为全量；8 条"增量==全量"对照测试（CJK、删空行合并、代码块内插入、标题前缀、末尾追加等）；md 45（总计 165） |
| 0.1.18 | 2026-10-12 | 增量重解析**健全性加固**并接入 `mazha-core`：① `reparse` 增加裁切点安全校验（tail 前必须是空行、区域末块不能是未闭合围栏），不安全时整段重解析到 EOF——修复"插入破坏围栏导致构造延伸"的错误结构；② `Document` 维护块缓存（编辑后经 `reparse` 增量刷新，`blocks()`/`projection()` 复用，`projection::build_from`）；③ 2 条块缓存一致性测试 + 10 步编辑序列测试 + 2 条裁切安全性回归测试；core 85 + md 48（总计 170） |
| 0.1.19 | 2026-10-12 | `Projection::refresh` 增量刷新：定位脏行 → 样式全量重算 + 只重建脏行 runs/`OffsetMap` → 后续旧行整体平移源坐标（`OffsetMap::shift`、`ProjectedLine::shifted`）；`Document` 持有投影缓存，`projection()` 改为 `&mut self -> &Projection` 并随编辑增量刷新；新增内部辅助 `old_range_of`/`new_range_of`（精确旧/新坐标对，修正 `Edit::source_range` 插入语义）；2 条"增量==全量"逐字段比对测试（6 类编辑 + 撤销/重做 + OffsetMap 双向换算逐点一致）；core 87（总计 172） |
| 0.1.20 | 2026-10-12 | `IntentMapper` 增强：新增 `Intent::Indent`（块级缩进/反缩进——选区扩展到整行，单个原子 `Replace`，一次一级=两空格或一个制表符，无改动则不产出编辑）与 `Intent::DeletePair`（光标位于空配对符号 `()[]{}""''``**__~~` 之间时成对退格）；10 条新测试；core 97（总计 182） |
| 0.1.21 | 2026-10-12 | `mazha-md` 序列化重写（嵌套结构正确性）：引入**前缀穿线**（`block_to_md(source, block, prefix)` + `prefix_lines` 逐行补前缀），修复嵌套列表无缩进、多行引用/段落续行漏前缀两处真实缺陷；顶层块间空行分隔、结果单换行收尾；新增 9 条往返测试（嵌套有序/无序/任务/引用、混合文档结构保持、规范化输入逐字节往返 + 幂等性）；md 57（总计 191） |
| 0.1.22 | 2026-10-12 | `mazha-crdt` 新增 `push()`（末尾追加）与 `snapshot()`（快照压缩：CRDT 历史清零、文本保留，把更新帧体积从 O(历史) 收敛到 O(文本)），并明确压缩与远端同步的兼容性代价；3 条新测试（体积显著下降、快照后续编辑、fork 与 snapshot 的历史差异）；crdt 11（总计 194） |
| 0.1.23 | 2026-10-12 | `Document` 导航助手（供 UI 光标操作/双击选词）：`line_range_of`（逻辑行范围，换行符位置归前一行、EOF 后为空行）与 `word_range_at`（CJK 感知——ASCII 词 `[0-9A-Za-z_'-]`、CJK 连续段成词，两侧词互不粘连，光标紧贴词的一侧归该词，空白/标点处为空区间；多字节中部偏移先吸附）；9 条新测试；core 106（总计 203） |
| 0.1.24 | 2026-10-12 | ① `Intent::InsertText` 改为携带**选区**（空区间=纯插入，非空=覆盖替换为原子 `Replace`，一次 undo 还原）+ 新增 `Intent::DeleteSelection`；② `Workspace` 文件管理 API：`create`（`read()` 探测存在性，父目录不存在也能建）、`remove`、`rename`、`list_markdown`（只列 `.md`/`.markdown` 文件）、`find_markdown`；8 条新测试；core 114（总计 211） |
| 0.1.25 | 2026-10-12 | **GFM 表格**支持（方言声明的核心特性补齐）：`BlockKind::Table { align: Vec<Align> }` + `Align`；解析要求"表头行含 `|` + 分隔行 `-`/`:` + 连续数据行"，分隔行决定列数与对齐，单元格按行内语法解析，竖线为可见 `TablePipe`、分隔行为隐藏 `TableDelim`；序列化按对齐重写分隔行（紧跟表头）；HTML 导出真 `<table>/<thead>/<tbody>` + 列对齐样式；投影侧表格行即普通行（竖线可见）；8 + 5 条新测试（含转义竖线、仅表头、字节往返）；md 65 + export 19（总计 224） |
| 0.1.26 | 2026-10-12 | 表格投影验证：`Document` 层补 2 条测试——① 表格投影显示文本为可见片段拼接（竖线可见、分隔行整行隐藏为空、单元格内容 trim 后无空格）；② 追加表格行后增量刷新投影与全量重建逐字段一致（`assert_projection_matches`）。core 116（总计 226） |
| 0.1.27 | 2026-10-13 | **Callout（提示块）**知识库扩展：`BlockKind::Callout { kind: CalloutKind, title: Option<String> }` + `CalloutKind`（Note/Tip/Important/Warning/Caution + Custom）；解析识别 `> [!TYPE]` 标记（大小写不敏感，空名 `[!]` 仍为普通引用），`CalloutMarker` 隐藏片段，自定义标题单独存储；序列化 `> [!TYPE]` + 标题 + 子块 `> ` 前缀；HTML 导出 `<div class="callout callout-{type}">` + 标题 + 内容；投影按引用深度（quote_depth）；15 条新测试（md 9 + export 5 + core 1）；md 74 + export 24 + core 117（总计 241） |
| 0.1.28 | 2026-10-13 | **Wiki-link** 知识库扩展：`SpanRole::{WikiLinkMarker, WikiLinkTarget, WikiLinkText}`；行内解析 `[[Page]]`（目标可见）/ `[[Page|Alias]]`（别名可见、目标隐藏），空内容/空别名不构成；序列化 `[[Target]]`/`[[Target|Alias]]` 重建（`reconstruct_wiki_link`）；HTML 导出 `<a href="目标" class="wikilink">显示文本</a>`（href 转义）；12 条新测试（md 9 + export 3）；md 83 + export 27（总计 253） |
| 0.1.29 | 2026-10-13 | **Front-matter** 知识库扩展：`BlockKind::FrontMatter { content: String }`（原始 YAML 文本）；解析仅识别文档首行恰为 `---`（无前导空格）且后续有闭合 `---`/`...`，否则按分割线/段落处理；序列化 `---` + 原文 + `---`（末尾不带换行，由 `serialize_source` 补）；HTML 导出隐藏（元数据不渲染）；投影为无片段空行；10 条新测试（md 8 + export 1 + core 1）；md 91 + export 28 + core 118（总计 263） |
| 0.1.30 | 2026-10-13 | **全管线集成测试**：`full_pipeline_all_features`——单文档覆盖 front-matter/标题/段落（含全部行内语法+wiki-link）/提示块/引用/列表/任务/表格/代码块/分割线/图片；验证①块种类齐全 ②序列化逐字节往返 ③追加段落后增量投影与全量一致。core 119（总计 264） |
| 0.1.31 | 2026-10-13 | **Tags** 知识库扩展（方言特性补齐）：`SpanRole::Tag`（可见）；行内解析 `#tag`/`#nested/tag`/CJK 标签（Unicode 字母数字 + `-`/`_`/`/`），词中 `a#b` 不误判、末尾 `/` 拒绝、`\#` 转义排除；序列化 Text 片段自动往返；HTML 导出 `<span class="tag">`；10 条新测试（md 8 + export 2）；md 99 + export 30（总计 274） |
| 0.1.32 | 2026-10-13 | **Typst 标记导出**（PDF 管线第一步）：`mazha-export::to_typst`——Markdown→Typst 标记纯文本转换（标题 `=`/段落/行内 `*`/`_`/`` ` ``/`#link`/`#strike`/列表 `-`/`+`/代码块围栏/引用 `#quote`/表格 `#table`+对齐/提示块 `#block`/wiki-link 可见文本/标签 `#text(fill: blue)`/图片 `#image`）；**编译 PDF 字节需 Typst `World` 实现（后续补）**；16 条新测试；export 46（总计 290） |
| 0.1.33 | 2026-10-13 | **PDF 编译**（PDF 管线第二步，D25）：`mazha-export::to_pdf`/`PdfExporter`——`ExportWorld` 实现 `typst::World`（虚拟主文件 + `base_dir` 资源根 + `typst-kit` 内置/系统字体）→ `typst::compile` → `typst_pdf::pdf`，返回 `PdfDocument{bytes,warnings}`；`PdfExportOptions{base_dir,system_fonts}`、`PdfError::{Compile,Export}`；标记侧修复 4 处可编译性问题——表格改 `align:` 数组 + 逗号分隔实参、图片 alt 改命名字符串参数、块间空行、Markdown 反斜杠转义需二次转义（新增 `escape_typst`/`escape_typst_string`）；workspace `rust-version` 1.85→1.92（typst MSRV），修 rustc 1.98 新增 `collapsible_if`；14 条新测试（export 60，总计 **304**） |
| 0.1.34 | 2026-10-14 | **Phase 2 起步：桌面只读投影渲染**（§7.1）：新建 `platforms/desktop`（独立 workspace，`gpui-kit` 0.7 锁定 GPUI 快照，`mazha-core` 路径依赖）→ `mazha-desktop`：读 `.md`（或内置示例）→ `Document`/`Projection` → 逐行绘制（标题字号字重、列表符号、任务复选框、引用缩进、代码块等宽+弱底、行内 粗/斜/删除线/行内代码/链接、图片占位、分割线），主题色跟随深浅色；`cargo run -- [path.md]` 可直接打开文件；§7.1 依赖形态按现状修正为 crates.io `gpui-pre` 家族（由 gpui-kit 锁定）；fmt/clippy/build 门禁覆盖该 crate |
| 0.1.35 | 2026-10-14 | **桌面光标/点击定位 + WYSIWYG 标记显隐**（§11.2）：`DocView` 增 `caret: usize`（**源字节偏移**，真相在源文本）+ `FocusHandle`。**点击定位**：每行 `TextLayout::index_for_position` 命中（窗口坐标→显示字符索引，Err 取最近字符边界）→ 光标行经"行首源偏移 + 索引"、非光标行经 `OffsetMap::display_to_source`（交叉点取更小源偏移=隐藏标记起点）→ `Document::snap_to_boundary`；每行均接 `on_click`（含光标行，可跨行跳转）。**标记显隐**（§11.2）：光标所在行渲染 `source[source_range]` **原文**（`#`/`**`/`-`/`>` 等标记可见、不画列表/任务前缀），其余行投影隐藏。**光标竖线**：文本前插零宽 `canvas`，`position_for_index`（返回**绝对窗口坐标**）+ `paint_quad` 画 1.5px 竖线；行内全部子元素先 prepaint 再 paint，故 prepaint 后的 layout 可安全命中。键盘 `left/right/up/down/home/end`（组合键交系统；上下移动按行内相对位置 + `snap_to_boundary`）。门禁：fmt/clippy/build 全绿，`cargo run` 启动无 panic（进程存活、日志为空） |
| 0.1.36 | 2026-10-16 | **DOCX 导出**（D26）：`mazha-export::to_docx`（基于 `docx-rs` 1.1 生成 OOXML，不经 HTML 中转）——标题（直接格式 加粗+字号）/段落/引用/提示块/列表（有序/无序/嵌套）/任务列表/GFM 表格（含列对齐）/围栏代码块（逐行保留换行）/分割线/行内 粗/斜/删除线/行内代码/链接（「文字 + (URL)」）/自动链接/wiki-link/标签/图片占位；front-matter 隐藏；文本经 `strong-xml` 自动转义 + `xml:space="preserve"`；`DocxError`（`docx::DocxError` 仅 `Debug`，转字符串）。边界处理：空输入仍产出合法 ZIP（`PK\x03\x04`）、代码块按行插换行、span 原样空白保留、块 `range` 越界防御。13 条新测试；export 73（总计 **317**） |
| 0.1.37 | 2026-10-16 | **EPUB 导出**（D27）：`mazha-export::to_epub` / `to_epub_with_meta` / `EpubMeta` / `EpubError`——Zip + XHTML 打包 EPUB 3（`mimetype` 首条目且 `Stored` 不压缩、`META-INF/container.xml`、`OEBPS/content.opf`、`nav.xhtml`、`toc.ncx`、`style.css`、单一 `text.xhtml`）；HTML 渲染新增 **XHTML 方言**（`html::to_xhtml_body`，空元素自闭合、布尔属性带值，`to_html` 行为不变），正文与 HTML 导出同源；`dc:identifier` 缺省用 FNV-1a 内容哈希派生、`dcterms:modified` 固定时间戳（输出确定）；元数据经 XML 转义。边界：空输入仍为合法 EPUB、空标题/语言回退、任务复选框 XHTML 化、`xmllint` 校验全部 XML 部件良构。15 条新测试；export 88（总计 **332**） |
| 0.1.38 | 2026-10-16 | **导出主题/模板**（D28）：新增 `mazha-export::theme` 模块——`Theme`（HTML/EPUB 的 CSS：内置 `light`/`dark`/`sepia`，可 `custom`）与 `Template`（PDF/Typst 版式前置：内置 `plain`/`article`，可 `custom`），均为确定性常量。接入：`to_html_themed`（主题 CSS 注入 `<style>`）、`to_epub_themed`（主题作 `style.css`，`EpubMeta` 复用）、`to_typst_with_template`（前置以空行分隔正文）、`PdfExporter::export_with`/`to_pdf_with`（编译带模板标记；`plain` 与旧行为逐字节一致）。`Template::article` 只设页面/字号/段距、不设字体族（保留 CJK 回退）。10 条新测试；export 98（总计 **342**） |
| 0.1.39 | 2026-10-16 | **PDF 真实文档回归**（风险 R8）：新增集成测试 `tests/pdf_real_doc.rs` + 基线文档（中文技术报告 `tests/data/real_report.md` + SVG 示意图）——验证多页排版（`PdfDocument` 新增 `pages` 字段）、逐字节可复现、标记转义扫描（`#`/`[]`/`` ` ``/`$`/`*_`/`~=`/`//`/`@`/`<>`/中文标点/emoji/组合字符）。回归发现并修复 `to_typst` 行内代码缺陷：内容含反引号时反引号围栏会与内容粘连成更长串导致 `unclosed raw text`，改用 `#raw("…")`；同时行内代码内容不再走 `escape_typst`（Typst raw 不解析标记，原实现会显示出多余反斜杠）。2 条新单测 + 3 条集成测试；export 100 单测 + 3 集成（总计 **347**） |
| 0.1.40 | 2026-10-16 | **文档级跨行坐标映射**（Phase 3 起步，SPEC §6.1/§11.3）：`mazha-core` 新增 `DocPos{line,col}`、`LineDisplayRange{line,cols}` 与 `Projection` 方法 `line_of_source`（行尾边界归前一行）、`source_to_doc_pos`（落隐藏标记吸附标记起点）、`doc_pos_to_source`（命中换算，列/行越界钳制）、`display_len_at`、`selection_display_ranges`（跨行选区→每行显示列区间，首行到行尾/中间整行/末行到终点）。把原先散落在 `platforms/desktop` 的行归属与选区换算收敛为核心纯 Rust API（桌面端改用留待下一提交）。先测试后实现，10 条新单测（含空投影、CJK 多字节、隐藏标记吸附、反向选区）；core 129（总计 **357**） |
| 0.1.41 | 2026-10-16 | **桌面端接入核心文档级 API + 修复历史编译债**（D29，Phase 3 后续）：`platforms/desktop` 改用 `Projection::{line_of_source, source_to_doc_pos, doc_pos_to_source, selection_display_ranges}`——行归属、上下移动按 `DocPos` 保留**显示列**（修正 CJK 字节相对偏移缺陷）、选区高亮在 prepaint 折成每行显示列（paint 直接按列画，删除源码→显示二次换算）、光标竖线改存 `caret_col`、IME 候选框用 `source_to_doc_pos` 定位（跨行覆盖到终点所在行）。**修复历史编译债**：`platforms/*` 独立 workspace 不在 `cargo --workspace` 门禁内，此前多光标/PDF 导出提交未过 `cargo build`，桌面端一度**无法编译**（1 处 `crate::Projection` 未解析 + 20 处借用冲突，均因 `cursors_mut(&mut self)` 借用整个 `self` 与 `self.doc` 相交）。改为按字段借用（`&mut self.cursors`）、先算后写；删除死代码（`line_index`/`primary_line_index`/`cursors_mut`/`has_selection`/`apply_edit_all`/`ranges_overlap`）。**同步独立 `Cargo.lock`**：`zip 0.5` 全系被 yank，桌面端独立解析失败，补齐 `docx`/`zip`/`strong-xml` 等缺失锁定项（含 yanked 版本锁定）。门禁：根 workspace fmt/clippy/test 全绿（357），桌面端 `fmt`/`clippy -D warnings`/`build` 全绿 |
| 0.1.42 | 2026-10-16 | **块级 widget 模型**（Phase 3，SPEC §6.1/§11.5，D30）：`mazha-core` 的 `Projection` 新增 `widgets: Vec<BlockWidget>`——把代码块/表格/分割线聚成**整块**模型（`source_range` 整块 + `line_range` 逻辑行 + `BlockWidgetKind::{Code{lang,fenced}, Table{align}, Rule}`），供 UI 按"光标不在块内=渲染态、进入=编辑态"整体绘制。新增查询 `block_widgets()`/`block_widget_at(src)`/`block_widget_on_line(line)`；`build_from`/`refresh` 均重算 widgets（refresh 由新块森林全量重算，O(块数)）；core 新增导出 `BlockWidget`/`BlockWidgetKind` 及 `Align`（复用解析层类型）。先测试后实现，9 条新单测（围栏代码块/无语言/表格对齐/分割线/源序/普通段落无 widget/内联图片不产块级 widget/lookup/引用内代码块）；`assert_projection_matches` 增加 widgets 一致性断言；core 138（总计 **366**）。桌面端渲染待下一提交 |
| 0.1.43 | 2026-10-16 | **桌面端块级 widget 渲染**（Phase 3，SPEC §11.5、D30）：`platforms/desktop` 依核心 `Projection::block_widgets` 在文本层**之下**先画块级装饰——代码块面板描边 + 右上角语言标签（等宽 `shape_line`）、表格弱底 + 外框 + 行分隔线、分割线真实细线（取代旧的 `─` 文本占位）；光标进入块内则隐藏装饰（编辑态），判定用 `Projection::block_widget_on_line`。`DocLayout` 新增 `widgets: Vec<WidgetDecor>`，prepaint 由 `block_widgets()` 投影（先取光标源偏移再借 `doc.projection()` 避免借用冲突）。门禁：workspace fmt/clippy/test 全绿（**366**）；桌面端 `fmt`/`clippy -D warnings`/`build` 全绿（本环境无法可视化验证渲染效果） |
| 0.1.44 | 2026-10-16 | **表格增删行列**（Phase 3，SPEC §11.5/§12.2，D31）：新增 `mazha-core::table`——纯函数 `edit_table(source, caret, TableOp) -> Option<Edit>`，把"光标所在表格块"解析成 `TableModel{header, align, rows}`，套用 `InsertRowAbove/Below`、`InsertColumnLeft/Right`、`DeleteRow/Column` 后规范化重写整块（带首尾竖线、按对齐重写分隔行），产出单次 `Edit::Replace`；核心导出 `TableOp`/`edit_table`。边界：光标在表格外、删唯一列、在表头/分隔行删行 → `None`；转义 `\|` 不拆列；列对齐经 `Align` 保留。先测试后实现：table 12 条 + document 集成 1 条（经 `Document::apply` 后投影全量一致），core **151**（总计 **379**）。桌面端接入：`apply_table`（走 `apply_edit`，纳入 undo/redo）+ 快捷键 Cmd+Option+↑/↓（增行）、←/→（增列）、Delete（删行）、Shift+Delete（删列）；桌面端 fmt/clippy/build 全绿（本环境无法可视化验证） |
| 0.1.45 | 2026-10-16 | **代码块语法高亮**（Phase 3，SPEC §11.5/§12.2，D32）：新增 `mazha-core::highlight`——用 **syntect**（纯 Rust fancy-regex 引擎）把一段代码按语言逐行着色，`Highlighter::with_theme(name)` 持有内置 `SyntaxSet`，`highlight(code, lang) -> Vec<Vec<ColoredRun>>` 返回每行的 `(源区间相对行首, (r,g,b))`；语言未命中回退纯文本；不泄漏 syntect 类型（对外 `ColoredRun`/`(u8,u8,u8)`）。`mazha-core` 依赖 syntect（`default-features=false` + `default-fancy/default-syntaxes/default-themes`，无 onig C 依赖）。先测试后实现：5 条单测（rust 关键字多色、未知语言回退单色、空/多行状态、无语言纯文本、区间相对行首），core **156**（总计 **384**）。桌面端接入：`DocView` 惰性持有 `Highlighter`（按背景明度选深/浅内置主题），prepaint 把连续代码行聚段着色，`build_line_runs` 按着色区间切分显示文本逐段上色（等宽 + 弱底）；桌面端 fmt/clippy/build 全绿（本环境无法可视化验证渲染效果） |
| 0.1.46 | 2026-10-16 | **软换行**（Phase 3，SPEC §7.1/§11.5/§12.2，D33）：核心新增 `ProjectedLine::{display_byte_len, display_byte_of_col, display_col_of_byte}`（显示字符列 ↔ 显示文本字节偏移；越界钳制、字节落多字节字符内向下吸附到字符起点），纯逻辑可单测；先测试后实现：4 条单测（混合宽字符、跨片段、空行、往返一致），core **160**（总计 **388**）。桌面端 `platforms/desktop` 把每条投影行由 `shape_line` 改为 `shape_text`（带按内容宽度算出的 `wrap_width`）折成一条 `WrappedLine`：`BlockGeom` 改存投影行 + `Arc<WrappedLine>` + 视觉行数 + 前缀字节数；`DocLayout` 增 `row_starts`（每块首视觉行的全局行号）；命中测试（`WrappedLine::closest_index_for_position` 求字节→显示列）、光标竖线/选区高亮（按视觉行拆分，`position_for_index` 取行内 x）、IME 候选框、代码块/表格/分割线装饰的纵向范围全部改为按**视觉行**定位；并修正桌面端先前以显示字符列误当字形字节索引（CJK 光标/命中错位）的隐患。桌面端 fmt/clippy/build 全绿（本环境无法可视化验证渲染效果）；已知限制：上下移动仍按逻辑行持列、不实现横向滚动 |
| 0.1.47 | 2026-10-16 | **系统剪贴板**（Phase 3，SPEC §11.7/§12.2，编辑器扩展）：桌面端 `platforms/desktop` 新增 `DocView::{copy_selection, cut_selection, paste}`——复制/剪切把所有光标选区（`all_selections` 去重合并）按文档顺序以换行拼接后 `write_to_clipboard`；粘贴读系统剪贴板纯文本（`item.text()`，忽略图片/文件）替换各光标选区（无选区则插入）。多光标安全：剪切/粘贴按光标**位置倒序**逐个应用 `Edit`，避免靠前编辑使靠后光标偏移失效。快捷键 `Cmd+C`/`Cmd+X`（不带 Shift，避免与 `Cmd+Shift+X` 删除线冲突）/`Cmd+V` 接入。仅桌面 UI 逻辑，无核心改动、无新单测（桌面端无测试基建），core **160**（总计 **388**）。桌面端 fmt/clippy/build 全绿（本环境无法可视化验证剪贴板效果） |
| 0.1.48 | 2026-10-16 | **按视觉行上下移动**（Phase 3 软换行 D33 收尾，SPEC §7.1/§11/§12.2）：桌面端上下移动由逻辑行 ±1 改为**视觉行** ±1。新增自由函数 `visual_vertical_target(layout, projection, caret, step) -> Option<usize>`：① 用投影换出 (逻辑行, 显示列) → `byte_of_col` + `position_for_index` 反查块内视觉行与块内 x，加块缩进得**绝对 x**；② 绝对视觉行 = `row_starts[行] + 块内行`，±1（越界 `None` 即不动）；③ `row_starts` 二分定位目标块与块内行，绝对 x 扣目标块缩进得块内 x；④ `closest_index_for_position` 取目标视觉行最近字节 → 显示列 → 源偏移。`move_up`/`move_down` 改调 `move_visual_vertical`（首帧布局未就绪时回退 `move_vertical_all` 逻辑行）。保留 Shift 扩展选区语义。仅桌面 UI 逻辑，无核心改动、无新单测，core **160**（总计 **388**）。桌面端 fmt/clippy/build 全绿（本环境无法可视化验证上下移动效果） |
| 0.1.49 | 2026-10-16 | **查找/替换**（Phase 3，SPEC §11.8/§12.2，编辑器扩展）：核心新增 `mazha-core::find`——纯函数 `find_all(text, pattern, case_sensitive) -> Vec<Range<usize>>`（非重叠、按出现顺序；大小写不敏感按字符 `to_lowercase` 比较、支持非 ASCII；空模式返回空）、`replace_all(...) -> String`、`replace_all_edit(...) -> Option<Edit>`（单次全文 `Edit::Replace`，无变化返回 `None`，一条撤销记录）。不支持正则捕获组。先测试后实现：11 条单测（大小写敏感/不敏感、非重叠相邻、空模式/无匹配、CJK、多字节偏移、替换基础/大小写/CJK、替换编辑产生/无变化为 `None`），core **171**（总计 **399**）。桌面端接入：`FindState`（展开态/聚焦框/大小写/查询/替换/匹配/序号）+ `open_find`/`close_find`/`recompute_matches`/`goto_match`/`find_next`/`find_prev`/`replace_current`/`replace_all_now`；`Cmd+F` 展开、查找条展开时 `handle_find_key` 接管全部按键（`Enter`/`Shift+Enter` 跳、`Tab` 切换框、`Cmd+Enter`/`Cmd+Shift+Enter` 替换、`Cmd+Alt+C` 大小写、`Esc` 关闭）；匹配以选区高亮显示。查找框为静态文本输入（无 IME），满足 MVP。桌面端 fmt/clippy/build 全绿（本环境无法可视化验证查找替换效果） |
| 0.1.50 | 2026-10-16 | **修复：多行围栏代码块导出丢换行**（R8 同类缺陷，非新决策）：`CodeContent` 片段按行切分且**不含行尾换行**，`mazha-export` 直接串接导致 `fn a() {}fn b() {}` 粘连（代码内空白行同理）。HTML `render_block` 与 Typst `block_to_typst` 的代码块分支改为片段间补 `\n`（空白行片段为空，仍占一个换行；DOCX 原本用 `line_break` 已正确）。新增 2 条回归测试；export **102**（总计 **401**）。仅影响导出，编辑区投影不受影响 |
| 0.1.51 | 2026-10-19 | **公式/图表解析与建模**（Phase 3，SPEC §6.1/§11.5/§11.9/§12.2，D34）：`mazha-md` 新增 `BlockKind::MathBlock`（行首缩进 ≤3 的 `$$…$$`；同行闭合=单行块，否则扫到仅含 `$$` 的行，未闭合延伸到文末，与未闭合围栏同规则）与 `SpanRole::{MathMarker, Math}`（`$$`/`$` 定界记 `Marker` 隐藏，公式体记 `Text` 可见）；行内 `$…$` 规则：开 `$` 左邻非字母数字且右非空白、闭 `$` 左非空白且右非数字（避开 `$5`/`$10`）、内容非空且不跨行、`$$` 不构成行内公式、`\$` 走既有转义分支。序列化按片段间隙重建换行（容器前缀丢弃、由 `prefix_lines` 回补）；增量 `is_unclosed_fence` 覆盖 `MathBlock`（`MathMarker` 少于 2 视为未闭合）。`mazha-core` 投影新增 `BlockWidgetKind::Math` 与 `Chart { lang }`（`` ```mermaid `` 归 `Chart`，其余语言仍 `Code`）与 `RunStyle::Math`（斜体 + 主题色）。导出：HTML `<div class="math">`/`<span class="math">`；Typst 字面量输出（`#raw`/反引号围栏，**不**进 `$…$` 数学模式，遵循 R8 先例）；DOCX 纯文本 + 软换行。桌面端 `WidgetDecorKind::{Math,Chart}`（公式描边、图表弱底 + 右上角语言标签）与 `RunStyle::Math`。**顺带修正**：容器内代码块片段起点改用行内容起点（原用行起点会把 `> x`/缩进串入可见文本，序列化再补前缀 → 双前缀）；列表项首子块非段落时标记压在该块首行（原先甩到块尾产出 `- \n` 破坏列表）；EPUB 导出 ZIP 条目时间戳固定（`FileOptions` 用 `DateTime::default()`——原取当前时间，跨 2 秒会让相同输入产出不同字节，`identifier_is_deterministic` 偶发失败）。新增测试：md 27（嵌套前缀 3、行内公式 8、块级公式 11、增量 5）、core 4、export 10；md **126**、core **175**、export **112** + 3 集成（总计 **442**）。所见即所得数学排版（ratex-*）与图表 SVG（mermaid-rs-renderer）留渲染层（R4） |
| 0.1.52 | 2026-10-19 | **脚注解析与建模**（Phase 3，SPEC §6.1/§9.1/§11.10/§12.2，D35）：`mazha-md` 新增行内引用 `[^label]`（`SpanRole::FootnoteMarker` 隐藏 `[^`/`]` + `SpanRole::FootnoteRef` 可见标签；label 非空且不含空白/`[`/`]`；紧跟 `(` 时让位于链接）与块级定义 `[^label]: body`（缩进 ≤3，新 `BlockKind::FootnoteDef { label }` 叶子块；`:` 之后的空格归可见正文以保证逐字节往返；仅缩进 ≥4 的非空行作续行且**恰好剥离 4 空格**，空行/缩进 ≤3 结束定义；不支持多段落定义）。`starts_new_block` 纳入定义行，扫描分支置于表格之前（避免 `[^1]: a | b` 被表格抢占）；序列化对续行对称补回 4 空格缩进（`footnote_def_to_md`）；增量侧无需特判（`is_unclosed_fence` 有 `_ => false` 兜底）。`mazha-core` 投影新增 `RunStyle::Footnote`（标签可见；**不**新增 `BlockWidgetKind`）。导出：HTML 标签一律 `<sup class="fnref">`、定义块 `<div class="footnote" id="fn-{label}">`（就地输出，不重排文末、无回链）+ DEFAULT_CSS；Typst 标签 `#super[…]`、定义块按普通段落（**不用** `#footnote`，避免跨块归集到页脚与源顺序错位）；DOCX 纯文本（docx-rs 1.1 无上标 API）。桌面端 `RunStyle::Footnote` 仅用主题色区分（gpui 字号按行设置，无法逐 run 缩小/上标）。新增测试：md 23（引用 4、定义 15、增量 4）、core 3、export 9；md **149**、core **178**、export **121** + 3 集成（总计 **477**）。跳转/自动编号/文末归集与多段落定义留后续 |

---

> **规范驱动开发原则**：
> 1. 所有实现必须严格遵循本 SPEC 定义的契约。
> 2. 任何接口/架构变更需先更新 SPEC 再实现。
> 3. SPEC 是单一事实来源；`[待确认]` 项确认后移入第 14 节。
