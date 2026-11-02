//! 真实依赖适配器（T30）：`eg-rubric` / `eg-inference` / `eg-platform`。
//!
//! 本层是 grade 唯一接触具体 crate 类型的地方；`pipeline` 只依赖 `ports` trait。
//! 对齐（align）尚未交付：见 [`crate::align`] 的 fixture + TODO。

pub mod align;
pub mod inference;
pub mod platform;
pub mod rubric;
