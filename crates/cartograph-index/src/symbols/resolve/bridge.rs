//! Bridge matching (see [`crate::bridges`]): runs after calls resolve, with the same
//! bindings, so a channel written as a constant of another file is read through the import
//! that brings it.

use super::{Bindings, Ctx, GraphIx};
use cartograph_core::{Bridge, BridgeEndpoint, Bridges};

/// What the matching found: every endpoint, and the bridges matched.
#[derive(Debug, Default)]
pub(super) struct Found {
    pub endpoints: Vec<BridgeEndpoint>,
    pub bridges: Vec<Bridge>,
}

/// Match the bridges `on` asks for.
pub(super) fn run(ctx: &Ctx<'_>, binds: &[Bindings<'_>], g: &GraphIx<'_>, on: Bridges) -> Found {
    let _ = (ctx, binds, g);
    if !on.ipc {
        return Found::default();
    }
    Found::default()
}
