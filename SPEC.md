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
| `mazha-md` | Markdown 解析、GFM+知识库方言、脚注（引用 + 定义 + 索引/编号）、公式（行内 `$…$` + 块级 `$$…$$`）、HTML 块（CommonMark 类型 1–5）、增量解析（脏块重解析）、序列化 | GUI | `parse` `SyntaxSpan` `Block` `FootnoteIndex` `serialize` |
| `mazha-crdt` | 用 yrs 维护 `Y.Text`，同步协议、状态向量、合并、持久化更新 | GUI | `CrdtDoc` `SyncMessage` |
| `mazha-store` | 存储 Provider 抽象 + 本地文件系统实现 + 文件监听（store 层，0.1.116） | GUI | `StorageProvider` `FileSystemProvider` `StorageCaps::watch` `WatchEvent` `WatchHandle` `normalize_watch_events` `coalesce_watch_events` |
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

/// Provider 能力（原四个 bool + 文件监听）
pub struct StorageCaps {
    pub writable: bool,
    pub streaming: bool,
    pub versioning: bool,
    pub collaboration: bool,
    pub watch: bool,                                       // 是否支持工作区文件监听
}

pub trait StorageProvider: Send + Sync {
    fn id(&self) -> &'static str;                          // "local" | "mazha-cloud" | "plugin:xxx"
    fn capabilities(&self) -> StorageCaps;
    fn list(&self, dir: &DocPath) -> Result<Vec<Entry>>;   // 不递归，目录在前
    fn read(&self, path: &DocPath) -> Result<Vec<u8>>;
    fn write(&self, path: &DocPath, data: &[u8]) -> Result<()>;   // 覆盖并自动建父目录
    fn rename(&self, from: &DocPath, to: &DocPath) -> Result<()>;
    fn remove(&self, path: &DocPath) -> Result<()>;        // 目录递归删除
    /// 递归监听 `dir` 子树；默认实现返回 `Ok(None)` 表示 Provider 无监听能力。
    /// 不支持时不得 panic；目录不存在/非目录 → `NotDirectory`；后端失败 → `StoreError::Watch`。
    fn watch(&self, dir: &DocPath) -> Result<Option<Box<dyn WatchHandle>>> { Ok(None) }
}

/// 规范化事件种类（仅四类）
pub enum WatchEventKind { Create, Modify, Remove, Rename }

/// 规范化文件事件：路径一律工作区相对 `DocPath`，仅 Markdown（`.md`/`.markdown`）
pub enum WatchEvent {
    Created  { path: DocPath },
    Modified { path: DocPath },
    Removed  { path: DocPath },
    Renamed  { from: DocPath, to: DocPath },  // 仅 notify 同时给出旧/新路径时产生
}
impl WatchEvent {
    pub fn kind(&self) -> WatchEventKind;
    pub fn path(&self) -> &DocPath;            // Rename 取 `to`
}

/// 监听句柄：`Send`，可跨线程持有；`recv` 阻塞、`try_recv` 非阻塞。
/// **drop 句柄即停止监听**并释放 OS watch（内部线程 join 后退出）。
pub trait WatchHandle: Send {
    fn recv(&self) -> Option<WatchEvent>;      // 已停止/通道关闭返回 None
    fn try_recv(&self) -> Option<WatchEvent>;
}

/// 纯函数：notify 原始事件 → 规范化事件（过滤目录/非 Markdown/非 UTF-8/根外路径）
pub fn normalize_watch_events(root: &Path, event: &notify::Event) -> Vec<WatchEvent>;
/// 纯函数：按到达顺序折叠短时间内同一路径的重复修改
pub fn coalesce_watch_events(events: &[(Instant, WatchEvent)], window: Duration) -> Vec<WatchEvent>;
```

**监听契约细则（0.1.116，D91）**：

- `StorageCaps.watch` 为 `true` 的 Provider 才可能返回 `Some(handle)`；其余 Provider 走 trait
  默认实现返回 `None`，调用方无需特判 panic。`FileSystemProvider::watch` 为**递归**监听，
  监听根 = `dir` 解析出的绝对路径，内部把 notify 事件的绝对/相对路径 `strip_prefix(root)`
  转成 [`DocPath`]。
- **过滤**：只上报 Markdown 文件（`.md`/`.markdown`，大小写不敏感）；目录事件、路径不能
  `strip_prefix`（工作区外）、非 UTF-8、无法构造合法 `DocPath` 的事件一律忽略。
- **事件映射**：`Create`→`Created`，`Modify::{Data,Metadata,Any}`/`EventKind::Any`→`Modified`，
  `Remove`→`Removed`，`Modify::Name(Both)`→`Renamed { from, to }`；`RenameMode::{From,To}`
  因单个事件拿不到成对路径，分别降级为 `Removed` / `Created`（不跨事件猜测配对）。
  `Access` 事件忽略。
- **去重/合并**：`coalesce_watch_events` 以**首次发出**的事件为准：同一路径（Rename 以 `to`
  为键）在 `window` 内的后续 `Modified` 被丢弃；`Created` 后紧随的 `Modified` 也折叠进
  `Created`；删除后的修改、重命名、窗口外的修改原样透传。内部监听线程在投递到接收器前
  执行该函数，窗口取 150ms（常量，不暴露配置项）；事件的 `Instant` 由接收回调打上。
- **自写文件反馈**：Provider 自身的 `write`/`rename`/`remove` 也会被 OS 监听器捕获，store
  **不做内部抑制**，一律按真实磁盘事件上报（D91）；上层（桌面）应按「该路径是否正被本应用
  编辑且脏」比对内容后决定是否提示/重载，store 层不感知编辑器状态。纯函数测试断言
  `Modify(Data)` 事件**原样产出 `Modified`**，以此锁定该策略。
- **生命周期/错误**：`watch()` 失败（目录不存在/后端初始化或注册失败）返回 `StoreError`；
  监听器在内部线程运行，不改变 `StorageProvider` 的同步契约（D24 不翻案）；后端运行期错误
  不进入事件流（回调里直接忽略），由 UI 层在需要时重新建监听。句柄 drop 后 `recv` 返回
  `None`。核心 crate 仍**不依赖任何 GUI**。

- 默认实现 `FileSystemProvider`（本地文件系统）；`DocPath` 定义在 `mazha-store`，
  由 `mazha-core` 复用（依赖方向 `core → store`，避免循环）。
- 云/自部署/用户自定义存储均以 Provider 形式接入，其中用户自定义走 **WASM 插件**（第 6.3 节）。
- 付费用户连接官方云服务，本质是 `mazha-cloud` Provider；是否收费与 Provider 接口无关。
- **MVP 采用同步接口**（决策 D24，第 14 节）：本地文件系统是唯一 Provider，同步 IO
  足够且免去异步运行时依赖。异步/流式能力仍推迟；**文件监听已在 Phase 5 的 store 层落地**
  （0.1.116，内部线程，同步 API 不变），并已由桌面接入（0.1.117，D92）；macOS 拆分重命名事件
  的配对在 0.1.118（D93）补齐，D91 的事件契约不变。
- **D91 当期边界**：仅 `mazha-store` API/实现/纯函数单测 + 文档与锁文件，**不修改**
  `platforms/desktop/src/main.rs`；外部文件变更提示/重载与脏文档冲突 UX 已由 D92（0.1.117）补齐，
  FSEvents 拆分名字事件的配对由 D93（0.1.118）在 store 层完成。

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
  - **系统剪贴板**（0.1.47、0.1.88，SPEC §11.7、D67）：`Cmd+C/X/V` 走 `gpui` 的 `write_to_clipboard`/`read_from_clipboard`；文本按原文粘贴，图片文件条目插入路径形式的 Markdown 图片，内存图片编码为 base64 `data:` URI；多光标编辑按位置倒序应用以保证偏移正确。
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
| PDF 导出 | Typst 0.15（`typst` / `typst-layout` / `typst-pdf` / `typst-kit`）→ [`mazha-export::to_typst`] + [`mazha-export::to_pdf`] | 纯 Rust；排版质量优先（D21，接入方式见 D25）：`to_typst` 产出标记，`ExportWorld` 实现 `typst::World` 后 `compile` + `typst_pdf::pdf` 出字节；字体走 `typst-kit`（内置 + 系统扫描）；`PdfDocument{bytes, warnings, pages}`（`pages` 为排出页数）；行内代码内容含反引号时改用 `#raw("…")`（反引号围栏会与内容粘连成更长串致 unclosed raw text）；**缺图降级（D82）**：`PdfExporter::{export,export_with}` 入口先用纯函数 `replace_unresolvable_images` 把无法由 `pdf_image_path(dest, base_dir)` 定位的图片 Widget 替换为占位文字，Typst 不再因缺资源中断；`base_dir` 由桌面传当前文档目录（`PdfExportOptions::base_dir`），远程/data/绝对路径图片退化为占位 |
| HTML 导出 | 基于 `mazha-md` 块/片段模型自研渲染（[`mazha-export::to_html`]） | 纯 Rust；与编辑器同源（D23 后 `pulldown-cmark` 仅 dev 对照） |
| DOCX 导出 | `docx-rs` 1.1（[`mazha-export::to_docx`]） | 纯 Rust；基于 `mazha-md` 块/片段模型直接生成 OOXML（标题用直接格式、行内 粗/斜/删除线/行内代码、链接「文字 + (URL)」、图片占位、GFM 表格、列表/任务/引用/提示块/代码块/分割线），决策 D26 |
| EPUB 导出 | Zip + XHTML（复用 `to_html` 同源渲染，[`mazha-export::to_epub`]） | 纯 Rust；EPUB 3 容器（`mimetype` 首条目且不压缩 / `container.xml` / `content.opf` / `nav.xhtml` / `toc.ncx` / XHTML 正文），正文走 XHTML 方言（空元素自闭合、布尔属性带值），决策 D27 |
| 导出主题/模板 | 自研 [`mazha-export::Theme`]（HTML/EPUB 的 CSS）与 [`mazha-export::Template`]（PDF/Typst 的版式前置） | 纯 Rust；内置主题 `light`/`dark`/`sepia`、模板 `plain`/`article`，均确定性常量，可 `custom`；`Template::article` 不覆盖字体族以保留 CJK 回退，决策 D28 |
| 文件监听 | `notify` 9（`mazha-store`，纯 Rust 无 GUI；MSRV 1.91 ≤ 工作区 1.92） | `recommended_watcher` 递归监听，内部线程 + `mpsc`，事件规范化/去重为纯函数；桌面独立 workspace 已有 notify 8 传递依赖，锁文件新增 notify 9 但保留原条目（D91） |
| 字体/中文字形 | `[待确认]` 系统字体 + 内置 CJK 子集 | Web/wasm 需内置 |

---

## 9. 功能范围

### 9.1 MVP（桌面首个可用版）

- [ ] 打开/保存 Markdown 文件；文件树（**层级化：目录展开折叠 + 目录内新建 + 重命名/删除**已实现 0.1.92，D70；**拖拽移动 + 新建文件夹**已实现 0.1.93，D71；**展开状态持久化**已实现 0.1.94，D72；**拖拽落点高亮 + 跨应用拖拽**已实现 0.1.95，D73）；新建/重命名/删除/移动
- [ ] WYSIWYG 编辑：标题、粗体、斜体、删除线、行内代码、链接、引用、有序/无序/任务列表
- [ ] 基础块：代码块（高亮）、图片（**本地/远程/data/协议相对真实渲染**已实现 0.1.74–0.1.76，D53/D54/D55；缺失仍占位）、表格（增删行列）、脚注（**解析与模型**已实现 0.1.52，**双向跳转 + 导出编号/文末归集**已实现 0.1.91，D35/D69；**多段落定义（容器体，D89）**已实现 0.1.114：桌面 Cmd+点击引用 ↔ 定义，HTML/EPUB 输出带锚点与 `<ol>`，Typst/DOCX 输出编号与文末定义；**多引用回跳（D90）**已实现 0.1.115：桌面按来源引用回跳并可循环，HTML/EPUB 每个引用一条回链）
- [ ] 公式（`$…$` / `$$…$$`）与 Mermaid 图表（**解析与模型**已实现 0.1.51；**桌面所见即所得排版**已实现：公式 RaTeX 0.1.98/D76、Mermaid 0.1.102/D77）
- [ ] 多标签页、命令面板、快捷键体系
- [ ] 导出 HTML / PDF / DOCX / EPUB（纯 Rust；桌面「文件」菜单与命令面板四格式入口已补齐，D82）

### 9.2 后续 Phase（已确认排除出 MVP）

- 大纲 / 目录导航（**已实现 0.1.53**：投影 `block.heading` → 侧栏标题列表 + 点击跳转，D39；**已移至右侧独立可拖动面板** 0.1.92，D70）→ Phase 4
- 文档内搜索（**当前文档**查找/替换已实现 0.1.49 `Cmd+F`；**全工作区**跨文件搜索 `Cmd+Shift+F` 已实现 0.1.95，见 D40）→ Phase 4
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
| Phase 5 | `mazha-crdt`（yrs）+ `mazha-store` Provider（含文件监听 store 层 0.1.116 与桌面事件接入 0.1.117/D92）+ WASM 插件 + 云服务端（独立 SPEC） | Phase 4 | 8–12 |
| Phase 6 | 移动端 iOS/Android（UniFFI + 原生 UI + 交互规范） | Phase 5 | 8–12 |
| Phase 7 | Web：spike → 方案 A/B → 实现 | Phase 6 | 4–8 |
| Phase 8 | HarmonyOS（ArkUI + NAPI） | Phase 6 | 待评估 |

**关键路径**：Phase 1 → 2 → 3。这三段决定产品手感，是最大风险区。

---

## 11. 编辑器交互规范（Typora 式）

1. **无预览窗**：所有渲染发生在编辑区本身。
2. **语法标记隐藏**：默认隐藏 `**`、`#`、`>` 等标记；光标落入某个 `[文字](目标)` 链接内时，**仅该链接**临时恢复原文（含 URL，便于继续编辑，D42），光标移开后复原。整行原文显示会改变整行显示坐标，需同时切换命中/选区/IME 映射，留后续。
3. **偏移映射**：任何显示位置都能映射回源字节偏移；所有命中测试、选区绘制、IME 定位都查 `OffsetMap`。
4. **增量解析**：按块（段落/列表/引用/代码块/表格/公式块/HTML 块/脚注定义）缓存解析结果，仅重解析受影响的块；未闭合的围栏/公式/HTML 块，以及**可跨空行续接的脚注定义**（多段落定义续行需对空行后做前瞻，D89）按可向后延伸构造做安全裁切（`is_unclosed_fence`/安全切点校验须覆盖），避免局部重解析破坏块边界。文档尺寸扩大时保持流畅。
5. **块级 widget**：图片、公式、图表、表格在光标不在其中时呈现渲染态，进入时切换为可编辑态（表格为单元格网格）。
6. **输入意图转换**：列表续行、Tab 缩进、`>`/`-`/`#` 自动补全、选区包裹（`Cmd+B` 等）、自动配对括号，全部先转成 `Edit` 再应用。
7. **系统剪贴板**：`Cmd+C`/`Cmd+X` 复制/剪切纯文本，`Cmd+V` 按“图片文件路径 → 内存图片 → 纯文本”的优先级读取剪贴板（D67）。图片文件条目按原路径生成 `![文件名](<路径>)`，不读取并内联文件内容；内存图片保留 GPUI 给出的格式并生成 `![图片](data:<mime>;base64,<payload>)`；多个图片路径逐行插入。多光标时：复制把所有选区按文档顺序以换行拼接；剪切/粘贴按光标**位置倒序**逐个应用，避免靠前编辑使靠后光标偏移失效；空选区不覆盖剪贴板，空内容不执行粘贴。
8. **查找/替换**：`Cmd+F` 展开查找条（查询框 + 替换框 + 匹配计数 + 大小写开关）。`Enter`/`Shift+Enter` 跳上/下一个匹配、`Tab` 切换查找/替换框、`Cmd+Enter` 替换当前、`Cmd+Shift+Enter` 替换全部、`Cmd+Alt+C` 切换大小写敏感、`Esc` 关闭。查找/替换逻辑是核心纯函数 `mazha_core::{find_all, replace_all, replace_all_edit}`（大小写不敏感按字符 `to_lowercase` 比较，支持非 ASCII；不支持正则捕获组），替换全量为单次全文 `Edit::Replace`（一条撤销记录）。桌面查找框为静态文本输入（无 IME），满足 MVP。
9. **公式/图表占位与渲染态**（D34/D76/D77）：`$$…$$` 显示公式块与 mermaid 围栏块各归类为一个**块级 widget**（`BlockWidgetKind::{Math, Chart}`），光标不在块内时呈现渲染态，光标进入切回编辑态（显示 TeX/mermaid 源文本，与代码块同规则）；行内 `$…$` 的 `$` 标记隐藏、公式体作为可见文本并用 `RunStyle::Math`（斜体 + 主题色）区分。所见即所得的排版属渲染层（`ratex-*` / `mermaid-rs-renderer`，风险 R4）：桌面端已接入——公式 0.1.98（D76）、Mermaid 图表 0.1.102（D77），渲染态源码转透明占位（保留 OffsetMap/光标/选区映射）、点图像进编辑态、失败回退占位面板；核心/导出不改（导出仍按字面量）。
10. **脚注**（D35/D69/D81/D89/D90）：行内引用 `[^label]` 与块级定义 `[^label]: body` 都只隐藏语法标记（`[^`、`]`、`]:`），label 与正文保留为**可见文本**（避免"丢字"）；引用与定义标签用 `RunStyle::Footnote`（主题色）区分，定义块**不**产生块级 widget（它是普通可见文本块/容器）。**WYSIWYG 视图始终显示原始标签，不做编号，也不移动定义块**（保持 1:1 偏移映射与可编辑性）。**多段落定义（D89，0.1.114）**：`BlockKind::FootnoteDef` 为**容器块**（`Block::is_container()` 为真），定义头（`[^label]:` 及首行正文）保留在定义块自身 `spans` 中，定义体解析为子块。**续行与边界**：缩进 **≥4** 的非空行属于定义，解析子块前剥离 **4 空格**定义缩进（保留更深缩进给子块自身语法）；空行**仅当**其后存在后续非空且缩进 ≥4 的续行时才归属该定义（可跨空行连接多段/多块），否则定义在该空行前结束；首行文本可与后续多块体并存。**不支持惰性续行（lazy continuation）**。**增量**：可跨空行续接的 `FootnoteDef` 视为可向后延伸构造，`is_unclosed_fence`/安全切点校验须将其判为非安全裁切，避免局部重解析破坏块边界。派生数据集中在 `mazha_md::FootnoteIndex`（`FootnoteIndex::new(source, blocks)`）：序号按**引用首次出现顺序** 1 起分配，只有定义未被引用者排在最后并按定义顺序编号，重复定义保留第一个；同时给出每个标签的全部引用区间与定义块区间。桌面端 `Cmd+点击`（`secondary`）引用标签跳转到定义块起点并记录来源引用（标签 + 该引用的源字节偏移，仅存于当前标签的会话状态），点击定义块首次跳回该来源引用，之后连续点击按引用出现顺序循环（末条后回到首条）；来源偏移因编辑失效或跨标签时回退到首个引用（`D90`，纯函数 `footnote_jump`），与链接点击同为修饰键手势；「引用 → 定义」方向不变。导出在入口调用 `mazha-export::footnotes::prepare` 做一次**源文本规整**：先把引用/定义头部标签重写为序号（只改 `SpanRole::FootnoteRef`，代码块字面量不受影响），再把定义块按序号移到文末（D69 仅顶层；D81 起含嵌套，剥离容器前缀后归集；D89 起定义体可为多块，按**整块字节范围**归集并从子块渲染）；随后各格式渲染器按原有逻辑输出。HTML/EPUB 引用为 `<sup class="fnref"><a id="fnref-{源字节偏移}" href="#fn-N">N</a></sup>`（D81 加唯一回链锚点），文末定义包在 `<section class="footnotes"><ol><li id="fn-N">…`（体由子块渲染）并追加 `↩` 回链（D81）：单引用仍为单条 `<a class="fnback" href="#fnref-{首个引用偏移}">↩</a>`（输出逐字节不变），多引用按出现顺序输出多条回链 `↩¹ ↩² …`，各自指向对应的 `fnref-{偏移}` 锚点（D90，静态 HTML、不引入 JS；上标数字取代额外标签/CSS，紧凑可区分且不破坏行内排版）；Typst/DOCX 得到编号与文末定义（多块体按子块输出），但无锚点/`<ol>` 结构。**限制**：编号不写回 Markdown 源文件；无定义的引用保留编号但导出时无对应定义条目；重复定义保留首个；不支持惰性续行；嵌套容器前缀剥离仍按既有规则（空格与 `>`/列表标记，tab/混合复杂前缀可能退化）。

11. **全工作区搜索**（D40）：`Cmd+Shift+F` 打开居中悬浮面板，输入即时递归扫描工作区全部 Markdown（不受目录展开状态限制，跳过非 UTF-8/读取失败文件并计数），按文件分组列出命中行与上下文（命中高亮、按字符截断不切 UTF-8），`↑`/`↓` 选择、`Enter`/点击打开文件并选中命中文本、`Cmd+Alt+C` 切换大小写、`Esc` 关闭；结果上限 500 条并提示「已截断」。搜索复用核心 `find_all`，面板与命令面板/查找条互斥（打开其一自动收起其它）。

