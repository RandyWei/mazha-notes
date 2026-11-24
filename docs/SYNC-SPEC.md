# 码札（Mazha）同步 SPEC 草案

> 状态：**S0 方向已确认（D95-D97）；S1 纯模型已实现（D98）；S2 协议/会话/假 transport 契约已定并实现（D99，见 §5.7 / §6.4）；S3 线上字节编码/持久化服务端/真实 HTTP 传输已实现（D100，见 §5.8 / §7.4 / §9.4）；S4 本地工作区同步编排契约已定并实现（D101，见 §5.9 / §9.5 / §11.3）；S5a 桌面接入第一批契约已定并实现（D102，见 §5.10 / §9.6 / §11.4）；S5b 第一批（增量 Document patch + 合并/覆盖交互）契约已定并实现（D103/D104，见 §5.11 / §9.7 / §11.5）**  
> 日期：2026-11-24  
> 关联：`SPEC.md` §4.3、§5、§6.2、§6.5、§10 Phase 5、§14 D24/D91-D104；`HANDOFF.md` 下一步 S5b 第二批  
> 目标：为“服务端/传输与跨设备删除墓碑”提供独立设计边界；本文不替代 `SPEC.md`（单一事实来源），S0 已确认的高优先级项由 D95-D97 记录，S1 数据结构契约由 D98 与 `SPEC.md` §6.5 记录，S2 内存协议/会话契约由 D99 与本文件 §5.7 / §6.4 记录，S3 线上字节编码/持久化服务端/真实 HTTP 传输由 D100 与本文件 §5.8 / §7.4 / §9.4 记录，S4 本地文件⇄名册编排与本地持久化由 D101 与本文件 §5.9 / §9.5 记录，S5a 桌面周期/手动同步与状态/冲突提示由 D102 与本文件 §5.10 / §9.6 / §11.4 记录，S5b 第一批的增量 patch 与显式合并/覆盖交互由 D103/D104 与本文件 §5.11 / §9.7 / §11.5 记录。S5b 第二批（WS 订阅、快照压缩、墓碑 GC、平台安全存储）仍待实现。

---

## 0. 文档说明

1. 本文是 Phase 5 同步方向的**独立设计文档**：S0/S1 为已确认的设计契约；S2（§5.7 / §6.4）与 S3（§5.8 / §7.4 / §9.4）均已在新 crate `mazha-sync` 及其上层独立 crate 中落地；S4（§5.9 / §9.5）把会话层接到本地文件监听与名册落盘（新 crate `mazha-sync-local`）。S5a（§5.10 / §9.6 / §11.4）只把引擎接到桌面 UI：注入 `HttpTransport`、周期/手动同步、状态栏与冲突横幅；WS 订阅、增量 Document patch 与“合并/覆盖”交互（S5b）不在 S5a 契约范围内。
2. 任何结论在落地前需评审；确认后应：
   - 在 `SPEC.md` §6.5 增加“详见 `docs/SYNC-SPEC.md`”索引；
   - 将关键决策拆成 `docs/ADR/*` 或 `SPEC.md` §14 的 Dxx；
   - 在 `SPEC.md` §15 记录变更。
3. 本草案遵守现有约束：`SPEC.md` 为单一事实来源；核心 crate 不依赖 GUI；不臆造现有代码中不存在的接口。

---

## 1. 背景与现状调研摘要

### 1.1 已有契约与实现边界

| 位置 | 现状 | 对同步的意义 |
|------|------|--------------|
| `SPEC.md` §4.3 | 数据真相链：磁盘 `.md` 为存储真相；CRDT `Y.Text` 保存同一份源文本；二者通过同步引擎保持一致。 | 同步不是替代本地文件，而是让多端源文本收敛；文件与 CRDT 的双向一致性仍是核心。 |
| `SPEC.md` §6.5 / D94 | 基于 yrs 的 Yjs 同步协议（状态向量 + 增量更新），未来可与 Web 端 Yjs 生态互通；核心只产出/消费标准 Yjs 更新帧；服务端/传输为独立 SPEC。`CrdtRoster` 已提供整册 `state_vectors`/`encode_diff`/`apply_updates`，值为标准 Yjs v1 帧；`remove` 仅本机生效，无删除墓碑。 | 同步协议应复用 D94 的整册映射形状，避免自造第二套名册同步格式；删除传播必须补墓碑/等价语义。 |
| `crates/mazha-crdt/src/lib.rs` | `CrdtDoc` 封装 `Doc<Y.Text(ROOT_TEXT="content")>`；对外编辑 API 使用字符索引，内部转 yrs UTF-8 字节；提供 `state_vector`/`encode_diff`/`encode_update`/`apply_update`/`fork`/`snapshot`；快照清空历史但保留文本。 | 单文档同步原语已具备；字符索引与字节偏移转换边界必须保持不变。 |
| `crates/mazha-crdt/src/roster.rs` | `CrdtRoster` 用 `BTreeMap<DocId, CrdtDoc>` 管理多文档；`DocId` 为非空、无控制字符字符串，约定工作区相对路径；`encode_diff` 以解码帧判空识别删除集变化；`apply_updates` 对未知 ID 自动建文档，坏帧整批拒绝、零改动。 | 名册是同步的本地聚合根候选；“未知 ID 自动建文档”适合内容更新，但与删除墓碑共存时需重新定义。 |
| `crates/mazha-crdt/src/error.rs` | 错误覆盖越界、解码、应用、过大、ID 非法/重复/不存在。 | 同步层需要新增协议/鉴权/传输错误，但不应把网络错误塞进纯 CRDT 错误。 |
| `crates/mazha-core/src/crdt.rs` | `document_to_crdt`/`crdt_to_document`/`apply_edit_to_crdt`/`apply_remote`/`save_crdt`/`load_crdt`；`apply_remote` 在文本变化时整体重建 `Document`，会丢弃 undo/redo 历史与选区（MVP 策略）。 | 未来实时/后台同步若沿用整体重建，编辑体验会受损；同步 SPEC 需要明确 MVP 是否接受该限制。 |
| `crates/mazha-core/src/workspace.rs` | `Workspace<S: StorageProvider>` 提供 `open/save/create/remove/rename/list/list_markdown/find_markdown`；基于同步 `StorageProvider`；不感知 CRDT。 | 同步引擎不能假设 GUI 存在，但需要与 Workspace/Store 协作；Workspace 可能需要扩展元数据/事件钩子，需谨慎设计。 |
| `crates/mazha-store/src/provider.rs` + `watcher.rs` + `local.rs` | 同步 `StorageProvider`（D24）；`watch` 能力返回 `WatchHandle`；本地监听已规范化/去重 Markdown 事件；自写事件不上抑制，由上层判断。 | 本地文件变化是同步输入源之一；需定义“本地编辑、外部编辑、远端同步”三类变化的去重与冲突策略。 |
| `HANDOFF.md` | D94 已完成名册；D91-D93 完成监听与桌面接入；下一步明确“服务端/传输与跨设备删除墓碑留独立同步 SPEC”。 | 本草案正是该待办的交付物。 |

### 1.2 现状缺口

1. **无删除墓碑**：`CrdtRoster::remove` 仅本机生效，远端可能复活已删文档。
2. **无同步会话/传输协议**：只有 Yjs 更新帧，没有握手、认证、游标、重试、快照引导。
3. **无服务端职责定义**：不知道最小服务端是“盲转发中继”还是“持久化副本”。
4. **无身份/权限模型**：设备、用户、工作区、分享范围未定义。
5. **无本地-远端编排层**：`mazha-core` 有单文档桥接，但没有“工作区级同步引擎”。
6. **远端应用体验粗糙**：`apply_remote` 整体重建 `Document`，丢 undo/redo 与选区。
7. **文件与 CRDT 的权威边界未细化**：离线编辑 `.md`、CRDT 快照、远端更新同时到达时的仲裁规则待定。

---

## 2. 目标与非目标

### 2.1 MVP 同步目标

- **G1 多设备同一工作区文本收敛**：同一用户的多设备在联网后最终看到一致的 Markdown 源文本。
- **G2 离线可编辑、上线可恢复**：离线产生的本地编辑在恢复连接后可增量同步。
- **G3 标准 Yjs 帧兼容**：文档内容同步继续使用标准 Yjs v1 状态向量/更新帧，不自造 CRDT 编码。
- **G4 墓碑化删除传播**：文档删除能跨设备传播，避免 D94 已知的“复活”问题。
- **G5 可插拔传输**：协议层与传输层解耦，至少支持未来的 WebSocket/HTTP 实现；不绑定单一云厂商。
- **G6 可观测与可测试**：同步状态、错误、冲突、重试可被 UI/CLI 观测；核心逻辑可无 GUI 单测。

### 2.2 非目标（本阶段不做）

- **N1 实时多人光标/选区协作**：属于 Phase 5 之后评估（SPEC §9.3）。
- **N2 富权限协作平台**：细粒度 ACL、组织管理、审批流不在 MVP。
- **N3 任意二进制大文件同步优化**：MVP 只关注 Markdown 文本与必要元数据；图片/附件另行设计。
- **N4 端到端加密产品化承诺**：可预留设计位，但 MVP 不承诺完整 E2EE 方案。
- **N5 自动解决所有结构性冲突**：Y.Text 保证不丢字符数据；表格/块结构语义冲突仍需后续策略。

---

## 3. 术语与身份模型

| 术语 | 定义 | 说明 |
|------|------|------|
| `WorkspaceId` | 工作区稳定标识 | 建议为随机 UUID/ULID，由创建端生成；不等于本地绝对路径。 |
| `DeviceId` | 设备稳定标识 | 每台设备安装/登录实例生成一次并持久化；用于诊断、游标、墓碑来源。 |
| `ReplicaId` | CRDT 副本标识 | yrs 文档内部 client id；不直接暴露为产品身份。 |
| `DocId` | 文档稳定标识 | MVP 建议继续沿用 D94 约定：工作区相对路径（如 `notes/a.md`），因为本地文件系统与 Workspace 已以此为键。 |
| `DocKey` | 全局文档键 | 建议为 `(WorkspaceId, DocId)`；服务端存储/鉴权不应只依赖裸路径。 |
| `Roster` | 名册 | 本地多文档 CRDT 容器，当前实现为 `CrdtRoster`。 |
| `Tombstone` | 删除墓碑 | 表示某 `DocId` 已被删除的元数据记录；用于阻止旧更新复活。 |
| `StateVector` | 状态向量 | Yjs v1 编码，描述某副本已知的更新状态。 |
| `UpdateFrame` | 增量更新帧 | Yjs v1 编码的文档更新。 |
| `Snapshot` | 快照 | 文档完整状态或压缩后的全量更新；用于新设备引导/压缩。 |
| `SyncCursor` | 同步游标 | 服务端或对端确认已接收的位置，用于增量续传。 |

### 3.1 DocId 取舍（待确认）

- **方案 A（MVP 推荐）**：`DocId = 工作区相对路径`。
  - 优点：与 `DocPath`、`CrdtRoster`、文件监听、Workspace API 一致；迁移成本低。
  - 缺点：重命名是“删除旧路径 + 创建新路径”还是“同文档改路径”需要额外语义；路径大小写在跨平台文件系统上有坑。
- **方案 B**：`DocId = 稳定 UUID`，路径作为元数据。
  - 优点：重命名自然；未来支持非文件后端更干净。
  - 缺点：需要引入路径映射层、迁移现有本地数据、改 Workspace/Store/UI 假设，范围明显扩大。

> 草案建议：MVP 采用方案 A，重命名先按“新路径文档 + 旧路径墓碑（可选保留重定向元数据）”处理；待产品确认后再决定是否升级为稳定 UUID。

---

## 4. 数据模型

### 4.1 文档内容

文档内容继续由 `CrdtDoc` 承载：

- 根文本：`Y.Text("content")`
- 编码：标准 Yjs v1
- 本地桥接：`mazha-core::crdt` 负责 `Document <-> CrdtDoc`
- 持久化：MVP 可继续使用 `encode_update()` 全量帧文件（现有 `save_crdt/load_crdt`），后续可加增量日志/快照压缩。

