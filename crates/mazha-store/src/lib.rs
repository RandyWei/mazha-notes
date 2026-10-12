//! # mazha-store
//!
//! 存储抽象层：`StorageProvider` trait（SPEC §6.2）与默认的本地文件系统实现。
//! 云端 / 自部署 / 用户自定义存储均以 Provider 形式接入。

/// 存储 Provider 能力描述（Phase 5 完善）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StorageCaps {
    /// 是否支持流式读写。
    pub streaming: bool,
    /// 是否支持版本历史。
    pub versioning: bool,
    /// 是否支持协作同步。
    pub collaboration: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_caps_are_off() {
        let caps = StorageCaps::default();
        assert!(!caps.streaming && !caps.versioning && !caps.collaboration);
    }
}
