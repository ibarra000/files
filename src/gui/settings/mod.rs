//! The settings window, drawn.
//!
//! The words are in [`crate::view::settings`], which knows nothing about
//! rectangles; this is the half that knows nothing about wording. The split
//! is the one [`crate::view`]'s module note argues for, and it is why the
//! house-style test can check every line on the form without a font.
//!
//! # A card per setting, and no shading
//!
//! The panel next door is soft-UI: a surface is the colour of its ground and
//! is legible only by the shading at its edges, which `theme::raise` and
//! `theme::press` paint. This window is not, and the difference is deliberate
//! rather than an oversight.
//!
//! A form is a list of small tiles, one per setting, and `raise` costs two
//! feathered shadow shapes plus a fill for every one of them. A page here
//! holds a dozen or more, and the shell asks for a frame every time one is
//! open, so the shading would be several thousand feathered shapes a second
//! to say something a flat fill one step off the ground says for nothing.
//!
//! The panel keeps its shading, where two surfaces earn it.

pub mod measure;
pub mod widgets;