### 4.2 名册元数据

建议引入 `RosterMeta`（名称待定，不要求与最终实现一致）：

```text
RosterMeta {
  workspace_id: WorkspaceId,
  docs: BTreeMap<DocId, DocMeta>,
  tombstones: BTreeMap<DocId, Tombstone>,
}
```

`DocMeta` 最小字段建议：

| 字段 | 类型 | 说明 |
|------|------|------|
| `doc_id` | `DocId` | 工作区相对路径。 |
| `exists` | bool | 当前是否存活；若有 tombstone 则为 false。 |
| `last_modified_ms` | i64 | 本地观测到的修改时间，仅诊断/UI，不参与 CRDT 收敛。 |
| `content_hash` | Option<String> | 可选，用于快速检测文件/CRDT 分叉，不作为权威。 |
| `snapshot_seq` | Option<u64> | 可选，本地快照版本。 |

### 4.3 删除墓碑

这是 D94 留下的关键缺口。草案建议：

```text
Tombstone {
  doc_id: DocId,
  deleted_at_ms: i64,      // 墙钟时间，仅展示/排查
  deleted_by: DeviceId,    // 诊断字段
  op_id: String,           // 单调或唯一操作 ID，建议 ULID
  reason: "delete" | "rename-source" | "replace", // 可选
}
```

语义建议：

1. **删除优先于旧内容复活**：若收到针对已墓碑化 `DocId` 的旧更新，默认忽略内容更新，但保留墓碑。
2. **墓碑可同步**：`remove` 不再只影响本机；需要通过同步消息传播。
3. **墓碑保留期**：MVP 不永久清理；后续可在“所有已知设备确认已越过墓碑游标”后 GC。未确认前不得 GC。
4. **重建同路径文档**：若用户在同一路径新建文档，需要“复活策略”：
   - 方案 R1：新建视为新操作，生成更高 `op_id` 的 `resurrect`/`upsert` 事件，覆盖墓碑。**推荐，MVP 简单。**
   - 方案 R2：禁止复活，只能新路径。实现简单但产品体验差。

> 待确认：R1/R2 选型；若选 R1，需要定义 `upsert` 与 `tombstone` 的因果/排序规则。

### 4.4 同步消息载荷（S2 已定，字节编码留 S3）

建议在 D94 的“ID → Yjs v1 帧映射”基础上扩展为信封：

```text
SyncEnvelope {
  protocol: "mazha-sync/0",
  workspace_id: WorkspaceId,
  device_id: DeviceId,
  sent_at_ms: i64,
  payload: Hello | HelloAck | Push | Pull | PullResponse | Snapshot | Ack | Error,
}
```

内容帧仍复用 Yjs v1：

```text
DocFrameMap = BTreeMap<DocId, Vec<u8>>   // 与 D94 一致：标准 Yjs v1
```

元数据变更建议单独建模，不塞进 Yjs 文本：

```text
MetaChange {
  doc_id: DocId,
  kind: "upsert" | "tombstone",
  tombstone?: Tombstone,
}
```

### 4.5 S1 数据结构契约（`mazha-crdt`，D98）

> S0 已确认 D95-D97；本节把 S-02/S-03 落到 S1 可实现的**纯内存模型 + 纯函数合并规则**，作为 `mazha-crdt` 的实现契约（主 SPEC §6.5 同步记录）。只定义内存类型与收敛规则，**不定义线上字节编码、信封字段名与传输顺序**（留 S2）。

#### 4.5.1 `OpId` 与全序

```text
OpId { clock: u64, device: String }
```

- 全序为 `(clock, device)` **字典序**：先比逻辑时钟，时钟相同再比设备标识（Rust `String` 字节序）。不用墙钟参与比较，避免时钟回拨破坏收敛。
- `device` 必须非空且不含控制字符（与 `DocId` 同一校验规则）；非法构造返回 `CrdtError::InvalidOpId`，**不 panic**。逻辑时钟由调用方保证单调推进，`mazha-crdt` 只做比较与校验，不读系统时间。
- 设备标识同时承担 D94 yrs 副本标识之外的“同步操作来源”职责；`ReplicaId` 仍是 yrs 内部概念，不暴露为产品身份（见 §3）。

#### 4.5.2 `Tombstone` 与可恢复记录

```text
Tombstone {
  doc_id: DocId,
  op_id: OpId,
  reason: "delete" | "rename-source" | "replace",
  conflict_frames: Vec<Vec<u8>>,
}
```

- `reason` 只用于诊断/UI 文案（D95 的重命名按“删旧建新”落为 `rename-source`），**不参与收敛判断**；纯模型不存墙钟时间，展示用时间由 S2 同步信封携带。
- `conflict_frames` 是 D95 “禁止静默丢数据”的落地：每个元素是一份**自包含的 Yjs v1 全量更新帧**（由 `CrdtDoc::encode_update()` 产出），代表一份被墓碑取代但必须可恢复的本地副本。来源包括：本机墓碑化删除时被移除的文档内容、墓碑 wins 时被忽略的离线编辑内容帧、被拒绝的低/等 `op_id` upsert 的帧。任取一帧应用到空 `CrdtDoc` 即可还原当时文本。
- 帧列表按字节字典序**排序 + 去重**，因此不同投递顺序的多端副本合并结果一致；每帧在构造/合并时校验可解码，坏帧使整批拒绝、零改动。MVP 不做墓碑 GC（D95/S-04），故帧只增不减地归并。
- 更高 `op_id` 的墓碑覆盖旧墓碑时，**必须归并双方的 `conflict_frames`**，不得因“胜出”丢弃旧墓碑已保存的副本。

#### 4.5.3 `RosterMeta` 与确定性合并

```text
RosterMeta { tombstones: BTreeMap<DocId, Tombstone> }
```

- 只用 `BTreeMap` 保存墓碑，保证枚举/快照确定；不保存存活文档（内容仍由 `CrdtRoster` 的 `CrdtDoc` 承载），也不保存游标/批次（S2 职责）。
- `merge_from` 纯元数据合并：对每个 `DocId`，`op_id` 高者胜出；低/等 `op_id` 不覆盖胜出墓碑的 `op_id`/`reason`，但**仍归并 `conflict_frames`**；不同 `DocId` 的墓碑相互独立。重复合并同一份元数据是幂等的。
- 名册暴露 `tombstones()` / `is_tombstoned()` / `meta_snapshot()` 只读查询；`meta_snapshot()` 返回克隆，调用方改动不影响名册内部状态。

#### 4.5.4 `MetaChange`：upsert / tombstone 与批次规则

```text
MetaChange =
  | Tombstone(Tombstone)                         // 删除/改名源/替换
  | Upsert { doc_id, op_id, frame }              // 同路径重建；frame 为自包含 Yjs v1 全量帧
```

- `Upsert.frame` 必须是**可独立应用的全量 Yjs v1 更新帧**（通常用 `CrdtDoc::encode_update()` 取得）；纯模型只校验可解码，无法验证“是否真的全量”，协议层需保证来源（S2）。
- 批次内先把 `MetaChange` 按 `(op_id, 类型, doc_id, frame)` 升序排序：类型序为 `Upsert < Tombstone`，因此 **`op_id` 相同时墓碑胜出**（落实 D95 默认墓碑 wins）；排序后再顺序应用，使批次结果与输入顺序无关。
- 应用规则：
  1. **墓碑 wins**：墓碑化的 `DocId` 从活动文档表移除；被移除文档的当前全量帧写入该墓碑的 `conflict_frames`。
  2. **不复活**：现存墓碑遇到低/等 `op_id` 的 upsert（或内容帧更新）时不复活文档，把该帧并入 `conflict_frames` 保底。
  3. **复活**：仅当 upsert 的 `op_id` **严格大于**现存墓碑时才删除墓碑并应用其 `frame` 建立活动文档。
  4. **覆盖**：更高 `op_id` 的墓碑覆盖旧墓碑（归并双方副本帧）；低/等墓碑不覆盖胜出者，但仍归并副本帧。
  5. **原子性**：非法 ID/OpId/元数据、坏帧（内容帧、upsert 帧或冲突副本帧）整批拒绝，活动文档与元数据都不变。
- D94 内容接口边界：`state_vectors` / `encode_diff` / `apply_updates` **签名与语义完全不变**，仍只处理 `BTreeMap<DocId, Vec<u8>>` 内容帧、不查墓碑。新增 `encode_diff_with_meta` 单独返回（内容 diff, 元数据快照）；新增 `apply_updates_with_meta(内容帧, 元数据变更)` 先按上述规则处理元数据，再处理内容帧——已墓碑化文档的内容帧改为存入 `conflict_frames`，不建活动文档。上层若混用旧 `apply_updates` 直接推内容帧，仍可能按 D94 旧语义重建文档，这是明确的兼容边界，S2 同步路径必须走新 API。

---

## 5. 同步协议草案

### 5.1 设计原则

- **P1 内容帧标准化**：文档内容只用 Yjs v1。
- **P2 元数据显式化**：删除墓碑、权限、游标不伪装成文档文本。
- **P3 幂等**：同一消息重复投递不应破坏本地状态。
- **P4 可恢复**：任意一步失败后可从已确认游标继续。
- **P5 小步可落地**：MVP 可先做“轮询/请求-响应”，再升级 WebSocket 推送。

### 5.2 最小握手

1. 客户端发送 `Hello`：
   - `workspace_id`
   - `device_id`
   - 支持的协议版本/能力（`content_v1`, `tombstone_v1`, `snapshot_v1`）
   - 认证凭证（见 §8）
2. 服务端返回 `HelloAck`：
   - 会话 ID / 短期 token
   - 服务端游标
   - 是否要求快照引导
   - 限流/最大帧大小

### 5.3 增量同步（MVP）

**拉取**：

1. 客户端发送本地 `state_vectors()` 与已知墓碑版本/游标。
2. 服务端计算缺失内容与缺失墓碑，返回 `PullResponse`。
3. 客户端先应用墓碑，再应用内容帧；应用顺序需确定（待 ADR）。
4. 客户端确认 `Ack(cursor)`。

**推送**：

1. 客户端本地编辑产生内容变更或墓碑变更。
2. 客户端发送 `Push`（内容帧 + 元数据变更 + 基于游标的幂等键）。
3. 服务端持久化并返回新游标。
4. 客户端标记该批次已确认。

### 5.4 冲突与并发

| 场景 | 预期语义 | 说明 |
|------|----------|------|
| 同文档并发文本编辑 | Y.Text 合并，不丢字符 | 已有 yrs 保证；UI 可能出现语义怪异。 |
| 一端删除、一端离线编辑同文档 | **待确认** | 推荐：若删除墓碑 wins，则文档保持删除并保留冲突副本/提示；若复活策略允许，可生成新 upsert。MVP 至少不能静默丢数据。 |
| 两端同时删除 | 幂等收敛 | 同一 `DocId` 多墓碑按 `op_id` 或全序规则去重。 |
| 重命名（路径即 DocId） | 建模为删除旧 + 新建新 | MVP 可接受；后续若引入稳定 DocId 再优化。 |
| 坏帧/部分失败 | 整批拒绝或按文档事务提交 | 复用 D94 “坏帧整批拒绝零改动”原则，扩展到墓碑+内容的原子性边界需定义。 |

### 5.5 离线恢复与幂等

- 客户端持久化：本地 roster 快照/更新、墓碑表、最后确认游标、未确认出站队列。
- 每个出站批次带 `batch_id`：**S2 已定为确定性 `{device_id}-{counter}`，不用 ULID/随机数**（§5.7.2）；服务端对 `workspace_id + device_id + batch_id` 去重。
- 重连后先 `Hello`，再按游标增量拉取，最后重放未确认出站批次。
- 服务端返回“游标过旧/历史已压缩”时，客户端回退到快照引导。

### 5.6 快照引导

适用场景：新设备首次加入、游标过旧、墓碑 GC 后历史不足。

