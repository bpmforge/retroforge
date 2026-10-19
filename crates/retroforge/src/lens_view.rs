//! The shell half of the HD-2D lens and lighting pass (ticket W30-04):
//! everything between a player's [`GameSettings`] and
//! [`rf_renderer::lens::LensPass`] that is not drawing -- settings to pass
//! parameters, the time of day, and cutting the core's sprite layer to the
//! picture being shown. Pure functions, so they are tested without a GPU.
//!
//! The pass runs on the UI thread over the finished frame; nothing here
//! touches the core thread (law 6).

use std::time::{Duration, Instant};

use rf_renderer::lens::{Effect, LensParams};

use crate::game_settings::GameSettings;

/// A stored percent as a strength in `0.0..=1.0`.
#[must_use]
fn strength(pct: u8) -> f32 {
    f32::from(pct.min(100)) / 100.0
}

/// The pass parameters for these settings. `clock` is the time of day to
/// use when the player asked to follow the clock; `frame` seeds the
/// weather.
#[must_use]
pub fn lens_params(settings: &GameSettings, clock: Option<f32>, frame: u32) -> LensParams {
    let time_of_day = if settings.lens_follow_clock {
        clock.unwrap_or_else(|| strength(settings.lens_time_of_day))
    } else {
        strength(settings.lens_time_of_day)
    };
    LensParams {
        bloom: Effect {
            enabled: settings.lens_bloom,
            strength: strength(settings.lens_bloom_pct),
        },
        tilt_shift: Effect {
            enabled: settings.lens_tiltshift,
            strength: strength(settings.lens_tiltshift_pct),
        },
        shadows: Effect {
            enabled: settings.lens_shadows,
            strength: strength(settings.lens_shadows_pct),
        },
        grade: Effect {
            enabled: settings.lens_grade,
            strength: strength(settings.lens_grade_pct),
        },
        time_of_day,
        weather: settings.lens_weather,
        weather_density: strength(settings.lens_weather_pct),
        frame,
        ..LensParams::default()
    }
}

/// The sun the Diorama is lit by (ticket W34-06): the same time of day, and
/// the same weather, the lens grade uses, so the light and the grade agree.
/// `side` mirrors it for an upright picture. `clock` is as for
/// [`lens_params`].
#[must_use]
pub fn diorama_lighting(
    settings: &GameSettings,
    clock: Option<f32>,
    side: bool,
) -> rf_renderer::lighting::Lighting {
    use rf_renderer::lighting::{LightView, Lighting, Sun};
    let p = lens_params(settings, clock, 0);
    let view = if side {
        LightView::Side
    } else {
        LightView::TopDown
    };
    Lighting::new(Sun::for_time(p.time_of_day, p.weather, view))
}

/// The lens parameters for a frame, with its 2D soft shadows forced off while
/// the 3D Diorama is the picture on screen (ticket W34-06): the shadow map
/// casts the shadows there, and an offset blur of the flat frame's sprites
/// would put a second, wrong one beside it. Bloom, tilt-shift, grade and
/// weather are untouched.
#[must_use]
pub fn without_shadows_over_diorama(mut params: LensParams, diorama_shown: bool) -> LensParams {
    if diorama_shown {
        params.shadows = Effect::OFF;
    }
    params
}

/// The clock label for a stored time-of-day percent (`50` -> `12:00`).
#[must_use]
pub fn clock_label(pct: u8) -> String {
    let minutes = (u32::from(pct.min(100)) * 1440 + 50) / 100;
    let minutes = minutes % 1440;
    format!("{:02}:{:02}", minutes / 60, minutes % 60)
}

/// `"HH:MM"` as a fraction of a day (`"06:00"` -> `0.25`).
#[must_use]
pub fn parse_clock(text: &str) -> Option<f32> {
    let (h, m) = text.trim().split_once(':')?;
    let (h, m) = (h.parse::<u32>().ok()?, m.parse::<u32>().ok()?);
    (h < 24 && m < 60).then(|| (h * 60 + m) as f32 / 1440.0)
}

/// Seconds since the Unix epoch as a fraction of a UTC day.
#[must_use]
pub fn utc_fraction(since_epoch: Duration) -> f32 {
    (since_epoch.as_secs() % 86_400) as f32 / 86_400.0
}

