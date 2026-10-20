# HANDOFF — 码札 / Mazha 交接与进度总览

> 每次提交代码时**同步更新本文件**（约定，见 `AGENTS.md`）。
> 单一事实来源是 `SPEC.md`（接口契约 §6、决策 §14、变更记录 §15）；本文件只做「当前进度 / 下一步」的归总，不代替 SPEC。

- 最近更新：2026-10-20
- 当前阶段：**Phase 1（共享核心）已完成**；**Phase 2/3 主体已完成**（桌面只读投影 → WYSIWYG 标记显隐/命中 → IME/编辑命令/多光标 → 文档级坐标 API → 块级 widget 渲染 → 表格增删行列/代码高亮/软换行/剪贴板/视觉行移动/查找替换/公式与图表/脚注）；**Phase 4 桌面 MVP 主体达成**（文件管理打开/保存/新建/重命名/删除/移动 + 多标签页 D37 + 命令面板 D38）；**导出线**并行：HTML / PDF / DOCX / EPUB 均可用
- 仓库：`git@github.com:RandyWei/mazha-notes.git`（私有，发版后转公开）；分支 `main`
- 当前版本：**0.1.52**（细节见 SPEC §15 变更记录；本节只列归总）

## 门禁（提交前必须全绿）

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
# 桌面端（独立 workspace，见 R10；触碰 platforms/* 必须单独跑）
cd platforms/desktop && cargo fmt --check && cargo clippy -- -D warnings && cargo build
```

> rustc 需 **≥ 1.92**（workspace `rust-version`，Typst 0.15 的 MSRV），工具链见 `rust-toolchain.toml`（stable）。

## crate 进度

> 详细变更逐版本记录在 `SPEC.md` §15；下表只给「能力面 + 待办」。

| crate | 状态 | 能力面（要点） | 待办 |
|-------|------|----------------|------|
| `mazha-core` | ✅ Phase 1 完成 | `Document`（rope/编辑/undo-redo/块缓存+投影缓存**双增量**维护/`apply_and_sync`）、`OffsetMap`、`Projection`（`BlockStyle`/`TextRun`/`Widget`/`RunStyle` + **块级 widget** `BlockWidget{source_range,line_range,kind}`、`BlockWidgetKind::{Code{lang,fenced},Table{align},Rule,Math,Chart{lang}}` + `block_widget_at`/`block_widget_on_line`）、**文档级跨行坐标**（`DocPos`/`LineDisplayRange`/`source_to_doc_pos`/`doc_pos_to_source`/`line_of_source`/`selection_display_ranges`）、显示列↔字节换算（软换行用）、`commands`、`IntentMapper`、`table::edit_table`、`highlight::Highlighter`（syntect）、`find::{find_all,replace_all,replace_all_edit}`、导航（行范围/CJK 词范围）、`Workspace`（open/save/create/remove/rename/list/find）、`crdt` 桥接 | `DocId` |
| `mazha-md` | ✅ Phase 1 完成（方言全齐） | `parse`（ATX/Setext 标题、段落、分割线、围栏/缩进代码、引用、列表/任务、GFM 表格、Callout、Front-matter、**`$$…$$` 显示公式**、**`[^label]: body` 脚注定义**）、`parse_inline`（强调/粗体/删除线/行内代码/链接/图片/自动链接/wiki-link/tags/`$…$` 行内公式/**`[^label]` 脚注引用**/转义）、`serialize_source`/`to_md`（前缀穿线，逐字节往返 + 幂等）、`reparse`（脏块增量 + 裁切点安全校验） | HTML 块、惰性续行、多段落脚注（见 SPEC §9.1/§15） |
| `mazha-store` | ✅ MVP | `StorageProvider`（同步，D24）、`FileSystemProvider`、`DocPath`、`Entry`、`StoreError` | 文件监听、异步/流式（Phase 5）、云端/插件 Provider |
| `mazha-crdt` | ✅ MVP | `CrdtDoc`（`Y.Text`，字符↔字节）、insert/delete/replace/push、状态向量 + 增量更新（Yjs v1）、`apply_update`、`fork`、`snapshot`、并发收敛、落盘（`save_crdt`/`load_crdt`） | 多文档名册 |
| `mazha-export` | ✅ HTML + PDF + DOCX + EPUB + 主题/模板 | `to_html*`（完整外壳 + 默认 CSS + 主题）、`to_typst*` + `to_pdf*`（`ExportWorld` 实现 `typst::World` → `typst::compile` + `typst_pdf`；`PdfDocument{bytes,warnings,pages}`；真实文档回归 `tests/pdf_real_doc.rs`）、`to_docx*`（docx-rs 1.1 直出 OOXML）、`to_epub*`（Zip + XHTML，EPUB 3，输出确定）、`Theme`/`Template`；块/行内特性与编辑器同源（含公式：HTML `div/span.math`、Typst 字面量 `#raw`、DOCX 纯文本；含脚注：HTML `sup.fnref` + `div.footnote`、Typst `#super[…]`、DOCX 纯文本） | 字体子集（Web，Phase 7） |
| `mazha-plugin` | ⬜ 占位 | — | WASM 宿主、能力授权（Phase 5） |
| `mazha-ffi` | ⬜ 占位 | — | UniFFI 绑定（Phase 6） |
| `mazha-wasm` | ⬜ 占位 | — | Web 绑定（Phase 7） |