建议流程：

1. 服务端提供 `Snapshot`：包含当前存活文档的全量 Yjs v1 帧 + 墓碑表 + 新游标。
2. 客户端清空或隔离本地未初始化 roster（需用户确认策略），应用快照。
3. 快照后进入正常增量模式。

> 待确认：已有本地数据的新设备加入时，是“合并”“覆盖”还是“新建本地工作区副本”。

### 5.7 S2 协议与会话契约（已定，D99 / 0.1.122）

> 本节把 §4.4 与 §5.2–§5.6 的草案落为 S2 的**内存协议模型 + 会话行为边界**：
> 只描述类型、字段语义与不变量，**不定义线上字节编码或信封字段名**（字节序列化、
> HTTP/WebSocket 与持久化留 S3）。实现位于新 crate `mazha-sync`：纯 Rust、
> 无网络 IO、无 GUI、无 async runtime，用 `Rc<RefCell<...>>` 的 in-memory server 跑通协议。

#### 5.7.1 消息/信封清单与字段语义

- `SyncEnvelope<P>`：统一的内存信封，`protocol` 固定为 `"mazha-sync/0"`（版本标识，
  不参与收敛）、`workspace_id` / `device_id` 为类型化 ID、`sent_at_ms: i64` 仅诊断/审计
  （默认 0，**不读系统时钟、不参与比较**，便于单测复现）、`payload: P` 为具体消息。
  请求 `HelloRequest` / `PullRequest` / `PushRequest` 即 `SyncEnvelope<对应 Payload>` 的别名，
  因此每条请求都自带工作区与设备身份；响应不带信封头，由 transport 的会话上下文定位。
- `SyncMessage` 是 9 种消息的标记联合，顺序固定（同时暴露 `SyncMessageKind` 供确定性断言）：
  `Hello → HelloAck → Pull → PullResponse → Push → PushAck → Snapshot → Ack → Error`。
- `HelloRequest` 载荷：`capabilities`（`content_v1` / `tombstone_v1` / `snapshot_v1` 的枚举集合；
  认证凭证留 S3）。`HelloAck`：`session_id`、服务端当前 `cursor`、
  `requires_snapshot` 标志与 `max_frame_bytes` 限流提示。
- `PullRequest` 载荷：`session_id`、`cursor`（客户端 `last_ack_cursor`，即 `since`）、
  `state_vectors`（`CrdtRoster::state_vectors()` 的整册映射）、`known_metadata`（本地墓碑快照，
  供服务端计算缺失元数据）。`PullResponse`：`cursor`、`frames`、`meta_changes`。
- `PushRequest` 载荷：`batch_id`、`frames`、`meta_changes`。`PushAck`：`cursor` 与
  `duplicate` 标志（重复投递返回首次 ack 的游标，不再改动日志）。
- `SnapshotPayload`：`frames`（全量 Yjs v1 帧）、`meta`（`RosterMeta` 快照）、`cursor`。
  `AckPayload`：通用 `cursor` 确认（预留给未来服务端主动推送；MVP transport 不产出）。
  `SyncErrorPayload`：`code`（协议错误码）+ 诊断 `message` + 可选 `cursor`。
- 内容帧字段一律 `BTreeMap<DocId, Vec<u8>>`（`DocId` 复用 `mazha_crdt::DocId`，值仍是
  标准 Yjs v1 帧）；元数据是 `Vec<MetaChange>` / `RosterMeta`，**不伪装进 Yjs 文本**。

#### 5.7.2 标识与游标规则

- `WorkspaceId` / `DeviceId` / `BatchId` 是类型化字符串：非空、不含控制字符，非法在构造期
  返回 `ProtocolError::InvalidId`，不 panic；CJK 字符合法（键序按 Rust 字符串字节序）。
- **`BatchId` 必须确定性**：`{device_id}-{monotonic_counter:020}`（session 内从 0 递增，
  device 固定）。不使用 ULID/随机数，使离线重放与单测可复现。
- 服务端去重键为 `(workspace_id, device_id, batch_id)`；重复批次返回首次记录的 ack 游标，
  内容/元数据/变更日志均不二次改动。
- `SyncCursor` 是单调 `u64`，**只有服务端解释其含义**，客户端只保存与回放：初始 0；
  `HelloAck` 告知服务端当前游标（诊断，不用作拉取起点，否则首轮会跳过服务端变更）；
  拉取用 `since = last_ack_cursor`；推送成功后以 `PushAck.cursor` 推进；
  应用成功/收到 ack 后才推进，失败原地不动。游标回退一律取 `max(旧值, 新值)`。
- 服务端保留按游标编号的变更日志与可裁剪下界 `log_floor`：`since < log_floor` 返回
  `CursorTooOld`；`since > cursor` 视为状态不一致，返回 `SnapshotRequired`。

#### 5.7.3 下游应用与原子性边界

- `PullResponse` 同时携带内容帧与元数据变更。客户端**只调用一次**
  `CrdtRoster::apply_updates_with_meta(frames, meta_changes)`：该方法内部先按
  `(op_id, 类型[Upsert<Tombstone>], doc_id, frame)` 排序处理元数据（墓碑 wins、严格更高
  `op_id` 的 upsert 才复活），再把内容帧应用到非墓碑文档，且两阶段提交保证原子性。
  这与“先应用元数据再应用内容帧”的朴素写法**等价**，但不会出现“先应用了一半才遇坏帧”
  的中间态；**同步路径严禁使用不查墓碑的旧 `apply_updates`**。
- 坏帧、坏元数据、非法 ID 的边界：客户端应用失败时本地活动文档、墓碑表、可恢复副本、
  `last_ack_cursor` 全部不变；服务端 push 校验失败时游标/日志/去重表也全部不变
  （错误码：`BadFrame` / `BadMetadata` / `InvalidId`）。
- 假服务端**采用服务端校验**（而非盲中继）：push 时先在暂存 `CrdtRoster` 上调用
  `apply_updates_with_meta` 验证帧与元数据，成功才提交到服务端状态、推进游标并记日志；
  复用 `mazha-crdt` 已有校验能力，避免在第二处实现 yrs 解码。S3 的真实服务端可沿用同一
  策略；若改为盲中继，也必须在客户端保留上述原子性边界与去重/游标语义。

#### 5.7.4 快照引导

- `HelloAck.requires_snapshot = true` 时，会话在拉增量前走快照；
  `pull` 收到 `CursorTooOld` 时自动改走快照（同一约束）。
- **自动 apply 的前提**：本地 `outbox` 为空（无未确认出站批次，也就没有本地未同步改动）。
  否则返回 `SnapshotNeedsConfirmation { pending_batches }`，交由用户确认“合并/覆盖/
  新建本地副本”策略（对齐 §5.6 待确认项），不静默丢弃本地改动。
- 应用快照时在**新的空 `CrdtRoster`** 上用 `apply_updates_with_meta(frames, meta)` 一次提交，
  成功才替换本地 roster 并把游标设为快照游标；失败本地状态与游标不变。

#### 5.7.5 阶段边界

- `subscribe` / WebSocket / 事件流仍后置，MVP `SyncTransport` **不放订阅方法**（§6.4）。
- 字节编码、压缩、认证凭证、真实 HTTP/WebSocket 与服务端持久化均属 S3；
  本阶段只有同步请求-响应序列、游标推进、批次幂等、快照回退与断线重放的纯逻辑。

### 5.8 S3 线上字节编码契约（已定，D100 / 0.1.123）

> 本节定义 S2 九种 `SyncMessage` 的**确定性字节编码**。编码只做“消息 ⇄ 字节”，
> 不解释游标语义、不做 CRDT 合并、不触碰网络或磁盘；实现位于 `mazha-sync/src/wire.rs`，
> 与 S2 一样保持纯 Rust、无网络 IO、无 GUI、无 async runtime、无新增外部依赖
> （只依赖 `mazha-crdt` + `thiserror`）。

#### 5.8.1 外层帧格式

- 每帧 = `magic(4) | version(1) | kind(1) | body_len(u32 LE) | body(body_len) | crc32(u32 LE)`。
  - `MAGIC = b"MZS0"`（4 字节 ASCII）。
  - `WIRE_VERSION = 1u8`；`decode_message` 只接受等于 1 的版本，其余返回 `WireError::UnsupportedVersion`。
  - `kind` 取固定值 `1..=9`，与 §5.7.1 声明顺序一一对应：
    `1=Hello`、`2=HelloAck`、`3=Pull`、`4=PullResponse`、`5=Push`、`6=PushAck`、
    `7=Snapshot`、`8=Ack`、`9=Error`。未知 kind 返回 `WireError::UnknownKind`。
  - `body_len` 为 `u32` 小端，仅描述 body 长度（不含头 10 字节与尾部 4 字节 CRC）。
  - `crc32` 为 IEEE 多项式（与 zlib CRC-32 兼容）对**整个“头 + body”**计算，小端写出；
    解码端先校验长度、再校验 CRC，损坏返回 `WireError::BadCrc`。
- 解码顺序固定为：magic → version → kind → body_len → 读 body → 校验 CRC → 解析 body。
  截断（剩余字节不足）、`body_len` 超过上限、body 内字符串/字节长度越界、非法 UTF-8、
  未知枚举判别值，一律返回错误而不是 panic，也不做部分解码。
- 帧总长上限由常量限制（头部 + body + CRC）：默认 `MAX_FRAME_BYTES = 8 MiB`（与
  `HelloAck.max_frame_bytes` 的默认值 `DEFAULT_MAX_FRAME_BYTES` 一致），可通过 helper 显式传入更小的上限做单测；
  超限在编码与解码两侧都拒绝（编码侧返回 `WireError::FrameTooLarge`，解码侧返回
  `WireError::BodyTooLarge`）。
- **golden vector（版本锁定）**：`Ack { cursor: 7 }` 的完整帧必须为
  `4d5a5330 01 08 08000000 0700000000000000 adc3f5b0`
  （连续小写 hex：`4d5a53300108080000000700000000000000adc3f5b0`），用于验证
  magic / 版本 / kind / 小端 u64 / CRC 不因重构漂移。

#### 5.8.2 Body 编码规则

- 所有整数使用**小端**：`u8` 1 字节、`u32` 4 字节、`u64` 8 字节；`i64` 以相同位宽的
  小端补码写出（`sent_at_ms` 为 0 时也是 8 字节，保证 golden vector 可复现）。
- 所有变长字段使用 `u32 LE` 长度前缀：`Vec<u8>`（内容帧、操作字节）、`String`/`&str`
  （以 UTF-8 字节数计长）、`Vec<T>`（元素数量）、`BTreeMap<K, V>`（条目数量）、
  `RosterMeta.ops/tombstones`（数量）。
- `BTreeMap` 必须按键升序编码（直接遍历 `BTreeMap`；`DocId = String` 按 Rust 字符串字节序），
  保证相同逻辑消息的字节输出唯一；解码端按同序重建 `BTreeMap`。
- 枚举以 `u8` 判别值编码，判别值与 S2 枚举的 `From`/`TryFrom` 一致：
  `Capability` = `content_v1=0 / tombstone_v1=1 / snapshot_v1=2`；
  `SyncErrorCode` = 与 §6.4 错误分层一一对应的固定序号；
  `MetaChange` 以 `u8` 区分 `Upsert(0)` / `Tombstone(1)`，再写各自的 `OpId` / `Tombstone` 字段。
- `SyncEnvelope<P>` 的请求体字段顺序固定为
  `protocol(String) | workspace_id(String) | device_id(String) | sent_at_ms(i64) | payload(P)`；
  响应消息（HelloAck/PullResponse/PushAck/SnapshotPayload/Ack/Error）不带信封头。
- 各消息 payload 的字段顺序按 §5.7.1 的声明顺序编码；`Option<T>` 用 `u8` 标志
  `0=None / 1=Some` 后接值。

#### 5.8.3 Snapshot 方向与身份携带

