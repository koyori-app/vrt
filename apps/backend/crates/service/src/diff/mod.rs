//! 画像差分エンジン。DB にも HTTP にも依存しない純粋な計算モジュール。

pub mod pixelmatch;

pub use pixelmatch::{DiffOptions, DiffResult, diff_images};
