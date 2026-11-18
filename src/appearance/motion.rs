//! Following the desktop's reduced-motion preference.

use super::*;

/// Whether the desktop asks apps to reduce motion, as it last said.
#[derive(Default)]
struct DesktopMotion {
    reduced: bool,
}

impl Global for DesktopMotion {}

const PORTAL: &str = "org.freedesktop.portal.Desktop";
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
const PORTAL_SETTINGS: &str = "org.freedesktop.portal.Settings";
const APPEARANCE_NAMESPACE: &str = "org.freedesktop.appearance";
const REDUCED_MOTION: &str = "reduced-motion";

/// Follows the desktop's reduced-motion preference, through the settings
/// portal, for as long as the app runs. Without a portal that has it, the app
/// animates unless its own setting says otherwise.
pub fn watch_desktop_motion(cx: &mut App) {
    cx.set_global(DesktopMotion::default());
    let (sender, received) = smol::channel::unbounded();
    cx.background_spawn(async move {
        if let Err(err) = listen_desktop_motion(sender).await {
            eprintln!("can't follow the desktop's reduced-motion preference: {err}");
        }
    })
    .detach();
    cx.spawn(async move |cx| {
        while let Ok(reduced) = received.recv().await {
            cx.update(|cx| {
                cx.set_global(DesktopMotion { reduced });
                apply_motion(cx);
            });
        }
    })
    .detach();
}

async fn listen_desktop_motion(sender: smol::channel::Sender<bool>) -> zbus::Result<()> {
    use smol::stream::StreamExt as _;
    use zbus::zvariant::OwnedValue;

    // The portal's values: 0 for no preference, 1 to reduce motion.
    let reduced = |value: &OwnedValue| u32::try_from(value).is_ok_and(|value| value == 1);
    let connection = zbus::Connection::session().await?;
    let proxy = zbus::Proxy::new(&connection, PORTAL, PORTAL_PATH, PORTAL_SETTINGS).await?;
    // Subscribed before the value is read, so no change in between is missed.
    let mut changes = proxy
        .receive_signal_with_args(
            "SettingChanged",
            &[(0, APPEARANCE_NAMESPACE), (1, REDUCED_MOTION)],
        )
        .await?;
    let value: OwnedValue = proxy
        .call("ReadOne", &(APPEARANCE_NAMESPACE, REDUCED_MOTION))
        .await?;
    if sender.send(reduced(&value)).await.is_err() {
        return Ok(());
    }
    while let Some(message) = changes.next().await {
        let Ok((_, _, value)) = message.body().deserialize::<(String, String, OwnedValue)>() else {
            continue;
        };
        if sender.send(reduced(&value)).await.is_err() {
            break;
        }
    }
    Ok(())
}

/// Animates as the motion setting says, or as the desktop does while it
/// says to follow the system.
pub fn apply_motion(cx: &mut App) {
    let desktop = cx
        .try_global::<DesktopMotion>()
        .is_some_and(|desktop| desktop.reduced);
    let reduce = match cx.try_global::<Settings>().map(|settings| settings.motion) {
        Some(Motion::Full) => false,
        Some(Motion::Reduced) => true,
        Some(Motion::System) | None => desktop,
    };
    cx.set_reduce_motion(reduce);
}
