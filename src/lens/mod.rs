//! Hover-to-select OCR: making text that is only pixels selectable anyway.
//!
//! A second binary over the same library. Hold a modifier, and the cursor
//! becomes an I-beam wherever there is text under it - in a scanned drawing, a
//! screenshot pasted into an email, a dialog that will not let go of its own
//! contents. Drag, and the text can be copied or searched. The search goes to
//! this program's own share index, which is why the feature lives here rather
//! than in a project of its own: reading a job code off a drawing and finding
//! its files without retyping it is the whole point, and the second half is
//! already built.
//!
//! # The order things were built in, and why it is visible in the layout
//!
//! Hit-testing, drag selection and cursor swapping are the hard parts, and none
//! of them need a single recognised pixel. So [`overlay::hit`] came first and
//! was finished against [`fixture`], a hardcoded list of rectangles and strings,
//! before any capture existed. It stays in the tree: it is the only way to test
//! selection on a machine with no screen, and every test in `overlay::hit` runs
//! anywhere.
//!
//! The same split runs through the rest. [`px`], [`overlay::hit`] - and, as they
//! land, the upscaler and the recognition ordering - are pure arithmetic with no
//! Windows in them at all, which is the arrangement [`crate::hotkey::geometry`]
//! already makes against [`crate::hotkey::win`] and for the same reason: the
//! calls that cannot be exercised off Windows are kept away from the logic that
//! can.

pub mod fixture;
pub mod log;
pub mod modifier;
pub mod overlay;
pub mod px;