- 为保持九种 kind 与 §5.7.1 顺序不变，`Snapshot` kind 的 body 以第一个字节作方向：
  - `0 = SnapshotRequest`：body = 方向字节 + `SyncEnvelope<()>`；供 `SyncTransport::snapshot()`
    在无参数签名下携带 workspace / device 身份，服务端校验后返回全量快照。
  - `1 = SnapshotPayload`：body = 方向字节 + `SnapshotPayload` 的 `frames / meta / cursor`。
  - 其它方向字节返回 `WireError::UnknownSnapshotDirection`。
- `decode_message` 只对方向 1 产出 `SyncMessage::Snapshot(SnapshotPayload)`；
  SnapshotRequest 由 HTTP 服务端路由在解析信封后直接消费，不进入 `SyncMessage` 联合，
  因此九种 `SyncMessage` 与 `SyncMessageKind` 的契约保持不变。

#### 5.8.4 线上 API 与错误边界

- `mazha-sync::wire` 导出：`MAGIC`、`WIRE_VERSION`、`MAX_FRAME_BYTES`、
  `encode_message(&SyncMessage) -> Result<Vec<u8>, WireError>`、
  `decode_message(&[u8]) -> Result<SyncMessage, WireError>`；
  以及供持久化层复用的低层助手 `WireReader`、`put_u8`、`put_u32`、`put_u64`、
  `put_i64`、`put_bytes`、`put_string`、`crc32`，以及结构级助手 `put_frame_map`/
  `read_frame_map`、`put_meta`/`read_meta`、`put_meta_changes`/`read_meta_changes`、
  `put_op_id`/`read_op_id`、`put_tombstone`/`read_tombstone`（磁盘格式与线上格式同一套规则）。
- `WireError` 分类：`UnsupportedVersion` / `UnknownKind` / `BadMagic` / `BadCrc` /
  `BodyTooLarge` / `FrameTooLarge` / `Truncated` / `InvalidUtf8` / `InvalidEnum` /
  `UnknownSnapshotDirection` / `UnsupportedProtocol` / `InvalidFrame` / `InvalidId` /
  `Io`（仅来自内存 writer，不产生外部 IO）。
- 严格拒绝：未知版本、未知 kind、截断帧、坏 CRC、坏 UTF-8、越界长度、非规范枚举值；
  解码失败不返回任何半构造的 `SyncMessage`。

---

### 5.9 S4 本地工作区同步编排契约（已定，D101 / 0.1.124）

> 本节把 §5.7 的 `SyncSession` 接到 D91/D92 已有的文件监听与本地 `.md` 文件，
> 定义“本地文件 ⇄ 名册 ⇄ 远端帧”的编排边界。实现位于**新 crate `mazha-sync-local`**：
> 依赖 `mazha-sync` + `mazha-crdt` + `mazha-store`，不依赖 `mazha-core`、不依赖
> GUI/网络/async runtime、不反向依赖 `mazha-sync-server` / `mazha-sync-http`；
> 真正的网络实现通过调用方传入的 `&mut dyn SyncTransport` 注入（S3 的
> `HttpTransport` / `FakeTransport` 均可），因此本 crate 本身仍属纯 Rust 核心边界。

#### 5.9.1 职责边界与触发/调度模型

- 本地引擎负责五件事：① 消费工作区文件监听事件；② 把本地 `.md` 文本暂存进
  `SyncSession`；③ 按调用方策略调用 `sync_once(&mut dyn SyncTransport)`；④ 把远端
  变更写回本地 `.md`（含删除）；⑤ 把名册帧、墓碑元数据、游标、会话 ID 与会话进度
  持久化到工作区隐藏目录。
- **不引入后台线程 / async runtime**：`SyncTransport` 与 `SyncSession` 已是同步
  `&mut self` 接口（D24/D99），引擎所有方法均同步阻塞。事件到达时**只标脏**
  （`BTreeSet<DocId>`），去重/合并窗口由纯函数 `merge_watch_events` 完成；
  真正的读文件、`stage_text`、网络同步统一由 `sync_once` 驱动。可选周期同步
  由上层 S5 的调度器调用 `sync_once`，本 crate 不自行起定时器。
- `handle_watch_events(&[WatchEvent])` 只过滤 + 合并 + 标脏，**不读文件**；
  `drain_dirty()`/`sync_once()` 才读盘并把内容暂存进会话。这样同一文件在窗口内的
  N 次写入只会产生一次 CRDT 更新与一个出站批次。
- 重命名按 D95 “删旧建新”处理：`Renamed{from,to}` 同时标脏旧路径（删除 →
  `stage_delete` 墓碑 `rename-source` 语义）与新路径（新建 → `stage_text`），
  不隐式保留文档身份。

#### 5.9.2 本地文件 ⇄ 名册帧映射

- `DocId` 沿用 D95：工作区**相对路径**（`/` 分隔、UTF-8），中文路径合法。
  `doc_id_from_path(root, absolute)` 先 `strip_prefix(root)` 拒绝越界，再按 `/`
  规范化为 `DocPath`；非 UTF-8、`..` 逃逸、绝对路径一律拒绝。
- `is_syncable_md(path)` 只接受 `.md`/`.markdown`（不区分大小写）且**任一路径段
  都不以 `.` 开头**；因此 `.mazha/`、`.obsidian/`、`.hidden.md` 等内部/隐藏文件
  全部忽略。该函数是纯函数，路径合法性由 `DocPath` 校验（D24）。
- 本地 → CRDT：读到的 UTF-8 文本经 `local_text_to_frames(doc_id, base, text)` 生成
  标准 Yjs v1 全量帧；`base` 是名册中**现有 `CrdtDoc` 的 fork**，在其上整段
  `replace` 后再 `encode_update`，因此保留原 CRDT client id、不会把整篇文本当作
  新文档合并。文档已存在且文本与磁盘一致时不 stage（避免无意义批次）；文档已
  墓碑化时 `base` 取不到、生成自包含全量帧，并携带 `Upsert` 经 `stage_local_change`
  入队（等价于 `stage_text`，高 `op_id` 才复活）。
- CRDT → 本地：`remote_frames_to_local_writes(before, after, before_tombstones)` 比较
  `sync_once` 前后的名册文本，产出 `Write{doc_id,bytes}` / `Remove{doc_id}`：
  新增或文本变化 → 写回；新出现的墓碑 → 删本地文件。写回直接把 `CrdtDoc::text()`
  的 UTF-8 字节交给 `StorageProvider::write`（文本即源真相，等价于
  `mazha-core::to_md`/`serialize_source` 的逐字节身份语义），**CJK 与
  `\r\n`（CRLF）原样保留**，不引入 Markdown 重排或 BOM。
- 删除按 `stage_delete` 走墓碑；远端墓碑到达后本地文件被删除且不再复活；
  被墓碑取代的离线编辑帧由 `apply_updates_with_meta` 留在 `conflict_frames`，
  引擎在 `status().conflict_docs` 暴露这些 `DocId`，不静默丢数据（D95）。

#### 5.9.3 回声抑制与去抖

- 引擎维护 `write_marks: BTreeMap<DocId, Option<Vec<u8>>>`：`Some(bytes)` 表示
  “刚写过这些字节”，`None` 表示“刚删除该文件”。纯函数
  `should_ignore_echo(path, observed, marks)` 做字节比对：磁盘字节与标记相同
  （或都缺失）才判为回声；命中后**消费标记**，避免后续真实编辑被永久忽略。
  非回声观察（含删除标记不匹配）必须由引擎清除该文档的旧标记，否则用户把文件
  改回旧字节时会被误判为回声而漏推；纯函数只判定，清理在 `drain_dirty` 完成。
- 即使用户在远端写回后立刻把文件改回同名同内容，`drain_dirty` 还会再次比对
  磁盘文本与名册文本，相同则不 stage，形成第二层保护。
- 合并窗口默认 150ms（与 store 的 `COALESCE_WINDOW` 对齐），可由 `load` 的参数
  覆盖；只影响“事件 → 脏集合”的标脏阶段，不影响同步协议的游标与批次语义。

#### 5.9.4 本地持久化与坏文件处理

- 状态文件固定为工作区内 `.mazha/sync-state.bin`，通过 `StorageProvider` 以
  `DocPath` 读写（D24），不另造 IO 抽象。写入顺序：`write(.mazha/sync-state.tmp)`
  → `rename(tmp, sync-state.bin)`，与 D100 服务端 `FileSyncStore` 同思路的
  **temp+rename 原子替换**；失败时清理临时文件，成功后不留临时文件。
- 磁盘格式为版本化字节：`magic(b"MZLS") | version(u32 LE=1) | last_cursor(u64 LE)
  | op_clock(u64 LE) | batch_counter(u64 LE) | session_id(u8 标志 + UTF-8)
  | frames（wire `put_frame_map`）| meta（wire `put_meta`）
  | outbox（数量 + `BatchId`/frames/`MetaChange`，复用 wire 助手）
  | crc32(u32 LE)`。与线上 `MZS0` 帧 magic 区分，复用 `mazha-sync::wire` 的
  小端/长度前缀/CRC32 助手，不新增依赖。
- `LocalSyncState` = `{frames, meta, last_cursor, session_id, op_clock,
  batch_counter, outbox}`。除名册与墓碑外，**必须**持久化 `op_clock` /
  `batch_counter` / 未确认 outbox：否则进程重启后批次号从 0 重来会被服务端按
  `(workspace, device, batch)` 去重而吞掉新编辑，逻辑时钟回退也会破坏 D98 全序。
- 落盘时机：`drain_dirty` 暂存成功即写盘，`sync_once` 的传输失败/快照确认失败
  路径同样写盘；因此断线或进程崩溃后 outbox、逻辑时钟与批次号仍可恢复（G2），
  离线编辑不会只留在内存中丢失。
- 为支持上述恢复，`mazha-sync` 增加**非破坏性** additive API：
  `PendingBatch::new/into_parts`、`SyncSession::checkpoint() ->
  SessionCheckpoint`、`SyncSession::restore_checkpoint(SessionCheckpoint)`。
  D99 的消息、游标、去重、快照确认门槛与 stage 行为语义均不变；恢复后
  `needs_handshake = true`（下次 `sync_once` 重新握手），会话 ID 仅保留作诊断。
- 坏文件（magic/版本/CRC/长度/UTF-8/帧非法）**拒绝加载并返回错误**
  `LocalSyncError::CorruptState`，不 panic、不回退为空状态以免静默丢数据；
  调用方可据此提示用户修复/删除状态文件后重建。首次启动状态文件不存在时
  返回空状态并扫描工作区 Markdown 建初始名册（不自动 stage 删除）。

#### 5.9.5 冲突与保护边界

- `SyncSession::apply_snapshot` 在 outbox 非空时返回
  `SnapshotNeedsConfirmation`，本地引擎捕获后设置
  `status().needs_confirmation = true`、保留全部本地改动与 outbox、写盘后
  把错误原样返回给调用方；**绝不静默覆盖**本地文件/名册。S5 UI 据此显示
  “需用户确认”的合并/覆盖选择。
- 远端删除 vs 本地未同步编辑：墓碑 wins，但被取代帧进入 `conflict_frames`
  并在 `status().conflict_docs` 可见；本地不被静默覆盖为“文件消失且无记录”。
- `platforms/*` 本阶段零改动；桌面状态栏/横幅与调度留 S5。文档整体重建的
  undo/选区代价（S-08）仍按 D97 MVP 接受，S4 不触碰 `mazha-core`。

### 5.10 S5a 桌面接入第一批契约（已定，D102 / 0.1.125）

> 只改 `platforms/desktop/*` 与桌面独立 `Cargo.lock`；根 workspace 依赖与
> `Cargo.lock` 不改。本节的“引擎”指 S4 的 `LocalSyncEngine`，其 D101 语义零改动。

#### 5.10.1 线程模型与生命周期

- `LocalSyncEngine` 与 `HttpTransport` **只在专用同步线程**上创建/调用；
  GPUI 主线程只发命令、收状态。验证方式：桌面纯 Rust 单测用泛型
  `assert_send::<LocalSyncEngine>()` 编译期断言引擎可跨线程转移
  （`StorageProvider: Send + Sync`，会话/名册均为可 `Send` 的同步结构）。
