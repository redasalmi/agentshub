//! Tests for how tall the project shell is drawn.

use crate::chat::{MAX_SHELL_SHARE, MIN_SHELL_HEIGHT, shell_height, shell_share};
use gpui_kit::px;

#[test]
fn keeps_the_shell_within_its_limits() {
    // As dragged, within its limits.
    assert_eq!(shell_share(px(300.), px(1000.)), 0.3);
    assert_eq!(shell_share(px(10.), px(1000.)), 0.096);
    assert_eq!(shell_share(px(990.), px(1000.)), MAX_SHELL_SHARE);
    // Drawn the share of the window it takes, never shorter than its
    // header and a few lines.
    assert_eq!(shell_height(0.3, px(1000.)), px(300.));
    assert_eq!(shell_height(0.3, px(200.)), MIN_SHELL_HEIGHT);
    assert_eq!(shell_height(0.99, px(1000.)), px(750.));
}
