//! # mazha-ffi
//!
//! 移动端绑定层：`mazha-core` 通过 **UniFFI** 暴露给 Swift（iOS）与
//! Kotlin（Android）。接口契约见 `SPEC.md` §6.4。
//!
//! HarmonyOS 单独走 OHOS NAPI，不在此 crate 内。

#[cfg(test)]
mod tests {
    #[test]
    fn smoke() {}
}