- 命令通道：`SyncNow`（手动/周期）、`WatchEvents(Vec<WatchEvent>)`（只标脏）、
  `Shutdown`；回传通道：`SyncState`（未配置/同步中/成功/失败与游标）、
  `SyncStatus`（`LocalSyncStatus` 快照）与启动失败原因。
- 工作区根确定后（`sync_workspace_watch`）启动一条线程；根变化时先 `Shutdown`
  旧线程再用当前 `FileSystemProvider`/root 重建引擎。`DocView` drop 时发
  `Shutdown` 并 join（join 放到独立等待线程，避免阻塞 GPUI 主线程）。
- 周期调度在同步线程内用 `recv_timeout(interval)`：收到 `SyncNow` 或超时都执行
  `sync_once`；网络失败只回传错误并保留 outbox，下个周期继续重试，线程不退出。
- 文件监听沿用 D92/D93 的**唯一**工作区根 watcher：在既有 `cx.spawn` 事件派发处
  把同一批 `WatchEvent` 过滤后转发给同步线程（只标脏），不另起 watcher；
  D92 的回声抑制、脏标签横幅与保存拦截语义不变。
- UI 回写统一走 `cx.spawn` + `WeakView::update`，禁止跨线程直接改 GPUI 状态；
  无配置/无 token 时不启动线程，状态显示“未配置”。

#### 5.10.2 配置与密钥边界

- `Settings`（`settings.json`）只存非敏感项：`sync_server_url`、
  `sync_workspace_id`、`sync_device_id`、`sync_interval_secs`（默认 30s，
  normalize 夹到 5–3600s），全部 `#[serde(default)]` 兼容旧配置。
- token **不明文落盘**（D97）：仅从环境变量 `MAZHA_SYNC_TOKEN` 读取；平台
  密钥链与设置 UI 留 S5b。`workspace_id` 省略时回退为规范化工作区根字符串，
  `device_id` 省略时取 hostname（非法时回退固定串）并**持久化回 settings**，
  保证重启身份稳定；两者最终都要通过 `WorkspaceId::new`/`DeviceId::new` 校验，
  非法配置在状态栏明确提示、不 panic。
- `server_url` 只接受 `http://` / `https://`，且必须有非空 host、无空白/控制
  字符；生产 TLS 由反向代理终结（D100），代码不做自签证书。

#### 5.10.3 状态、冲突与保护边界

- 底部状态栏与 D83 `ExportStatus` 共用右侧提示位：用纯函数拼接两段文案，
  任一段为失败即用强调色，避免二者互相覆盖。文案覆盖：未配置、空闲游标、
  同步中、上次同步成功游标、离线/失败原因、需确认、冲突 N。
- `needs_confirmation = true` 时显示明确提示与“重试同步”按钮（`SyncNow`）：
  “本地有未推送改动，已保留文件与 outbox”；**绝不自动丢弃 outbox 或本地文件**。
- `conflict_docs` 非空时复用 D92 `ExternalChange` 横幅样式与生命周期，列出冲突
  `DocId`（中文路径原样显示）并说明“数据已保存在同步状态中，未静默删除”；
  不提供“覆盖”按钮（S5b 才做真正的合并/覆盖交互）。
- 同步路径仍只走 `apply_updates_with_meta`；S5a 不引入 S-08 的增量 Document
  patch，远端写回后打开文档的 undo/选区代价按 D97 MVP 接受。

#### 5.10.4 S5a / S5b 边界

| 批次 | 本次范围 | 后置 |
|------|----------|------|
| S5a | 桌面注入 `HttpTransport`、周期/手动同步、状态栏/冲突横幅、快照需确认提示与重试；配置走 settings.json + 环境变量 | — |
| S5b 第一批 | S-08 增量 Document patch（减少 undo/选区丢失）、真正的“合并/覆盖”交互动作与快照确认选择（D103/D104） | S5b 第二批 |
| S5b 第二批 | — | WebSocket 订阅、快照压缩、墓碑 GC、平台安全存储（token 不再依赖环境变量） |

---

### 5.11 S5b 第一批契约（已定，D103/D104 / 0.1.126）

> 本批是 **additive**：D95–D102 的既有公开签名与语义零改动。`apply_remote` 仍返回
> `CrdtResult<bool>`；`LocalSyncEngine::sync_once`/`status` 的行为不变；桌面仍复用
> D102 的 `SyncHandle` / `poll_sync_replies` / `SyncReply` 与 D92 横幅样式。
> 不新增根 workspace 依赖；核心 crate 不引入 GUI/网络/async。

#### 5.11.1 S-08 增量 Document patch（D103）

1. **patch 生成**：`apply_remote` 先记录 `Document` 旧文本，把远端帧应用到 `CrdtDoc`
   后取新文本；对两个 UTF-8 字符串裁剪**最长公共前后缀**（裁剪点必须落在字符边界，
   禁止切坏 CJK/emoji），得到唯一一个 `Edit::Replace { range, text }` 语义的补丁。
   `RemotePatch` 暴露 `old_len`、`new_len`、`range`、`text`、`removed`，供校验与偏移重映射。
2. **应用路径**：新增 `apply_remote_detailed` 返回 `RemoteApplyOutcome { changed, patch }`；
   `apply_remote` 委托给它并只回传 `changed`，**签名与返回值保持不变**。无变化时不得
   触碰文档文本、undo/redo 栈与缓存。
3. **undo/选区契约**：`Document::apply_remote_patch` 经 `apply_and_refresh` 维护块/投影
   缓存，但**不把远端变更 push 进 undo 栈**；对已有 undo 栈的历史编辑偏移做重映射，
   规则为：`offset <= range.start` 不变，`offset >= range.end` 平移 `delta = new_len - old_len`，
   落在被替换区间内则钳到 `range.start + text.len()`。`redo` 栈直接清空：其
   `original/inverse` 绑定旧文本，偏移重映射不安全。光标/选区由调用方用同一个
   `RemotePatch::remap_offset` / `remap_range` 纯函数重映射，核心不持有 UI 选区。
4. **纯 Rust 单测**必须覆盖：CJK 多字节、公共前后缀、纯插入、纯删除、全替换、空文本、
   无变化不触动 undo/redo、多字节边界安全、undo 栈重映射与 redo 清空。

#### 5.11.2 冲突材料与三种选择的精确定义（D104）

冲突材料来自已有墓碑：`Tombstone::conflict_frames()` 是自包含 Yjs v1 全量帧，可应用到
空 `CrdtDoc` 恢复“冲突副本文本”；坏帧必须整批拒绝、零改动并返回错误。快照确认材料来自
`status().needs_confirmation`（本地文件/outbox 已保留，不自动清空）。三种选择：

| 选择 | 墓碑冲突（`resolve_conflict`） | 快照确认（`resolve_snapshot_confirmation`） |
|------|-------------------------------|--------------------------------------------|
| `KeepLocal`（保留本地） | 把**冲突副本文本恢复为当前文档正文**（需用户显式确认），墓碑与冲突副本保留；以严格更高 `op_id` stage 一次 upsert 复活该文档 | 放弃快照回退、保留本地文件与 outbox，清除本轮确认标记；下次同步仍可重试 |
| `UseRemote`（采用远端） | 接受远端删除：保留墓碑与冲突副本作审计，**不写回文档正文**、不静默删本地文件；清除本轮确认标记 | 接受服务端快照覆盖本地文本与 outbox：先以检查点显式清空 outbox，再允许下一轮快照引导 |
| `Merge`（合并） | **确定性非 CRDT 拼接**：`冲突副本文本 + "\n" + 当前本地正文`，写回本地并 stage 全量帧复活；墓碑与冲突副本保留 | **不拼接服务端快照文本**：保留本地文件与 outbox、只清除本轮确认标记，让下一轮同步由 CRDT 自身合并本地与远端；不清空 outbox |

- `Merge` 只在**墓碑冲突**场景做确定性拼接，**不是 CRDT 语义合并**；快照确认场景的
  `Merge` 连文本都不拼，只保留 outbox 交给下一轮 CRDT 自行合并，避免把服务端快照硬拼进正文。
- 任何“覆盖/替换/清空 outbox”都必须由 UI 先给出破坏性确认文案、用户二次确认后才发命令；
  引擎 API 本身只执行显式传入的选择，**绝不静默丢弃**。
- 解决动作只改本地状态/文件与会话暂存，**不自动触发网络同步**；`sync_once` 仍由
  “立即同步/周期超时”驱动。坏帧/非法 DocId/过期冲突材料一律返回错误且零改动。
- `resolve_conflict` / `resolve_snapshot_confirmation` 返回 `ResolveOutcome`，其中
  `document_text: Option<String>` 交给桌面通过既有本地保存路径写回；引擎不依赖
  `mazha-core`、不直接操作已打开的 `Document`。
- 已确认状态仅存于内存：引擎为冲突副本保存“帧字节指纹”、为快照门槛保存
  “outbox 批次 ID 指纹”，只有材料完全一致时同一会话内才抑制重复横幅；新冲突/
  新批次照常提示。指纹不写入 `.mazha/sync-state.bin`，重启后横幅可能重现；
  持久化确认状态留 S5b 第二批，本批不引入新的状态文件字段。

#### 5.11.3 桌面交互入口（D104）

- `SyncBanner` additive 增加 `choices: Vec<SyncChoice>`（保留本地/采用远端/合并），
  既有 `action: Option<SyncBannerAction>` 与 `Retry` 保持原语义；冲突横幅与
  `needs_confirmation` 横幅都显示选择按钮。
- 破坏性选择（`UseRemote`、`KeepLocal` 的正文替换）第一次点击只切换到确认文案
  （由纯函数给出），第二次点击才通过 `SyncHandle::resolve_*` 发 `SyncCommand`；
  `Merge` 为非破坏性选择，可直接发送。UI 只发命令/回写状态，绝不跨线程改 GPUI 状态。
- 同步线程收到 `ResolveConflict { doc_id, choice, local_text }` /
  `ResolveSnapshot { choice }` 后调用引擎新 API，把 `ResolveOutcome` 连同目标 `doc_id`
  经 `SyncReply::Resolved { doc_id, outcome }` 回传；主线程在 `apply_sync_replies` 中
  按工作区根校验写回路径、应用 `document_text` 到对应标签并刷新状态栏/横幅，写盘失败
  时保留内存文档并提示失败。

---

## 6. 传输层抽象

### 6.1 目标

传输层只负责“可靠投递字节/消息”，不负责 CRDT 合并规则。

### 6.2 建议接口边界（草案，非最终签名）

```rust
// 草案示意：实际 crate/签名待 ADR 确认
pub trait SyncTransport {
    fn hello(&mut self, req: HelloRequest) -> Result<HelloAck, TransportError>;
    fn pull(&mut self, req: PullRequest) -> Result<PullResponse, TransportError>;
    fn push(&mut self, req: PushRequest) -> Result<PushAck, TransportError>;
    fn subscribe(&mut self, req: SubscribeRequest) -> Result<Box<dyn SyncSubscription>, TransportError>;
}

pub trait SyncSubscription: Send {
    fn next_event(&mut self) -> Result<Option<SyncEvent>, TransportError>;
}
```

约束：

- 协议类型应可序列化（`serde`/JSON/CBOR/二进制待定），但 **CRDT 内容帧字段保持原始 Yjs v1 bytes**。
- MVP 可只实现请求-响应；`subscribe` 为可选能力。
- 传输实现可放独立 crate，避免核心 crate 引入网络/async runtime。

### 6.3 传输候选

| 方案 | 优点 | 缺点 | 建议 |
|------|------|------|------|
| HTTP + JSON/CBOR | 实现简单、易调试、易穿透 | 实时性一般，需要轮询 | MVP 首选 |
| WebSocket | 双向推送、实时性好 | 连接管理复杂 | Phase 5.2/5.3 可加 |
| QUIC/gRPC | 性能/流控好 | 依赖与运维成本高 | 暂不建议 MVP |

