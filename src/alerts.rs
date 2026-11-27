//! Wiring notification policy to the layout.

use std::rc::Rc;

use gpui_kit::*;

use agentshub_ui_kit::Frontend;

pub(crate) fn init(frontend: &Rc<dyn Frontend>, cx: &mut App) {
    // The runtime shuts the agents down when the layout lets go of it as the
    // app quits, which the alerts' global would otherwise prevent.
    let frontend = Rc::downgrade(frontend);
    let open = Rc::new(move |chat: i64, token: Option<String>, cx: &mut App| {
        let Some(frontend) = frontend.upgrade() else {
            return;
        };
        crate::tray::show(token, cx);
        frontend.open_chat(chat, cx);
    });
    agentshub_app::alerts::start(open, cx);
}