## 平台进度（`platforms/*`，各自独立 workspace）

| 平台 | 状态 | 已实现（要点） | 待办 |
|------|------|----------------|------|
| `desktop`（GPUI + gpui-kit 0.7） | 🟡 Phase 2/3 主体完成（Phase 4 MVP 主体达成） | 只读投影渲染 + 主题；光标/点击命中 + WYSIWYG 标记显隐（光标行显原文）；自写 `Element`（连续布局 + 视觉行表 `row_starts`）+ 跨行选区；IME 全实现（UTF-8↔UTF-16）；编辑命令（行内 Cmd+B/I/X/`、块级 Cmd+1-6/段落/引用/列表）；多光标（Alt+Click）；PDF 导出（Cmd+E）；文档级坐标 API 接入；块级 widget 装饰（代码面板+语言标签、表格网格、分割线、**公式描边、图表弱底+标签**）；表格增删行列；代码高亮；软换行；剪贴板 Cmd+C/X/V；视觉行上下移动；查找/替换 Cmd+F；行内公式 `RunStyle::Math`（斜体+主题色）；脚注标签 `RunStyle::Footnote`（主题色）；**Phase 4：打开 Cmd+O / 保存 Cmd+S / 另存为 Cmd+Shift+S、文件树侧栏（新建/重命名 ✎/删除 ✕/移动）、脏状态 `*`；多标签页（D37，Cmd+T/W/Shift+W/1..9/0）；命令面板（D38，Cmd+P，命令表+过滤+执行）；大纲/目录导航（D39，侧栏标题列表+点击跳转，Cmd+Shift+O）** | 文档内搜索、主题与专注模式（Phase 4 余项）；文件树目录层级/移动拖拽
| `ios` / `android` / `harmony` / `web` | ⬜ 仅 README | — | Phase 6 / 8 / 7 |

> 桌面端入口：`cargo run -- [path/to/file.md]`。本环境无法可视化验证渲染效果，逻辑改动靠核心单测 + 编译门禁保证。

## 测试现状

| crate | 数量 |
|-------|------|
| `mazha-core` | 178 |
| `mazha-md` | 149 |
| `mazha-export` | 121 单测 + 3 集成（`tests/pdf_real_doc.rs`） |
| `mazha-crdt` | 11 |
| 合计 | **477**（其余为占位 crate 的版本号冒烟测试） |

## 关键决策索引（全文见 SPEC §14）

- **D1–D22**：重写、GPUI+gpui-kit、纯 Rust 核心、源文本真相、GFM+知识库方言、yrs CRDT、WASM 插件、AGPL-3.0 等。
- **D23**：Markdown **自研行扫描**（精确字节偏移）；`pulldown-cmark` 仅 dev 对照 → [`docs/ADR/ADR-001`](docs/ADR/ADR-001-self-authored-markdown-parser.md)。
- **D24**：`StorageProvider` 同步接口（本地文件为唯一 MVP Provider）。
- **D25**：PDF = `to_typst` + 自实现 `ExportWorld` → `typst::compile` + `typst_pdf`；MSRV 提到 1.92。
- **D26/D27/D28**：DOCX 走 docx-rs 直出 OOXML；EPUB 走 Zip + XHTML（EPUB 3）；导出外观拆 `Theme`（CSS）与 `Template`（Typst 前置）。
- **D29**：桌面端坐标/选区统一走核心文档级 API（并修复历史编译债 + 独立 `Cargo.lock`）。
- **D30**：块级 widget 建模在**核心投影**，核心只给「分组与范围」，绘制留 UI；"光标进入才可编辑"由 UI 判定。
- **D31**：表格增删行列是核心纯函数 `edit_table` → 规范化重写整块 → 单次 `Edit::Replace`。
- **D32**：代码高亮用 **syntect**（纯 Rust fancy-regex）放在核心，不泄漏 syntect 类型。
- **D33**：软换行放 **UI 层**（gpui `shape_text` + 视觉行），核心保持无 GUI；显示列↔字节换算下沉核心可单测。
- **D34**：公式/图表「解析在 `mazha-md`、归类在 `mazha-core`、真排版留渲染层」——`$$…$$`→`MathBlock`/`BlockWidgetKind::Math`，`$…$`→`SpanRole::Math`+`RunStyle::Math`，` ```mermaid `→`Chart{lang}`；导出按字面量（Typst **不**进 `$…$`，R8）。
- **D35**：脚注「解析在 `mazha-md`、样式/导出按可见文本、真渲染留渲染层」——行内 `[^label]`（`FootnoteMarker` 隐藏 + `FootnoteRef` 可见）、块级 `[^label]: body`→`BlockKind::FootnoteDef`；续行缩进 ≥4 且恰好剥 4 空格；`RunStyle::Footnote`（不加 `BlockWidgetKind`）；HTML `sup.fnref`/`div.footnote`、Typst `#super[…]`（**不**用 `#footnote`）、DOCX 纯文本；跳转/编号/文末归集留后续。
- **R8**（PDF 真实文档回归）/ **R10**（`platforms/*` 独立 workspace，须单独门禁 + 同步独立 `Cargo.lock`）：见 SPEC §13。

