//! The notification sounds, made here so no sound files ship with the app.

use std::f32::consts::TAU;
use std::num::NonZero;

use super::*;

pub(super) fn play(kind: Alert, cx: &mut App) {
    let alerts = cx.global_mut::<Alerts>();
    let now = Instant::now();
    if alerts
        .last_sound
        .is_some_and(|last| now.duration_since(last) < SOUND_GAP)
    {
        return;
    }
    alerts.last_sound = Some(now);
    // Opening the device and waiting for the sound to end both block.
    let spawned = std::thread::Builder::new()
        .name("alert-sound".into())
        .spawn(move || {
            if let Err(err) = play_blocking(kind) {
                eprintln!("can't play the notification sound: {err}");
            }
        });
    if let Err(err) = spawned {
        eprintln!("can't play the notification sound: {err}");
    }
}

fn play_blocking(kind: Alert) -> Result<()> {
    let mut sink = rodio::DeviceSinkBuilder::open_default_sink()?;
    sink.log_on_drop(false);
    let player = rodio::Player::connect_new(sink.mixer());
    player.append(rodio::buffer::SamplesBuffer::new(
        const { NonZero::new(1).unwrap() },
        const { NonZero::new(SAMPLE_RATE).unwrap() },
        chime(kind),
    ));
    player.sleep_until_end();
    // What the device still holds plays out before it's closed.
    std::thread::sleep(Duration::from_millis(150));
    Ok(())
}

const SAMPLE_RATE: u32 = 48_000;

/// The notes of each sound: their pitch in Hz and when they start, in
/// seconds. Finishing rises, needing the user knocks, failing falls.
fn notes(kind: Alert) -> &'static [(f32, f32)] {
    match kind {
        Alert::Finished => &[(659.25, 0.0), (880.0, 0.11)],
        Alert::NeedsYou => &[(987.77, 0.0), (987.77, 0.13), (1318.51, 0.26)],
        Alert::Failed => &[(880.0, 0.0), (587.33, 0.14)],
    }
}

/// A soft bell-like sound, made here so no sound files ship with the app.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "sample counts for a short tone, from non-negative times"
)]
pub(super) fn chime(kind: Alert) -> Vec<f32> {
    const NOTE_LEN: f32 = 0.7;
    const ATTACK: f32 = 0.004;
    const DECAY: f32 = 0.16;
    const PEAK: f32 = 0.3;

    let notes = notes(kind);
    let end = notes.iter().map(|&(_, start)| start).fold(0.0, f32::max) + NOTE_LEN;
    let rate = SAMPLE_RATE as f32;
    let mut samples = vec![0.0f32; (end * rate) as usize];
    for &(pitch, start) in notes {
        let first = (start * rate) as usize;
        for (i, sample) in samples[first..].iter_mut().enumerate() {
            let t = i as f32 / rate;
            if t >= NOTE_LEN {
                break;
            }
            let envelope = (t / ATTACK).min(1.0) * (-t / DECAY).exp();
            // A few overtones, fading faster than the note, make it ring.
            let tone = (TAU * pitch * t).sin()
                + 0.25 * (TAU * 2.0 * pitch * t).sin() * (-t / 0.05).exp()
                + 0.1 * (TAU * 3.0 * pitch * t).sin() * (-t / 0.03).exp();
            *sample += envelope * tone;
        }
    }
    let loudest = samples.iter().fold(0.0f32, |max, s| max.max(s.abs()));
    if loudest > 0.0 {
        for sample in &mut samples {
            *sample *= PEAK / loudest;
        }
    }
    samples
}
