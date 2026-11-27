//! The code editors installed on this machine, looked up once at startup.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui_kit::*;

use agentshub_core::editors::{self, Dirs, Found};

/// How big an editor's icon is drawn, in logical pixels.
pub const ICON_SIZE: u16 = 20;

/// An installed editor, with its icon ready to draw.
#[derive(Clone)]
pub struct InstalledEditor {
    pub found: Found,
    pub icon: Option<ImageSource>,
}

/// The installed editors on the built-in list, empty until they are looked up.
#[derive(Default)]
pub struct Installed(pub Vec<InstalledEditor>);

impl Global for Installed {}

/// An SVG as it is, or a raster icon shrunk to twice `ICON_SIZE`, which stays
/// sharp on a screen scaled up to twice. Made off the foreground thread, where
/// an `ImageSource` can't go.
enum Loaded {
    Svg(PathBuf),
    Png(Vec<u8>),
}

fn load(path: &Path) -> Option<Loaded> {
    if path.extension().is_some_and(|extension| extension == "svg") {
        return Some(Loaded::Svg(path.to_path_buf()));
    }
    editors::icon_png(path, u32::from(ICON_SIZE) * 2).map(Loaded::Png)
}

impl From<Loaded> for ImageSource {
    fn from(loaded: Loaded) -> Self {
        match loaded {
            Loaded::Svg(path) => path.into(),
            Loaded::Png(png) => Arc::new(Image::from_bytes(ImageFormat::Png, png)).into(),
        }
    }
}

/// Looks up the installed editors and their icons off the foreground thread,
/// and redraws once they are known.
pub fn look_up(cx: &mut App) {
    cx.default_global::<Installed>();
    cx.spawn(async move |cx| {
        let loaded: Vec<(Found, Option<Loaded>)> = cx
            .background_spawn(async move {
                editors::detect(&Dirs::from_env())
                    .into_iter()
                    .map(|found| {
                        let icon = found.icon.as_deref().and_then(load);
                        (found, icon)
                    })
                    .collect()
            })
            .await;
        let installed = loaded
            .into_iter()
            .map(|(found, icon)| InstalledEditor {
                found,
                icon: icon.map(Into::into),
            })
            .collect();
        cx.update(|cx| {
            cx.set_global(Installed(installed));
            cx.refresh_windows();
        });
    })
    .detach();
}
