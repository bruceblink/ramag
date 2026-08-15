//! 对象数据面操作。

mod metadata_preview;
mod transfer;

pub use metadata_preview::{read_text_preview, stat};
pub use transfer::{download, upload};
