// SPDX-License-Identifier: MIT OR Apache-2.0

//! Isolated brand identity and clean-room trademark metadata for `coree`.
//!
//! All user-facing application identifiers are centralized here so that product
//! metadata stays completely separate from geometry and document logic.

use serde::{Deserialize, Serialize};

/// Canonical machine-readable application identifier.
pub const APP_NAME: &str = "coree";

/// Canonical user-facing display name.
pub const DISPLAY_NAME: &str = "coree";

/// Native project file extension (without leading dot).
pub const NATIVE_EXT: &str = "coree";

/// SPDX license identifier.
pub const LICENSE: &str = "MIT OR Apache-2.0";

/// Public source repository URL.
pub const REPO_URL: &str = "https://github.com/ahrrfy/coree";

/// Crate semantic version compiled into the binary.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Clean-room independence and trademark disclaimer notice.
pub const TRADEMARK_NOTICE: &str = "\
coree is an independent, clean-room open-source vector graphics and print \
production suite licensed under MIT OR Apache-2.0. It is NOT affiliated with, endorsed \
by, sponsored by, or connected in any way to Corel Corporation, Alludo, or \
Pantone LLC. Corel, CorelDRAW, Corel PHOTO-PAINT, and PowerTRACE are \
trademarks or registered trademarks of Corel Corporation and/or Alludo. \
PANTONE is a trademark of Pantone LLC. No proprietary source code, assets, \
or licensed color books are included. References to file formats such as \
.cdr, .cdt, .cmx, .cpt, .svg, .pdf, .eps, .dxf, and .plt are strictly \
nominative for interoperability.";

/// Snapshot of compile-time brand metadata suitable for serialization or UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrandInfo {
    pub app_name: String,
    pub display_name: String,
    pub native_ext: String,
    pub license: String,
    pub repo_url: String,
    pub version: String,
    pub trademark_notice: String,
}

impl BrandInfo {
    /// Returns the active brand metadata for this build.
    #[must_use]
    pub fn current() -> Self {
        Self {
            app_name: APP_NAME.to_owned(),
            display_name: DISPLAY_NAME.to_owned(),
            native_ext: NATIVE_EXT.to_owned(),
            license: LICENSE.to_owned(),
            repo_url: REPO_URL.to_owned(),
            version: VERSION.to_owned(),
            trademark_notice: TRADEMARK_NOTICE.to_owned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn brand_constants_match_spec() {
        assert_eq!(APP_NAME, "coree");
        assert_eq!(DISPLAY_NAME, "coree");
        assert_eq!(NATIVE_EXT, "coree");
        assert_eq!(LICENSE, "MIT OR Apache-2.0");
        assert_eq!(REPO_URL, "https://github.com/ahrrfy/coree");
        assert!(TRADEMARK_NOTICE.contains("clean-room"));
        assert!(TRADEMARK_NOTICE.contains("NOT affiliated"));

        let info = BrandInfo::current();
        assert_eq!(info.app_name, APP_NAME);
        assert_eq!(info.native_ext, NATIVE_EXT);
    }
}