## 依赖方向（不可违反）

- `mazha-core → mazha-md`、`mazha-core → mazha-store`、`mazha-core → mazha-crdt`（core 是共享核心顶层）
- `mazha-md` **不得**反向依赖 `mazha-core`
- 所有核心 crate **不得**依赖任何 GUI（CI 校验）

## 测试策略（已建立）

- **增量 == 全量**逐字段比对：块缓存（`reparse`）、投影 + OffsetMap（`Projection::refresh`）、编辑序列
- **CRDT 双端收敛**：fork → 双端编辑 → 合并 → 文本一致
- **序列化**：结构保持 + 逐字节往返 + 幂等（`to_md(to_md(x)) == to_md(x)`）
- **全管线集成**：`full_pipeline_all_features` 单文档覆盖全部语法 + 编辑后投影一致
- **容器前缀**：嵌套（引用/列表内）代码块/公式的片段起点与序列化前缀不串扰
- 先写测试再实现（尤其光标/偏移映射，R2）

## 下一步（建议顺序）

1. **Phase 4 余项**：桌面**文档内搜索 / 主题与专注模式**（文件管理、多标签页 D37、命令面板 D38、大纲 D39 均已落地）；文件树可择机深化为目录层级/拖拽移动。
2. **渲染层深化**（R4）：数学排版（`ratex-*`）与图表 SVG（`mermaid-rs-renderer`）替换块级占位面板；脚注的点击跳转/自动编号/文末归集；软换行**横向滚动**（D33 已知限制）。
3. §9.1 剩余非 MVP 项：HTML 块、惰性续行、多段落脚注定义。
4. `mazha-export`：字体子集（Web，Phase 7）；`mazha-crdt`：多文档名册；`mazha-store`：文件监听（Phase 5）。

