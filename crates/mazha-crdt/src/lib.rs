//! # mazha-crdt
//!
//! 基于 [yrs](https://crates.io/crates/yrs) 的多端同步层：在 `Y.Text` 中保存
//! Markdown 源文本，产出/消费标准 Yjs 更新帧，支持状态向量与合并。
//! 见 `SPEC.md` §6.5。

/// 状态向量类型别名占位（Phase 5 由 yrs 提供具体类型）。
pub type StateVector = Vec<u8>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_vector_is_bytes() {
        let sv: StateVector = vec![];
        assert!(sv.is_empty());
    }
}
