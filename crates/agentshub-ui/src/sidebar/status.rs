//! The sidebar's chat glyph that pops.

use super::*;

/// A chat's status glyph whose circle swells and settles back when its agent
/// has just come to need the user, `since` then.
pub(super) fn popping_dot(tone: Tone, since: Option<Instant>, cx: &App) -> Div {
    // Named after when it popped, so a later pop starts again.
    agentshub_ui_kit::status::status_glyph(tone, false, since.map(name_of), cx)
}

/// An animation's name that changes with the moment it starts at.
pub(super) fn name_of(at: Instant) -> u64 {
    let mut hasher = std::hash::DefaultHasher::new();
    std::hash::Hash::hash(&at, &mut hasher);
    std::hash::Hasher::finish(&hasher)
}
