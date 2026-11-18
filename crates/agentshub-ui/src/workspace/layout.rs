//! Resizing the sidebar, the rail and the shell.

use super::*;

impl Workspace {
    /// The sidebar's right edge, which resizes it when dragged and restores
    /// its default width when double clicked.
    pub(super) fn render_sidebar_edge(&self, cx: &Context<Self>) -> impl IntoElement {
        let workspace = cx.weak_entity();
        resize_handle("sidebar-edge", Axis::Horizontal)
            .inside(HandleEdge::Trailing)
            .on_drag(ResizeSidebar, move |drag, _, _, cx| {
                cx.stop_propagation();
                let workspace = workspace.clone();
                cx.new(|cx| {
                    // The drag lets go of what it carries once it ends, which
                    // is when the width the user settled on is saved.
                    cx.on_release(move |_, cx| {
                        workspace
                            .update(cx, |this, cx| this.save_sidebar_width(cx))
                            .ok();
                    })
                    .detach();
                    drag.clone()
                })
            })
            .on_double_click(cx.listener(|this, _, _, cx| {
                this.sidebar_width = px(DEFAULT_SIDEBAR_WIDTH);
                this.save_sidebar_width(cx);
                cx.notify();
            }))
    }

    /// The rail's right edge, which resizes the rail when dragged, folds it
    /// to the projects' marks when dragged narrow enough, and restores its
    /// default width when double clicked. The sidebar grows or shrinks with
    /// it, so its list keeps its width.
    pub(super) fn render_rail_edge(
        &self,
        window_width: Pixels,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let workspace = cx.weak_entity();
        div()
            .absolute()
            .top_0()
            .bottom_0()
            .left_0()
            .w(sidebar::rail(window_width, cx).1)
            .child(
                resize_handle("rail-edge", Axis::Horizontal)
                    .inside(HandleEdge::Trailing)
                    .on_drag(ResizeRail, move |drag, _, _, cx| {
                        cx.stop_propagation();
                        let workspace = workspace.clone();
                        cx.new(|cx| {
                            // Saved once the drag ends, as the sidebar's width.
                            cx.on_release(move |_, cx| {
                                workspace.update(cx, |this, cx| this.save_rail(cx)).ok();
                            })
                            .detach();
                            drag.clone()
                        })
                    })
                    .on_double_click(cx.listener(|this, _, _, cx| {
                        this.resize_rail(true, DEFAULT_RAIL_WIDTH, cx);
                        this.save_rail(cx);
                    })),
            )
    }

    /// Shows the rail `width` wide, naming the projects, or with their marks
    /// alone, without saving it.
    pub(super) fn resize_rail(&mut self, named: bool, width: f32, cx: &mut Context<Self>) {
        let settings = *cx.global::<Settings>();
        if settings.project_names == named && settings.rail_width == width {
            return;
        }
        cx.set_global(Settings {
            project_names: named,
            rail_width: width,
            ..settings
        });
        self.sidebar.update(cx, |_, cx| cx.notify());
        cx.notify();
    }

    pub(super) fn save_rail(&mut self, cx: &mut Context<Self>) {
        let settings = *cx.global::<Settings>();
        let width = Settings::set_rail_width(&self.store, settings.rail_width, cx);
        let names = Settings::set_project_names(&self.store, settings.project_names, cx);
        self.save_setting(
            async move {
                width.await?;
                names.await
            },
            cx,
        );
    }

    /// Shows the shell taking `share` of the window's height, without saving
    /// it.
    pub(super) fn resize_shell(&mut self, share: f32, cx: &mut Context<Self>) {
        let settings = *cx.global::<Settings>();
        if settings.shell_share == share {
            return;
        }
        cx.set_global(Settings {
            shell_share: share,
            ..settings
        });
        cx.notify();
    }

    pub(super) fn save_shell_share(&mut self, cx: &mut Context<Self>) {
        let share = cx.global::<Settings>().shell_share;
        let saved = Settings::set_shell_share(&self.store, share, cx);
        self.save_setting(saved, cx);
    }

    pub(super) fn save_sidebar_width(&mut self, cx: &mut Context<Self>) {
        let width = f32::from(self.sidebar_width);
        if cx.global::<Settings>().sidebar_width == width {
            return;
        }
        let saved = Settings::set_sidebar_width(&self.store, width, cx);
        self.save_setting(saved, cx);
    }
}