### 6.4 S2 transport trait 与错误分层（已定，D99）

```rust
pub trait SyncTransport {
    fn hello(&mut self, req: HelloRequest) -> Result<HelloAck, TransportError>;
    fn pull(&mut self, req: PullRequest) -> Result<PullResponse, TransportError>;
    fn push(&mut self, req: PushRequest) -> Result<PushAck, TransportError>;
    fn snapshot(&mut self) -> Result<SnapshotPayload, TransportError>;
}
```

约束与错误分层：

- 四个方法全部**同步**、拿 `&mut self`；MVP 不放 `subscribe`/`next_event`，
  也不允许 async runtime、网络 IO 与 GUI 依赖进入 `mazha-sync`。
- `TransportError` 只表达传输/协议层失败：`Disconnected`（断线，outbox 必须保留）、
  `Server { code, message }`（服务端协议错误码）、`InvalidResponse`（如对端回包自相矛盾）、
  `Protocol`（ID/信封校验失败）。协议错误码包括 `CursorTooOld`、`BadFrame`、
  `BadMetadata`、`SnapshotRequired`、`Unauthorized`、`InvalidRequest`、`Internal`。
- `std::io::Error` 与 yrs 的解码错误**不得塞进 CRDT 层**：yrs 的错误由 `mazha-crdt`
  的 `CrdtError` 与 `apply_updates_with_meta` 的原子边界吸收；传输实现可有自己内部的 IO
  类型，但对外只映射为上述分层错误。
- `FakeTransport` 持有 `Rc<RefCell<InMemorySyncServer>>`，可一键模拟断线（所有方法返回
  `Disconnected`）；两个 `FakeTransport` 绑定同一 `WorkspaceId`、不同 `DeviceId` 即可
  跑“两端 + 一服务端”的最终一致性测试，全程无网络与线程。

---

## 7. 服务端职责边界

### 7.1 最小服务端（MVP 推荐）

最小服务端应是**“带持久化的同步中继”**，而不是纯内存转发：

- 保存工作区维度的最新快照或可重放日志。
- 保存墓碑表与游标。
- 对批次去重。
- 做认证与基本授权。
- 不理解 Markdown 语义，不做富文本合并。

### 7.2 可选能力（后续）

- 多设备在线状态与推送。
- 历史版本/回收站。
- 快照压缩、垃圾回收。
- 分享链接、团队权限、多租户管理。
- 审计日志与速率限制。

### 7.3 明确不做（服务端）

- 不把 Markdown 渲染为权威内容。
- 不在服务端改写用户文本。
- 不把 GUI 状态（选区、滚动位置）作为同步真相。

### 7.4 S3 最小持久化服务端与 HTTP 契约（已定，D100 / 0.1.123）

> S3 把 §7.1 的“带持久化的同步中继”落为独立二进制 `mazha-sync-server`，
> 并把 §6.4 的 `SyncTransport` 落为独立 crate `mazha-sync-http` 的真正网络实现。
> 服务端不做 Markdown 语义合并，只校验、去重、记日志、落盘与鉴权。

#### 7.4.1 存储（SyncStore / FileSyncStore）

- `SyncStore` trait 以工作区为粒度保存 `WorkspaceState`：`cursor`、`log_floor`、
  `session_counter`、内容帧映射 `frames`、`RosterMeta`、按游标编号的变更 `log`，
  以及去重表 `(device_id, batch_id) -> ack_cursor`。
- `FileSyncStore` 把一个工作区持久化为单个 hex 文件名（工作区 ID 编码为十六进制字符串，
  避免路径穿越），写入采用**临时文件 + `rename` 原子替换**：先写同目录临时文件并 flush，
  再 rename 到目标路径，保证进程被杀时要么是旧文件、要么是新文件，无半写状态。
- 持久化编码复用 `mazha-sync::wire` 的小端/长度前缀助手（`put_u32`/`put_u64`/`put_bytes`/
  `put_string`/`WireReader`/`crc32`），但不套外层帧 magic（磁盘格式与线上帧各自独立）。
- push 提交在**暂存 `CrdtRoster`** 上先调用 `apply_updates_with_meta(frames, meta_changes)`
  做校验：坏帧/坏元数据/非法 ID 直接返回 `SyncErrorCode::BadFrame`/`BadMetadata`，
  且**游标、日志、去重表、持久化文件零改动**（与 §5.7.3 原子性边界一致）。
  校验成功才把变更一次性应用到服务端 roster、推进游标、记日志并落盘。
- 去重：重复 `(workspace_id, device_id, batch_id)` 返回首次 ack 的游标，`duplicate=true`，
  不推进游标、不二次追加日志、不重复落盘（对齐 §5.2 / §5.7.2）。
- 日志裁剪：`log_floor` 之前的历史不保留；`since < log_floor` 返回 `CursorTooOld`，
  `since > cursor` 返回 `SnapshotRequired`，由客户端 `SyncSession` 走快照回退。

#### 7.4.2 服务层（SyncService）与鉴权（TokenAuth）

- `SyncService` 组合 `SyncStore` + `TokenAuth`，提供与四个 `SyncTransport` 方法对应的
  纯逻辑处理函数（不依赖 HTTP）：`hello` / `pull` / `push` / `snapshot_request`。
  所有协议错误用它映射为带 `SyncErrorCode` 的响应，HTTP 层只做状态码/字节映射。
- `TokenAuth` 用 `Bearer <token>` 凭证做最小授权：token → 允许访问的 `WorkspaceId` 集合
  由启动配置注入；请求的 `SyncEnvelope.workspace_id` 必须在该集合内，否则返回
  `Unauthorized`。MVP 不做用户/组织权限，只做“设备 token + 工作区授权”（对齐 S-09）。
- `HelloAck.session_id` 由服务端 `session_counter` 递增生成（形如 `sess-{counter}`），
  只要求非空即可（fake server 同样如此）；不维护强会话表，避免多设备 hello 相互失效。
- `HelloAck.max_frame_bytes` 用 §5.8.1 的 `MAX_FRAME_BYTES` 默认提示。

#### 7.4.3 HTTP 路由与状态码

- 四个端点，全部 `POST`：
  `/sync/v0/hello`、`/sync/v0/pull`、`/sync/v0/push`、`/sync/v0/snapshot`。
- 请求 `Content-Type` 固定 `application/x-mazha-sync+wire`，body 是 §5.8 编码的完整帧。
  `batch_id` / 游标 / 身份只出现在编码信封内，**不放 HTTP header**；
  唯一额外的 header 是认证 `Authorization: Bearer <token>`。
- 响应 body 也是 wire 编码的 `SyncMessage`（请求成功）或
  `SyncMessage::Error(SyncErrorPayload)`（失败），Content-Type 同上。
- 错误码 → HTTP 状态码：`InvalidRequest/BadFrame/BadMetadata → 400`、
  `Unauthorized → 401`、`CursorTooOld/SnapshotRequired → 409`、`Internal → 500`。
- 客户端 `HttpTransport` 用同步 HTTP（与 D24 同步风格一致）：`ureq` 发字节请求，
  网络层错误映射 `TransportError::Disconnected`（outbox 保留），HTTP 错误体的 wire
  `SyncMessage::Error` 解析为 `TransportError::Server { code, message }`。
- **TLS/证书由反向代理或部署层终结**，代码不实现自签证书、不绑定 HTTPS；
  本地集成测试只绑定 `127.0.0.1:0` 的明文端口。
- **WebSocket 订阅（S4）后置**：本阶段只有请求-响应，无 `subscribe`，`SyncMessage`
  九种顺序与 `SyncTransport` 四方法不变。

---

## 8. 安全与隐私边界

MVP 最低要求（待产品/部署确认）：

1. **传输加密**：公网传输必须使用 TLS（HTTPS/WSS）。
2. **认证**：设备或用户必须有可撤销凭证；匿名开放写入不允许。
3. **授权**：至少按 `WorkspaceId` 隔离；一个凭证只能访问被授权工作区。
4. **审计**：记录设备、时间、批次 ID、错误码；不记录明文文档内容到普通日志。
5. **数据驻留**：自部署/云部署的数据位置、备份、保留策略需产品化说明。
6. **密钥管理**：本地 token/密钥存储使用平台安全存储（Keychain/Credential Manager 等），不明文写入普通设置文件。

待决策：

- 是否支持端到端加密（E2EE）；若支持，服务端只能存密文帧与密文元数据。
- 是否支持分享只读链接/协作写入。
- 日志与崩溃报告是否允许包含 DocId（路径可能含敏感信息）。

---

## 9. 与现有 crate 的依赖方向与改动点

### 9.1 依赖方向原则

- `mazha-md` 不反向依赖 `mazha-core`（保持不变）。
- 所有核心 crate 不依赖 GUI（保持不变）。
- 网络/HTTP/WebSocket/平台密钥链等实现不得进入 `mazha-core`、`mazha-crdt`、`mazha-store` 的纯逻辑核心。

### 9.2 建议改动（草案）

| crate | 建议改动 | 说明 |
|------|----------|------|
| `mazha-crdt` | 扩展名册元数据：墓碑表、批次/游标辅助类型、快照导入导出；可能新增 `RosterMeta`/`Tombstone`/`SyncBatch` 等纯数据类型。 | 保持纯 Rust、无 IO、无 GUI；继续产出/消费 Yjs v1 内容帧。 |
| `mazha-core` | 新增工作区级同步编排（建议新模块，如 `sync`）；定义本地文件事件、CRDT 变更、远端变更的应用策略；改进 `apply_remote` 的 UI 体验。 | 不能依赖 GUI；可暴露状态快照给桌面端订阅。 |
| `mazha-store` | 保持 `StorageProvider` 同步接口；可能补充元数据文件读写约定或原子写辅助。 | 不承担网络同步。 |
| 新 crate `mazha-sync`（建议，待确认） | 放协议模型、传输 trait、客户端会话状态机、重试/游标逻辑。 | 若引入 async，仅限该 crate 或其子模块；需评估与 D24 同步风格的边界。 |
| 新 crate `mazha-sync-server` 或 `services/sync`（待确认） | 最小服务端实现、存储、鉴权。 | 不进入 workspace 默认 GUI 构建路径。 |
| `platforms/desktop` | 显示同步状态、冲突提示、登录/工作区绑定 UI；后台任务调度。 | GUI 只消费核心状态，不实现协议规则。 |

### 9.3 推荐的 crate 边界

```text
mazha-md ──┐
mazha-store┼─> mazha-core ─> platforms/*
mazha-crdt ┘      ▲
                   │
               mazha-sync (协议/会话/transport trait，可选独立 crate)
                   ▲
                   │
          transport impls / server (独立部署或独立 crate)
```

> 关键约束：`mazha-sync` 若创建，必须不被 `mazha-md` 反向依赖；其是否依赖 `mazha-core` 需 ADR 明确，优先保持“协议层可独立测试”。

### 9.4 S3 已定 crate 边界与依赖方向（D100 / 0.1.123）

```text
                 mazha-sync (纯：协议/会话/wire 编码，无网络/async/GUI)
                          ▲                    ▲
                          │                    │
        mazha-sync-http (ureq,              mazha-sync-server (tiny_http,
        实现 SyncTransport,                  SyncStore/FileSyncStore,
        同步 HTTP client)                     SyncService/TokenAuth, 二进制)
                                              ▲ (dev-dep, 仅集成测试)
                                              │
                                        mazha-sync-http
```

- **核心 crate 保持纯净**：`mazha-crdt` / `mazha-core` / `mazha-store` / `mazha-sync`
  不引入网络、async runtime、GUI 依赖，也不新增外部依赖；`mazha-sync` 只依赖
  `mazha-crdt` + `thiserror`，wire 编码是唯一的 S3 增量。
