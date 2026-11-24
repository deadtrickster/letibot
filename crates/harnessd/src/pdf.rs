//! rano's PDF renderers, registered with the crate that keeps a document as bytes.
//!
//! A row keeps a PDF inline, so nothing above this crate can hand a renderer a path: the transcript
//! crate writes the temporary file and calls these. The implementation is `letibot_plugins_rano`'s,
//! which is also where the head gets it - one spelling of "rano behind the trait" for the two
//! processes that render a document, and this module is the daemon's door to it.

pub use letibot_plugins_rano::pdf::install;
