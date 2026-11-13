//! 集合类型。
//!
//! 转发 `alloc::collections` 里的容器，另外补一个不需要 std 的 [`HashMap`]。

pub use alloc::collections::*;

mod hashmap;

pub use self::hashmap::{FnvBuildHasher, HashMap};
