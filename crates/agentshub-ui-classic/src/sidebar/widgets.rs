//! Headings of the sidebar's lists.

use super::*;

pub(super) fn section(label: &'static str, cx: &App) -> impl IntoElement {
    heading(label, cx).px_2().pt_3().pb_1()
}