/// The time of day for "follow the clock": the local wall clock, read from
/// the system's `date` command at most once every [`Self::REFRESH`] (the
/// standard library has no time zone; a new dependency is not worth one
/// slider). Where `date` is missing or its answer does not parse (it does
/// not on Windows), UTC is used instead.
#[derive(Debug, Default)]
pub struct LensClock {
    cached: Option<(Instant, f32)>,
}

impl LensClock {
    pub const REFRESH: Duration = Duration::from_secs(60);

    /// The time of day as a fraction of a day, `0.0..1.0`.
    pub fn fraction(&mut self) -> f32 {
        if let Some((at, f)) = self.cached {
            if at.elapsed() < Self::REFRESH {
                return f;
            }
        }
        let f = Self::read();
        self.cached = Some((Instant::now(), f));
        f
    }

    fn read() -> f32 {
        std::process::Command::new("date")
            .arg("+%H:%M")
            .output()
            .ok()
            .filter(|o| o.status.success())
            .and_then(|o| parse_clock(&String::from_utf8_lossy(&o.stdout)))
            .unwrap_or_else(|| {
                utc_fraction(
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default(),
                )
            })
    }
}

/// The core's sprite-only layer, cut to the picture being shown, or `None`
/// when it does not line up with it.
///
/// `layer` is `layer_w` x `layer_h` (the core's frame, e.g. 256x240); the
/// shown picture is `shown_w` x `shown_h`, which is that frame's visible
/// `rows` at some whole-number scale (an HD Mode 7 composite, an HD pack).
/// A widescreen canvas or any other geometry has no such relation, and
/// shadows are skipped rather than drawn in the wrong place.
#[must_use]
pub fn fit_sprite_layer(
    layer: &[u8],
    layer_w: usize,
    layer_h: usize,
    rows: usize,
    shown_w: usize,
    shown_h: usize,
) -> Option<(&[u8], u32, u32)> {
    if layer_w == 0
        || rows == 0
        || rows > layer_h
        || layer.len() != layer_w * layer_h * 4
        || !shown_w.is_multiple_of(layer_w)
    {
        return None;
    }
    let scale = shown_w / layer_w;
    if scale == 0 || shown_h != rows * scale {
        return None;
    }
    let bytes = layer_w * rows * 4;
    Some((
        &layer[..bytes],
        u32::try_from(layer_w).ok()?,
        u32::try_from(rows).ok()?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rf_renderer::lens::Weather;

    #[test]
    fn defaults_make_no_effect_and_percents_become_strengths() {
        let p = lens_params(&GameSettings::default(), None, 0);
        assert!(!p.any_active());
        let mut s = GameSettings::default();
        s.lens_bloom = true;
        s.lens_bloom_pct = 80;
        s.lens_tiltshift = true;
        s.lens_tiltshift_pct = 0;
        s.lens_shadows = true;
        s.lens_shadows_pct = 100;
        s.lens_grade = true;
        s.lens_grade_pct = 25;
        s.lens_weather = Weather::Rain;
        s.lens_weather_pct = 40;
        s.lens_time_of_day = 25;
        let p = lens_params(&s, Some(0.9), 77);
        assert_eq!(p.bloom.level(), 0.8);
        assert_eq!(p.tilt_shift.level(), 0.0, "0 percent is off");
        assert_eq!(p.shadows.level(), 1.0);
        assert_eq!(p.grade.level(), 0.25);
        assert_eq!(p.time_of_day, 0.25, "the slider, when not following");
        assert_eq!(
            (p.weather, p.weather_density, p.frame),
            (Weather::Rain, 0.4, 77)
        );
    }

    #[test]
    fn following_the_clock_uses_it_and_falls_back_to_the_slider() {
        let mut s = GameSettings::default();
        s.lens_grade = true;
        s.lens_follow_clock = true;
        s.lens_time_of_day = 75;
        assert_eq!(lens_params(&s, Some(0.1), 0).time_of_day, 0.1);
        assert_eq!(lens_params(&s, None, 0).time_of_day, 0.75);
    }

    #[test]
    fn the_sun_follows_the_lens_time_of_day_and_mirrors_for_side_view() {
        let mut s = GameSettings::default();
        s.lens_time_of_day = 30;
        let top = diorama_lighting(&s, None, false);
        assert!(top.sun.to_sun[0] > 0.2, "morning, screen-right: {top:?}");
        assert!(top.sun.to_sun[2] < 0.0);
        let side = diorama_lighting(&s, None, true);
        assert!(
            side.sun.to_sun[2] > 0.0,
            "side view: the sun is on the camera side"
        );
        s.lens_time_of_day = 80;
        assert!(diorama_lighting(&s, None, false).sun.to_sun[0] < -0.1);
        // Following the clock overrides the slider, as the grade does.
        s.lens_grade = true;
        s.lens_follow_clock = true;
        assert_eq!(
            diorama_lighting(&s, Some(0.5), false).sun,
            diorama_lighting(&GameSettings::default(), None, false).sun,
            "noon is noon"
        );
        s.lens_weather = Weather::Rain;
        let rain = diorama_lighting(&s, Some(0.5), false).sun.intensity;
        assert!((rain - 0.6).abs() < 0.01, "rain dims the sun: {rain}");
    }

    #[test]
    fn the_lens_shadows_step_aside_for_the_3d_view_and_nothing_else_does() {
        let mut s = GameSettings::default();
        s.lens_shadows = true;
        s.lens_bloom = true;
        s.lens_grade = true;
        s.lens_weather = Weather::Snow;
        let flat = lens_params(&s, None, 0);
        assert_eq!(without_shadows_over_diorama(flat, false), flat);
        let over = without_shadows_over_diorama(flat, true);
        assert!(over.shadows.level() == 0.0);
        assert_eq!(
            (over.bloom, over.grade, over.weather, over.time_of_day),
            (flat.bloom, flat.grade, flat.weather, flat.time_of_day)
        );
    }

    #[test]
    fn a_switched_off_effect_keeps_its_strength_but_does_nothing() {
        let mut s = GameSettings::default();
        s.lens_bloom = false;
        s.lens_bloom_pct = 100;
        assert!(!lens_params(&s, None, 0).any_active());
    }

    #[test]
    fn the_clock_label_reads_as_a_time() {
        assert_eq!(clock_label(0), "00:00");
        assert_eq!(clock_label(25), "06:00");
        assert_eq!(clock_label(50), "12:00");
        assert_eq!(clock_label(100), "00:00");
        assert_eq!(clock_label(73), "17:31");
    }

    #[test]
    fn clock_text_parses_and_refuses_nonsense() {
        assert_eq!(parse_clock("06:00\n"), Some(0.25));
        assert_eq!(parse_clock("23:59"), Some(1439.0 / 1440.0));
        for bad in ["", "24:00", "12:60", "noon", "12", "-1:00"] {
            assert_eq!(parse_clock(bad), None, "{bad:?}");
        }
        assert_eq!(utc_fraction(Duration::from_secs(43_200 + 86_400 * 9)), 0.5);
    }

    #[test]
    fn the_sprite_layer_is_cut_to_the_visible_rows_at_any_whole_scale() {
        let layer = vec![7u8; 256 * 240 * 4];
        let (cut, w, h) = fit_sprite_layer(&layer, 256, 240, 224, 256, 224).expect("native");
        assert_eq!((w, h, cut.len()), (256, 224, 256 * 224 * 4));
        let (_, w, h) = fit_sprite_layer(&layer, 256, 240, 224, 1024, 896).expect("4x");
        assert_eq!(
            (w, h),
            (256, 224),
            "the layer stays native; the shader samples it"
        );
    }

    #[test]
    fn a_sprite_layer_that_does_not_line_up_is_refused() {
        let layer = vec![0u8; 256 * 240 * 4];
        // Widescreen: wider than any whole multiple.
        assert!(fit_sprite_layer(&layer, 256, 240, 224, 398, 224).is_none());
        // Wrong height for the width's scale.
        assert!(fit_sprite_layer(&layer, 256, 240, 224, 512, 224).is_none());
        // More rows than the layer has, zero rows, a short buffer.
        assert!(fit_sprite_layer(&layer, 256, 240, 241, 256, 241).is_none());
        assert!(fit_sprite_layer(&layer, 256, 240, 0, 256, 0).is_none());
        assert!(fit_sprite_layer(&layer[..100], 256, 240, 224, 256, 224).is_none());
        assert!(fit_sprite_layer(&layer, 0, 240, 224, 256, 224).is_none());
    }
}
