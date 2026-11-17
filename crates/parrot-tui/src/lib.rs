//! Labelled terminal trees, profiles, locators, waits and actions over parrot-term screens.
//!
//! Import [`ScreenExt`] and [`SessionExt`] to query screens and automate sessions.

pub mod automate;
pub mod locator;
pub mod measure;
pub mod profile;
pub mod tree;

pub use automate::{
    ActionReport, AutomateOptions, Automation, Query, SessionExt, Tracker, TuiError, TuiErrorKind, TuiEvent,
    TuiSnapshot,
};
pub use locator::{BoxRegion, Locator, Region, ScreenExt, TextMatch};
pub use profile::{Profile, ProfileError, Tagger};
pub use tree::{Node, NodeState, Role, Surface, SurfaceKind, Tree};
