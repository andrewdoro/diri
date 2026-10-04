// Adapted from Ely GPUI Components (MIT OR Apache-2.0), https://github.com/ZacharyZhang-NY/Ely-GPUI-Components

//! The code editor behind the Files surface.
//!
//! A virtualized, multi-cursor text editor for one file at a time:
//! lexical highlighting in the terminal theme's colors, bracket pairs and
//! rainbow depth, bracket- or indent-based folding, indent guides, rulers,
//! sticky block headers, a minimap, find and replace, undo grouped by
//! typing bursts, IME, and atomic saves that refuse to overwrite a file
//! changed on disk. Hover cards, go-to-definition, completion, signature
//! help, and code lenses draw on the workspace symbol index rather than a
//! language server.
//!
//! Pure logic lives in small modules (`buffer`, `cursor`, `history`,
//! `syntax`, `find`, `search`, `intel`, `palette`) and is unit-tested;
//! `editor` holds state and operations, `render` paints, `input` wires keys
//! and the platform input method.

mod buffer;
mod cursor;
mod editor;
pub(crate) mod find;
mod history;
mod input;
pub(crate) mod intel;
pub(crate) mod palette;
mod render;
pub(crate) mod search;
pub(crate) mod syntax;

#[cfg(test)]
pub(crate) use editor::Completion;
pub(crate) use editor::{CodeEditor, Document, EditorEvent};
pub(crate) use input::key_bindings;
pub(crate) use palette::EditorPalette;

#[cfg(test)]
#[allow(clippy::single_range_in_vec_init)]
mod tests;
