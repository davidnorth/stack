//! TIFF decoding/encoding and metadata handling.
//!
//! This is the only module allowed to know TIFF tags.

pub mod exif;
pub mod metadata;
pub mod tiff_read;
pub mod tiff_write;