## 提交历史（近期，共 86 条；本次提交为最新一条，下次同步时补入哈希）

```
<本次提交：feat(desktop): 大纲/目录导航（侧栏标题列表+点击跳转，D39，Phase 4）>
62b80e1 feat(desktop): 文件 CRUD（重命名/删除/移动，D36 遗留，Phase 4）
81b0fb6 feat(desktop): 命令面板 Cmd+P（D38，命令集 + 过滤 + 执行）
060f96f docs: Phase 4 命令面板设计（D38）
7c7ffdb feat(desktop): 多标签页支持（标题栏/切换/关闭/新建，D37 基础）
08323fa feat(desktop): 多标签页快捷键（Cmd+1..9/0、Cmd+W/Shift+W）
ae14bb3 feat(desktop): 文件树新建 Markdown（最小）
fb9ed20 feat(desktop): 文件树点击切换文档（最小可用）
cb8fd3e docs: Phase 4 多标签页设计（D37）
23f8012 feat(desktop): 文件管理首增量（Cmd+O/S/Shift+S + 脏状态，兼容 Workspace）
1c2a393 feat(md): 脚注解析与建模（[^label] 引用 + [^label]: body 定义，0.1.52，Phase 3）
6c7ea52 feat(md): 公式/图表解析与建模（$$…$$ / $…$ / mermaid 归类，0.1.51，Phase 3）
1f42f08 fix(export): 多行代码块导出丢失换行（HTML/Typst，R8 同类缺陷）
b6fd01b feat(core): 查找/替换（find_all/replace_all）+ 桌面 Cmd+F 查找条（0.1.49，Phase 3）
```
c8136b7 feat(core): 代码块语法高亮（syntect Highlighter）+ 桌面着色（D32，Phase 3）
25a4670 feat(core): 表格增删行列（TableOp/edit_table）+ 桌面快捷键（D31，Phase 3）
3c497ff feat(desktop): 块级 widget 渲染（代码块面板/表格网格/分割线，Phase 3）
c80a24a feat(core): 块级 widget 模型（BlockWidget/BlockWidgetKind，Phase 3）
fdd1252 feat(desktop): 接入核心文档级 API + 修复历史编译债（D29，Phase 3）
c5d6d72 feat(core): 文档级跨行坐标映射（DocPos/选区区间，Phase 3）
05e8086 test(export): PDF 真实文档回归（R8）+ 修复行内代码反引号
398f20f feat(export): 导出主题/模板（Theme/Template，D28）
fc4721f feat(export): EPUB 导出（Zip + XHTML，D27）
6922f6a feat(export): DOCX 导出（docx-rs，D26）
917d9ce chore: update Cargo.lock for mazha-export + rfd deps
531bdc7 feat(desktop): PDF 导出（Cmd+E）
72ee12a feat(desktop): 多光标支持
2a9c1c8 feat(desktop): 编辑命令接入（commands/apply_and_sync）
9e188db feat(desktop): IME 支持 + EntityInputHandler 完整实现
2e9a5af feat(desktop): 自写 Element + shape_line 连续布局 + 跨行选区
9e24880 feat(desktop): 光标/点击定位 + WYSIWYG 标记显隐（光标行原文，其余投影隐藏）
13e0295 feat(desktop): Phase 2 起步——GPUI 只读投影渲染（可运行）
e14e14f feat(export): PDF 编译——Typst World 实现 + to_pdf（PDF 管线第二步）
d60189c docs: HANDOFF 全面刷新（Phase 1 完成状态）
```

> 更早提交见 `git log`；历史累计 = 69（含本次）。