12. **外部文件变更（D92，0.1.117 已实现）**：桌面在工作区根确定后对整个根**只调一次** `FileSystemProvider::watch`（D91），`WatchHandle` 存于 `DocView`，根变化/视图释放时 drop；后台线程 `recv` 事件，`cx.spawn` + `WeakView::update` 派发到 GPUI 主线程，UI 状态只在主线程改（D91 已做 150ms 同路径折叠，桌面不二次防抖）。**回声抑制**：D91 不抑制自写事件，桌面按「磁盘字节 vs 该标签 `saved_text` 字节」判定：`==` 视为本应用写入回声（仅 `refresh_workspace`，不提示不重载），`!=` 才是外部实质修改；读失败/非 UTF-8 按冲突处理，不静默重载。基准取 `saved_text` 而非 `doc.text()`：脏标签的后者本就不同，只有保存/打开时的磁盘快照能判断磁盘是否被外部改动；活动标签在 `self.saved_text`、非活动在 `tabs[i].saved_text`。**干净标签**：外部修改时整文档重载（`Document::new(磁盘文本)` + `saved_text` 同步 + 清 layout/撤销/高亮/查找/回跳游标 + 光标 clamp），等同重新打开，不弹模态框。**脏标签**：绝不自动重载；标签会话态记 `ExternalChange`（路径 + 磁盘文本，随 `sync_current_tab`/`activate_tab` 迁移、不写 Markdown）驱动**非模态横幅**「文件已在外部被修改」+「使用磁盘版本」（丢弃本地编辑并重载）/「保留本地版本」（把 `saved_text` 更新为磁盘文本，使本地编辑成为相对新基线的脏，清标记，后续同内容事件按回声过滤）；标记未解决时 `Cmd+S`/`Cmd+Shift+S`/命令面板保存一律拦截，避免静默覆盖外部改动。**删除**：`Removed` 不自动关闭/不重载，横幅「文件已在外部删除」+「关闭标签」（不保存）与「另存为…」（写新路径并更新标签 path），选定前拦截保存。**重命名**：`Renamed{from,to}` 且 `from` 对应已打开标签、`to` 在工作区内且未被其它标签占用时，只更新该标签 `path`（`saved_text` 不变、不重载、不产生脏）；否则拆为 `Removed(from)` + `Created(to)` 分别处理。**Created**：无对应标签仅刷新文件树；对应「读取失败保留的空标签」时按修改处理。路径统一 canonicalize 后比对；判定/文案/路径安全条件抽桌面纯函数并配单测；复用 `refresh_workspace`，不引入新依赖、不改核心 crate 契约与 OffsetMap/光标映射规则（重载=整文档替换）；监听不可用则静默降级为现状。**macOS 重命名配对（D93，0.1.118）**：FSEvents 即时模式下一次改名只给前后两条 `Modify(Name(Any))`（先旧后新），store 监听线程在 250ms 窗口内配对成一条 `Renamed{from,to}` 再投递；配对失败退回普通 `Modified`，不改 `WatchEvent` 四变体契约，桌面 D92 的安全重命名分支因此可在 macOS 正常触发（已打开 Tab 跟随改名，从资源管理器点新路径仍命中原 Tab）。

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
- [ ] 表格可增删行列（核心 `edit_table` + 桌面快捷键已实现，0.1.44）；代码块有语法高亮（核心 `Highlighter` + 桌面接入已实现，0.1.45）；软换行（核心显示列↔字节换算 + 桌面按视觉行布局已实现，0.1.46）；系统剪贴板复制/剪切/粘贴（桌面已实现，0.1.47）；查找/替换（核心 `find_all`/`replace_all` + 桌面 `Cmd+F` 面板已实现，0.1.49）；公式与图表**已解析并入块级 widget 模型**（核心 `BlockWidgetKind::{Math,Chart}` + 桌面板与 `RunStyle::Math` 已实现，0.1.51），**桌面真实渲染已接入**（公式 RaTeX 0.1.98/D76、Mermaid 图表 0.1.102/D77；核心/导出仍按字面量）；脚注**已解析并入模型**（行内 `[^label]` + 块级 `[^label]: body`，核心 `RunStyle::Footnote` + HTML/Typst/DOCX 导出已实现，0.1.52），**双向跳转、导出编号/文末归集已实现 0.1.91（D69/D81），多段落定义（容器体、子块渲染与增量安全裁切）已实现 0.1.114（D89）**
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
| R4 | `mermaid-rs-renderer` 早期，渲染质量未完全对齐 mermaid-cli | 图表观感 | 桌面已于 0.1.102（D77）接入真渲染（mermaid-rs → SVG → GPUI `SvgRenderer`），失败/不支持语法回退占位面板；允许降级；跟踪上游。**已实测的具体缺陷**（0.1.103）：含**回边**的流程图（如 `B -->|否| A`）菱形 branching shape 端口落在边中点而非几何顶点、回边标签锚点偏离连线——应用层无法改其布局算法，内置示例改为无回边分叉规避；用户文档若含回边图仍会复现，属上游限制。截图环境本轮不可用，但用户已实机目视反馈（连接线/标签/背景边框）并据此细化 |
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
| D40 | Phase 4 文档内搜索（已实现） | **当前文档**查找/替换已实现（0.1.49，`Cmd+F` 查找条 + `mazha-core::find`）。**全工作区**跨文件搜索已实现（0.1.95，`Cmd+Shift+F`）：居中悬浮面板，输入即时递归扫描工作区全部 Markdown（含未展开目录），按文件分组列出命中行 + 前后上下文片段（命中处高亮、按字符截断不切 UTF-8），`↑`/`↓` 选择、`Enter`/点击打开文件并选中命中文本、滚到视口顶部，`Cmd+Alt+C` 切换大小写；上限 500 条并标记「已截断」，非 UTF-8/读取失败文件计入「跳过」。复用 `collect_markdown_paths`（`Workspace::list` 递归 + 规范路径去重防符号链接成环）与 `mazha-core::find::find_all`。**限制**：每次查询变化同步重扫（超大工作区可能短暂卡顿，后续可做异步/防抖）；`↑`/`↓` 选择暂不自动滚动结果区（可手动滚动）；暂不接 IME（与查找条/命令面板一致，仅可打印 ASCII 直键）。 | 确认（0.1.95 落地） |
| D41 | Phase 4 桌面原生对话框必须用**异步** API（`AsyncFileDialog` + `cx.spawn`） | **约束**：macOS 原生文件对话框若用 rfd **同步** `FileDialog`，会走 `NSApp runModal` **嵌套泵主 runloop**；其间系统重查键盘布局会回调 gpui 的 `platform.on_keyboard_layout_change` → `app.borrow_mut()`，而此时 gpui 仍持有 app 的 `RefCell`，导致 `RefCell already borrowed` panic（`app.rs`）。**`window.defer` 无效**：defer 的闭包仍在同一次 gpui 借用内执行，同步对话框照样嵌套泵 runloop。**做法**：改用 rfd **异步** `AsyncFileDialog`（macOS 走 `beginSheetModalForWindow`，非阻塞、不嵌套泵 runloop），在 gpui 的 `cx.spawn(async move \|this, cx\| { … .await })` 里驱动，完成后经 `this.update(cx, …)` 回写；`DocView` 的 `open_file_dialog`/`save_as`/`move_current_file_dialog`/`export_pdf` 均如此。 | 确认（修复 Cmd+O panic） |
| D42 | 桌面端链接局部展开 + Cmd+点击打开 | **局部展开**：光标落入 `[文字](目标)` 时，UI 只为该逻辑行构造一份展开副本（`mazha-md::parse_inline` 分组 → 把 `[` / `](` / URL / `)` 作为普通 run 插回，重建该行 `OffsetMap`），其余行仍用投影；不采用整行原文（SPEC §11.2 全量版），因为整行显示坐标会变，命中/选区/IME/上下移动都要整行分叉。展开行的 `OffsetMap` 以**行首为基准**，`BlockGeom.revealed` 标记后在 `source_of_col` 加回行起点，光标/IME 列统一走 `BlockGeom::col_of_source`。**Cmd+点击**：同一套链接分组取目标 URL（剥掉可选 `<...>`），`App::open_url` 交给系统默认应用，不改光标。 | 确认（Phase 4 UI 优化） |
| D43 | 桌面端中文斜体用**渲染器级合成倾斜**（而非字体回退） | **问题**：macOS 中文字体（PingFang SC / Hiragino / Songti）没有 italic 字面；给斜体 run 配置 CJK `font.fallbacks` 后，CoreText 的级联表里这些字体仍是 Regular（`kCTFontSlantTrait` 不置位），中文保持直立，只有拉丁字母倾斜。**决策**：不改字体、不改核心投影，在 UI 渲染层自绘——斜体 run 中的 CJK 片段用**透明占位 run** 保留排版（advance/换行/光标/选区/IME 全部复用 gpui 布局），同时收集该段字节区间；paint 时用 CoreText 取字形轮廓，按 `x' = x + tan(12°)·y` 剪切后 `paint_path` 填充。**取舍**：12° 与 Chromium/VS Code 的合成斜体视觉接近；只处理 `Emphasis`/`Math`；透明占位而非删除 run，避免破坏字节↔列映射；字体对象按族名+字号缓存，paint 每字形只做轮廓几何。**验证限制**：本机无屏幕录制权限（截图全黑），观感需实机验收；单测覆盖 `split_segments`、透明 run + span 产出、CoreText 可取 CJK 轮廓。**已知不足（待改进）**：自绘倾斜与原生 italic 字面在字符间距/抗锯齿上仍有差异（透明占位 run 的 advance 与轮廓起点可能差 <1px），且 paint 期逐字取轮廓未缓存；后续按"轮廓缓存 + advance 校正"优化。详见 `platforms/desktop/src/oblique.rs`。 | 确认（Phase 4 UI 优化） |
| D44 | 桌面编辑器内 Tab/Shift+Tab 必须用**具体 key context 的 action**接管 | **问题**：`gpui-base` 的 Root 插件在 `"Root"` 上下文绑定了 `tab`/`shift-tab`（焦点切换）；gpui 的按键派发在**命中 key binding 后直接执行 action、不再调用 `on_key_down`**（`window.rs` 中 binding 分支早退），因此 `#doc` 上的 `on_key_down` 永远收不到 Tab，`insert_tab` 成了死代码。**决策**：在 `mazha-doc` 上下文绑定更具体的 `tab`→`InsertTab`、`shift-tab`→`Outdent`，用 `on_action` 处理并 `stop_propagation`，避免落到 Root 的焦点切换。**行为**：无选区时 Tab 在光标处插入 4 空格；有选区时对选中的每一行整体缩进 4 格；Shift+Tab 去掉每行最多 4 个前导空格或一个 `\t`。多光标先合并重叠行区间、**从后往前**替换，并按每行前缀增删量重映射光标/锚点（落在被删空格内夹到行首）。查找条打开时 Tab 仍切换查找/替换焦点；命令面板/重命名打开时不缩进。 | 确认（Phase 4 UI 优化） |
| D45 | 桌面回车走核心 `IntentMapper`；空列表项样式由**容器自身**写入 | **问题**：桌面 `insert_newline` 一直直接插入 `"\n"`，核心 `Intent::Newline`（续列表、有序序号 +1、任务补框、空项退出列表）从未被调用；即使调用，空列表项（`- [ ] ` 无子块）在投影里也没有样式——`assign_style` 只把样式写给叶子块，`ListItem` 作为容器且 `children` 为空时不写任何行，导致"回车后新项的图标不显示，直到输入文字"。**决策**：① 桌面回车改为 `DefaultIntentMapper.map(Intent::Newline)`，有选区时仍按覆盖为新行处理；多光标从后往前应用；② `assign_style` **始终**给 `ListItem` 的标记行写继承的 `bullet/task/quote_depth`（仅判空不够：新项后跟缩进内容时子块非空，但子块不覆盖标记行）；③ `list_prefix` 识别缩进并保留到续行标记（嵌套项回车不再跳回顶层）。**行为**：列表/任务项回车立刻出现下一项的项目符号/复选框；在新空项上再次回车删除标记退出列表（即"两次回车结束列表"）；有序列表自动递增。 | 确认（Phase 4 UI 优化） |
| D46 | 表格观感（观感优先，不做列重排） | **问题**：表格只有浅底+外框，表头与表体无区别；隐藏的分隔行仍占一整行空白；单元格内容被 trim，渲染成 `\|a\|b\|` 很挤。**决策**：① 投影层给表格行标注 `TableLine::{Header, Delimiter}`，表头行在 UI 加粗+底色；② 分隔行在**非光标行**时高度归零，表头行加高约 12px 提供上下内边距（光标进入分隔行时恢复高度）；③ 保留单元格源码两端空格作为显示内边距，导出 HTML/DOCX 时裁掉；④ 表体分隔线从表体首行开始画。**不做**：按内容重排列宽/竖向网格线（当前按源码字符排版，重排会改动源文本或需要新的布局模型）、单元格 Tab 导航（留作编辑体验项）。 | 确认（Phase 4 UI 优化） |

| D47 | 表格按内容列宽 + 竖向网格线 + 单元格 Tab 导航（D46 的"不做"项升级为做） | **问题**：D46 后表格仍按源码字符排版，列宽随字符数变化、中文/英文混排对不齐，也没有竖向列边界；Tab 在单元格里只会插入 4 个空格，无法在单元格间移动。**决策**：① 核心投影给表体行也标 `TableLine::Body`，`ProjectedLine` 新增 `table_cells: Option<Vec<Range<usize>>>`（显示文本内、已裁掉竖线与两端空白的单元格字节范围；用 `SpanRole::TablePipe` 而不是字符 `\|` 判断，转义竖线不切列）；② 核心新增 `table::table_cells` / `cell_index_at`，与 `split_cells` 共用边界规则，供 UI 导航；③ 桌面端**整表测量**：逐单元格 `shape_line` 取自然宽，列宽 = 该列最大自然宽 + 2×10px（下限 40px），总宽 + 缩进 ≤ 可用宽度才启用网格；否则整表回退 D46 的流式排版（不压缩、不裁字、不换行；超宽表格的压缩 + 软换行见 D57）。列对齐沿用分隔行的 `Align`（左/中/右）；④ 网格模式逐单元格绘制文本、光标、选区与点击命中，表头行高 +12px 且文字垂直居中；竖向网格线画在内部列边界，表格装饰在编辑态也保留；⑤ Tab/Shift+Tab 在单光标无选区时按单元格跳转（同一行 next/prev，跨行跳过分隔行，到表格边界吞掉按键），**不**在末格自动加行/列。**限制**：含中文合成斜体（`oblique_spans` 非空）的表格整表回退流式；单元格不换行（超宽表格压缩后换行见 D57）；表格行不做行内格式局部展开（展开会改变单元格字节范围）。 | 确认（Phase 4 UI 优化） |

| D48 | 表格内左右方向键按**显示文本**移动，跳过隐形标记与单元格分隔 | **问题**：0.1.68 网格化后，方向键仍按源码字符逐字节走——会依次落在隐藏的竖线、单元格两端填充空格和 `**`/`*` 等行内标记上；这些位置在网格里没有可见位移，表现为"连着按几次后需要按两次才动一下"。**决策**：表格行内左右移动改走核心 `table::cell_step(display, cells, offset, forward)`：在**显示文本**内逐字符走（隐藏标记本来就不在显示文本里），单元格末尾一步跳到下一格首字符、下一格首字符反向一步回到上一格末尾；本行到边界时跨到相邻表格行（跳过分隔行），表格边界移到相邻逻辑行行首/行尾；非表格行仍用普通字符移动。**取舍**：每条表格行移动时 `display_text()` 会分配一次字符串（表格规模小，可接受）；点击仍可落在竖线/填充附近的最近单元格，不额外改写源文本。 | 确认（Phase 4 UI 优化） |

| D49 | 表格命中测试用**单元格距离**选最近格，不能拿格内相对字节当距离 | **问题**：0.1.68 的 `table_cell_of_col` 把 `(cell, local)` 存进 `best`，比较时误把 `local` 当 `best_dist`。多字单元格末尾的 `local`（如 6）大于下一格到边界的距离（如 3），于是 caret 源位置虽在本格末尾（输入仍落本格），视觉 x 却跳到下一格首字符；再按右键才真正切换，表现为"多字单元格每到格尾就像被吞一次"，单字单元格 `local` 与下一格距离恰好相等而不复现。**决策**：抽出纯函数 `nearest_cell(cells, byte) -> (序号, relative)`，显式记录 `(index, local, dist)`，只在距离更小（相等保留靠前格）时替换；`table_cell_of_col` 只负责取几何。新增多字格尾/间隙/单字格尾回归测试。 | 确认（Phase 4 UI 优化） |

| D50 | 从表格外横移进入表格时，第一下就落到**单元格内** | **问题**：普通字符左右移动到表格行时，会先落在行首（首个 `|` 前）或行尾（末个 `|` 后）。网格渲染把行首/行尾都画在首/末单元格边界上，视觉看不出还隔着竖线；此时输入文字会落到竖线外，Markdown 把新增文字解析成新单元格（左侧多一格 / 右侧多一格）。**决策**：`move_left/right` 先走 `table_arrow_step`；未接管时用普通字符移动算出 raw target，再经 `table_entry_snap` 把表格行首吸附到首格显示文本起点、行尾吸附到末格显示文本末尾。只改横向移动，点击/Home/End 等既有行为不变；表格前若有空行，空行仍是独立一行，需再按一次才进表（正常行导航）。 | 确认（Phase 4 UI 优化） |

| D51 | 代码块右侧内边距与编辑区边界对齐，长行不得越出面板 | **问题**：0.1.60 给代码块加左右 padding 时，`wrap_width` 用了 `+24px`，而文本本身又右移 `12px`，导致代码文本右缘实际到 `content_right + 24px`；同时块级装饰统一用 `content_right + 16px` 作右缘，代码面板/表格框/分割线都顶出正文右边距，窗口边缘看起来被切掉。**决策**：新增 `CODE_TEXT_INSET = 12px`、`CODE_RIGHT_INSET = 20px` 常量与 `code_block_wrap_width`：面板右缘固定到 `content_width - BASE`（与正文右边距一致），代码文本右缘 = 面板右缘 − 20px，左右内边距对称；块级装饰右缘统一改为 `content_right`（不再 `+16px`）。新增几何单测断言长行右缘不越过面板。 | 确认（Phase 4 UI 优化） |

| D52 | 引用块底色限制在正文文本列内，左右各留 4px | **问题**：0.1.58 的引用底色相对**整个元素边界**左右各 4px，而正文文本列本身还有 `BASE=16px` 左右边距（块缩进从 `BASE` 起算）。结果底色左缘比正文左缘还靠左 12px、右缘超出正文右边距 12px；代码块边界修正后引用块更显得左右"探出"。**决策**：底色改为 `正文左缘 + BASE + 4px` 到 `正文右缘 − BASE − 4px`（即元素内缩 `BASE+4px`），左侧 3px 竖线跟随底色左缘；引用文字右侧额外收 `QUOTE_TEXT_RIGHT_INSET=16px`，使长行右缘距底色右缘 12px（与左侧留白对称）。嵌套引用仍共用同一条底色带，靠文字缩进体现层级。新增引用块几何回归测试，并用窗口截图核对底色从旧 `x=522..3131` 收到 `x=554..3099`（正文列内）。 | 确认（Phase 4 UI 优化） |

| D53 | 图片本地真实渲染（补齐 Phase 3 漏项） | **问题**：图片此前只解析为隐藏 `SpanKind::Widget`，核心 `WidgetKind::Image` 只留 `alt`、丢弃 `dest`，桌面端始终画 `[图片: alt]` 小标签；Phase 3 规划中的"图片块级 widget / 渲染态"实际未落地。**决策**：① 核心 `WidgetKind::Image` 增加 `dest`，按最小语法解析 `![alt](dest)` / `![alt](<带 空格 路径> "title")`（title 忽略）；当段落整块恰好是一条单行图片语法时，投影额外产出 `BlockWidgetKind::Image{alt,dest}`，混排图片仍是行内占位。② 0.1.74 先只加载**本地文件**：相对路径相对当前文档目录，支持绝对路径、`file://` 与 `%XX` 转义；`http(s)`（0.1.75 起由 D54 支持）、`data:` 与协议相对地址（0.1.76 起由 D55 支持）/空地址/缺失文件继续显示占位标签。③ 图片用 GPUI `ImageAssetLoader` + `Resource::Path` 异步加载与缓存；渲染态按正文可用宽度等比缩小、**不放大**小图，行高 = 图片高 + 上下 `IMAGE_VERTICAL_PAD=8px`，铺弱底 + 1px 边框 + 6px 圆角。④ 光标进入图片行时整行显示原始 Markdown（复用/改名 `BlockGeom.raw_src` 的 1:1 字符↔字节映射），移开恢复图片；行内图片、加载失败、远程/缺失仍回退占位标签。**限制**：不监听文件变化、GIF 只画第 0 帧、无宽度/对齐/图注编辑控件、超大图只按宽度适配不限制高度；远程加载见 D54。**验证**：core 新增独占行/混排/带空格路径单测；桌面新增独占行检测、路径解析、缩放、百分号解码单测；截图核对 1800×700 图缩放至约 862.5×335 逻辑像素、120×60 小图保持原宽、引用内图片渲染、光标进入显示 `![宽图](assets/wide.png)`、点击别处恢复图片。 | 确认（Phase 3 补齐） |

| D54 | 远程图片加载（`http(s)://`） | **问题**：D53 只把图片地址解析为本地路径，远程地址统一回退占位标签，用户贴 URL 时不显示。**决策**：① `http://` / `https://` 直接映射为 GPUI `Resource::Uri`，由 `ImageAssetLoader` 复用现有「下载 → 解码 → 缓存 → 等比缩宽」管线；`mailto:` 与空地址仍占位；`data:`、协议相对 `//host/...` 当时占位，0.1.76 起由 D55 支持。② GPUI 的 `application()` 默认安装 `NullHttpClient`（请求必失败），桌面启动时显式安装 `gpui-pre-reqwest-client::ReqwestClient`，UA 为 `Mazha/<crate version>`。③ 远程失败/非 2xx/解码失败与本地缺失一样回退占位标签；不额外提供认证头、Cookie、Referer 或磁盘缓存，远程资源仅按 URL 走 GPUI 内存缓存。**验证**：用用户给的 `https://static.yun.chinahrt.com/.../login-bj_02.899e63d.jpg` 启动桌面端，OCR 不再出现「图片：」占位，截图核对 1920×891 原图按正文宽显示为约 1284×595.9 逻辑像素；新增 `remote_image_uses_uri_resource` 单测覆盖 URL 资源映射（当时 data/协议相对回退）。 | 确认（Phase 3 补齐） |