- 网络与持久化只出现在两个新 crate：
  - `mazha-sync-http`：依赖 `mazha-sync` + `ureq`，实现 `SyncTransport` 四方法；绑定
    base URL / workspace / device / token（因 `snapshot()` 无参数）。不改动 S2 任何 API。
  - `mazha-sync-server`：依赖 `mazha-sync` + `tiny_http`，提供 `SyncStore`/`FileSyncStore`、
    `SyncService`、`TokenAuth` 与四个 POST 路由，并产出 `main` 二进制；
    测试用 dev-dependency 依赖 `mazha-sync-http` 跑本地真实 HTTP 全链路。
- 依赖方向严格向下：两个新 crate 依赖 `mazha-sync`，**不反向依赖**；`platforms/*` 本阶段
  零改动，`platforms/desktop/Cargo.lock` 不动、桌面门禁不重跑。
- 新依赖（`tiny_http` / `ureq` 及其传递依赖）只进入根 `Cargo.lock`，不污染桌面锁文件；
  同步风格沿用 D24，不引入 async runtime（与 `SyncTransport` 的四方法 `&mut self` 同步签名一致）。

### 9.5 S4 本地编排 crate 边界（已定，D101 / 0.1.124）

```text
mazha-store (StorageProvider / WatchEvent)
        ├─▶ mazha-sync-local (mapping / state / engine；同步阻塞、无 IO 抽象新增)
        │        ├─▶ mazha-sync (SyncSession / SyncTransport trait / wire 助手 / checkpoint)
        │        └─▶ mazha-crdt (CrdtDoc / CrdtRoster / RosterMeta)
        └─▶ mazha-core（既有桌面路径，S4 零改动）

调用方注入 &mut dyn SyncTransport ─▶ mazha-sync-http (ureq) 或 FakeTransport
                                      └─▶ mazha-sync-server / InMemorySyncServer
```

- `mazha-sync-local` **不依赖** `mazha-core`：本阶段只做“文件字节 ⇄ Yjs 文本”
  的编排，不需要 `Document`/`Workspace` 的块结构能力，因此没有 `core → sync-local →
  core` 的环；`mazha-core` 与 `platforms/*` 均零改动。
- 核心 crate（`mazha-crdt` / `mazha-core` / `mazha-store` / `mazha-sync` /
  `mazha-sync-local`）均不含网络、GUI、async runtime；`mazha-sync-local` 只在
  方法参数中持有 `&mut dyn SyncTransport`  trait object，不依赖 `ureq`/`tiny_http`。
- 反向依赖只允许向上：HTTP/server 依赖 `mazha-sync`；本地编排依赖 store/sync/crdt，
  不被 server/http 依赖。新增依赖仅 `thiserror`（与既有 crate 一致）及测试用
  `tempfile`；只进根 `Cargo.lock`，桌面独立锁不动。

### 9.6 S5a 桌面接入依赖边界（已定，D102 / 0.1.125）

```text
platforms/desktop（独立 workspace；只改本目录与桌面 Cargo.lock）
   ├─▶ mazha-sync-local（LocalSyncEngine；纯 Rust、同步阻塞）
   ├─▶ mazha-sync-http（HttpTransport；ureq 同步 HTTP，token 仅来自环境变量）
   ├─▶ mazha-sync（WorkspaceId / DeviceId 身份校验）
   └─▶ mazha-store（FileSystemProvider / WatchEvent；与 D92 共用同一 watcher）
```

- 根 `Cargo.toml` / 根 `Cargo.lock` **不新增依赖**：三个 crate 已是根成员，
  桌面只加 path 依赖并更新桌面独立锁。
- 核心 crate（`mazha-core` / `mazha-crdt` / `mazha-store` / `mazha-sync` /
  `mazha-sync-local`）仍无 GUI、网络实现与 async runtime；网络只在
  `mazha-sync-http` / `mazha-sync-server`，GUI 只在桌面。
- 桌面新增纯函数模块（配置解析、URL 校验、状态文案、横幅文案、事件转发过滤、
  状态栏拼接）不依赖 GPUI，沿用既有 `#[cfg(test)]` 风格测试；无真实网络/端口。

### 9.7 S5b 第一批依赖边界（已定，D103/D104 / 0.1.126）

```text
mazha-core（additive：remote_patch 模块；无新外部依赖）
   └─▶ mazha-crdt / mazha-store（既有依赖方向不变）

mazha-sync-local（additive：resolve 模块；仍不依赖 mazha-core）
   ├─▶ mazha-crdt（冲突副本帧解码、strictly higher op_id 复活）
   ├─▶ mazha-sync（checkpoint + restore_checkpoint 显式清空 outbox）
   └─▶ mazha-store（本地文件读写）

platforms/desktop（独立 workspace，只改本目录与桌面 Cargo.lock）
   └─▶ mazha-sync-local（ResolveOutcome.document_text 经 DocId 根路径校验后写盘/重载标签）
```

- 不新增根 workspace 依赖、不改根 `Cargo.toml`；仅桌面独立锁随新增调用更新。
- `mazha-sync` 的 `checkpoint` / `restore_checkpoint` 只按既有 additive API 使用，
  不新增清空 outbox 的方法；`mazha-crdt` 的 D94/D95/D98 语义零改动。

---

## 10. 迁移与兼容策略

### 10.1 本地已有 `.md` 工作区接入

1. 首次启用同步时扫描工作区 Markdown（可复用 `Workspace::list_markdown`/目录遍历能力）。
2. 为每个文档建立或加载 `CrdtDoc`；`DocId` 使用工作区相对路径。
3. 生成初始快照/更新帧并写入本地 CRDT 存储区（路径待定，如 `.mazha/crdt/...`）。
4. 若远端已有同 `WorkspaceId` 数据，执行首轮合并而非盲目覆盖。

### 10.2 兼容 D94

- `state_vectors/encode_diff/apply_updates` 的 `BTreeMap<DocId, Vec<u8>>` 内容帧格式应保持兼容或提供明确版本升级。
- 新增墓碑后，整册同步接口可能需要从“仅内容帧”扩展为“内容帧 + 元数据帧”；建议保留旧 API 作为兼容层或在 major design 切换时一次性迁移（待 ADR）。

### 10.3 文件监听与 CRDT

- D91/D92 已建立本地文件监听与自写回声抑制。同步引擎必须复用同一“写入来源”标识，避免：
  - 远端更新写盘 → watcher 误判为外部用户编辑 → 再次生成 CRDT 更新；
  - 本地保存 → watcher + CRDT 双重触发。
- 建议引入统一的变更来源枚举：`LocalUserEdit` / `LocalFileWatch` / `RemoteSync` / `SnapshotLoad`。

---

## 11. 测试策略与验收标准

### 11.1 测试层次

| 层次 | 目标 | 最低要求 |
|------|------|----------|
| 纯函数单测 | 墓碑合并、游标比较、批次去重、路径规范化 | 无 IO、无 GUI、确定性。 |
| `mazha-crdt` 单测 | 名册+墓碑同步、坏帧回滚、重复应用幂等、快照压缩 | 扩展现有 32 条测试风格。 |
| `mazha-core` 单测 | 本地编辑→CRDT→远端应用→Document 投影一致；冲突场景不丢数据 | 覆盖 CJK、删除、重命名、离线重放。 |
| 传输契约测试 | 使用假 transport/in-memory server 验证握手、拉取、推送、重试 | 不依赖真实网络。 |
| 集成测试 | 两设备+一服务端（本地进程）最终一致性 | 包含离线、断线重连、墓碑传播。 |
| 桌面手测 | 同步状态提示、冲突横幅、删除/重命名体验 | 仅在 UI 接入阶段要求（S5）。 |

### 11.2 MVP 验收标准

- 两个客户端离线编辑不同文档，联网后文档集合与内容一致。
- 两个客户端并发编辑同一文档，文本最终一致且无 panic/数据丢失。
- A 删除文档、B 离线编辑该文档后上线，结果符合已确认的删除/复活策略，且无静默丢失。
- 重复投递同一批次更新不会产生重复内容或错误状态。
- 坏帧/非法墓碑不会部分污染本地名册。
- 新设备通过快照引导可加入现有工作区。

### 11.3 S4 验收标准与测试分层（已定，D101）

- **纯函数**：`doc_id_from_path` 覆盖中文路径、`../` 越界、非 UTF-8、非 Markdown；
  `is_syncable_md` 覆盖 `.markdown`/大小写/隐藏目录；`local_text_to_frames` 经
  空文档/中文/CRLF/长文本往返；`remote_frames_to_local_writes` 只输出变化的
  写入与新墓碑删除；`should_ignore_echo` 验证相同字节抑制、改动放行、删除标记；
  `merge_watch_events` 验证同路径去重与 rename 旧删新建。
- **本地状态存储**：save → drop → load 后 frames/meta/游标/会话 ID/时钟/批次计数/
  outbox 全部保持；版本/magic/CRC 损坏返回错误且不 panic；原子写后无 `.tmp` 残留。
- **引擎端到端（FakeTransport + InMemorySyncServer，无真实网络/端口）**：
  1. A 工作区中文 `.md` → watch 事件 → `sync_once` 推送 → B 工作区
     `sync_once` 拉取 → 两端 `.md` 字节一致；
  2. 删除文件 → 墓碑传播 → 对端本地文件删除且不复活；
  3. 断线离线编辑 → 重连重放；注入 `CursorTooOld` + 非空 outbox 时
     `needs_confirmation` 可见、本地改动与 outbox 不被覆盖；
  4. drop 后从磁盘 `load`，游标/墓碑/时钟/批次号继续有效且不重复推送；
  5. 写回标记在非回声后失效（用户改回旧字节仍产生新批次）；断线时已暂存的
     编辑在重启后仍能重放 outbox。
- 每个用例独立临时目录（进程 ID + 测试名），结束清理；无外部网络、无线程定时器。

### 11.4 S5a 验收标准与测试分层（已定，D102）

- **纯函数**：server URL 只收 `http(s)://` 且有 host；workspace/device ID 回退与
  `WorkspaceId::new`/`DeviceId::new` 校验；`Settings` 同步字段默认/normalize
  与 5–3600s 夹取；状态文案覆盖未配置/空闲/同步中/成功/失败/需确认/冲突计数；
  冲突横幅列出 `DocId` 且不含“覆盖”；事件转发过滤隐藏文件与非 Markdown；
  状态栏同步提示与 D83 导出提示共存且失败用强调色。
- **线程边界**：`assert_send::<LocalSyncEngine>()` 编译期断言；同步线程只在
  `recv_timeout` 超时或 `SyncNow` 时调用 `sync_once`，`WatchEvents` 只标脏；
  失败后线程继续存活。
- **无真实网络**：全部测试用临时目录/纯函数/假数据，不启动服务端、不访问外网。
- **门禁**：根 workspace 747 → 保持（S5a 只改桌面，核心零改动）；桌面独立
  workspace 112 → 新增纯函数单测，`fmt/clippy/test/build` 四门禁全绿。

### 11.5 S5b 第一批验收标准与测试分层（已定，D103/D104）

- **核心纯 Rust**：`RemotePatch::from_texts` 的前后缀裁剪停在字符边界；`remap_offset`/
  `remap_range` 在插入/删除/替换/全替换/空文本下满足 §5.11.1 规则；`apply_remote`
  无变化时不改动文本/undo/redo；有变化时 undo 历史保留且偏移已重映射、redo 清空；
  `apply_remote_detailed` 返回的 patch 能重放回新文本。
- **本地编排纯 Rust**：三种冲突选择与快照确认的结果逐项锁死（正文/墓碑/冲突副本/
  outbox/确认标记）；坏帧或非法 DocId 零改动；`resolve_*` 不调用 `sync_once`、
  不触发网络；只有快照 `UseRemote` 清空 outbox，且只经 checkpoint/restore 实现，
  快照 `Merge` 必须保留 outbox。
- **桌面纯函数**：横幅 choices 文案、破坏性二次确认文案、`ResolveOutcome` 回写后
  状态刷新；既有 D102 横幅测试（重试同步、非破坏性）继续成立。
