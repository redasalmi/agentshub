//! Opening a chat's folder in a code editor.

use agentshub_app::editors::Installed;
use agentshub_core::editors::{self, Editor};

use super::*;

impl Workspace {
    /// Opens the shown chat's folder in the default editor.
    pub(super) fn open_in_editor(
        &mut self,
        _: &OpenInEditor,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(id) = self.runtime.read(cx).active() else {
            return;
        };
        let Some((current, _)) = crate::chat::editor_choices(cx) else {
            self.say("No code editor Agents Hub knows is installed".into(), cx);
            return;
        };
        self.open_chat_in(current.editor, id, cx);
    }

    /// Opens the folder chat `id` works in with `editor`, and says so if it
    /// can't.
    pub(super) fn open_chat_in(&self, editor: Editor, id: i64, cx: &mut Context<Self>) {
        let Some(folder) = self
            .runtime
            .read(cx)
            .chat(id)
            .map(|chat| chat.cwd.to_path_buf())
        else {
            return;
        };
        let Some(found) = cx
            .try_global::<Installed>()
            .and_then(|installed| installed.0.iter().find(|each| each.found.editor == editor))
            .map(|each| each.found.clone())
        else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let opened = cx
                .background_spawn(async move { editors::open(&found, &folder, terminal::adopt) })
                .await;
            if let Err(err) = opened {
                eprintln!("can't open {}: {err}", editor.name());
                let message = format!("Couldn't open {}: {err}", editor.name());
                this.update(cx, |this, cx| this.say(message, cx)).ok();
            }
        })
        .detach();
    }
}
