//! Native state for Nix's `@nix` internal-JSON build protocol.
//!
//! This crate is a clean-room behavioral implementation. Its wire model is
//! derived from Nix's `logging.hh` and observed Nix streams. nom 2.1.8 was
//! used only as a black-box/outcome reference and to identify behaviors worth
//! preserving; no EUPL-licensed implementation code was translated.
//!
//! The core has no Mandala dependency. [`msg`] parses the wire protocol,
//! [`drv`] reads the ATerm derivation graph, and [`forest`] folds both into
//! versioned snapshots. Renderers are optional edge features.

pub mod drv;
pub mod duration;
pub mod forest;
pub mod msg;
pub mod sort;

#[cfg(feature = "plain")]
pub mod plain;
#[cfg(feature = "ratatui")]
pub mod widget;

pub use drv::{Derivation, DrvReader, FsDrvReader, parse_derivation};
pub use duration::{DURATION_CACHE_RELATIVE_PATH, DurationCache};
pub use forest::{
    BuildForest, DerivationNode, DerivationStatus, FeedOutcome, ForestCounts, ForestSnapshot,
    Transfer,
};
pub use msg::{ActivityType, NixMessage, ResultType, parse_nix_line};
#[cfg(feature = "ratatui")]
pub use widget::{ForestStyles, ForestWidget};
