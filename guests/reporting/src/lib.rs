//! # kanbrick-guest-reporting
//!
//! WASM guest scaffold for the **reporting** module. Phase 5 implements the real
//! logic and compiles this to `wasm32-wasip1`. For Phase 0 it is an empty
//! scaffold that builds on the host target.

/// Guest module name, surfaced through the host ABI in Phase 3+.
pub const NAME: &str = "reporting";

#[cfg(test)]
mod tests {
    #[test]
    fn name_is_set() {
        assert_eq!(super::NAME, "reporting");
    }
}