| D55 | 内联 data URI 与协议相对图片 | **问题**：D54 只支持 `http(s)://`，用户贴 base64 内联图或 `//host/path` 仍显示占位。**决策**：① `data:<mime>[;base64],<payload>` 解析 MIME（png/jpeg/webp/gif/svg/bmp/tiff/ico/pnm）为 GPUI `ImageFormat`；base64 先剔除空白再解码，非 base64 用字节级 `%XX` 解码；经 `Image::from_bytes(format, bytes).use_render_image` 复用 GPUI 图片解码与内容哈希缓存，再走现有等比缩宽/行高/编辑态原文。② 协议相对 `//host/path` 在本地编辑器没有页面协议可继承，统一补 `https:` 后交给 `Resource::Uri`，不按本地 UNC 路径处理。③ 未知/缺失 MIME、base64 损坏、空载荷继续占位；`data:` 不进入 HTTP 客户端。**限制**：每次 prepaint 重新 base64 解码（GPUI 按图片内容哈希缓存解码结果）、无 data URI 大小上限、无图注/对齐。**验证**：新增 `data_image_decodes_base64_and_percent` 与协议相对资源映射断言；截图核对 360×140 内联 PNG 按原尺寸显示、`//static.yun...jpg` 按 https 加载并等比缩宽，OCR 无「图片：」占位。 | 确认（Phase 3 补齐） |
| D56 | 内联 HTML 标签（`<font>`/`<span>`/强调类）最小解析与 `<br>` 剥离 | **问题**：用户粘贴的 Markdown 常混入 Typora/网页导出的内联 HTML（`<font color="red">…</font>`、`<span style="color:#0af">…</span>`、`<b>`/`<strong>`/`<i>`/`<em>`/`<s>`/`<del>`/`<code>`/`<mark>`、`<br>`）；原解析器只认自动链接，标签会原样显示成噪声，颜色等样式也丢失。**决策**：① `mazha-md::parse_inline` 在 `<` 处先试 `html_tag_inline`，再退回 `autolink`；仅处理上述白名单标签，大小写不敏感。② 起始标签与匹配的结束标签**从可见文本中隐藏**，内部内容递归按行内语法解析；`<b>/<strong>`、`<i>/<em>`、`<s>/<del>/<strike>`、`<code>` 分别映射到既有 `SpanRole::{Strong,Emphasis,Strikethrough,InlineCode}`。③ `<font>`/`<span>` 解析 `color="…"` 与 `style="…color:…"`，支持 `#rgb`/`#rrggbb`、`rgb(r,g,b)` 与少量命名色；`SyntaxSpan` 新增 `color: Option<(u8,u8,u8)>`，核心 `TextRun` 透传并在合并 run 时把颜色纳入相等判断，桌面 `to_gpui_runs` 用该颜色覆盖前景色（行内代码仍保持主题色）。④ 结束标签**只在当前行内解析区间内查找**；未闭合标签按普通文本原样显示，避免跨行吞内容。⑤ `<br>` 为零宽 `HtmlBreak`：正文不插入显示字符，D66 的表格网格按其位置强制分行。**限制**：不处理 `style` 的字号/背景/字体族等其他属性，不处理属性值中的转义引号，`<mark>`/`<u>` 仅隐藏标签（无高亮/下划线样式），嵌套同名标签取第一个结束标签（依赖递归 + 后续标签剥离）。**验证**：`mazha-md` 新增 9 条单测（命名色/hex/rgb、`<strong>` 角色、`<br>` 隐藏、未闭合与跨行不吞、嵌套强调、坏 hex 不 panic），core 新增 1 条颜色透传单测；workspace 合计 507、桌面 24 全绿。 | 确认（Phase 3 补齐） |
| D57 | 超宽表格不再回退流式：列宽压缩 + 单元格软换行 | **问题**：D47 在自然总宽超过可用宽度时整表回退流式，用户看到 `|` 和拼接单元格，超宽表格无法阅读。**决策**：① 保留自然宽度测量，列宽在 `[min_width, natural]` 间线性压缩到可用宽度；`min_width = 2×10px padding + 48px 最小内容宽`（可用宽度连内边距都放不下时仍回退流式）。② 单元格按压缩后的内部宽度 `shape_wrapped_line` 软换行；行高 = `line_height × 最大视觉行数` + 表头 12px，表头文字在加高行内垂直居中。③ 命中/光标/选区按 `WrappedLine::position_for_index` / `closest_index_for_position` 逐视觉行换算；换行单元格统一左对齐，单行仍保留分隔行对齐。**限制**：列宽不会小于最小内容宽；含中文合成斜体的表格仍回退流式；单元格内不自动断词。 | 确认（Phase 4 UI 优化） |
| D58 | 标签切换统一装载 + 侧栏拖动缩放/文件名省略 | **问题**：点击标签或从侧栏打开已打开文件时只改 `active`，没有把目标标签的 `doc/cursors` 装载到编辑器；同时编辑态只存在 `DocView` 的工作字段里，标签内的 `doc` 快照长期不更新，切走再切回会看到旧内容。侧栏固定 220px，长文件名虽设了不换行但缺少省略号，挤压图标后视觉堆叠。**决策**：① 新增 `sync_current_tab` / `activate_tab`，所有切换入口（标签点击、侧栏、Cmd+数字、新建/关闭、打开文件）统一走 `switch_tab`；`sync_current_tab` 用 `std::mem::replace` 把 `Document` move 回当前标签（`Document` 未实现 `Clone`，move 可保留撤销/重做历史），`activate_tab` 再把目标标签的 `Document` move 出来。② 切换时重建布局/滚动句柄、清空查找与高亮器，避免沿用上一个文档的视口坐标；当前标签标题从实时字段计算，其余标签用切走时回写的快照。③ 侧栏宽度存于 `DocView.sidebar_width`（默认 220px），在侧栏右侧加 4px 分隔条；按下后由根节点 `on_mouse_move` 统一改宽度并限幅到 160–420px，鼠标移出手柄也不丢拖拽。④ 文件树名称用 GPUI `truncate()`（nowrap + hidden + 末尾省略号）。**限制**：侧栏宽度不持久化；标签切换不保留各文档滚动位置；切走脏标签仍不弹保存确认。 | 确认（Phase 4 UI 优化） |
| D59 | 浅灰选中态 + 大纲跳转置顶 + 标签关闭按钮 + 底部状态栏 | **问题**：① 活动标签和侧栏当前项复用 `Palette.selection`（蓝色文本选区色），选中态与正文选区混淆；② 点击大纲只移动光标，正文滚动跟随只保证光标"最小可见"，目标标题可能停在窗口底部附近；③ 标签只能通过快捷键关闭，没有可见关闭入口；④ 文件/行数/快捷键提示占用标签栏下方一行，压缩正文高度，且不在窗口底部。**决策**：① `Palette` 新增 `selected_bg`（前景色 alpha 0.12 的中性浅灰），活动标签与侧栏当前项改用它，`selection` 继续只用于文本选区；② `DocView` 新增一次性 `scroll_to_source_top`，大纲跳转时记录源偏移；`DocElement::paint` 用 `Projection::source_to_doc_pos` + `row_starts`/`row_top` 求目标视觉行顶边，按 `total_height - viewport` 自行 clamp 后 `set_offset` 置顶，并写入 `caret_scroll_key` 抑制普通“最小滚动”跟随；③ 每个标签增加 `✕` 关闭按钮（仅一个标签时隐藏），新增 `close_tab(idx)`：非当前标签直接移除并修正 `active`，当前标签复用 `close_current_tab` 激活相邻标签；④ 根布局改为纵向：`#main`（侧栏 + 分隔条 + 文档列）`flex_1` + 24px 全宽 `#statusbar`；文件信息/行数/光标数/快捷键从标签栏下方移入状态栏，长路径用 `truncate()`。**限制**：切换标签仍不保留各文档滚动位置；关闭脏标签仍不弹保存确认；状态栏不显示编码/语言模式。 | 确认（Phase 4 UI 优化） |
| D60 | macOS Dock 应用图标 | **问题**：桌面端以 `cargo run`/裸可执行文件启动，没有 `.app/Contents/Resources`，Dock 与 Cmd+Tab 显示系统通用可执行图标。**决策**：① 图标源文件放 `platforms/desktop/assets/appicon.png`（1024×1024 RGBA）；② 用 `include_bytes!` 编进二进制，启动时经 `NSData::with_bytes` → `NSImage::initWithData` 解码；③ 在 GPUI 主线程回调中取 `MainThreadMarker`，调用 `NSApplication::sharedApplication(mtm).setApplicationIconImage(...)` 设置运行中应用的 Dock 图标；④ 非 macOS 平台提供空实现，保持现有构建。**限制**：只设置运行中应用的图标，尚未生成 `.icns` 或打包 `.app`（D61 已补齐正式打包）。 | 确认（Phase 4 UI 优化） |
| D61 | macOS `.app` 正式打包 | **问题**：D60 只解决运行中 Dock 图标；裸二进制在 Finder 中没有应用图标，也没有 Info.plist，无法作为常规 macOS 应用分发。**决策**：① 新增 `macos/Info.plist`（Bundle ID `icu.bughub.app.mazha`、显示名「码札」、可执行文件 `mazha-desktop`、`CFBundleIconFile=AppIcon`）；② 新增 `scripts/bundle-macos.sh`：release/debug 构建后创建 `码札.app/Contents/{MacOS,Resources}`，用 `sips` 从 `assets/appicon.png` 生成全套 `AppIcon.icns`（`iconutil`），写入 Info.plist 并用 `plutil` 注入版本号（默认取 SPEC 最新变更记录），复制二进制后签名并打包 zip；③ 默认 ad-hoc 签名保证本机可运行，设置 `MAZHA_CODESIGN_IDENTITY` 可切换 Developer ID；④ 新增 `scripts/notarize-macos.sh`，对 Developer ID 签名的 `.app` 执行 `notarytool submit --wait`、`stapler staple/validate` 与 `spctl` 校验；⑤ 打包脚本产物落在已忽略的 `platforms/desktop/build/macos/`。**限制**：公证脚本需要外部 Apple Developer 凭据（`notarytool` keychain profile），仓库内不保存；默认 ad-hoc 签名下载后仍会触发 Gatekeeper。当前只打宿主架构（本机 arm64），未做 universal binary；Windows/Linux 打包未覆盖。 | 确认（Phase 4 分发） |
| D62 | 桌面外观设置（主题 / 字体 / 字号）与持久化 | **问题**：桌面端主题固定为 gpui-kit 初始化时的浅色（或依赖全局状态），正文与等宽字号写死为 `BASE=16px` / 主题默认值；用户无法切换深浅色、调整字号或选择字体，重启后偏好也丢失。**决策**：① 新增 `platforms/desktop/src/settings.rs`，`Settings { theme, font_family, mono_font_family, font_size, mono_font_size }` 用 `serde`/`serde_json` 持久化；路径默认 `~/Library/Application Support/Mazha/settings.json`（Windows `%APPDATA%\Mazha`、Linux `$XDG_CONFIG_HOME/mazha` 或 `~/.config/mazha`），可用 `MAZHA_SETTINGS_PATH` 覆盖（测试/隔离启动用）。② 主题三态 `System/Light/Dark`：启动时在 `open_window` 内先应用，`System` 用 `Theme::sync_system_appearance` 并订阅 `observe_window_appearance` 跟随系统变化；切换时调用 `Theme::change` 后刷新 `Palette`、字体与高亮器缓存并重绘。③ 正文字号新增 `DocView.font_size`，布局快照不再用 `BASE` 当字号；行高取正文与等宽字号的较大者（`block_line_px_with_mono`），避免行内代码/代码块在字号调大后压到相邻行；`BASE` 继续只作左右内边距/列表标记区基数。④ 命令面板新增主题三选项、正文字体/等宽字体循环、正文字号/等宽字号增减、重置外观；快捷键 `Cmd+=`/`Cmd+-` 调正文，`Cmd+Option+=`/`Cmd+Option+-` 调等宽，`Cmd+Shift+L` 循环主题，`Cmd+Option+0` 重置；外观快捷键在命中后 `stop_propagation`，避免 macOS 把 `Cmd+=` 同时作为 key equivalent 与 key down 派发两次。⑤ 状态栏显示当前正文字号与主题。字体预设列表覆盖 PingFang/宋体/冬青/Helvetica/Arial 与 SF Mono/Menlo/Monaco/JetBrains Mono/Fira Code/Cascadia Code；未知字体名视为“自定义”并从列表首项继续循环。**限制**：未用 `TextSystem::all_font_names()` 做完整字体选择器；字体族只作用于编辑区正文/代码，窗口控件仍用主题字体；设置写入失败只写 stderr，无 UI 提示；系统主题跟随依赖窗口 appearance 回调，未额外轮询。 | 确认（Phase 4 外观） |
| D63 | macOS 原生应用菜单 | **问题**：GPUI 不会自动创建 macOS 默认菜单，`cargo run`/裸二进制从 `main` 启动时菜单栏缺失；现有快捷键只在窗口内派发，也没有撤销/重做/剪贴板/窗口/应用级命令入口。**决策**：① 用 `gpui_kit::{Menu, MenuItem, OsAction, SystemMenuType}` 组装菜单，经 `App::set_menus` 注入；顺序为「码札 / 文件 / 编辑 / 视图 / 窗口」，应用菜单含关于/服务/隐藏/退出。② 新增 `MenuCommand` 汇总应用、文件、编辑、视图、窗口命令；`MenuAction` 以 `namespace = mazha_doc` 注册，菜单项统一走 `div#app` 的 `on_action` → `run_menu_command`；应用级命令优先执行，其余在 `dialog_open || renaming`、命令面板、查找条打开时拦截。③ 编辑菜单剪切/复制/粘贴/全选使用 `MenuItem::os_action` + `OsAction::{Cut,Copy,Paste,SelectAll}`，让原生文本控件/保存面板优先消费标准 selector；撤销/重做等仍走应用命令。④ `menu_key_bindings` 注册真实 key binding，使 NSMenu 显示并响应快捷键：`Cmd+Q/H/Option+H` 为应用级，文档命令带 `mazha-doc` 上下文，窗口命令带窗口上下文；菜单项在应用不前台时由 macOS 自动禁用。⑤ 主题子菜单依据 `Settings.theme` 设置勾选，`set_theme_preference`/`reset_appearance` 调 `cx.set_menus(app_menus(&self.settings))` 动态刷新。⑥ 原生异步文件/消息对话框期间置 `dialog_open`，返回后清除，避免菜单快捷键重复开对话框。**限制**：未提供「帮助」菜单；窗口菜单使用中文名「窗口」，不启用 macOS 自动窗口列表；裸二进制应用菜单标题仍为可执行名 `mazha-desktop`（`.app` 使用 Info.plist 的「码札」）；未接管系统 Services 具体子项。 | 确认（Phase 4 桌面菜单） |
| D64 | HTML 音视频解析与 macOS 桌面播放 | **问题**：Markdown 中的 `<video>` / `<audio>` 原先被当作普通 HTML 字面文本，无法预览网络或本地媒体；GPUI 没有原生视频控件，且 `PlatformWindow` 不暴露可叠加 `AVPlayerLayer` 的 NSView。**决策**：① `mazha-md` 只识别单行完整媒体元素，支持 `src`、`poster`、`title` 与首个 `<source src>`，生成 `SpanRole::MediaMarker`；缺地址、未闭合或跨行标签保持原文字面量。② `mazha-core` 新增 `WidgetKind::Media { kind, src, poster, title }`，独占行由桌面渲染播放控件，混排媒体仍显示占位 chip；光标进入独占行时切回源码编辑。③ macOS 桌面首次点击才创建 AVPlayer，音频显示固定 50×50 播放/暂停按钮并在右侧显示总时长，不占满正文宽度；首次加载或网络缓冲时在按钮外围显示四点循环指示。视频通过 `AVPlayerItemVideoOutput` 请求 32BGRA `CVPixelBuffer`，逐行去除 stride 后转为 GPUI `RenderImage` 自绘。播放时视频约 33ms、音频约 100ms 轮询；视频支持进度条 seek，首帧前显示 poster 或 16:9 占位。④ 结束判断同时覆盖“接近总时长”和“已实际播放后 AVPlayer 自行回到 Paused”，结束后暂停并 seek 到 0，恢复播放图标，保证可重复播放。⑤ 播放状态按 `kind:src` 复用；标签删除/改写或切换文档时 drop 播放器与任务，避免后台残留。⑥ HTML/EPUB 导出标准 `video/audio controls` 标签；DOCX 与 Typst/PDF 只输出类型占位，不嵌入媒体资源。**限制**：播放仅在 macOS 桌面启用，格式/编码与网络行为取决于 AVFoundation；支持 http(s)、协议相对 URL、绝对/相对/file 本地路径，不支持 data/blob 媒体；音频不显示标题或可操作进度；尚无音量、字幕、倍速、循环、全屏、画中画、拖拽进度与键盘媒体控制；同文档相同 `kind:src` 的重复引用共享播放状态。网络音视频链接已实机验证，本地媒体兼容性仍需回归。 | 确认（Phase 4 媒体预览） |
| D66 | 表格单元格强制换行与会话尺寸拖动 | **问题**：D57 只有随列宽自动软换行，用户无法在指定位置手动换行；表格宽高完全由内容自动计算，也无法直接拖动边框调整。**决策**：① 表头规则保持标准 GFM 现状，本轮不支持无表头表格。② 单光标位于非分隔行单元格内时，`Shift+Enter` 插入 `<br>`；`mazha-md` 产出零宽 `SpanRole::HtmlBreak`，序列化原样保留，核心 `ProjectedLine.html_breaks` 记录显示字节位置。桌面表格把单元格按这些位置拆成多个排版段，每段内部继续支持 D57 软换行；光标、点击命中、选区、行高与导出（HTML/XHTML `<br>`、DOCX/Typst 换行）同步处理。③ 换行后的每个排版段及其自动软换行视觉行继承分隔行的 GFM 列对齐：`:---` 左、`:---:` 中、`---:` 右；绘制、光标、点击命中和选区统一补偿逐视觉行对齐偏移。④ 内部竖线建立 6px 命中区，拖动时调整左右相邻两列并保持两列总宽不变；右外边框拖动最后一列以改变整表宽度；各行底边可调整该行高度；整表右下角 10×10 句柄可同时调整最后一列和最后一行。列宽最小 48px，行高不得低于内容自然高度，整表不得超过正文可用宽度。⑤ 自绘句柄注册 GPUI hitbox，内部/右边框显示水平 resize 光标，横边框显示垂直 resize 光标，右下角显示对角 resize 光标。⑥ 列宽以表格源起点、行高以行源起点保存在当前文档会话，不写入 Markdown；切换文档时清空，窗口过窄时列宽临时回退自动计算。**限制**：手动尺寸不跨会话持久化，表格前方编辑导致源起点变化时可能恢复自动尺寸；不支持拖动左/上外边框；含中文合成斜体/极窄表格仍可能回退流式布局。 | 确认（Phase 4 表格交互） |
| D67 | 从剪贴板粘贴图片 | **问题**：0.1.47 的粘贴只调用 `ClipboardItem::text()`，原始图片没有文本而被忽略；文件复制虽可退化为路径文本，但无法生成 Markdown 图片语法。**决策**：① 桌面端直接消费 GPUI 已跨平台抽象的 `ClipboardEntry::{ExternalPaths,Image,String}`，不引入 AppKit/Win32 专用剪贴板依赖，也不把平台能力下沉到核心。② 优先查找 `ExternalPaths` 中扩展名为 png/jpg/jpeg/webp/gif/svg/bmp/tif/tiff/ico/pbm/pgm/ppm/pnm 的文件，按剪贴板顺序逐行生成 `![文件名](<原路径>)`；尖括号保留空格路径，路径中的尖括号、百分号与换行做百分号编码。只要存在图片路径，其它非图片文件与伴随文本都不插入。③ 没有图片路径时，取首个 `Image`，保留 GPUI 给出的 MIME 与原始编码字节，生成 `![图片](data:<mime>;base64,<payload>)`；不做二次解码/转码，避免 GIF/SVG 等格式丢失。④ 最后回退 `ClipboardItem::text()`；粘贴仍复用既有多光标倒序替换。**限制**：图片文件路径使用绝对路径，不自动复制到文档资源目录或转相对路径；仅按扩展名判断文件条目；不处理同一次剪贴板中的路径图片与内存图片混合插入。**验证**：桌面纯函数单测覆盖多路径、大小写扩展名、路径转义、非图片回退文本、内存 PNG data URI 与路径优先级；核心补充转义 `]` 图片 alt 的块级识别回归测试。 | 确认（Phase 4 编辑体验） |
| D68 | 专注/打字机/源码模式（代码模式） | **问题**：桌面编辑区缺少写作辅助视图：无法聚焦当前段落、无法在输入时稳定视线位置，也无法在 WYSIWYG 与原始 Markdown 源码间切换。**决策**：① 新增三种可独立开关的视图模式并持久化到桌面设置：专注模式、打字机模式、源码模式（用户语境下亦称代码模式）。② **专注模式**：开启后仅当前主光标所在的投影块（按逻辑行所属块）保持正常渲染，其余块以较低不透明度淡化；关闭后恢复。③ **打字机模式**：开启后在光标移动或编辑导致需保证光标可见时，尽量将当前光标所在视觉行滚动到编辑区可视高度的居中位置（受滚动边界约束）；不改变文档内容。④ **源码模式**：开启后编辑区按源码行显示原始 Markdown 文本（语法标记可见、图片/公式/图表等不做 WYSIWYG 预览渲染，按源码文本编辑与命中），用于直接编辑源码；关闭后回到 WYSIWYG 投影视图。源码模式与专注/打字机模式可组合（专注作用于当前源码行块，打字机同样居中光标行）。⑤ 通过菜单「视图」、命令面板与快捷键切换，并在设置中持久化。**限制**：专注模式按块淡化为渲染层行为，不影响导出与文档结构；源码模式下部分 WYSIWYG 专属交互（如图片预览点击左右边界）退化为源码编辑语义；模式状态不写入 Markdown 文件。 | 确认（Phase 4 视图模式） |
| D69 | 脚注交互闭环：双向跳转 + 导出编号/文末归集 | **问题**：D35 只做了脚注解析与按可见文本导出，引用与定义之间无法跳转，导出既不编号也不归集，读者看到的是原始 `[^label]` 与散落在文中的定义。**决策**：① **编号与归集只属于导出**，WYSIWYG 编辑视图保持标签原文与定义原位，避免破坏 D35 的 1:1 偏移映射和可编辑性。② 派生数据集中在 `mazha-md::FootnoteIndex`（不依赖 core，供 core/桌面与 export 共用）：`new(source, blocks)` 深度优先收集引用与定义，定义头部自身不计为引用；序号按**引用首次出现顺序**从 1 分配，只有定义、从未被引用者排在最后并按定义出现顺序继续编号；重复定义保留第一个；每条记录暴露 `label/number/references/definition` 与 `entry_for_reference`/`entry_for_definition` 偏移查询。③ 核心新增 `Document::footnote_index()`（用当前源文本与块森林构建，纯派生，不进撤销栈）。④ 桌面端 `Cmd+点击`（`secondary`）先于链接命中脚注：点引用标签跳到定义块起点，点定义标签跳回首个引用（`footnote_jump`），否则退回普通放光标/打开链接；不改变 WYSIWYG 显示。⑤ 导出在入口统一调用 `mazha-export::footnotes::prepare` 做一次源文本预规整：先按倒序替换所有 `SpanRole::FootnoteRef` 可见标签为序号（代码块/公式等字面量不受影响），重新解析后把 `BlockKind::FootnoteDef` 整块按序号排序移到文末（D81 起递归收集嵌套定义并剥离容器前缀后归集；D89 起定义为容器块、体可含多子块，仍按**整块字节范围**归集）；无脚注文档走快速路径逐字节原样返回。各格式渲染器随后按既有逻辑输出：HTML/EPUB 引用为 `<sup class="fnref"><a id="fnref-{源字节偏移}" href="#fn-N">N</a></sup>`，文末定义包在 `<section class="footnotes"><ol><li id="fn-N">…`（体由子块渲染）并追加 `↩` 回链（D81）；Typst/DOCX 获得编号与文末定义（多块体按子块输出），但无锚点/有序列表结构。**限制**：编号不写回 Markdown；未被引用但有定义的脚注仍占一个序号；无定义的引用只改编号、导出时无对应定义条目；重复定义保留首个；多次引用回跳由 D90 从「仅首个」改为「来源引用 + 顺序循环」；不支持惰性续行（D89）。 | 确认（Phase 4 脚注闭环，D81/D89/D90 细化） |
| D70 | 桌面文件树层级化 + 大纲移至右侧面板 | **问题**：文件树此前只列根目录的 Markdown（平铺），看不到也进不去子目录；大纲（目录导航）与文件树同处左侧栏上下堆叠，两类信息互相挤占，且大纲描述的是「当前文档」却占用左侧文件导航空间。**决策**：① 文件树改为按 `Workspace::list` **递归渲染层级**：目录始终显示（含空目录，便于定位/新建），仅当目录出现在 `DocView::expanded_dirs`（会话内 `HashSet<String>`，存相对工作区根的路径）时展开其子项；文件只保留 Markdown（复用 `DocPath::is_markdown`），其余格式过滤。每层缩进 12px。② 目录行提供 `▸/▾` 展开折叠、`＋` 在该目录内新建 Markdown、`✎` 重命名、`✕` 递归删除；文件行保持打开/重命名/删除。删除或重命名目录时同步迁移 `expanded_dirs` 的旧前缀，并更新当前打开文档的绝对路径（若当前文档落在被删目录内则清空，避免悬挂）。③ 侧栏标题栏新增 `＋` 在根目录新建。④ **大纲移出左侧栏**，成为**右侧独立可拖动面板**（沿用 `outline_open` 控制显隐，新增 `outline_width` 160–420px 与 `outline_resizing`），左侧栏只保留文件树；`ToggleOutline`/菜单/命令面板行为不变。⑤ 递归收集 `collect_tree_rows(ws, dir, depth, expanded, rows)` 为**纯函数**（不依赖 GPUI），便于单测覆盖层级/展开/过滤。**限制**：展开状态不持久化（每次启动全折叠，根目录恒展开）；未做目录拖拽移动与新建目录（移动仍走命令面板「移动当前文件…」，新建仅在已有目录内）；空目录会显示但无法在其内新建子目录。 | 确认（Phase 4 文件树深化） |
| D71 | 桌面文件树拖拽移动 + 新建文件夹 | **问题**：D70 后文件树已层级化，但**移动**只能走命令面板「移动当前文件…」弹原生目录对话框（无法移动目录、路径不直观），**新建**只能建 Markdown、不能建子目录（空目录也无法在其内建子目录）。**决策**：① 目录行与文件行都注册 GPUI `on_drag`（负载 `TreeDrag { rel, name, is_dir, bg/fg/border }`），拖拽时由 constructor 生成 `TreeDragPreview` 跟随标签。② 目录行同时注册 `can_drop` + `on_drop` 作为落点，侧栏标题 `sb-h` 作为「根目录」落点；`can_drop` 用纯函数 `drop_allowed(from_rel, from_is_dir, target_rel)` 拒绝「拖到自身所在目录」与「把目录拖进自身/后代」，其余放行。③ 落点统一调用新方法 `DocView::move_entry(from, target_dir)`：拒绝自身/后代/同名已存在，`std::fs::rename` 后跟随更新当前打开文档绝对路径，并用纯函数 `remap_expanded` 迁移 `expanded_dirs` 旧前缀；原 `move_file` 并入该方法（命令面板入口复用）。④ 新增 `new_folder_in_root` / `new_folder_in(dir)`（名字从「新建文件夹」递增、父目录保持展开、创建后进入重命名态）；侧栏标题栏按钮改为 `＋文件` / `＋文件夹`，命令面板新增「新建 Markdown」「新建文件夹」。⑤ 为避免拖拽误触：文件行「打开」与目录行「展开折叠」从 `on_mouse_down` 改为 `on_click`（GPUI 拖拽起手会清空 `pending_mouse_down`，故拖动不再误打开/误折叠）。**限制**：展开状态仍不持久化（重启全折叠，已由 D72 补齐）；拖拽落点高亮与跨应用/窗口外拖拽由 D73 补齐；新建文件夹只在侧栏标题栏入口（子目录内新建靠「根目录建好再拖入」）。 | 确认（Phase 4 文件树深化） |
| D72 | 桌面文件树展开状态持久化 | **问题**：D70/D71 的目录展开状态只存在会话内 `expanded_dirs`，重启后回到全折叠，用户每次都要重新展开。**决策**：① 展开状态**按工作区根**持久化到既有 `Mazha/settings.json`（`Settings` 新增 `tree_expanded: BTreeMap<String, Vec<String>>`，键为工作区根目录字符串，值为已展开的相对目录路径列表）。② `Settings::tree_expanded_for(root)` 读取、`set_tree_expanded_for(root, dirs)` 写入（空列表即删除键，避免配置无限膨胀）；`reset_appearance`（恢复默认外观）保留展开状态。③ `DocView` 启动时按 `workspace_root` 键恢复 `expanded_dirs`；在 `toggle_dir` 与 `refresh_workspace`（覆盖新建/重命名/移动/删除等 CRUD）后调用 `persist_tree_expanded` 落盘（排序后写入，保证稳定）。④ 根目录恒展开不记录；工作区根字符串用 `to_string_lossy`（非 UTF-8 路径会有损，可接受）。**限制**：以路径字符串为键，同一目录的不同写法（软链/`..`）会被视为不同工作区；配置随工作区数量线性增长（无清理策略）；拖拽落点高亮由 D73 补齐。 | 确认（Phase 4 文件树深化） |
| D73 | 桌面拖拽落点高亮 + 跨应用/窗口外拖拽 | **问题**：D71 的拖拽只有 `can_drop` 合法性判断，拖动时看不出当前落点（无任何视觉反馈）；文件/目录也无法拖出窗口交给系统（不能拖到 Finder/桌面/其它应用）。**决策**：① **落点高亮**用 GPUI `drag_over::<TreeDrag>`：目录行与侧栏标题（根目录落点）在拖拽悬停且 `can_drop` 为真时铺 `palette.selection` 底色，非法落点由既有 `can_drop` 直接阻止样式应用；`drag_over` 闭包返回 `StyleRefinement`（`style.bg(...)`，不 `mut` 后二次返回）。② **跨应用/窗口外**只做**出站**：目录行/文件行在 `on_drag` 之后追加 `external_drag_payload`，返回 `ExternalDragPayload::Files(FileDragPaths::new([(绝对路径, is_dir)]))`；GPUI 在指针离开窗口且左键仍按下时把当前 `TreeDrag` 转成系统原生文件拖拽（macOS 已由平台层实现）。③ **不做入站**（从 Finder 拖文件进窗口导入/移动）——需要窗口级 `on_drop` 与外部负载解析，留后续（D74 已补齐）。**取舍**：外站拖拽由指针离开窗口触发，自动化测试无法覆盖，已于 2026-11-18 实机验收通过——把条目拖出窗口交给系统原生拖拽、落入目标目录时按**复制文件**方式落地（复制文件到目标目录，原条目保留）；出站只带条目自身（目录即整个目录），不做多选批量拖拽。 | 确认（Phase 4 文件树深化） |
| D74 | 桌面入站外部拖拽（从 Finder 拖入） | **问题**：D73 只支持把工作区条目拖出窗口，反过来从 Finder/资源管理器拖文件进码札没有任何落点，`FileDropEvent` 到达 GPUI 后无人消费；用户无法把外部 Markdown/图片/目录导入工作区。**决策**：① 平台层已注册文件拖入类型并把外部路径作为 `ExternalPaths` 拖拽负载派发；在根元素 `#app`、侧栏标题 `sb-h`（根目录落点）、目录行注册 `can_drop` + `drag_over::<ExternalPaths>` + `on_drop::<ExternalPaths>`，与既有 `TreeDrag` 落点共存（GPUI 按 TypeId 分发 `drop_listeners`，子元素命中后 `stop_propagation` 阻止根元素重复导入）。② **落点目录**：侧栏标题 = 工作区根；目录行 = 该目录（含未展开目录）；编辑区 `#doc` 与其余区域 = 工作区根，无工作区则退回当前文档父目录，仍未保存则直接打开原路径/插入绝对路径链接。③ 外部条目一律**复制**（不移动、不删除 Finder 原件）到目标目录：纯函数 `plan_imports` 为冲突名追加 ` 2`/` 3`（保留扩展名）并保证同批次的多个同名条目互不覆盖；`copy_entry` 递归复制目录，跳过符号链接，拒绝把目录复制到自身或其后代。④ `.md`/`.markdown` 导入后自动打开为标签（与侧栏「打开」一致）；其它文件（含图片）在**编辑区**拖入时按当前文档目录计算相对路径，图片生成 `![名字](<相对路径>)`、其余生成 `[名字](<相对路径>)`（沿用 D67 的尖括号转义规则）并插入全部光标；侧栏拖入只导入、不改文档。⑤ 抽出 `DocView::insert_text_at_cursors` 供粘贴（D67）与拖入链接共用，保持多光标倒序插入语义。**限制**：不接受 Finder 拖入的 URL/纯文本；无拷贝进度与取消、无冲突策略 UI（自动改名）；符号链接不跟随；同批次深目录大文件可能阻塞主线程；系统原生拖拽无法自动化模拟，入站（Finder→窗口）已于 2026-11-18 实机验收通过（目录导入、同名改序、Markdown 打开、图片/附件相对链接）；出站（D73）亦已实机验收通过（拖出窗口→目标目录复制文件）。 | 确认（Phase 4 文件树深化） |
| D75 | 侧栏标题栏固定（标题与滚动列表分离） | **问题**：左侧「资源管理器 / ＋文件 / ＋文件夹」标题与文件树条目共用一个 `overflow_y_scroll` 容器，条目多时标题随列表一起滚出视野，新建入口要回滚才能点到。**决策**：① 把 `sb-h` 从 `sb: Vec<AnyElement>` 中提取为独立元素 `sb_header`，作为 `#sidebar` 的直接子节点，`h(28px)` 加 `flex_none()` 固定高度。② 其余条目装入新的 `sidebar-list` 容器（`flex_1` + `min_h(0)` + `overflow_y_scroll`）：`min_h(0)` 是 flex 子项能收缩到内容高以下的必要条件，否则滚动容器不会出现；滚动条只覆盖列表区。③ `sb-h` 原有的根目录拖拽落点（`TreeDrag` / `ExternalPaths` 的 `can_drop` / `drag_over` / `on_drop`）原样保留，元素改为直接子节点后落点命中不变。**限制**：右侧大纲面板的 `ol-h` 仍是同构布局，长大纲时标题也会随列表滚动（不在本决策范围）；未做标题的吸顶动画或折叠。 | 确认（Phase 4 桌面交互） |
| D76 | 桌面数学公式真实排版（R4 第一步） | **问题**：D34 只把公式解析为可见 TeX 字面文本 + 占位面板，桌面端没有真正的数学排版（风险 R4 未消化），用户看到的仍是 `E=mc^2` 这类源码而非排版结果。**决策**：① 桌面新增 `ratex-gpui = { version = "0.4", default-features = false }`（render-only，`ratex_gpui::render::render_latex` 输出透明底、着色字形的 `gpui::RenderImage`）；它依赖 `gpui-pre ^0.3`，与 gpui-kit 0.7 锁定的 `gpui-pre 0.3.9` 同一图，避免两份 GPUI 类型不兼容。② `DocView` 新增按 `(tex, 字号, DPR, 颜色)` 索引的公式渲染缓存（含失败缓存，避免每帧重试非法 TeX），主题变化时清空。③ **块级 `$$…$$`**：光标不在公式块内且非源码模式时，把该块所有投影行的 TeX 体（定界行显示为空）拼成一条公式渲染为图像，锚点行隐藏 TeX 文本、行高跟随图像高 + 上下 `MATH_VERTICAL_PAD`，其余行高度归零；**图像居左对齐正文文本起点（同图片块，不做居中）、不画背景与边框**；点击图像任意处即把光标放进公式块、整块显示 TeX 原文供编辑（同行内公式，移开恢复渲染态），命中靠布局阶段记录的图像矩形而非透明占位 run 的视觉行映射；编辑态整块按**原始源码逐行显示**（含 `$$` 定界行，走 1:1 字节映射，同行/围栏编辑态），避免块 body 的 `RunStyle::Math` 被误收进行内公式预渲染集合而继续透明化。渲染失败回退字面文本。④ **行内 `$…$`**：`RunStyle::Math` 的 TeX 文本在 gpui 侧变为透明占位（尺寸/偏移/选区映射不变），paint 阶段按显示字节区间算出占位槽宽、裁掉 RaTeX 左右各 `PAD`(8px) 透明留白后再叠图（字形区宽于槽位才等比缩小，短公式保持原尺寸），保证图像不越过槽宽压到右侧普通文字；行内预渲染集合须排除代码块与块级公式行；`inline_reveals` 纳入 `SpanRole::MathMarker`，光标进入时恢复 `$` 定界符显示原文。⑤ 核心 OffsetMap / 增量解析 / 导出均不改：真实排版只在桌面渲染态发生，导出仍是 TeX 字面量（SVG 导出留后续）。**限制**：不引入 RaTeX 结构化公式编辑器（仍按 LaTeX 源文本编辑）；多行块公式拼接为一条 TeX，锚点之外的行在渲染态高度为 0；不自动缩放超过正文宽的公式；mermaid 图表的真实渲染由 D77 补齐（R4 第二步）。**验证**：桌面新增公式缓存键、TeX 拼接、数学 span 透明占位的纯函数单测；根与桌面门禁全绿。 | 确认（Phase 4 渲染） |
| D77 | 桌面 Mermaid 图表真实渲染（R4 第二步） | **问题**：D34 将 `` ```mermaid `` 围栏归类为 `BlockWidgetKind::Chart`，D76 又让公式块共用一套「渲染态透明占位 + 点击进编辑」范式，但桌面 mermaid 块仍是弱底 + 语言标签的占位面板（风险 R4 半未消化），看不到图表。**决策**：① 桌面新增 `mermaid-rs-renderer = { version = "0.3.1", default-features = false }`（纯 Rust：`mermaid_rs_renderer::render(&str) -> Result<String>` 产 SVG，无浏览器依赖）；关闭 `default-features` 以免引入 CLI 与内置 resvg 0.47 PNG 光栅化。② SVG → 图像复用 GPUI 自带 `cx.svg_renderer()` 的 `SvgRenderer::render_single_frame(svg, 1.0)`（内部 resvg/usvg，避免第二套光栅化依赖）；该函数按 `SMOOTH_SVG_SCALE_FACTOR` 放大出图抗锯齿，故逻辑尺寸除回该系数；0 宽/高判为失败。③ `DocView` 新增 `chart_cache: HashMap<String, Option<ChartPreview>>`，键为 DSL 源码（配色固定 mermaid 主题，与 DPR/字号/应用主题无关，故不纳入键）；失败结果同样缓存避免每帧重试。④ `DocElement::prepaint` 单独一次 `view.update` 收集所有 `BlockWidgetKind::Chart { lang: "mermaid" }` 块（`source_mode` 与光标在块内时跳过），从投影可见行取 DSL 正文（去围栏/空行、按行拼接保留缩进），渲染后的锚点行隐藏源码文本、行高跟随图像 + `CHART_VERTICAL_PAD`，块内其余行归零；围栏行本就显示为空。⑤ paint 阶段在锚点行叠加图像，**0.1.103 起背景透明且不画弱底/描边**（SVG 根节点白底设 `transparent`，节点自带浅底/描边；实机反馈外加面板底与编辑区割裂，详见 §15），解析失败/不支持语法时保留旧的弱底 + 语言标签占位面板兜底。⑥ 布局阶段记录 `DocLayout::chart_boxes`（图像矩形 + 块源范围），`DocView::hit_source` 先命中它把光标落进围栏块，下一帧显示完整 DSL 原文供编辑（移开恢复渲染态），不依赖透明占位 run 的视觉行命中。`hide_text_runs` 把渲染态源码文本转透明并去掉代码块弱底，映射不变。⑦ 核心 `mazha-core`/`mazha-md`/`mazha-export` 均不改，导出仍按字面量。**限制**：渲染配色固定 mermaid 默认主题，不随应用明/暗主题切换；只渲染围栏语言标记为 `mermaid` 的块，`mermaid-rs-renderer` 早期语法覆盖面与观感未必对齐 mermaid-cli（风险 R4，失败回退占位）；不提供图表结构编辑（仍按 DSL 源文本编辑）；不自动缩放超过正文宽的图表。本次截图环境不可用（`screencapture` 取窗口返回空/全黑），未做实机目视验收，改由渲染管线单测（栅格化非空像素断言）验证。**验证**：桌面新增 DSL 提取、缓存键、SVG 栅格化出图、命中框、文本透明化 5 条单测；根 workspace 548 项、桌面 68 项门禁全绿。 | 确认（Phase 4 渲染） |
| D78 | 专注模式段落粒度与打字机中心带细化 | **问题**：D68 落地后按实机体验有两点可改进：① 专注模式只高亮光标所在的**单个逻辑行**，中文写作常见的「一段多行（源文本手动换行）」会被相邻行淡化，与 Typora「聚焦当前段落」的预期不符；② 打字机模式每次光标行几何变化都**强制居中**，连续移动方向键时视口持续跟随、晃动感强。**决策**：① 专注范围改为**段落粒度**：光标行属于块级 widget（代码/表格/公式/图表/图片）时仍整块高亮，否则向上下扩展连续的普通文本行，遇空行、标题、列表/任务、引用、代码块、表格、分割线或块 widget 行即截断；抽出桌面纯函数 `focus_line_range` 并配单测。② 打字机滚动改为**中心带**策略：光标中心落在视口中间 50%（半带 = 视口高 1/4）内不滚动，越出时才滚回带边缘（而非强制居中），抽出纯函数 `typewriter_scroll_y` 并配单测；大纲/查找跳转置顶（D59）行为不变，滚动边界 clamp 不变。③ 核心 / `mazha-md` / 导出 / 源码模式不变，模式状态仍只存桌面设置。**限制**：列表项续行、引用内多行仍按单行高亮（需块级段落建模才能精确）；中心带比例固定 50%、不可配置；专注淡化仍只作用于文本 run（图片/公式图像等叠加层不随淡化，留后续）。 | 确认（Phase 4 视图模式打磨） |
| D79 | 中文斜体合成优化（D43 遗留） | **问题**：D43 的 `oblique.rs` 用 CoreText 轮廓 + `paint_path` 自绘中文斜体，仍有两处缺陷：① 每帧每字都调用 `CTFontCreatePathForGlyph` 并重新三角化，滚动/打字时开销重复；② 自绘起点只按行基线 + `position_for_index().x` 估算，而 gpui 原生 `paint_glyph` 还会叠加形状化字形的 `position.y` 微调，并把原点按设备像素栅格对齐（half toward zero），两者会有不足 1px 的错位。**决策**：① 轮廓按 `(字符, 字号位模式)` 线程局部缓存为**本地坐标** `Path<Pixels>`（与既有 `FONT_CACHE` 同风格；跨帧、跨块、跨文档复用，字号变化自然落到新键；失败结果也缓存），paint 阶段只克隆路径并平移其顶点/包围盒到目标原点，不再逐字取 CoreText 轮廓。② 绘制原点与 gpui `paint_line` 的原生字形公式对齐：x 沿用 `position_for_index`（内部即首个 `index >= byte` 字形的 x，软换行时已扣除行首偏移），y 追加从 `WrappedLine::runs()` 中按同样规则取到的 `glyph.position.y`，最后按 `window.scale_factor()` 做 `(abs(v) - 0.5).ceil().copysign(v)` 设备像素对齐（与 gpui `round_half_toward_zero` 一致），使自绘轮廓起点落在原生字形同一像素栅格。③ 不采用 Metal 自定义着色器：需重写 gpui 场景管线、风险高且无法纯函数验证。**限制**：抗锯齿仍由 gpui 路径 st coverage 而非 CoreText 字形栅格化产生，笔画边缘与原生 italic 字面不可能逐像素一致；字形仍按固定 `CJK_FALLBACKS` 顺序取，个别标点可能与 CoreText 级联选中的字面不同。本次截图环境不可用，未做实机目视验收，由纯函数单测验证缓存/平移/对齐/字形 y 偏移契约。核心 / `mazha-md` / 导出 / 其它平台不改。 | 确认（Phase 4 渲染打磨） |
| D80 | 合成斜体中文发虚的根治：CoreText 直接栅格化倾斜字形 | **问题**：D79 落地后用户实机明确反馈「英文任何效果 OK、普通中文正文/标题清晰、**仅合成斜体中文发虚不清楚**」，问题锁定在 `oblique.rs` 的自绘抗锯齿而非全局字体/DPR/栅格化。查 gpui-pre 0.3.9 源码确认根因：`PathBuilder::build_path` 给所有三角形写入**常量** `st=(0,1)`，路径着色器 `fs_path_rasterization` 的 `dpdx(st)=0` 因此落入 `alpha=1` 分支，**解析式 AA 实际未生效**；边缘平滑只剩路径离屏纹理的 MSAA（最多 4× → 5 级覆盖），既无 CoreText 字形的 8bit 灰阶 AA / LCD 亚像素，也无字体平滑（字干加粗），1px 字干被采成半透明灰即「发虚」。D79 的缓存/对齐只解决「位置」，不解决「质量」。**决策**：① macOS 改由 **CoreText 自己栅格化倾斜字形**：`CTFontDrawGlyphs` + `CGContextSetTextMatrix(CGAffineTransform(1,0,k,1,0,0))` 把 12° 剪切交给 CoreText 栅格器（灰阶 AA + 字体平滑），绘到 `CGBitmapContext`（按字号×DPR 的设备像素分辨率），读 alpha 作 coverage、配 span 颜色生成直 alpha RGBA 掩码，封装 `RenderImage` 交 `window.paint_image` 叠加。② 位图几何抽纯函数 `shear_ink_bounds`/`raster_geometry`/`raster_placement`：由 CoreText 字形墨迹包围盒算剪切后范围，位图尺寸与文本原点都取**整数设备像素**，绘制时按「字形原点 − 原点在位图中的偏移」定位，保证 texel↔pixel 1:1，避免缩放二次模糊；CJK 透明占位 run 的 advance 一字不动（OffsetMap/光标/选区映射不变）。③ 缓存键 `(字符, 字号, DPR, 颜色)`，失败也缓存并设容量上限；非 macOS 恒返回 `None` 走既有降级路径。④ D79 的轮廓路径保留为**回退**：位图栅格化失败时仍显示倾斜字（质量回落到 D79 水平，不出现空白）。⑤ 仍不选 Metal 自定义着色器/重写 gpui 场景管线（风险高、无法纯函数验证）。**限制**：栅格化用整数设备像素原点，不做亚像素变体（同 D79）；CoreText 对剪切矩阵可能关闭部分 hinting，灰阶 AA + 字干加粗仍在但与原生直立字面不可能逐像素一致；`CJK_FALLBACKS` 固定顺序仍可能与 CoreText 级联字面不同；缓存含颜色维度，主题切换自然生成新条目（容量上限保护）。本机截图已能取到窗口（暗色主题下斜体 CJK 行统计 181 级灰阶、ASCII 目测顶部右倾无重影），**已由用户实机验收通过**；核心 / `mazha-md` / 导出 / 其它平台不改。 | 确认（Phase 4 渲染打磨） |
| D81 | 脚注增强：导出反向回链 + 嵌套定义归集（D69 遗留） | **问题**：D69 落地后仍有两项遗留：① HTML/EPUB 导出只有「引用 → 文末定义」的正向锚点，读者看完脚注无法返回正文（缺 definition → reference 的反向链接）；② `footnotes::prepare` 只搬运**顶层** `FootnoteDef`，嵌套在引用/列表内的定义虽被重编号却留在原位，无法在统一文末脚注区阅读。同时评估了 HANDOFF 另一候选项「软换行横向滚动（D33 已知限制）」：桌面 `DocLayout` 的滚动与视觉行坐标体系是**纯纵向**（`ScrollHandle`/`overflow_y_scroll`/`row_starts`/`row_tops`），加入横向滚动需给 paint、命中测试、光标/选区、IME、块级装饰与数学/图表/图片叠加层统一补 x 偏移并扩展内容宽度，属高风险坐标体系重构，本轮**不选**，记录在案。**决策**：① HTML/EPUB 引用输出 `<sup class="fnref"><a id="fnref-{源字节偏移}" href="#fn-N">N</a></sup>`：`prepare` 已把标签重编号，锚点用**重编号后源文本字节偏移**（唯一且稳定），避免把引用序号状态贯穿渲染器各层；文末 `<li id="fn-N">` 尾部追加 `<a class="fnback" href="#fnref-{首个引用偏移}">↩</a>` 回链，多次引用当时只回跳首个（与桌面 `footnote_jump` 定义→首个引用行为一致；D90 已改为多条回链）；就地 `<div class="footnote">` 仅在无法归集时作为兜底、不生成回链（D81 后嵌套定义均已归集为顶层 `<li>`）；`DEFAULT_CSS` 增 `.fnback` 样式。② `move_definitions` 改为**递归收集所有** `FootnoteDef`（含嵌套），按序号排序统一归集文末；嵌套定义先按首行 `[^` 之前的容器前缀逐行剥离（引用 `> `、列表标记与缩进），续行/定义体保留定义自身缩进结构，再作为顶层定义块文本输出（D89 起 `FootnoteDef` 为容器块，体可含多子块，仍按**整块字节范围**搬运/裁切）；删除原块按源偏移**倒序整行裁切**，避免区间失效。③ `mazha-md` 顺带修正列表项续行缩进解析（原 `min(indent(nrest), nl.content + marker.len)` 混用相对缩进与绝对偏移，把续行空白剥光，致嵌套定义的续行脱离定义块；改为 `min(indent, marker.len)`），否则嵌套列表内定义带续行时归集会与续行分离；④ 桌面已有定义→首个引用的 `Cmd+点击` 双向跳转（D69），本轮**不改桌面 / `mazha-core`**，只改 `mazha-export` 纯函数与 `mazha-md` 上述一处解析修正。**限制**：锚点是单次导出的源字节偏移（源文本变化后重新导出即失效，不跨会话）；多次引用回跳由 D90 扩展为全部引用；前缀剥离只识别空格缩进 + `>` / 列表标记，tab 或混合复杂容器前缀保留原样（退化为就地渲染）；删除嵌套定义可能留下空容器行；重复定义仍各自成项（沿用 D69）；Typst/DOCX/PDF 无锚点结构，回链不适用、不改；WYSIWYG 视图仍显示标签原文与原位定义（保持 1:1 偏移映射）。**验证**：`footnotes.rs`/`html.rs` 新增纯函数单测（嵌套归集/前缀剥离/续行/回链/唯一锚点），根 workspace 门禁全绿、桌面测试数不变、待用户实机验收。 | 确认（Phase 4 脚注增强，D89 兼容） |
| D82 | 桌面导出四格式补齐 + PDF 缺图降级（用户报告修复） | **问题**：用户报告「菜单里只有导出 PDF 一项，且导出 PDF 失败：`file not found (searched at /assets/sample.png)`」。调查确认两件事：① `mazha-export` 早已有 `to_html*`/`to_docx`/`to_epub*`/`to_pdf*` 全套 API，桌面 `main.rs` 却只把 `to_pdf` 接到 `PaletteAction::ExportPdf`（菜单/面板/快捷键/分发均只有一项）；② 内置 `SAMPLE` 写了 `![示例图片](assets/sample.png)`，磁盘无此文件；`export_pdf` 走一次性 `to_pdf` → `PdfExporter::default()` → `PdfExportOptions::base_dir = None`，`ExportWorld::resolve` 返回 `FileError::NotFound`，Typst 编译中断为 `PdfError::Compile`。`IMKCFRunLoopWakeUpReliable` 经确认是 macOS 输入法无害日志，**不修**。**决策**：① **导出面**：桌面新增 `ExportFormat::{Html,Pdf,Docx,Epub}`，纯函数 `export_target(format)` 统一给出对话框标题/过滤器/后缀/默认文件名，`export_title(doc_path)` 给出 HTML/EPUB 的书名；通用 `export_document` 复用异步 `AsyncFileDialog`（D41 不变）与错误上报，`render_export` 按格式调已有 API，不新增任何导出算法。菜单「文件」补三项、命令面板补三项，`Cmd+E` 仍只对应 PDF（不为其它格式抢快捷键）；各 API 内部的 `footnotes::prepare`（D69/D81）已覆盖 PDF/DOCX/HTML/EPUB，**桌面不再重复调用**。② **base_dir**：`PdfExporter::with_options(&PdfExportOptions{ base_dir: pdf_base_dir(self.path.as_deref()), .. })`，`pdf_base_dir` 为纯函数（取当前文档路径的父目录，无父目录/空父目录返回 `None`），真实文档的相对路径图片据此解析。③ **缺图降级放在导出 crate 根因处**：`PdfExporter::{export,export_with}` 先把 Markdown 过一遍纯函数 `replace_unresolvable_images(source, is_available)`——用 `mazha_md::parse` 递归收集 `SpanRole::ImageMarker` 源区间（代码块中的相同文本不是 Widget，不受影响），按偏移**倒序**替换为「（图片：alt）」占位文字；可用性由回调注入，使替换逻辑无 I/O、可纯测。纯函数 `pdf_image_path(dest, base_dir)` 把图片地址解析为 base_dir 下的文件路径；远程（`http(s)`/协议相对）、`data:`/`mailto:`、`file://` 与绝对路径一律返回 `None`（Typst `ExportWorld` 只能从 base_dir 读**相对**路径资源），其余交给 `is_file()` 判断。于是 `to_pdf`（无 base_dir）遇到图片必降级为占位、不再阻断编译；有 base_dir 且文件存在的图片保留 `#image`。导出处理的是独立字符串副本，不影响编辑器 OffsetMap/源文本。④ **内置 SAMPLE** 移除 `![示例图片](assets/sample.png)`，改为一段说明图片用法的纯文字：消除开箱即用的损坏占位 + 报错源；真实文档图片能力由 ②③ 保证。**取舍/限制**：远程/data/绝对路径图片在 PDF 中退化为占位（Typst World 不支持远程/内联资源，内嵌与临时文件写出成本过高），HTML/EPUB 仍原样输出 `<img>` 但不打包资源（读端可能显示破图，既有 MVP 限制）；不引入新的对话框/快捷键框架；无父目录的相对裸文件名按 `None` 处理（找不到资源根，图片走降级）。**验证**：导出 crate 新增 `pdf_image_path`/`replace_unresolvable_images` 纯函数单测与「缺图 PDF 仍成功」回归，桌面新增目标映射/base_dir/标题纯函数单测；根与桌面门禁全绿，待用户实机验收四类导出文件与含图 PDF。 | 确认（Phase 4 导出修复） |
| D83 | 导出不阻塞窗口 + 状态栏导出状态 + 成功提示（用户实机验收反馈） | **问题**：用户反馈「导出时希望不要阻塞当前窗口，在底部状态栏显示导出状态；成功后有提示；若必须阻塞则全局覆盖 Loading 弹窗」。调查确认：桌面 `export_document`（D82）虽用 `AsyncFileDialog` + `cx.spawn`，但 gpui-pre 0.3.9 的 `cx.spawn` 运行在**前台 executor（UI 主线程）**——对话框 await 后同步调用 `render_export`，其中 `PdfExporter::with_options`（系统字体扫描）与 `typst::compile` 是 CPU 密集任务，会冻结窗口；状态栏（D59）是静态 `h_flex`，`DocView` 无导出状态字段，成功/失败只 `eprintln!`，用户看不到任何反馈。**决策**：① 导出重活搬到 `cx.background_executor().spawn(future: Send + 'static)` 线程池：渲染资源收集、`render_export`（含 PDF 编译）、`std::fs::write` 全部在后台线程执行，完成后用 `this.update(cx, …)` 回主线程更新 `DocView` 状态；`cx.spawn` 只负责异步对话框与状态派发，不再做 CPU 密集工作，因此**不采用**用户给的备选「全局 Loading 弹窗」。② `DocView` 新增 `export_status: Option<ExportStatus>`（`id` 单调递增 + 文案 + 状态种类）与 `export_status_seq`：状态栏新增分支，按「正在导出 X…／已导出 X：文件名／导出 X 失败：原因」显示，进行中与成功用链接色、失败用强调色（现有 `Palette` 无独立成功色，避免扩大主题面）；进度态在选文件前即设置，取消对话框则清除。③ 文案生成与「旧计时器不得清除新状态」判定抽纯函数（`export_format_label`/`export_progress_text`/`export_success_text`/`export_failure_text`/`export_status_should_clear`）并配桌面单测。④ 完成态由 `cx.background_executor().timer(6s)` 自动清除，闭包按 `id` 校验，避免连续导出时旧计时器误清新状态；进度态不自动清除。⑤ `dialog_open` 的键盘抑制逻辑（D41）不变；不引入 toast/通知框架，状态栏文字即成功提示。**限制**：rfd 异步保存对话框本身是系统 sheet（视觉模态但不冻结窗口）；后台导出的是点击导出时的文档快照，导出期间用户继续编辑不影响本次结果；其它状态栏消息（如后续若有）与本状态共用一行，当前无冲突；截图环境不稳定，不做实机目视验收，以纯函数单测 + 门禁保证契约。 | 确认（Phase 4 导出体验） |
| D84 | 导出文档显示渲染后的公式与图表（用户实机验收反馈） | **问题**：用户反馈「现在导出的文档不能显示渲染后的公式及图表」。`typst.rs`/`html.rs`/`docx.rs` 对 `$$…$$`、`$…$` 与 mermaid 围栏都按 TeX/DSL **字面量**输出（D34 起明确如此，R8 曾因 LaTeX 命令无法进 Typst math 模式而改为 raw），桌面虽有 RaTeX（D76）与 mermaid-rs（D77）真实渲染能力，但都在 GUI 侧，导出 crate 必须保持无 GUI 依赖、可无 UI 单测。**调查结论**：① `ratex-gpui 0.4.1` 的 `render::render_latex_to_svg(latex, font_size) -> Option<String>` / `render_latex_to_png` 是 **gpui-free 纯函数**（parse→layout→display list→ratex_svg/ratex_render，不碰 `App`/`Window`），但 crate 本身依赖 `gpui-pre`，故**不能**移入 `mazha-export`；其 `ratex-svg` 开了 `embed-fonts`，产出的 SVG 内嵌字形轮廓、自包含。② `mermaid-rs-renderer 0.3.1` 纯 Rust 解析/排版产 SVG（无浏览器），且可设 `theme.background = transparent`；SVG 以 `<svg …>` 开头、无 XML 声明，可内联进 HTML/XHTML。③ Typst 0.15 原生支持 SVG 图片（`#image` 自动识别 SVG），`ExportWorld` 可从内存资源表按虚拟路径供文件，故 PDF 可矢量嵌入注入的 SVG。④ `docx-rs 1.1.2` 无图片嵌入 API（现有 DOCX 图片就是占位文字），四格式中 DOCX 无法低成本内嵌渲染图。**决策**：① 渲染能力留在桌面：导出 crate 新增**纯函数** `collect_render_requests(source) -> Vec<RenderRequest>`（按 `mazha-md` 块模型收集 `MathBlock`/行内 Math/`lang=mermaid` 代码块，键为归一化源码文本，区间为源偏移），桌面据此调 `render_latex_to_svg` / `mermaid_rs_renderer::render_with_options` 填 `RenderAssets { math, charts }`。② 纯函数 `prepare_renderables(source, &assets, target)` 在**独立字符串副本**上按倒序把请求区间替换为图片 Widget `![公式|图表](dest)`：`target=DataUri` 用 `data:image/svg+xml;base64,…`（HTML/EPUB 复用既有 `<img>` 输出，零改渲染器），`target=VirtualFile` 用 `__mz_asset_N.svg`（PDF）。③ PDF 新增 `PdfExporter::export_with_assets` / `to_pdf_with_assets`：先用既有 `replace_unresolvable_images` 处理真实缺图，再 `prepare_renderables(VirtualFile)`，把注入 SVG 放进 `ExportWorld` 内存资源表（`file()` 优先命中），生成的 Typst 标记因此拿到 `#image("__mz_asset_N.svg")` 并编译为**渲染后的矢量公式/图表**；顺序不可颠倒，否则虚拟路径会被缺图降级吃掉。④ DOCX 保持纯文本字面并在 SPEC/HANDOFF 明确标注限制（不引入第二套 OOXML 图片实现）。⑤ 全流程只处理导出副本，编辑器 `Document`/`OffsetMap`/光标/选区/源文本零改动；SVG 缺失（渲染失败）时跳过替换、保留原字面作为降级。**取舍理由**：相比「TeX→Typst math 标记」不依赖 LaTeX 子集兼容（D34/R8 已证实易触发编译错误）；相比把 resvg/usvg/ratex/mermaid 加进导出 crate，避免 GUI 依赖泄漏与重复渲染栈；一套自包含 SVG 同时服务 HTML/EPUB/PDF，风险最低且确实呈现渲染结果。**限制**：HTML/EPUB 依赖阅读器对 `data:` URI SVG 的支持（现代浏览器 OK，老旧 EPUB 阅读器可能不显示）；DOCX 仍为字面公式/图表；图表语法覆盖与观感同 D77（mermaid-rs 早期实现，失败回退字面）；公式字形观感由 RaTeX 决定（与编辑器一致，可能与 KaTeX 官网略有差异）；每篇导出都需桌面先渲染 assets，超长公式/图表会增大导出文件。 | 确认（Phase 4 导出渲染） |
| D86 | 导出图片尺寸合理化与 PDF 行内图片不换行（用户实机验收反馈） | **问题**：用户验收 0.1.110 反馈「图片都能正常显示了，但还可以优化：①图片太大；②PDF 中行内图片换行了」。**根因核实**：① 桌面 `EXPORT_MATH_FONT_SIZE = 32.0` 让 RaTeX 产出的 SVG **固有尺寸**按 32px 字号计算，而 HTML 正文约 16px、Typst 正文 `#set text(size: 11pt)`；`html::render_widget` 的 `<img>` 与 `typst::image_to_typst` 的 `#image` 都未给显示尺寸，按固有尺寸落地 → 行内公式约为正文两倍高，观感「太大」。② D84 把行内公式替换成 `![公式](dest)` 后导出器只看到普通图片，Typst 按 SVG 固有尺寸当作**行内盒子**放置；宽度超过当前行剩余空间就被挤到下一行，形成「行内图片换行」。③ D84/D85 的 `prepare_renderables` 对行内公式与块级公式/图表一律输出相同的 `![公式](dest)` / `![图表](dest)`，渲染器无法区分，不能只对行内压缩。**决策**：① `RenderRequest` 新增 `inline: bool`（纯数据字段，桌面按请求 kind/key/range 渲染，不构造该结构，故无破坏性）：`collect_from_blocks` 对行内 `Math` 片段置 `true`，块级 `$$…$$` 与 mermaid 图表置 `false`。② `prepare_renderables` 仅对 `inline && kind == Math` 的图片 alt 使用**保留内部标记** `__mz_inline_math__`（块级公式/图表 alt 仍为「公式」/「图表」，不外泄）；HTML/EPUB 与 Typst 渲染器识别该标记后按行高缩放：`<img style="max-height:1.1em;vertical-align:-0.15em">`、`#image(..., height: 1.1em)`，并把 alt 还原为面向读者的「公式」。em 相对当前字号，图片宽度按 SVG 宽高比自动缩放，因此既贴近正文又不会撑破行宽而换行；块级公式/图表不加尺寸约束，保留原固有尺寸独占显示（避免被压得过小），HTML 既有 `img{max-width:100%}` 仍防超宽。③ **不降低 `EXPORT_MATH_FONT_SIZE`**：SVG 是矢量字形轮廓，32px 只决定固有尺寸不影响缩放清晰度，保留大字号可保证块级公式/图表在纸面与宽屏上的可读性；也不改导出架构、不改 `to_html`/`to_typst` 的对外签名。④ 只改导出副本，编辑器 `Document`/`OffsetMap`/光标/选区/源文本零改动。**限制**：保留 alt 标记属于导出副本内部约定，用户图片若恰好把 alt 写成该字面串也会被按行内公式缩放（概率极低，文档化）；行内尺寸为固定 1.1em，不随用户自定义 PDF 字号以外的排版参数自适应（PDF 正文 11pt、HTML 正文约 16px 均可接受）；DOCX 字面限制（D84）不变；EPUB 阅读器需支持 CSS `max-height`（现代阅读器 OK）；截图环境不稳定，以纯函数单测 + 全门禁保证参数契约，待用户实机验收观感。 | 确认（Phase 4 导出优化） |
| D87 | 块级公式导出图片缩小 + 导出默认文件名跟随 Markdown 主干（用户实机验收反馈） | **问题**：用户验收 0.1.111 反馈「图表图片大小可以保持现有的。但块级公式的图片希望缩小一点。然后导出的文档名称保持和 markdown 名称一致」。**根因核实**：① D86 的保留 alt 机制只区分行内公式（`__mz_inline_math__`），`prepare_renderables` 对块级公式与 mermaid 图表分别输出普通 alt「公式」/「图表」，`html::render_widget` 与 `typst::image_to_typst` 只对行内标记施加尺寸约束，块级公式与图表都按 `EXPORT_MATH_FONT_SIZE=32` 生成的 SVG **固有尺寸**落地，渲染器无从区分二者；因此要缩小块级公式而不动图表，必须沿用保留 alt 再引入一个仅块级公式携带的标记。② 桌面 `ExportTarget` 的 `default_stem` 对 HTML/PDF/DOCX/EPUB 四格式全部硬编码 `"document"`，`export_document` 直接 `.set_file_name(target.default_file_name())`；而 `export_title`（HTML/EPUB 文档内标题）已会从 `doc_path.file_stem()` 取主干，两者各走各的，导致保存对话框默认名与文件名不一致。**决策**：① **块级公式尺寸**：`render_assets.rs` 新增块级公式专用保留 alt `BLOCK_MATH_ALT = "__mz_block_math__"`，`prepare_renderables` 仅对「Math 且 !inline」的图片使用它（行内仍 `INLINE_MATH_ALT`、图表仍「图表」，三者互不相同）；HTML/EPUB 识别后输出 `style="max-height:2em"`、Typst 输出 `height: 2em`，alt 都还原为「公式」，内部标记不外泄。选 **2em** 的理由：行内 1.1em 已验收 OK，块级取约两倍（HTML 16px 正文≈32px、Typst 11pt 正文≈22pt），既明显小于当前 32px 固有尺寸、又显著大于正文；公式是矢量 SVG，缩放不失真，分式/积分/上下标在 2em 仍清晰可辨；`max-height` 只封顶、宽度按宽高比自适应，不改块级图片独占显示与水平位置（原实现本就无额外居中/对齐规则，HTML 沿用 `img{max-width:100%}` 防超宽）。**不降低 `EXPORT_MATH_FONT_SIZE`**，不改导出架构与 `to_html`/`to_typst` 对外签名；mermaid 图表、行内公式、用户普通图片一律保持原有尺寸行为。② **导出文件名**：删除 `ExportTarget.default_stem`/`default_file_name()`，新增纯函数 `export_default_file_name(doc_path: Option<&Path>, target: &ExportTarget) -> String`：取 `file_stem()` 非空主干 + `target.extension`；无路径或空主干回退 ASCII `document.{ext}`（`export_title` 的「码札文档」仅作文档内标题，**不**作文件名）；`export_document` 改调该函数，用户仍可在保存对话框改名；`export_title` 文档内标题行为不变。**限制**：保留 alt 属导出副本内部约定，用户图片 alt 恰为 `__mz_block_math__` 会被同样缩放（概率极低，文档化）；2em 是固定相对值，含超多层分式的极端公式可能偏小（仍可缩放阅读）；DOCX 字面限制（D84）不变；EPUB 阅读器需支持 CSS `max-height`（同 D86）；截图环境不稳定，以纯函数单测（标记区分、em 参数、尺寸样式、文件名规则与回退、特殊路径健壮性）+ 全门禁保证契约，待用户实机验收观感。 | 确认（Phase 4 导出优化） |
| D88 | CommonMark 类型 1–5 HTML 块（`HtmlBlock`） | **目标**：补齐 CommonMark 类型 1–5 HTML 块，作为不透明原文叶子块纳入解析/投影/序列化/导出与增量链路，不引入 HTML 解析与 DOM 建模。**类型与识别**（`mazha-md`）：仅识别起始行缩进 ≤3 的类型 1–5——① 类型 1：`<script|pre|style|textarea`（标签名 ASCII 不区分大小写，后跟空白/`>`/行尾），结束条件为对应关闭标签（大小写不敏感，可同起始行出现）；② 类型 2：`<!--`，以 `-->` 结束；③ 类型 3：`<?`，以 `?>` 结束；④ 类型 4：`<!` 后跟大写 ASCII 字母，以 `>` 结束；⑤ 类型 5：`<![CDATA[`，以 `]]>` 结束。起始行满足结束条件则同一块闭合，否则逐行延伸直到首次出现结束条件的行；找不到结束条件则延伸至文档末尾（EOF）。扫描分支位于围栏代码/公式块之后、脚注定义/表格之前；`starts_new_block` 纳入 HTML 块起始，使段落遇到起始行可中断；**不**与既有行内 HTML（D56）及单行 `<video>`/`<audio>` 媒体元素（D64）冲突。**模型与投影**：新增 `BlockKind::HtmlBlock` 叶子块（不引入 `BlockWidgetKind`），块内容按**原文逐行**产出 `SpanKind::Text`/`SpanRole::Normal` 片段，**不做行内 Markdown 解析**；投影复用既有叶子块文本行路径，无专用块级 widget。**序列化**：`serialize.rs` 提取 `html_block_to_md`，按块内行片段原样拼回内容并经 `prefix_lines` 回填容器前缀，保证引用/列表等容器内逐字节往返与幂等（容器前缀不泄漏、不重复）。**增量**：`incremental.rs` 将未闭合 `HtmlBlock` 纳入可向后延伸的非安全裁切构造（与围栏/公式块同类），`is_unclosed_fence` 改为基于源文本判定并委托 `scanner::html_block_is_closed`；未闭合→补闭合、删闭合、改写起始行、容器内编辑等场景增量结果与全量一致。**导出**：HTML/EPUB（含 XHTML 路径）按片段逐行原样透传内容（剥离容器前缀后不转义、不做行内解析）；Typst 按原文降级为 raw 块字面输出（动态 fence）；DOCX 按原文降级为纯文本段落并保留行间软换行；容器场景下内容前缀剥离正确。**限制**：仅类型 1–5，不含类型 6/7；不尝试解析/高亮/校验 HTML 结构；桌面端无专用渲染态/编辑态 widget（按普通文本块显示原文），导出降级为字面呈现而非可执行/可排版的 HTML。**实现**：`crates/mazha-md/src/{block,scanner,incremental,serialize}.rs`、`crates/mazha-core/src/document.rs`（管线映射）、`crates/mazha-export/src/{html,typst,docx}.rs`；clippy 适配（`u8::is_ascii_uppercase`、提取 `html_block_to_md` 消除 `too_many_lines`）。 | 确认（Phase 1/3 方言补齐） |
| D89 | 脚注多段落定义（容器体，lazy continuation 排除） | **问题**：D35/D69/D81 仅支持叶子式脚注定义（首行正文 + 缩进 ≥4 的续行按同一段落软/硬换行拼接），无法表达定义内的多段落/多块内容；导出虽可归集定义，但体只能按单段渲染。**决策**：① **模型**：`BlockKind::FootnoteDef` 改为**容器块**（`Block::is_container()` 为真）。定义头（`[^label]:` 标记与首行正文）仍以 `spans` 表达；定义体解析为 `children` 子块。② **续行与边界**（`mazha-md::scanner`）：缩进 **≥4** 的非空行属于同一定义，构造子块扫描输入前剥离 **4 空格**定义缩进（保留更深缩进给子块自身语法，如嵌套列表/代码块/引用前缀）；空行**仅当**其后存在后续非空且缩进 ≥4 的续行时才归属该定义（可跨空行连接多段/多块），否则定义在该空行前结束；首行文本可与后续多块体并存。**不支持惰性续行（lazy continuation）**（与定义前缀剥离、空行前瞻和逐字节往返约束冲突）。③ **序列化**：`FootnoteDef` 作为容器序列化——头部行按原 spans/行写回，子块按常规块序列化后逐行补加定义前缀（4 空格）并保持幂等/逐字节往返。④ **增量**：因定义可跨空行续接（需前瞻），可形成此类续接的 `FootnoteDef` 视为可向后延伸构造；`is_unclosed_fence`/安全切点校验须将其判为非安全裁切，避免局部重解析破坏块边界。⑤ **索引/导出/核心**：`FootnoteIndex` 仍按整块范围与头部标签工作（`entry_for_definition` 覆盖整个容器）；`mazha-export::footnotes::prepare` 继续按整块字节范围重编号/归集并剥离外层容器前缀（D81）；HTML/EPUB/Typst/DOCX 渲染定义体时从子块生成内容，保留锚点/回链/编号/文末归集语义（D35/D69/D81）。`mazha-core` 投影与 `Document::footnote_index()` 兼容容器定义。⑥ **限制**：不支持惰性续行；复杂 tab/混合容器前缀剥离仍按既有退化策略；WYSIWYG 不改变定义原位与标签原文。**实现**：`crates/mazha-md/src/{block,scanner,serialize,incremental,footnote}.rs`、`crates/mazha-export/src/{footnotes,html,typst,docx}.rs`、`crates/mazha-core/src/{projection,document}.rs` 及相关测试。 | 确认（Phase 3/4 脚注补齐） |
| D90 | 脚注多引用回跳：桌面来源引用 + 顺序循环，HTML/EPUB 多回链 | **问题**：D69/D81 的多次引用只能回跳**首个**引用：桌面 `footnote_jump` 每次点击都重建 `FootnoteIndex`（已含全部 `references` 区间）却只取 `first()`，来源没有存活到定义点击；HTML/EPUB 的每个引用已有唯一 `fnref-{偏移}` 锚点，但定义侧只生成一条 `↩`，读者从第二/第三个引用进入定义后回不到点击处。**决策**：① **桌面**：`DocView` 会话字段保存最近一次「引用 → 定义」的来源（脚注标签 + 下次应回到的引用起点源字节偏移），切换标签时清空、不写回 Markdown；纯函数 `footnote_jump(index, label, src, origin)` 同时返回目标偏移与更新后的游标（回跳后游标前移一条，末条后回到首条），GPUI 事件处理只负责调用与 `goto_source`。点引用仍跳定义块起点并把游标重置到该引用；点定义首次回到该来源引用，之后连续点击按 `references` 文档顺序循环。来源标签不匹配或起始偏移在重建索引中找不到（编辑导致偏移漂移）时回退首个引用并进入下一次循环；无对应条目、无定义、无引用或无关落点返回 `None`、不改状态。② **HTML/EPUB**：`push_backlinks` 为同一定义的每条引用生成一条 `<a class="fnback" href="#fnref-{偏移}">`，可视文本为 `↩¹ ↩² …`（序号用 Unicode 上标数字，逐位映射，可覆盖两位数）；**单引用仍输出原样的单条 `↩`**，既有导出逐字节不变；多链接之间只加一个空格，沿用既有 `.fnback` 样式，静态 HTML 不引入 JS；EPUB 复用同一 XHTML 渲染路径。③ **不改动**：编辑器源文本 / `OffsetMap` / 光标与选区映射，`mazha-md::FootnoteIndex` 契约与 `FootnoteEntry` 结构（全部引用区间早已保存），编号与文末归集（D69/D81/D89），Typst/DOCX（纯编号无锚点）。**取舍**：桌面回跳状态只在当前标签会话内有效且不持久化；编辑造成偏移漂移时只能回退首引用（不做偏移追踪/CRDT 锚点，成本高且偏离低风险目标）；多回链用上标字符而非 `<sup>`/额外 CSS，避免改动单引用输出与行高。**验证**：桌面纯函数单测覆盖双向、多引用顺序循环、失效/跨标签来源回退、无定义/无引用/非法偏移；导出单测覆盖单引用输出逐字节不变、多引用回链数量/锚点/顺序/文案；根 workspace 与桌面门禁全绿。 | 确认（Phase 4 脚注打磨） |
| D85 | 修复导出行内公式显示为 Markdown 图片源码（用户实机验收反馈） | **问题**：用户验收 0.1.109 反馈「行内公式现在显示 markdown 的图片源码了，没有正确显示图片」——块级 `$$…$$` 与 mermaid 图表正常，仅行内 `$…$` 出错。**根因核实**：`mazha-md` 把 `$x^2$` 解析为三个相邻片段：开 `$`(`MathMarker`，`abs..abs+1`)、公式体(`Math`，`abs+1..base+k`)、闭 `$`(`MathMarker`，`base+k..base+k+1`)；D84 的 `collect_from_blocks` 收集行内公式时只取 `Math` 片段的 `sp.range`（**仅公式体，不含两端 `$`**）作为 `RenderRequest::range`，键为 `source[sp.range]`。`prepare_renderables` 把 `![公式](dest)` 替换到公式体后，源码变成 `$![公式](dest)$`——图片语法被夹在 `$…$` 中；导出器 `to_html_document`/Typst 重新解析该副本时又把它识别为行内公式，将 `![公式](dest)` 作为公式**字面文本**输出，于是用户看到 markdown 图片源码。块级公式（覆盖整块含 `$$`）与 mermaid（覆盖整段围栏）替换后无残留定界符，故正常，与反馈一致。**决策**：① `render_assets.rs` 新增纯函数 `inline_math_range(spans, index)`：给定块内 `Math` 片段的下标，仅当紧邻的前一片段为 `MathMarker`、长度为 1 且 `prev.range.end == math.range.start`，以及紧邻的后一片段为 `MathMarker`、长度为 1 且 `next.range.start == math.range.end` 时，才把区间扩到覆盖这两个 `$`；否则保留公式体原区间。边界判定保证只合并与该 `Math` 首尾相接、左右各恰好一个 `$` 的定界符，不误伤其它 marker、不跨公式合并。② `collect_from_blocks` 改用该合并区间作 `RenderRequest::range`，但 `key` 仍取纯公式体 `source[sp.range]`，保证与桌面 RaTeX 渲染键完全一致（不能把 `$` 塞进 key 导致查不到资产而降级）。③ `prepare_renderables` 的倒序 `replace_range` 逻辑不变：所有请求区间在同一次遍历中收集且互不重叠，变长的区间不影响其它区间（由单测覆盖同一行多个公式互不串扰）。**限制**：只修导出副本的替换区间，编辑器源文本/`OffsetMap`/光标/选区零改动；非法/空公式仍保留原字面降级（D34/D84 既有行为）；外嵌 Markdown 中的公式若被自定义语法包裹（非 `mazha-md` 的三片段结构）不在本轮范围；DOCX 字面限制（D84）不变。截图环境不稳定，以纯函数单测 + 全门禁保证契约，待用户实机验收导出行内公式。 | 确认（Phase 4 导出修复） |
| D91 | `mazha-store` 文件监听（D24 遗留，Phase 5 第一步） | **问题**：D24 把 `watch` 推迟到 Phase 5；MVP 后外部程序修改工作区 `.md` 时编辑器/文件树无感知，store 层缺少能力位与事件契约。**方案取舍**：① 选 `notify` 9（`recommended_watcher`，rust-version 1.91 兼容工作区 1.92，跨平台 FSEvents/inotify/ReadDirectoryChanges/kqueue；纯 Rust 无 GUI），不选自写 std 轮询（延迟高、无事件语义、要自己处理重命名配对与增量扫描）也不选异步 runtime（保持 D24 同步 trait）；监听线程封在 `FileSystemProvider` 内部，对外仍是同步 `StorageProvider::watch(dir) -> StoreResult<Option<Box<dyn WatchHandle>>>`（`Send`，drop 即停）。② `StorageCaps` 增 `watch: bool`，trait 默认返回 `Ok(None)`，不支持的 Provider 不 panic；`FileSystemProvider::watch` 对不存在/非目录返回 `NotDirectory`，后端失败返回 `StoreError::Watch`。③ 事件模型固定四类（Created/Modified/Removed/Renamed{from,to}），路径一律工作区相对 `DocPath`；只上报 `.md`/`.markdown`，忽略目录/非 UTF-8/根外路径；`RenameMode::{From,To}` 降级为 Removed/Created（不跨事件猜配对）。④ 原始事件→规范化事件与「短时间内同一路径重复修改折叠」抽成**纯函数**（`normalize_watch_events`/`coalesce_watch_events`）并配单测，监听线程只负责收事件、打时间戳、调纯函数、投递；窗口固定 150ms，首次发出的事件优先（Created+Modify 折叠为 Created）。⑤ **自写文件反馈**：store 不内部抑制自身 `write`/`rename`/`remove` 产生的事件，一律按真实磁盘事件上报；上层按「路径是否正在编辑且脏」比对内容后决定是否提示/重载，store 不感知编辑器状态（避免过度设计与误抑制外部同档修改）。⑥ **桌面接入边界**：本期只做 store 层，不改 `platforms/desktop/src/main.rs`；外部变更提示/重载与脏文档冲突 UX 留下一期。⑦ 锁文件：根 `Cargo.lock` 与桌面独立 `platforms/desktop/Cargo.lock` 均只**新增** notify 9 及其依赖，不升级既有条目，保留桌面锁中已 yanked 的 `zip 0.5.13`。**限制**：后端运行期错误不进入事件流（回调忽略）；无跨事件重命名配对、无文件内容哈希、无防抖配置；递归监听整棵子树，非 Markdown 事件在纯函数层过滤。 | 确认（Phase 5 第一步，store 层） |
| D92 | 桌面接入文件监听事件（外部变更提示/重载与脏文档冲突处理） | **问题**：D91 已提供 store 层 `WatchEvent`/`WatchHandle`，但自写事件不抑制且桌面未接线；外部程序改/删/重命名工作区 `.md` 时标签与文件树无感知，脏标签静默重载会丢未保存编辑，静默保存会覆盖外部改动。**决策**：① **监听生命周期**：`DocView` 在 `workspace_root` 确定后对整个根**只调一次** `FileSystemProvider::watch(root)`（D91），句柄随视图持有，根变化/释放时 drop（drop 即停）；无工作区静默降级为现状。后台线程 `recv`，`cx.spawn` + `WeakView::update` 派发到 GPUI 主线程，所有 UI 变更只在主线程做；D91 已做 150ms 同路径折叠，桌面不二次防抖。② **回声抑制（关键）**：D91 决策⑤把自写事件照常上报，桌面用**磁盘字节 vs 该标签 `saved_text` 字节**比对：`==` 判为本应用写入回声（仅刷新文件树，不提示不重载），`!=` 才是外部实质修改；读失败/非 UTF-8 不静默重载。基准取 `saved_text` 而非 `doc.text()`：脏标签的后者本就不同，只有保存/打开时的磁盘快照能判断磁盘是否被外部改动；活动标签在 `self.saved_text`、非活动在 `tabs[i].saved_text`。③ **干净标签**：外部修改时整文档重载（`Document::new(磁盘文本)` + `saved_text` 同步 + 清 layout/撤销/高亮/查找/回跳游标 + 光标 clamp），等同重新打开、不产生撤销记录、不弹模态框。④ **脏标签**：绝不自动重载；标签会话态 `ExternalChange{path, disk_text}`（随 `sync_current_tab`/`activate_tab` 迁移，不写 Markdown、不持久化）驱动**非模态横幅**「文件已在外部被修改」+「使用磁盘版本」（丢弃本地编辑并重载）/「保留本地版本」（仅把 `saved_text` 更新为磁盘文本，使本地编辑成为「相对新磁盘基线的脏」，清标记；后续同内容事件自然按回声过滤，用户 `Cmd+S` 才显式覆盖）。标记未解决时拦截 `Cmd+S`/`Cmd+Shift+S`/命令面板保存，避免静默覆盖。⑤ **删除/重命名/Created**：`Removed` 不自动关闭也不重载，横幅「文件已在外部删除」+「关闭标签」（不保存）/`另存为…`（写新路径并更新标签 path），选定前拦截保存；`Renamed{from,to}` 在 `from` 对应已打开标签、`to` 在工作区内且未被其它标签占用时**只更新标签 path**（`saved_text` 不变、不重载、不产生脏），否则拆为 `Removed(from)` + `Created(to)` 分别处理；`Created` 无对应标签仅刷新树，对应「读取失败保留的空标签」按修改处理。路径统一 canonicalize 后比对。⑥ **纯函数与验证**：事件分类（`classify_watch_event`，输入为事件 + 根 + 标签只读快照）、重命名安全性、横幅文案、保存拦截判定抽桌面纯函数并配单测（不依赖 GPUI/文件系统）；复用 `refresh_workspace`，不引入新依赖、不改核心/`mazha-md`/导出契约与 OffsetMap/光标映射规则（重载=整文档替换）。**取舍**：不做跨设备冲突自动合并（CRDT 同步另期）、不做可配置防抖/内容哈希（D91 固定 150ms + 字节比对足够）、编码异常只提示不猜测、横幅为会话态（关标签即丢）。 | 确认（Phase 5 第二步，0.1.117 落地） |
| D93 | macOS 拆分重命名事件配对（用户实机验收反馈：外部改名后 Tab 标题不跟随、点新路径开新标签） | **问题**：D92 的安全重命名分支依赖 `WatchEvent::Renamed{from,to}`，但 D91 选的 `notify` 9 `recommended_watcher` 在 macOS FSEvents **即时模式**下一次改名只产生前后两条 `Modify(Name(Any))`（先旧路径、后新路径），**没有** `Name(Both)`/`From`/`To`；`normalize_watch_events` 把这些按 `Modify(_)` 规范化为两条 `Modified`，于是桌面把旧路径当外部修改/读取失败、新路径仅刷新文件树：Tab 仍保留旧 `path`（标题不更新），从资源管理器点新文件 `open_path` 匹配不到已打开标签而新建 Tab。**实机探针确认**：两次事件间隔为 0ms、各带一个路径。**决策**：① **store 层配对**：`mazha-store::watcher` 新增内部 `RenamePairer`（250ms 窗口），notify 回调给每条规范化事件附带「是否来自 `Name(Any)`」标记；窗口内前后两条不同路径的名字事件合并成一条 `Renamed{from,to}`，用**第一条的到达时刻**投递；超时/同路径则旧事件退回普通 `Modified`，新事件继续挂起等待；挂起期间旧路径的尾随事件丢弃（随改名失效），其它路径照常透传；工作线程每轮先排空事件再冲刷超时项，避免配对被定时冲刷抢先打断。② **不改契约**：`WatchEvent` 仍是 Created/Modified/Removed/Renamed 四变体，`normalize_watch_events` 对 `Name(Any)` 仍返回 `Modified`；配对是监听线程的内部状态机，纯函数 `normalize_watch_events`/`coalesce_watch_events` 签名与 150ms 折叠规则不变；`From`/`To` 仍按 D91 降级、不跨事件猜测。③ **桌面零改动**：D92 的 `safe_rename_target` 与 Tab path 更新逻辑原样复用，因此「from 已打开、to 在根内且未被占用 → 只更新 path」（`saved_text` 不变、不重载、不脏）在 macOS 真正生效；真删除仍走 `Removed` 横幅，不会误判为改名。④ **验证**：新增 5 条 store 纯函数单测（Name(Any) 规范化、配对、未配对回退、同路径/尾随事件、超窗口不配对）+ 真实 FS watch 探针端到端确认输出单条 `Renamed`；根 637 → **642**、桌面 112 不变。**限制**：只配对称重命名的两条名字事件（窗口 250ms 固定、不开放配置）；连续 A→B→C 只配首对，剩余 B→C 按普通修改处理；不引入 `notify-debouncer-full` 等新依赖，也不改 `Cargo.lock`。 | 确认（Phase 5 第一步补丁，0.1.118 落地） |

## 15. 变更记录

| 版本 | 日期 | 变更内容 |
|------|--------|----------|
| 0.1.118 | 2026-11-23 | **macOS 拆分重命名事件配对（D93，D92 实机验收补丁）**：① **问题/调查**——用户反馈外部改名后资源管理器刷新但已打开 Tab 标题不变、点新路径会另开标签；确认 D92 安全重命名分支依赖 `Renamed`，而 `notify` 9 的 FSEvents 即时模式只给前后两条 `Modify(Name(Any))`，被 D91 规范化成两条 `Modified`，桌面因此只刷新树、不更新 Tab path。本机原始事件探针确认两条事件各带一个路径且间隔 0ms。② **实现（`crates/mazha-store/src/watcher.rs`）**——新增内部 `RenamePairer`（250ms 窗口）：notify 回调为事件附「是否来自 `Name(Any)`」标记，监听线程把窗口内前后不同路径的两条名字事件合并成一条 `Renamed{from,to}`（取首条时间戳），超时/同路径退回普通 `Modified`，挂起期间旧路径尾随事件丢弃、其它路径透传；每轮先排空事件再冲刷超时项。`WatchEvent` 四变体、`normalize_watch_events`/`coalesce_watch_events` 签名与 150ms 折叠规则均不变，`From`/`To` 仍按 D91 降级。③ **桌面**——零改动，D92 的安全重命名判定与 Tab path 更新原样生效；真删除仍走 `Removed` 横幅。④ **测试**——store 新增 5 条纯函数单测（Name(Any) 规范化/配对/未配对回退/同路径与尾随事件/超窗口不配对），store 21 → **26**，根 workspace **637 → 642**；桌面 **112** 不变。⑤ **边界/验证**——不新增依赖、两个 `Cargo.lock` 不动、核心 crate 零改动；根 workspace `cargo fmt --all --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace` 与桌面 `cargo fmt --check`、`cargo clippy -- -D warnings`、`cargo test`、`cargo build` 全绿。SPEC 版本升至 **0.1.118**。 |
| 0.1.117 | 2026-11-23 | **桌面接入文件监听事件（Phase 5 第二步，D92）**：① **契约/文档**——SPEC §6.2/§7/§11.12/§14 D92 状态改为已实现，D91 的「桌面接入留下一期」标记为已由 D92 补齐；§15 记本版。② **实现（`platforms/desktop/src/main.rs`）**——`DocView` 在 `workspace_root` 确定后只对整根 `watch` 一次并持有 `WatchHandle`，根变化/视图 drop 时停旧根并释放；后台线程 `try_recv` + 停止标志投递，前台 `cx.spawn` 20ms 轮询 + `WeakView::update` 派发到 GPUI 主线程（D91 已做 150ms 同路径折叠，桌面不二次防抖）。事件路径 `root.join(DocPath)` 并 canonicalize 后交给纯函数；磁盘字节 vs `saved_text` 抑制自写回声，干净标签整文档重载（`Document::new` + 清布局/撤销/高亮/查找/回跳/滚动跟随 + 光标 clamp），脏标签挂非模态横幅「文件已在外部被修改」+「使用磁盘版本」/「保留本地版本」，删除挂「文件已在外部删除」+「关闭标签」/「另存为…」，未解决前拦截普通保存（另存为仅放行删除/读失败冲突；读失败或非 UTF-8 的修改无磁盘文本可用时，横幅退化为「关闭标签 / 另存为…」，避免两个按钮都空转的死横幅）。安全重命名只更新标签 path，否则拆 `Removed`+`Created`；Created 命中读取失败空标签按修改处理（干净空标签直接重载）。③ **纯函数与测试**——新增 `classify_watch_event`/`safe_rename_target`/`external_change_banner`/`external_change_status_text`/`should_block_save(_as)` 等纯函数与 18 条桌面单测（回声/干净重载/脏冲突/读失败/删除/重命名/Created/目标选择/横幅文案/保存拦截），桌面单测 94 → **112**。④ **边界**——只改 `platforms/desktop/src/main.rs` 与 SPEC/HANDOFF；核心/`mazha-md`/导出/store 契约、OffsetMap/光标映射零改动；不新增依赖，两个 `Cargo.lock` 均不动。⑤ **验证**——根 workspace `cargo fmt --all --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace` 全绿（**637** 项不变）；桌面 `cargo fmt --check`、`cargo clippy -- -D warnings`、`cargo test`（**112** 项）、`cargo build` 全绿。SPEC 版本升至 **0.1.117**。 |
| 0.1.116 | 2026-11-23 | **`mazha-store` 文件监听 store 层（Phase 5 第一步，D91；D24 遗留）**：① **契约/文档**——SPEC §5/§6.2/§8/§10 补齐 `StorageCaps::watch`、`StorageProvider::watch`（默认 `Ok(None)`）、`WatchEvent` 四变体（Created/Modified/Removed/Renamed{from,to}）、`WatchHandle`（`Send`，drop 即停）、错误映射与事件过滤/去重语义，新增 D91（方案取舍、自写文件反馈策略、桌面接入边界）；§15 记本版。② **实现（`crates/mazha-store`）**——新增 `watcher.rs`（模块名避开 clippy `module_name_repetitions`）：纯函数 `normalize_watch_events(root, &notify::Event)`（工作区相对路径、仅 Markdown、非 UTF-8/根外/目录忽略；Rename 双路径配对）与 `coalesce_watch_events(&[(Instant, WatchEvent)], window)`（同路径 150ms 内重复修改折叠、Created+Modify 折叠为 Created）；`FileSystemProvider::watch` 用 `notify` 9 `recommended_watcher` 递归监听，内部线程 + `mpsc` 投递，句柄 drop 发停机信号并 join；`StorageCaps.watch = true`（local），`StoreError::Watch` 新变体。③ **自写反馈**——store 不抑制自身写事件，一律上报真实磁盘事件，上层按编辑/脏状态比对过滤（D91）。④ **边界**——不改 `platforms/desktop/src/main.rs`；外部变更提示/重载与脏文档冲突 UX 留下一期；核心 crate 无 GUI 依赖不变，D24 同步接口不翻案。⑤ **依赖/锁**——`notify = "9"` 加入 `mazha-store`；根 `Cargo.lock` 与桌面独立 `platforms/desktop/Cargo.lock` 仅新增依赖，保留桌面锁中 `zip 0.5.13` 等既有 yanked 条目。⑥ **验证**——`mazha-store` 单测 12 → **21**（+8 纯函数映射/去重/自写反馈 +1 真实 FS 监听，5/5 单测隔离、3/3 全套件重复运行稳定）；根 workspace `cargo fmt --all --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace` 全绿（**637** 项：core 205、crdt 11、export 188+3、ffi 1、md 206、plugin 1、store **21**、wasm 1）；桌面 `cargo fmt --check`、`cargo clippy -- -D warnings`、`cargo test`（**94** 项不变）、`cargo build` 全绿（构建仅有既有 `__eh_frame section too large` 链接器提示）。SPEC 版本升至 **0.1.116**。 |
| 0.1.115 | 2026-11-23 | **脚注多引用回跳（D90）**：① **契约/文档**——SPEC §9.1/§11.10 改为「桌面来源引用回跳 + 顺序循环、HTML/EPUB 多回链」，D69/D81 的「只回跳首个」限制标记为已由 D90 细化，新增 D90 决策记录（§14）。② **桌面（`platforms/desktop/src/main.rs`）**——新增 `FootnoteOrigin`（标签 + 下次应回到的引用起始字节偏移）与纯函数 `footnote_jump(index, label, src, origin)`：点引用仍跳定义块起点并把游标重置到该引用；点定义首次回到该来源，之后连续点击按 `references` 顺序循环（末条后回到首条）；游标标签不匹配或偏移失效（编辑漂移）回退首引用，无定义/无引用/非法落点返回 `None` 且游标不变；`DocView.footnote_origin` 只在当前标签会话保存，切换标签清空，GPUI 事件只调用纯函数与 `goto_source`。③ **导出（`crates/mazha-export/src/html.rs`）**——`push_backlinks` 按 `FootnoteIndex` 的全部引用生成多条 `.fnback`，各自指向 `fnref-{偏移}`；单引用仍输出原样单条 `↩`（逐字节不变），多条用 Unicode 上标数字 `↩¹ ↩² …` 区分并以单空格间隔，沿用既有 CSS、不引入 JS；EPUB/XHTML 共用同一渲染路径，Typst/DOCX 不受影响。④ **边界/约束**——编辑器源文本、`OffsetMap`、光标/选区映射零改动；`mazha-md` 索引结构与 `mazha-core` 契约不变；编号/文末归集（D69/D81/D89）不变。⑤ **验证**——桌面 3 条纯函数单测（双向 + 游标、三引用循环、跨标签/漂移回退与无定义/无引用/非法偏移），导出新增 2 条并加强 1 条（单引用逐字节、多引用锚点顺序与文案、XHTML 路径）；根 workspace `cargo fmt --all --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace` 全绿（**628** 项：core 205、crdt 11、export 188+3、ffi 1、md 206、plugin 1、store 12、wasm 1）；桌面 `cargo fmt --check`、`cargo clippy -- -D warnings`、`cargo test`（**94** 项）、`cargo build` 全绿（构建仅有既有 `__eh_frame section too large` 链接器提示）。SPEC 版本升至 **0.1.115**。 |
| 0.1.114 | 2026-11-20 | **脚注多段落定义（容器体，D89；lazy continuation 排除）**：① **契约/文档**——SPEC 同步 §9.1/§11.4/§11.10/§12.2，修订 D69/D81 并新增 D89（§14）；`FootnoteDef` 改为容器块：头部 `[^label]:` 与首行正文保留在定义块 `spans`，定义体解析为子块；缩进 **≥4** 的非空行为续行并在子块解析前剥离 4 空格定义缩进；空行仅在其后存在后续非空且缩进 ≥4 的续行时归属定义（可跨空行连接多段/多块）；首行文本可与后续多块体并存；**不支持惰性续行**。② **解析/模型/序列化（`mazha-md`）**——`block.rs` 将 `FootnoteDef` 纳入容器；`scanner.rs` 实现续行/空行前瞻与子块扫描输入构造；`serialize.rs` 按容器序列化头部与子块并回填定义前缀，保持逐字节往返与幂等；`footnote.rs` 索引兼容容器定义（整块范围、头部标签不计引用）。③ **增量（`mazha-md`）**——可跨空行续接的 `FootnoteDef` 视为可向后延伸构造，安全裁切校验将其判为非安全/未闭合类切点，避免局部重解析破坏块边界。④ **导出（`mazha-export`，D35/D69/D81 兼容）**——`footnotes::prepare` 继续按整块字节范围重编号与文末归集并剥离外层容器前缀；HTML/EPUB/Typst/DOCX 从定义体子块渲染多块内容，保留引用锚点/回链/编号/`<section class="footnotes"><ol>` 等语义。⑤ **核心（`mazha-core`）**——投影与 `Document::footnote_index()` 兼容容器 `FootnoteDef`。⑥ **验证**——根 workspace `cargo fmt --all --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace` 全绿（**626** 项：core 205、crdt 11、export 186+3、ffi 1、md 206、plugin 1、store 12、wasm 1）；未改 `platforms/*`。SPEC 版本升至 **0.1.114**。 |
| 0.1.113 | 2026-11-20 | **CommonMark 类型 1–5 HTML 块（D88，方言补齐）**：① **解析/模型**——`mazha-md` 新增 `BlockKind::HtmlBlock` 叶子块，识别起始行缩进 ≤3 的类型 1（`<script|pre|style|textarea>`，大小写不敏感，按对应关闭标签闭合）、2（`<!--`→`-->`）、3（`<?`→`?>`）、4（`<!`+大写字母→`>`）、5（`<![CDATA[`→`]]>`）；同起始行可闭合，未闭合延伸至 EOF；扫描位于围栏/公式后、脚注/表格前，`starts_new_block` 纳入以支持段落中断；内容按行产出 `SpanKind::Text`/`SpanRole::Normal` 原文片段，**不做行内 Markdown 解析**，与既有行内 HTML（D56）及单行 `<video>`/`<audio>` 媒体（D64）不冲突。② **序列化/投影**——`html_block_to_md` 按行片段原样拼回并经 `prefix_lines` 回填容器前缀，引用/列表内逐字节往返与幂等；投影复用既有叶子块文本路径，无 `BlockWidgetKind`。③ **增量**——未闭合 `HtmlBlock` 纳入可向后延伸的非安全裁切构造，`is_unclosed_fence` 改为 `(block, source)` 并委托 `scanner::html_block_is_closed` 判定，闭合/删闭合/改写起始行等场景增量与全量一致。④ **导出**——HTML/XHTML/EPUB 按行原样透传（剥离容器前缀、不转义）；Typst raw fence 字面降级；DOCX 纯文本段落降级并保留行间软换行。⑤ **文件/适配**——`crates/mazha-md/src/{block,scanner,incremental,serialize}.rs`、`crates/mazha-core/src/document.rs`、`crates/mazha-export/src/{html,typst,docx}.rs`；clippy：`u8::is_ascii_uppercase`、提取 `html_block_to_md`。⑥ **验证**——根 workspace fmt/clippy/test 全绿（**612** 项：core 204、md **198**、export **181+3**、crdt 11、store 12、ffi/plugin/wasm 各 1）；未改 `platforms/*`。SPEC 版本升至 **0.1.113**。 |
| 0.1.112 | 2026-11-18 | **块级公式导出缩小 + 导出默认文件名跟随 Markdown 主干（D87，用户实机验收反馈）**：① **根因**——D86 的保留 alt 只标记行内公式，块级公式与 mermaid 图表同为普通 alt、按 32px SVG 固有尺寸落地，渲染器无法区分；桌面 `ExportTarget.default_stem` 四格式硬编码 `"document"`，与已能取主干的 `export_title` 脱节。② **方案**——`render_assets.rs` 新增保留 alt `__mz_block_math__`，仅块级 `Math`（`!inline`）图片携带；HTML/EPUB 输出 `style="max-height:2em"`、Typst 输出 `height: 2em`（约行内 1.1em 的两倍，既小于 32px 固有尺寸又大于正文，矢量缩放清晰；分式/积分仍可读），alt 还原为「公式」；图表与行内公式标记/尺寸一律不动，用户普通图片仍按固有尺寸，`EXPORT_MATH_FONT_SIZE` 不变。桌面删除 `default_stem`/`default_file_name()`，新增纯函数 `export_default_file_name(doc_path, target)`：有路径取 `file_stem`、无路径回退 `document.{ext}`（不拿「码札文档」当文件名），`export_document` 改调该函数，`export_title` 文档内标题不变。③ **文件**——`SPEC.md`（D87 + 本记录）、`HANDOFF.md`、`crates/mazha-export/src/{render_assets,html,typst}.rs`（新保留 alt、em 尺寸常量、渲染分支、中文注释与单测）、`platforms/desktop/src/main.rs`（纯函数与调用点、单测）。编辑器源文本/OffsetMap/光标/选区零改动；导出架构与对外签名不变。④ **验证**——新增单测覆盖三类 alt 互不相同、块级公式 HTML/XHTML 尺寸样式与 data URI/引号转义、Typst 块级公式 `height: 2em` 且图表无 height、文件名有路径取主干/无路径回退/四格式后缀/多点点号与特殊路径；既有 D85 区间、Widget 识别、D86 行内尺寸、缺资产降级与 PDF 虚拟资源注入测试继续通过。根 workspace 与桌面门禁全绿，待用户实机验收观感。 |
| 0.1.111 | 2026-11-18 | **导出图片尺寸合理化 + PDF 行内图片不换行（D86，用户实机验收反馈）**：① **根因**——桌面 `EXPORT_MATH_FONT_SIZE = 32.0` 使 RaTeX SVG 固有尺寸按 32px 计算，HTML/PDF 图片输出未给显示尺寸，行内公式约为正文两倍高；Typst 把行内 `#image` 按固有尺寸放成行内盒子，行宽不够即换行。D84/D85 对行内与块级公式/图表都输出同样的 `![公式](dest)` / `![图表](dest)`，渲染器无从区分。② **方案**——`RenderRequest` 新增 `inline: bool`，`collect_from_blocks` 只对行内 `Math` 置 `true`；`prepare_renderables` 对内嵌行内公式图片使用保留 alt 标记 `__mz_inline_math__`；`html::render_widget` 与该标记配对输出 `style="max-height:1.1em;vertical-align:-0.15em"`、alt 还原为「公式」，`typst::image_to_typst` 输出 `#image("dest", alt: "公式", height: 1.1em)`。em 随字号缩放、宽度按宽高比自适应，解决「太大」与「换行」；块级公式与 mermaid 图表维持固有尺寸不压缩，`EXPORT_MATH_FONT_SIZE` 保持 32 以保矢量质量与块级可读性。③ **文件**——`SPEC.md`（D86 + 本记录）、`HANDOFF.md`、`crates/mazha-export/src/{render_assets,html,typst}.rs`（请求标记、保留 alt 约定、尺寸约束、中文注释与单测）。桌面导出管线/`mazha-md`/核心/其它平台不改；编辑器源文本/OffsetMap/光标/选区零改动。④ **验证**——新增单测覆盖：行内请求 `inline=true` 且替换后带保留 alt、块级公式/图表 `inline=false` 且 alt 不变、Typst 行内图片带 `height: 1.1em` 且 alt 正确转义、HTML/XHTML 两种模式行内图片尺寸与 data URI/引号转义、既有块级公式/mermaid/缺资产降级/D85 区间测试继续通过。根 workspace 与桌面门禁全绿，待用户实机验收导出观感。 |
| 0.1.110 | 2026-11-18 | **修复导出行内公式显示为 Markdown 图片源码（D85，用户实机验收反馈）**：① **根因**——`mazha-md` 将 `$x^2$` 解析为开 `$`(`MathMarker`) / 公式体(`Math`) / 闭 `$`(`MathMarker`) 三个首尾相接片段，但 D84 的 `render_assets::collect_from_blocks` 仅用 `Math` 片段区间（不含两端 `$`）作 `RenderRequest::range`，`prepare_renderables` 替换后残留 `$![公式](dest)$`，导出器重解析时把图片语法当作公式字面输出，导致用户看到 markdown 图片源码；块级公式与 mermaid 请求区间本就覆盖完整定界符，故正常。② **方案**——新增纯函数 `inline_math_range(spans, index)`：仅在 `Math` 片段前后紧邻、首尾相接且长度恰为 1 的 `MathMarker` 成对存在时，才把替换区间扩展为覆盖整个 `$…$`，否则退回公式体区间；`key` 仍取纯公式体，确保与桌面 RaTeX 渲染键一致。`collect_from_blocks` 行内公式收集改用该完整区间，`prepare_renderables` 倒序替换逻辑保持不变（同次收集区间互不重叠）。③ **文件**——`SPEC.md`（D85 + 本记录）、`HANDOFF.md`、`crates/mazha-export/src/render_assets.rs`（合并区间函数/注释/单测）。不改核心/`mazha-md`/桌面导出管线/其它平台；编辑器 `Document`/`OffsetMap`/光标/选区零改动。④ **验证**——新增单测覆盖：行内公式请求区间含两端 `$` 且替换后无残留 `$`、prepared 副本重解析后行内公式变为 `ImageMarker` Widget（不再是 `Math` 字面）、同一行多个行内公式不串扰；块级公式/mermaid/非法与缺资产降级既有测试保持通过。根 workspace 与桌面门禁全绿，待用户实机验收导出行内公式。 |
| 0.1.109 | 2026-11-18 | **导出不阻塞 + 状态栏状态 + 公式/图表渲染进导出（D83/D84，用户实机验收反馈）**：① 桌面导出重活（资源收集、`render_export`、写文件）移到 `cx.background_executor().spawn` 后台线程池，`cx.spawn` 只驱动异步对话框与状态派发，窗口不再被 Typst 编译冻结；`DocView` 增 `export_status`/`export_status_seq`，底部状态栏显示「正在导出 X…／已导出 X：文件名／导出 X 失败：原因」，完成态 6 秒后按 id 自动清除（文案/清除判定为纯函数 + 单测），不引入 toast 框架与全局 Loading 弹窗。② `mazha-export` 新增纯函数 `collect_render_requests`/`prepare_renderables` 与 `RenderAssets`/`AssetTarget`：桌面用 RaTeX `render_latex_to_svg`（gpui-free 纯函数、自包含字形）与 mermaid-rs 产 SVG，注入导出副本；HTML/EPUB 用 `data:image/svg+xml;base64` 图片复用既有 `<img>` 输出，PDF 用 `export_with_assets` 把 SVG 经 `ExportWorld` 内存资源表喂给 Typst `#image`（矢量嵌入），DOCX 因 docx-rs 1.1 无图片 API 维持字面并标注限制。编辑器源文本/OffsetMap/光标不改；渲染失败保留原字面降级。根 workspace 与桌面门禁全绿，待用户实机验收导出观感。 |
| 0.1.108 | 2026-11-18 | **桌面导出四格式补齐 + PDF 缺图降级（D82，用户报告修复）**：① 桌面「文件」菜单/命令面板补齐「导出 HTML / DOCX / EPUB」，`PaletteAction` 增三变体，`export_target`/`export_title`/`render_export` + 通用 `export_document` 复用现有异步对话框链路（各自后缀与过滤器），`Cmd+E` 仍保留 PDF；各 `to_*` API 内建脚注规整（D69/D81），桌面不重复调 `footnotes::prepare`。② PDF 导出改用 `PdfExporter::with_options`，`base_dir` 取当前文档父目录（纯函数 `pdf_base_dir`），真实文档相对路径图片可解析。③ `mazha-export` 新增纯函数 `pdf_image_path`/`replace_unresolvable_images`（回调注入可用性判定，倒序替换图片 Widget 为占位文字），在 `PdfExporter::{export,export_with}` 入口对独立副本预处理：缺文件/远程/data/绝对路径图片一律降级为「（图片：alt）」，Typst 不再因缺资源中断；`to_pdf` 直接导出内置 SAMPLE 也不再失败。④ 内置 `SAMPLE` 移除不存在的 `assets/sample.png` 引用，改为纯文字说明，消除开箱即用的损坏图与报错源。新增纯函数单测与缺图回归；根 workspace fmt/clippy/test 全绿，桌面 fmt/clippy/test/build 全绿，待用户实机验收四类导出文件、含图 PDF 与内置 SAMPLE 导出。 |
| 0.1.107 | 2026-11-18 | **脚注增强：导出反向回链 + 嵌套定义归集（D81，D69 遗留；A 软换行横向滚动评估为高风险未选）**：① HTML/EPUB 引用新增唯一锚点 `id="fnref-{源字节偏移}"`，文末 `<li id="fn-N">` 追加 `↩` 回链 `<a class="fnback" href="#fnref-{首个引用偏移}">`（就地 `<div class="footnote">` 回链不适用，归集后均为顶层项），`DEFAULT_CSS` 增 `.fnback` 样式；② `footnotes::prepare` 的 `move_definitions` 改为递归收集**所有** `FootnoteDef`（含引用/列表内嵌套），剥离容器前缀后按序号统一归集文末，原块按倒序整行裁切；③ `mazha-md` 修正列表项续行缩进解析（`indent(nrest).min(nl.content + marker.len)` 混用相对/绝对偏移，把续行空白剥光导致嵌套定义的续行脱离定义块），改为 `min(indent, marker.len)` 并补回归单测；④ 桌面已有 `footnote_jump` 定义→首个引用跳转（D69）不变，`mazha-core` / 其它平台不改。新增纯函数单测（前缀剥离、嵌套归集与排序、续行、回链、多次引用唯一锚点）；根 workspace fmt/clippy/test 全绿（**553** 项：core 204、md **177**、export **143** + 3 集成、crdt 11、store 12、ffi/plugin/wasm 各 1），桌面 fmt/clippy/test/build 全绿（**81** 项不变），待用户实机验收。 |
| 0.1.106 | 2026-11-18 | **合成斜体中文发虚根治（D80，实机反馈）**：① 查 gpui-pre 0.3.9 路径管线定位根因——`PathBuilder::build_path` 写死 `st=(0,1)` 使解析式 AA 失效，仅剩 ≤4× MSAA（5 级覆盖）、无灰阶 AA/字干加粗，故 1px 字干发虚；② macOS 改由 CoreText `CTFontDrawGlyphs` + 剪切文本矩阵直接栅格化倾斜字形到 DPR 分辨率位图，读 alpha 配 span 颜色生成 `RenderImage`，`window.paint_image` 以 texel 1:1 叠加；③ 新增纯函数 `shear_ink_bounds`/`raster_geometry`/`raster_placement` 与 4 条单测（剪切墨迹范围、整数设备像素几何、DPR 放置、边界），保证位图边界/原点整数像素对齐且透明占位 advance 不变；④ 位图按 `(字符, 字号, DPR, 颜色)` 缓存（含失败缓存 + 容量上限），失败回退 D79 轮廓路径，非 macOS 仍无合成斜体。核心 / `mazha-md` / 导出 / 其它平台不改；本机截图可用（暗色主题斜体 CJK 行 181 级灰阶），已由用户实机验收通过。 |
| 0.1.105 | 2026-11-18 | **中文斜体合成优化（D43 遗留，D79）**：① 合成斜体的字形轮廓按 `(字符, 字号)` 跨帧缓存（本地坐标，paint 阶段仅平移顶点/包围盒），不再每帧逐字取 CoreText 轮廓；② 自绘起点按 gpui 原生字形公式校正：基线叠加形状化字形的 `position.y` 微调、原点按 `window.scale_factor()` 做 half-toward-zero 设备像素对齐，消除与透明占位 run 不足 1px 的错位；③ 桌面新增轮廓缓存复用/路径平移/像素对齐/字形 y 偏移 4 条纯函数单测。核心 / `mazha-md` / 导出 / 其它平台不改；截图环境不可用，未做实机目视验收。 |
| 0.1.104 | 2026-11-18 | **专注/打字机模式打磨（D68 细化，D78）**：① 专注范围由「当前单个逻辑行」改为**段落粒度**——光标在块级 widget 内仍整块高亮，否则向上下扩展连续的普通文本行（空行/标题/列表/任务/引用/代码块/表格/分割线/块 widget 行截断），抽出桌面纯函数 `focus_line_range`。② 打字机滚动由「每次强制居中」改为**中心带**策略——光标中心落在视口中间 50% 内不滚动，越出才滚回带边缘，抽出纯函数 `typewriter_scroll_y`；大纲/查找跳转置顶（D59）与滚动边界 clamp 不变。核心 / `mazha-md` / 导出 / 源码模式不改，新增 2 条桌面纯函数单测。 |
| 0.1.103 | 2026-11-18 | **Mermaid 渲染观感细化（D77 细化，实机反馈）**：① **去掉图表弱底与描边**——`render_chart_preview` 改用 `render_with_options` 把 `theme.background` 设为 `transparent`（mermaid-rs 默认在 SVG 根节点画白色 `<rect>`，深色主题下是突兀白块），paint 阶段不再画 `palette.subtle` 弱底与边框（同 D76 公式块，节点自带浅底/描边）。② **内置示例改用无回边分叉**——`B -->|否| A` 改为 `B -->|否| D[重试]`：mermaid-rs-renderer 0.3.1 对菱形 branching shape 在 degree≥3 时用边中点做端口、回边标签用路由前锚点，导致「否」连线不从菱形顶点出发、标签甩到线外（上游布局缺陷，0.3.1 为最新版无修复）；无回边分叉时两条出边精确从菱形右顶点出发、标签落在连线附近。新增透明背景单测断言（角落 alpha=0）；桌面 68 项、根 workspace 548 项门禁全绿。 |
| 0.1.102 | 2026-11-18 | **桌面 Mermaid 图表真实渲染（R4 第二步，D77）**：桌面新增 `mermaid-rs-renderer 0.3.1`（render-only，纯 Rust 产 SVG，关 CLI/内置 PNG），SVG 经 GPUI `SvgRenderer::render_single_frame` 栅格化为图像（逻辑尺寸除 `SMOOTH_SVG_SCALE_FACTOR`）；`DocView` 按 DSL 源码缓存（含失败缓存）。渲染态 mermaid 块源码转透明占位、锚点行显示图像（弱底 + 圆角 + 描边，按正文宽等比适配），其余行归零；`DocLayout::chart_boxes` 命中图像把光标放进围栏块显示 DSL 原文，移开恢复；渲染失败回退弱底 + 语言标签占位面板。内置 `SAMPLE` 增加 mermaid 示例。核心/导出不改，导出仍按字面量。桌面新增 DSL 提取/缓存键/SVG 栅格化出图/命中框/文本透明化 5 条单测（68 项）；根 workspace 548 项门禁全绿。截图环境不可用，未做实机目视验收，由渲染管线单测验证。 |
| 0.1.101 | 2026-11-18 | **块级公式编辑态显示 TeX 原文 + 行内公式不重叠**（桌面，D76 细化）：① 块级公式编辑态改为整块按原始源码逐行显示（含 `$$` 定界行、1:1 字节映射，复用围栏 `raw_src` 通道），修掉「点进公式后看不到 TeX 原文」——根因是块 body 同为 `RunStyle::Math`，被 `inline_texes` 收进预渲染集合后又被当作行内公式透明化；新增纯函数 `collect_inline_texes` 排除块级公式行与代码块行。② 行内公式叠加层按透明占位槽的起止位置算可用宽度，裁掉 RaTeX 左右各 8px 透明留白后再画，字形区宽于槽位才等比缩小，避免图像越过槽宽压到右侧普通文字；新增纯函数 `math_inline_draw_geom`。新增 2 条桌面单测（行内绘图几何边界、块级 TeX 不混入行内集合）。桌面 fmt/clippy/test/build 全绿（63 项）；实机截图确认块级点击显示含 `$$` 的完整 TeX、移开恢复公式图像、行内公式与右侧文字无重叠。 |
| 0.1.100 | 2026-11-18 | **块级公式点击编辑 + 去装饰**（桌面，D76 细化）：渲染态 `$$…$$` 公式不再画背景/边框；布局阶段新增图像命中矩形（`math_hit_rect` + `DocLayout::math_boxes`），点击图像任意处即把光标落到公式块源范围起点、整块显示 TeX 原文供编辑（与行内公式一致，移开自动恢复渲染态），避免透明占位 run 的视觉行命中落到块外空行。新增桌面单测验证命中矩形边界。桌面 fmt/clippy/test/build 全绿（61 项）；实机截图确认点击进源码、移开恢复公式图像、无边框。 |
| 0.1.99 | 2026-11-18 | **块级公式居左对齐**（桌面，D76 细化）：`$$…$$` 块级公式图像由居中改为居左对齐正文文本起点（与图片块一致），删除 paint 阶段的居中偏移计算；RaTeX 图像自带四周留白，不紧贴 widget 外框。仅调整绘制坐标，不影响 OffsetMap/光标/选区/缓存/导出；行内公式仍垂直居中叠在文本行。桌面 fmt/clippy/test/build 全绿（60 项）；实机截图确认公式字形位于正文起点侧（字形逻辑 x≈249，正文起点 240 + RaTeX 留白），积分/上下限/分数横线完整。 |
| 0.1.98 | 2026-11-18 | **桌面数学公式真实排版（R4 第一步，D76）**：桌面新增 `ratex-gpui`（render-only），`$$…$$` 块与 `$…$` 行内在渲染态由 RaTeX 栅格化为透明底公式图像并叠加显示，TeX 文本改为透明占位（OffsetMap/光标/选区映射不变）；光标进入与源码模式显示原文，渲染失败回退字面量；公式图像按 `(tex, 字号, DPR, 颜色)` 缓存、主题变化时清空。核心/导出不改，mermaid 真渲染留后续。桌面新增缓存键/TeX 拼接/透明占位单测。 |
| 0.1.97 | 2026-11-18 | **侧栏标题栏固定**（桌面，Phase 4，D75）：把侧栏标题 `sb-h`（资源管理器 / ＋文件 / ＋文件夹）从滚动列表中拆出，作为 sidebar 的直接子节点固定 28px 高度；条目列表改由 `sidebar-list`（`flex_1` + `min_h(0)` + `overflow_y_scroll`）承载，列表过长时标题与新建入口不再随滚动消失。标题的拖拽落点（`TreeDrag` / 外部文件）逻辑保持不变。桌面 fmt/clippy/test/build 全绿（55 项）。 |
| 0.1.96 | 2026-11-18 | **入站外部拖拽（从 Finder 拖入）**（桌面，Phase 4，D74）：根元素、侧栏标题与目录行接受 GPUI `ExternalPaths` 拖拽负载并高亮落点；拖入的条目复制到目标目录（同名自动加序号、目录递归复制、跳过符号链接），Markdown 自动打开为标签，编辑区拖入的非 Markdown 文件按当前文档目录生成相对 Markdown 链接并插入光标。新增纯函数 `plan_imports`/`unique_import_path`/`copy_entry`/`dropped_link_markdown` 与桌面单测；抽出 `insert_text_at_cursors` 复用粘贴逻辑。桌面 fmt/clippy/test/build 全绿（55 项）；根 workspace 门禁全绿（548 项）。Finder→窗口入站拖拽已实机验收通过。 |
| 0.1.95 | 2026-11-18 | **拖拽落点高亮 + 跨应用拖拽 + 全工作区搜索**（桌面，Phase 4，D40/D73）：文件树目录行与侧栏标题（根目录落点）新增 `drag_over::<TreeDrag>` 高亮（铺 `palette.selection`，非法落点由 `can_drop` 阻止），侧栏标题 `can_drop` 只接受非根层条目；目录行/文件行新增 `external_drag_payload`（`ExternalDragPayload::Files`），支持拖出窗口交给系统原生文件拖拽（跨应用/Finder，已于 2026-11-18 实机验收通过：落入目标目录时复制文件到该目录，原条目保留）。新增全工作区搜索（`Cmd+Shift+F`）：居中悬浮面板即时递归扫描工作区 Markdown，按文件分组列出命中行与上下文（命中高亮、按字符截断），`↑↓` 选择、`Enter`/点击打开并选中命中、`Cmd+Alt+C` 切换大小写、`Esc` 关闭；上限 500 条标记截断，读取失败计入跳过；命令面板与编辑菜单新增入口。新增纯函数 `collect_markdown_paths`/`search_text_lines`/`tail_chars`/`head_chars`；修复 `mazha-core::find::find_all` 大小写不敏感模式下「模式比文本长」切片越界 panic。桌面新增 3 条单测、core 补 1 条回归断言；桌面 fmt/clippy/test/build 全绿（52 项）。 |
| 0.1.94 | 2026-11-17 | **文件树展开状态持久化**（桌面，Phase 4，D72）：`Settings` 新增 `tree_expanded: BTreeMap<String, Vec<String>>`（键为工作区根，值为展开目录列表），新增 `tree_expanded_for`/`set_tree_expanded_for`（空列表删键），`reset_appearance` 保留展开状态。`DocView` 启动按工作区根恢复 `expanded_dirs`，在 `toggle_dir` 与 `refresh_workspace` 后落盘（排序写入）。新增 1 条设置单测；桌面 fmt/clippy/test/build 全绿（49 项）。 |
| 0.1.93 | 2026-11-17 | **文件树拖拽移动 + 新建文件夹**（桌面，Phase 4，D71）：文件树行支持 GPUI 拖拽（`TreeDrag` 负载 + `TreeDragPreview` 跟随标签），目录行与侧栏标题 `sb-h` 为落点（`can_drop` + `on_drop`），落点经 `move_entry` 移动文件/目录（拒绝自身/后代/同名，跟随更新当前文档路径与展开态前缀）。新增 `new_folder_in_root`/`new_folder_in` 创建文件夹并进入重命名态；侧栏标题改为 `＋文件` / `＋文件夹`，命令面板新增「新建 Markdown」「新建文件夹」。文件行打开、目录行展开折叠由 `on_mouse_down` 改为 `on_click`，避免拖拽误触。新增纯函数 `drop_allowed`/`remap_expanded` 与 2 条单测；桌面 fmt/clippy/test/build 全绿（48 项）。 |
| 0.1.92 | 2026-11-17 | **文件树层级化 + 大纲移至右侧面板**（桌面，Phase 4，D70）：文件树改为按 `Workspace::list` 递归渲染层级，目录可 `▸/▾` 展开折叠、目录行 `＋` 在其内新建 Markdown、`✎` 重命名、`✕` 递归删除，文件只保留 Markdown；侧栏标题栏新增 `＋` 在根目录新建。删除/重命名目录同步迁移 `expanded_dirs` 前缀与当前打开文档路径（删除时若当前文档在被删目录内则清空）。大纲（目录导航）移出左侧栏，改为**右侧独立可拖动面板**（`outline_width` 160–420px），左侧栏只保留文件树；`ToggleOutline`/菜单/命令面板行为不变。新增纯函数 `collect_tree_rows` 与桌面 1 条层级/展开/过滤单测；桌面 fmt/clippy/test/build 全绿（46 项）。 |
| 0.1.91 | 2026-11-16 | **脚注交互闭环：双向跳转 + 导出编号/文末归集**（Phase 4，D69）：新增 `mazha-md::FootnoteIndex`（按引用首次出现顺序编号，未被引用定义排后，重复定义取首个，提供引用/定义区间查询）与 `Document::footnote_index()`；桌面 `Cmd+点击` 引用标签跳到定义块起点、点击定义标签跳回首个引用，不改变 WYSIWYG 显示。导出新增源文本预规整 `footnotes::prepare`（重写标签为序号 + 顶层定义按序号归集文末），HTML/EPUB 输出 `<sup class="fnref"><a href="#fn-N">N</a></sup>` 与 `<section class="footnotes"><ol><li id="fn-N">…`，Typst/DOCX 输出编号与文末定义（无锚点结构）；无脚注文档逐字节原样直出。限制：嵌套定义只重编号不移动、无回链、编号不写回 Markdown。新增 md 8 / core 1 / export 12 / 桌面 1 条测试；workspace 合计 548（core 204、md 176、export 139 + 3 集成、crdt 11、store 12、ffi/plugin/wasm 各 1），桌面 45，fmt/clippy/test/build 全绿。 |
| 0.1.90 | 2026-11-16 | **专注/打字机/源码模式（代码模式）**（桌面，Phase 4，D68）：新增三种可持久化视图模式。专注模式淡化非当前块，突出主光标所在段落/块；打字机模式在光标移动与编辑后尽量将当前视觉行居中滚动；源码模式按原始 Markdown 源码行显示与编辑（语法标记可见、停用 WYSIWYG 隐藏与媒体/公式/图表预览渲染）。通过「视图」菜单、命令面板与快捷键切换，状态写入桌面设置。桌面 fmt/clippy/test/build 与 workspace 门禁全绿。 |
| 0.1.89 | 2026-11-16 | **图片点击边界定位与整图删除**（桌面，D53 交互补齐）：渲染态独占图片点击左半区将光标定位到图片源码起点、右半区定位到源码终点，光标落到对应边界并按既有逻辑进入源码编辑态；源码编辑态仍按原文逐字符命中。无选区时，光标紧贴图片末尾按 `Backspace`、紧贴图片开头按 `Delete` 直接删除整段图片源码（`BlockWidgetKind::Image` 的 `source_range`），图片作为整体处理。桌面 fmt/clippy/test/build 全绿（43 项）。 |
| 0.1.88 | 2026-11-13 | **从剪贴板粘贴图片**（D67）：桌面端读取 GPUI `ClipboardEntry`；图片文件条目按原路径生成 `![文件名](<路径>)`，多个路径逐行插入；没有图片路径时把内存图片原始字节编码为对应 MIME 的 base64 `data:` URI；最后回退普通文本。保持多光标倒序替换；新增 3 条桌面路径识别/转义、优先级和 data URI 单测，核心新增 1 条转义 alt 图片投影回归测试。根与桌面完整门禁通过（workspace 527 项，桌面 43 项）。 |
| 0.1.87 | 2026-11-13 | **表格单元格强制换行、列对齐与边框尺寸拖动**（D66）：保留标准 GFM 表头规则；表格单元格内 `Shift+Enter` 写入 `<br>`，新增零宽 `HtmlBreak` 投影位置，桌面按强制换行点拆分单元格排版段，光标/命中/选区/行高与 HTML、DOCX、Typst 导出同步支持；换行段与自动软换行视觉行继承 `:---` 左、`:---:` 中、`---:` 右对齐，编辑坐标同步对齐偏移；内部竖线调整相邻列，右外边框改变整表宽度，横向边框调整单行高度，右下角可同时调整宽高；悬浮分别显示水平、垂直和对角 resize 光标，列宽最小 48px、行高不低于内容高度，尺寸仅在当前文档会话保存。修复 resize 光标 hitbox 生命周期：`window.insert_hitbox` 必须在 prepaint 注册，paint 仅调用 `set_cursor_style`；原先在 paint 插入会触发 GPUI 断言并在 macOS FFI 回调中 `panic_cannot_unwind`。根 fmt/clippy/test 全绿（526 项，含 3 个 PDF 集成测试），桌面 fmt/clippy/test/build 全绿（40 项）；内置文档与含 `<br>` 表格文档启动 8 秒无 panic。 |
| 0.1.86 | 2026-11-13 | **音频控件状态修复与反馈完善**（D64）：音频 50×50 按钮右侧显示“当前时间 / 总时长”，首次加载/网络缓冲时按钮外围显示四点循环指示；播放状态新增 `loading`/`started`，结束判断除时长阈值外同时识别“实际播放后 AVPlayer 自行回到 Paused”，结束后主动 pause + seek 到 0 并恢复播放图标，修复播放完毕后按钮状态不重置且无法再次播放。桌面 fmt/clippy/test/build 全绿（36 项）。 |
| 0.1.85 | 2026-11-13 | **HTML 音视频解析与 macOS 桌面播放**（D64）：新增单行 `<video>` / `<audio>` 解析、`SpanRole::MediaMarker` 与 `WidgetKind::Media`；桌面接入 AVPlayer，视频经 `AVPlayerItemVideoOutput` 取 32BGRA 帧后由 GPUI 自绘并保留进度 seek，音频使用固定 50×50 播放/暂停按钮；支持懒加载、poster/16:9 占位、源码编辑态、删除/切换文档清理状态；HTML/EPUB 保留可播放标签，DOCX 与 Typst/PDF 输出媒体占位。根 fmt/clippy/test 全绿（521 项，含 3 个 PDF 集成测试），桌面 fmt/clippy/test/build 全绿（34 项）；网络音视频链接已实机验证，本地媒体仍需回归。 |
| 0.1.84 | 2026-11-13 | **macOS 原生应用菜单**（D63）：新增 `MenuCommand`/`MenuAction` 与 `app_menus`，通过 `App::set_menus` 创建「码札/文件/编辑/视图/窗口」菜单；应用级命令含关于/服务/隐藏/退出，文件与视图命令复用命令面板动作，编辑菜单剪切/复制/粘贴/全选使用 `OsAction` 走原生 selector；`menu_key_bindings` 让 NSMenu 显示并响应快捷键，主题子菜单按当前设置勾选并随切换动态刷新；原生异步对话框期间用 `dialog_open` 拦截菜单快捷键。桌面 fmt/clippy/test/build 全绿（32 项）；实机验证菜单栏、快捷键、主题勾选、命令面板/查找、关于弹窗，并确认应用不在前台时菜单项禁用。限制：无「帮助」菜单；裸二进制应用菜单标题仍为 `mazha-desktop`。 |
| 0.1.83 | 2026-11-09 | **桌面外观设置**（D62）：新增 `platforms/desktop/src/settings.rs`，持久化主题（跟随系统/浅色/深色）、正文字体/等宽字体、正文字号/等宽字号到平台配置目录；启动时应用主题，`System` 订阅窗口 appearance 变化；正文布局改用可调字号，行高兼容更大的等宽字号；命令面板新增外观命令，快捷键 `Cmd+=`/`Cmd+-`、`Cmd+Option+=`/`Cmd+Option+-`、`Cmd+Shift+L`、`Cmd+Option+0`，状态栏显示字号与主题。桌面 fmt/clippy/test/build 全绿（31 项）；实机验证深浅色切换、字号调整、字体循环、重置、重启持久化，以及正文 12px + 等宽 24px 极端组合。 |
| 0.1.82 | 2026-11-06 | **macOS `.app` 正式打包**（D61）：新增 `platforms/desktop/macos/Info.plist` 与 `scripts/bundle-macos.sh`；脚本从 `assets/appicon.png` 生成完整 `AppIcon.icns`，写入 Bundle ID/显示名/版本，复制 release 二进制，默认 ad-hoc 签名并输出 `码札.app` 与 `Mazha-<版本>-macos.zip`；新增 `scripts/notarize-macos.sh` 执行 Developer ID 公证 + 装订 + Gatekeeper 校验。验证 `plutil`、`codesign --verify`、`NSWorkspace` 文件图标与 `.app` 启动；公证需外部 Apple Developer 凭据。桌面 fmt/clippy/test/build 全绿（24 项）。 |
| 0.1.81 | 2026-11-06 | **macOS Dock 图标**（D60）：新增 `platforms/desktop/assets/appicon.png`；启动时 `include_bytes!` 嵌入的 PNG 经 AppKit `NSData`/`NSImage` 解码，在主线程调用 `NSApplication.setApplicationIconImage`，使 `cargo run` 的 Dock 与 Cmd+Tab 显示应用图标；新增 macOS 专用 `objc2`/`objc2-foundation`/`objc2-app-kit` 依赖。桌面 fmt/clippy/test/build 全绿（24 项）。 |
| 0.1.80 | 2026-11-05 | **桌面 UI 四项优化**（D59）：① `Palette.selected_bg`（前景 alpha 0.12 浅灰）用于活动标签与侧栏当前项，文本选区保持蓝色；② 大纲跳转新增 `scroll_to_source_top`，prepaint 按目标视觉行顶边强制置顶并按内容总高 clamp；③ 标签增加 `✕`（单标签隐藏），`close_tab` 支持关闭非当前标签并修正 `active`；④ 文件信息/行数/光标数/快捷键移入窗口底部 24px 全宽状态栏，长路径省略号。桌面 fmt/clippy/test/build 全绿（24 项）；实机核对浅灰选中、章节 8 置顶、双标签关闭与底部状态栏。 |
| 0.1.79 | 2026-11-03 | **标签切换与侧栏可用性**（D58）：修复点击标签/从侧栏打开已打开文件时只改高亮、正文不装载的问题；新增 `sync_current_tab`/`activate_tab`，切换前用 move 把当前 `Document` 回写标签、切换时装载目标文档，保留各标签撤销历史并重建布局/滚动/查找态。侧栏增加 4px 拖动分隔条，宽度限幅 160–420px；长文件名改用 `truncate()` 显示末尾省略号，不再挤压图标或越界。桌面 fmt/clippy/test/build 全绿（24 项）；手工验证侧栏打开 B、Tab 切回 A、拖动侧栏与长文件名省略号。 |
| 0.1.78 | 2026-11-03 | **软换行视觉行布局与超宽表格换行**（D57）：桌面 `DocLayout::row_tops` 改为按**全局视觉行**逐行建顶边（此前每逻辑行只推一个顶边，软换行后用视觉行号索引会取错/越界，导致块重叠、首行乱码、图片/标题错位与内容下方大块空白）。超宽表格不再回退 `|` 流式：自然宽度测量后按可用宽度在 `[最小内宽, 自然宽]` 间线性压缩列宽，单元格 `shape_wrapped_line` 软换行，行高按最大视觉行数计算；命中/光标/选区改用 `WrappedLine` 的逐视觉行定位，单行保留列对齐、多行统一左对齐。侧栏文件名增加 `min_w(0)` / `whitespace_nowrap` / `overflow_hidden`，防止长文件名越界到正文（曾伪装成表头乱码）。并入 0.1.77 未提交的内联 HTML 边界修正（`<br>`、非 ASCII hex、行内闭合范围）与测试；workspace 507 / 桌面 24 全绿。 |
| 0.1.77 | 2026-11-02 | **内联 HTML 标签与 `<font>` 样式**（D56）：`parse_inline` 识别 `<font>/<span>/<b>/<strong>/<i>/<em>/<s>/<del>/<strike>/<code>/<mark>/<br>`，隐藏标签并映射强调/行内代码角色；`<font>`/`<span>` 的 `color`/`style` 颜色解析为 `SyntaxSpan.color`，经核心 `TextRun.color` 透传到桌面前景色；`<br>` 仅剥离标签（行内模型不换行）。结束标签限制在当前行内区间查找，未闭合标签按原文显示；修复坏 hex 颜色的切片 panic。表格分隔行隐藏时行数计为 0，避免表头/表体之间多出空白行。补 `mazha-md` 9 / core 1 条单测；修复遗留 clippy（手动 `strip_prefix`、`range_plus_one`、`uninlined_format_args`、`collapsible_if`、未读 `viewport` 字段）与桌面 rustfmt；core 199 / 总计 507 / 桌面 24 全绿 |
| 0.1.76 | 2026-10-27 | **内联 data URI 与协议相对图片**（D55）：`data:image/...` 支持 base64 与百分号编码，MIME 映射 GPUI `ImageFormat`，经 `Image::from_bytes(...).use_render_image` 复用解码/缓存/等比缩宽；协议相对 `//host/path` 在本地编辑器按 `https://` 加载；未知 MIME/base64 损坏/空载荷仍占位。新增 data URI 解码与协议相对映射单测；截图核对 360×140 内联 PNG 与 `//static...jpg` 均真实渲染且无占位标签；core 198 / 总计 497 / 桌面 24 全绿 |
| 0.1.75 | 2026-10-27 | **远程图片加载**（D54）：`http(s)://` 图片改用 GPUI `Resource::Uri`，复用 `ImageAssetLoader` 的下载/解码/缓存/等比缩宽管线；桌面启动显式安装 `gpui-pre-reqwest-client`（GPUI `application()` 默认 `NullHttpClient`），UA `Mazha/<version>`。`data:`/`mailto:`/协议相对地址与下载失败仍回退占位；不提供认证头/Cookie/磁盘缓存。新增 URL 资源单测；用 1920×891 远程 JPEG 实测显示为约 1284×595.9 逻辑像素，OCR 无占位标签；core 198 / 总计 497 / 桌面 23 全绿 |
| 0.1.74 | 2026-10-27 | **图片本地真实渲染（补齐 Phase 3 漏项）**（D53）：核心 `WidgetKind::Image` 增加 `dest`，独占单行图片额外产出 `BlockWidgetKind::Image{alt,dest}`（混排仍行内占位）；桌面端按文档目录解析本地路径（支持绝对路径/`file://`/`%XX`），用 GPUI `ImageAssetLoader` 异步加载缓存，渲染态按正文宽等比缩小且不放大、行高随图片、铺弱底/边框/圆角；光标进入图片行显示原始 `![alt](dest)`，移开恢复图片，远程/缺失/行内图片继续占位。新增 core 2 / 桌面 4 条单测；截图核对 1800×700 缩放、120×60 不放大、引用内图片与编辑态原文；core 198 / 总计 497 / 桌面 22 全绿 |
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