- **门禁**：根 `cargo fmt --all --check`、`cargo clippy --workspace --all-targets -- -D warnings`、
  `cargo test --workspace` 全绿；桌面 `cargo fmt --check`、`cargo clippy -- -D warnings`、
  `cargo test`、`cargo build` 全绿；测试计数变化记入 `HANDOFF.md`。

---

## 12. 开放问题与建议决策项

| ID | 问题 | 建议 | 优先级 |
|----|------|------|--------|
| S-01 | `DocId` 继续用路径还是引入稳定 UUID？ | MVP 用路径，重命名按删旧建新；后续再评估 UUID。 | 高 |
| S-02 | 删除与离线编辑冲突时谁赢？ | **已确认（D95）**：默认墓碑 wins，但必须保留冲突副本或可恢复记录；禁止静默丢数据。S1 契约见 §4.5.4。 | 高（已决） |
| S-03 | 同路径“删除后重建”是否允许复活？ | **已确认（D95/D98）**：允许，使用更高 `op_id` 的 upsert 覆盖墓碑；全序为 `(clock, device)`，`op_id` 相同时墓碑胜出。 | 高（已决） |
| S-04 | 墓碑何时 GC？ | MVP 不 GC；引入“所有已知设备游标越过墓碑”后再 GC。 | 中 |
| S-05 | 最小服务端形态？ | **已定（D100）**：`mazha-sync-server`，带持久化的同步中继（temp+rename 落盘、批次去重、服务端 CRDT 校验），不做 Markdown 语义合并。 | 高（已决） |
| S-06 | 传输 MVP 选型？ | **已定（D100）**：同步 HTTP（ureq client + tiny_http server），请求-响应优先，WebSocket 订阅后置。 | 中（已决） |
| S-07 | `mazha-sync` 是否独立 crate？ | **已定（D99）**：独立 crate，协议/会话/transport trait/假服务端都在此；核心 crate 保持纯净，无网络/GUI/async。 | 高（已决） |
| S-08 | `apply_remote` 整体重建导致 undo/选区丢失如何处理？ | **已实现（D103，S5b 第一批）**：`apply_remote` 签名/返回值不变，内部生成 `RemotePatch` 并增量应用；远端变更不进 undo，已有 undo 历史偏移重映射、redo 清空，光标/选区用同一个纯函数重映射。 | 中（已决） |
| S-09 | 认证模型？ | **已定（D100）**：MVP 先设备 token + 工作区授权（`Authorization: Bearer`，工作区隔离），团队/组织权限后置。 | 高（已决） |
| S-10 | 是否需要 E2EE？ | MVP 不承诺；协议预留密文载荷字段。 | 中 |
| S-11 | 快照与增量日志的本地存储格式？ | **已定（D100）**：先复用 wire 小端/长度前缀助手做单文件 hex 持久化（temp+rename），后续引入 append-only log + compaction。 | 中（已决） |
| S-12 | 结构性并发（表格/块）冲突提示？ | 继续沿用 §6.5 `[待确认]`；先记录冲突事件，不自动重写语义结构。S4 只在 `status().conflict_docs` 暴露墓碑冲突副本所在的 `DocId`。 | 低（S4 仅暴露 DocId） |
| S-13 | 桌面何时接入本地引擎与冲突 UI？ | **已实现（D102，S5a；D104，S5b 第一批）**：S5a 注入 `HttpTransport`、周期/手动同步与状态/冲突提示；S5b 第一批补上“保留本地/采用远端/合并”三选一与破坏性二次确认，所有覆盖均需用户显式确认，绝不静默丢弃。 | 高（已决） |

建议 ADR 拆分：

- `ADR-00x-sync-doc-identity.md`：DocId/重命名/墓碑语义
- `ADR-00x-sync-transport-and-server.md`：传输、最小服务端、认证边界
- `ADR-00x-sync-local-integration.md`：本地文件、CRDT、watcher、Document 重建策略

---

## 13. 实施拆分建议

### Phase S0：文档与决策（本次）

- 评审本草案，确认 S-01~S-12 中的高优先级项。
- 在 `SPEC.md` §6.5 增加指向本文的索引；新增 D95+ 决策记录。

### Phase S1：纯模型扩展（`mazha-crdt`）

- **契约已定（§4.5 / D98），实现随 0.1.121 落地**：`OpId`、`Tombstone`、`RosterMeta`、`MetaChange` 纯数据类型与全序/幂等规则。
- 名册新增墓碑化删除、只读查询、元数据合并与内容+元数据原子应用 API；D94 内容接口零改动。
- 单测覆盖 D94 已知复活场景、墓碑 wins 保底副本、高 `op_id` 复活、坏元数据零改动与 CJK 路径。

### Phase S2：同步会话层（新 crate `mazha-sync`，D99，0.1.122 已落地）

- 协议消息/信封/ID/游标（`protocol.rs`）、请求-响应 transport trait（`transport.rs`）、
  in-memory 服务端与假 transport（`fake.rs`）、会话状态机（`session.rs`）。
- 契约见 §5.7 / §6.4：`BatchId` 确定性、服务端按三元组去重、`SyncCursor` 只由服务端解释、
  `PullResponse` 经 `apply_updates_with_meta` 一次原子应用、快照引导受未确认批次约束。
- 不接真实网络也能跑通“两端 + 假服务端”的最终一致性、离线重放、断线保留 outbox；
  字节编码、真实传输与持久化留 S3。

### Phase S3：最小服务端与真实传输（D100，0.1.123 已落地）

- `mazha-sync/src/wire.rs`：§5.8 确定性字节编码（外层帧 + body 小端/长度前缀 +
  CRC32），九种消息 roundtrip、golden vector、拒绝测试；`mazha-sync` 仍纯 Rust。
- `mazha-sync-server`：`SyncStore`/`FileSyncStore`（temp+rename、hex 文件名）、
  `SyncService`、`TokenAuth`、tiny_http 四个 POST 路由与二进制入口。
- `mazha-sync-http`：`HttpTransport` 实现 `SyncTransport` 四方法，绑定
  base URL / workspace / device / token，同步 ureq；网络错误映射 `Disconnected`，
  wire 错误体映射 `TransportError::Server`。
- 测试覆盖 wire 编解码、store 重启/去重/裁剪、service 鉴权/隔离/墓碑不复活、
  HTTP 状态码纯映射、本地真实 HTTP 集成与 CJK 全链路；新依赖只进根 `Cargo.lock`。

### Phase S4：本地工作区同步编排（D101，0.1.124 已落地）

- 新 crate `mazha-sync-local`：`mapping.rs` 纯函数（路径/DocId、文本⇄帧、回声、事件合并）、
  `state.rs`（版本化字节 + temp/rename 原子写 + 坏文件拒绝）与 `engine.rs`
  （标脏 → stage → `sync_once` → 写回 → 落盘 + 状态暴露）。
- `mazha-sync` 只做 additive checkpoint API（`PendingBatch::new/into_parts`、
  `SessionCheckpoint`、`checkpoint`/`restore_checkpoint`），D99 语义零改动。
- 测试见 §11.3；`platforms/*` 与 `mazha-core` 零改动，桌面 UI 留 S5。

### Phase S5a：桌面接入第一批（D102，0.1.125 已落地）

- 桌面注入 `HttpTransport`（token 仅环境变量 `MAZHA_SYNC_TOKEN`），
  专用同步线程 + `mpsc` 命令/状态通道，`recv_timeout(interval)` 周期同步与
  “立即同步”命令；复用 D92 唯一 watcher 转发事件只标脏。
- 状态栏显示未配置/同步中/成功游标/离线失败/需确认/冲突 N，与 D83 导出状态拼接；
  冲突横幅列 `DocId` 并说明数据未静默删除；“重试同步”只发 `SyncNow`。
- 只改 `platforms/desktop/*` 与桌面 `Cargo.lock`；核心 crate 与 D95-D101 语义零改动。

### Phase S5b 第一批：增量 patch 与显式合并/覆盖（D103/D104，0.1.126 已落地）

- `mazha-core` additive `RemotePatch` / `apply_remote_detailed` / `Document::apply_remote_patch`：
  S-08 落地，远端变更不进 undo，undo 历史重映射、redo 清空，选区由纯函数重映射。
- `mazha-sync-local` additive `resolve_conflict` / `resolve_snapshot_confirmation`：
  三选一精确语义、坏帧零改动、不自动触发网络；只用既有 checkpoint/restore API 清 outbox。
- 桌面横幅加三选一按钮与破坏性二次确认，复用 `SyncHandle`/`SyncReply`/D92 样式；
  D95–D102 签名与语义零改动，根依赖不变。

### Phase S5b 第二批：产品化（后置）

- WebSocket 订阅、快照压缩、墓碑 GC、平台安全存储（token 不再从环境变量读取）。
- 已确认状态持久化（避免重启后横幅重现）与确认后的自动重试策略。

---

## 14. 风险清单

| 风险 | 影响 | 缓解 |
|------|------|------|
| 删除墓碑语义不清 | 文档复活或静默丢数据 | 先定 S-02/S-03，再写实现。 |
| DocId 使用路径导致重命名复杂 | 历史断裂、重复文档 | MVP 明确“删旧建新”；保留未来 UUID 迁移口。 |
| Document 整体重建 | 丢 undo/选区，编辑体验差 | MVP 明示限制；后续增量 patch。 |
| watcher 与同步写盘回环 | 重复更新、性能抖动、假冲突 | 统一变更来源与回声抑制。 |
| 服务端过早产品化 | 范围膨胀 | MVP 只做带持久化中继。 |
| 安全边界不清 | 数据泄露或越权 | TLS + 工作区授权 + 最小日志作为上线门槛。 |

---

## 15. 本草案建议的 SPEC 回填点（S0/S1/S2/S3 已回填）

- `SPEC.md` §5 新增 `mazha-sync` 行（S2，D99）并把误挂在 `mazha-crdt` 名下的 `SyncMessage` 归位；§6.5 增 S2 会话层段落；§14 D99；§15 0.1.122。
- `SPEC.md` §5 `mazha-crdt`：补充“墓碑/同步元数据（S1 已实现）”。
- `SPEC.md` §5 新增 `mazha-sync-server` / `mazha-sync-http` 行与依赖图（S3，D100）；§6.5 增 S3 段；§14 更新 D96 状态并新增 D100；§15 0.1.123。
- `SPEC.md` §6.5：增加指向 `docs/SYNC-SPEC.md` 的索引，保留“核心只产出/消费标准 Yjs v1 帧”约束。
- `SPEC.md` §10 Phase 5：细化为 S0-S5 子阶段。
- `SPEC.md` §14：新增 D95（DocId/墓碑策略）、D96（最小服务端/传输）、D97（本地集成策略）等决策。
- `SPEC.md` §15：记录草案创建与后续实现版本。
- **S4 回填（0.1.124）**：本文 §5.9（本地编排契约）/§9.5（crate 边界）/§11.3（验收）、
  §12 S-08/S-13；`SPEC.md` §5 增 `mazha-sync-local` 行与依赖图、§6.5 增 S4 段、
  §14 新增 D101；`HANDOFF.md` 同步版本/状态/测试数/下一步 S5。
- **S5a 回填（0.1.125）**：本文 §5.10（桌面线程/配置/状态契约）/§9.6（桌面依赖边界）/
  §11.4（验收）/§12 S-13/§13 S5a/S5b 拆分；`SPEC.md` §6.5 增 S5a 段、
  §14 新增 D102、§15 记 0.1.125；`HANDOFF.md` 同步版本/平台状态/测试数/下一步 S5b。
- **S5b 第一批回填（0.1.126）**：本文 §5.11（增量 patch/三选一/桌面入口）/§9.7（依赖边界）/
  §11.5（验收）/§12 S-08/S-13/§13 S5b 第一批与第二批拆分；`SPEC.md` §5 crate 表补
  `mazha-core`/`mazha-sync-local` 的 additive 能力、§6.5 增 S5b 第一批段、
  §14 新增 D103/D104、§15 记 0.1.126；`HANDOFF.md` 同步版本/状态/测试数/下一步 S5b 第二批。
