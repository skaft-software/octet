//! Identity and liveness: which image IDs exist, and which of them may still be
//! addressed.
//!
//! The registry allocates IDs monotonically and never reuses a retired value.
//! That is not an optimisation, it is the whole reason the type exists: a
//! terminal delete is asynchronous and a renderer may emit one long after it
//! decided to replace an image, so a reused ID would let a delayed delete
//! remove a *newer* placement. Monotonic allocation makes a stale delete
//! harmless by construction, and the live set bounds how many placements a
//! single frame may carry.
//!
//! [`ImageAction`] is the ordered instruction a caller emits, and it lives here
//! rather than with the encoder because the action is a statement about registry
//! state — what the caller believes is on screen — and not about any protocol's
//! wire syntax.

use std::collections::BTreeSet;
use std::num::NonZeroU32;

use super::error::ImageError;
use super::limits::HARD_MAX_LIVE_IMAGES;

/// A stable nonzero terminal image ID.
///
/// [`ImageRegistry`] allocates IDs monotonically and never reuses a retired
/// value, which prevents a delayed delete from targeting a newer image.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ImageId(NonZeroU32);

impl ImageId {
    /// Validate a caller-supplied nonzero image ID.
    pub fn new(value: u32) -> Result<Self, ImageError> {
        NonZeroU32::new(value)
            .map(Self)
            .ok_or(ImageError::InvalidImageId)
    }

    /// Numeric protocol ID.
    pub const fn get(self) -> u32 {
        self.0.get()
    }
}

/// A targetable image lifecycle operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageAction {
    /// Display a newly allocated image ID.
    Place(ImageId),
    /// Replace an existing live image ID.
    Replace(ImageId),
    /// Delete an existing live image ID.
    Delete(ImageId),
}

impl ImageAction {
    /// The target image ID.
    pub const fn id(self) -> ImageId {
        match self {
            Self::Place(id) | Self::Replace(id) | Self::Delete(id) => id,
        }
    }
}

/// Monotonic, stale-delete-resistant image ID bookkeeping.
///
/// The registry tracks at most [`HARD_MAX_LIVE_IMAGES`] logical IDs at once.
/// Callers should encode and emit the returned action in order; a terminal write
/// failure may require a renderer to redraw its surface, but cannot cause a
/// future allocation to reuse an ID.
#[derive(Debug)]
pub struct ImageRegistry {
    pub(super) next: u32,
    pub(super) live: BTreeSet<ImageId>,
}

impl Default for ImageRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ImageRegistry {
    /// Construct an empty registry whose first placed ID is one.
    pub const fn new() -> Self {
        Self {
            next: 1,
            live: BTreeSet::new(),
        }
    }

    /// Allocate and mark a new stable image ID live.
    pub fn place(&mut self) -> Result<ImageAction, ImageError> {
        if self.next == 0 {
            return Err(ImageError::ImageIdExhausted);
        }
        if self.live.len() >= HARD_MAX_LIVE_IMAGES {
            return Err(ImageError::TooManyLiveImages);
        }
        let id = ImageId::new(self.next)?;
        // Zero is an exhaustion sentinel, never a reusable ID.
        self.next = self.next.checked_add(1).unwrap_or(0);
        self.live.insert(id);
        Ok(ImageAction::Place(id))
    }

    /// Build a replacement action only for a currently live ID.
    pub fn replace(&self, id: ImageId) -> Result<ImageAction, ImageError> {
        self.live
            .contains(&id)
            .then_some(ImageAction::Replace(id))
            .ok_or(ImageError::StaleImageId)
    }

    /// Retire an ID and build its delete action. Retired values never re-enter
    /// the allocator, so a delayed protocol delete cannot target a later image.
    pub fn delete(&mut self, id: ImageId) -> Result<ImageAction, ImageError> {
        self.live
            .remove(&id)
            .then_some(ImageAction::Delete(id))
            .ok_or(ImageError::StaleImageId)
    }

    /// Whether an ID is still logically live.
    pub fn is_live(&self, id: ImageId) -> bool {
        self.live.contains(&id)
    }
}
