//! Applying the app's motion setting.

use super::*;

/// Animates as the motion setting says, or as the desktop does while it
/// says to follow the system.
pub fn apply_motion(cx: &mut App) {
    let desktop = agentshub_platform::motion::reduced(cx);
    let reduce = match cx.try_global::<Settings>().map(|settings| settings.motion) {
        Some(Motion::Full) => false,
        Some(Motion::Reduced) => true,
        Some(Motion::System) | None => desktop,
    };
    cx.set_reduce_motion(reduce);
}
