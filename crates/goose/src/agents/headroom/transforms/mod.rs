//! Compression transforms — core compressor modules.
//!
//! This module re-exports the main public APIs from headroom's transform
//! pipeline. Each transform is a standalone compressor or classifier that
//! can be used independently or composed into a full pipeline.

pub use crate::agents::headroom::adaptive_sizer;

pub mod anchor_selector;
pub mod content_detector;
pub mod detection;
pub mod log_compressor;
pub mod safety;
pub mod search_compressor;
pub mod tag_protector;
pub mod unidiff_detector;

pub use anchor_selector::{AnchorConfig, AnchorSelector};
pub use content_detector::{
    detect_content_type, is_json_array_of_dicts, ContentType, DetectionResult,
};
pub use detection::detect;
pub use log_compressor::{
    LogCompressionResult, LogCompressor, LogCompressorConfig, LogFormat, LogLevel,
};
pub use safety::{tool_pair_indices, ToolPair};
pub use search_compressor::{SearchCompressionResult, SearchCompressor, SearchCompressorConfig};
pub use tag_protector::{protect_tags, restore_tags, ProtectStats};
pub use unidiff_detector::{detect_diff, is_diff};
