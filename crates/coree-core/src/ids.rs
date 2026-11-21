// SPDX-License-Identifier: MIT OR Apache-2.0

//! Strongly typed identifiers shared across the core document model.

use serde::{Deserialize, Serialize};
use std::fmt;
use uuid::Uuid;

/// Unique identifier for a scene object inside a [`crate::document::Document`].
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
pub struct ObjectId(pub u64);

impl fmt::Display for ObjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "obj#{}", self.0)
    }
}

/// Unique identifier for a page inside a [`crate::document::Document`].
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
pub struct PageId(pub u64);

impl fmt::Display for PageId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "page#{}", self.0)
    }
}

/// Unique identifier for a layer inside a page.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
pub struct LayerId(pub u64);

impl fmt::Display for LayerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "layer#{}", self.0)
    }
}

/// Unique identifier for an embedded raster image asset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ImageAssetId(pub Uuid);

impl ImageAssetId {
    /// Generates a new random v4 asset identifier.
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for ImageAssetId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for ImageAssetId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "img:{}", self.0)
    }
}

/// Unique identifier for a reusable vector symbol definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SymbolId(pub Uuid);

impl SymbolId {
    /// Generates a new random v4 symbol identifier.
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for SymbolId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for SymbolId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "sym:{}", self.0)
    }
}

/// Unique identifier for a reusable object style definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ObjectStyleId(pub Uuid);

impl ObjectStyleId {
    /// Generates a new random v4 object style identifier.
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for ObjectStyleId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for ObjectStyleId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "style:{}", self.0)
    }
}
