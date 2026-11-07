//! The pictures a message carries: fetching a blob once, keeping it, and the chip a pasted file
//! becomes on its way out. P0 defines the seam - every call answers *nothing yet* - and P4 fills the
//! cache (`GetBlob` parts), the drawing side and the upload (`PutBlob` parts).

#![allow(dead_code)] // The seam is P0's and its readers are P4's.

use crate::app::rooms::Attach;
use crate::app::*;

impl App {
    /// The image a blob is already drawn from: `(image id, width, height)`. `None` is *not fetched
    /// yet*, which draws the one-row fallback rather than a hole.
    pub(crate) fn room_picture(&self, _blob: &str) -> Option<(u32, u32, u32)> {
        None
    }

    /// Ask for every blob of these a message draws and the cache has not seen.
    pub(crate) fn room_pictures_wanted(&mut self, _blobs: &[&Attach]) {}

    /// A pasted or dropped picture: the chip that goes in the composer, and the `PutBlob` parts it
    /// will be uploaded as. P0 uploads nothing and shows no chip.
    pub(crate) fn room_upload(&mut self, _png: Vec<u8>, _name: &str) -> String {
        String::new()
    }
}
