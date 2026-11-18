//! Small formatting helpers shared by the app's views.

use crate::store;

/// How long ago something happened, given in seconds since the Unix epoch,
/// in its largest whole unit.
pub fn age(at: i64) -> String {
    let minutes = (store::now() - at).max(0) / 60;
    match minutes {
        0 => "now".to_string(),
        1..60 => format!("{minutes}m"),
        60..1440 => format!("{}h", minutes / 60),
        1440..10080 => format!("{}d", minutes / 1440),
        10080..525600 => format!("{}w", minutes / 10080),
        _ => format!("{}y", minutes / 525600),
    }
}
