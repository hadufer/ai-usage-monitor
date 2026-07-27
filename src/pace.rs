use std::time::{Duration, SystemTime};

use crate::models::UsageSection;
use crate::native_interop::Color;

pub const SESSION_WINDOW: Duration = Duration::from_secs(5 * 60 * 60);
pub const WEEKLY_WINDOW: Duration = Duration::from_secs(7 * 24 * 60 * 60);

pub const DEFAULT_ON_TRACK: f64 = 85.0;
pub const DEFAULT_AT_RISK: f64 = 115.0;
pub const DEFAULT_MIN_ELAPSED_FRACTION: f64 = 0.1;
pub const DEFAULT_COLOR_ON_TRACK: &str = "#3F9142";
pub const DEFAULT_COLOR_AT_RISK: &str = "#E8A33C";
pub const DEFAULT_COLOR_OVER: &str = "#C4402F";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Band {
    OnTrack,
    AtRisk,
    Over,
}

/// Everything the pace colouring reads from the settings file. Values that make
/// no sense are dropped in favour of the defaults rather than trusted.
#[derive(Clone, Copy, Debug)]
pub struct Settings {
    pub enabled: bool,
    pub on_track: f64,
    pub at_risk: f64,
    pub min_elapsed_fraction: f64,
    pub on_track_color: Color,
    pub at_risk_color: Color,
    pub over_color: Color,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: true,
            on_track: DEFAULT_ON_TRACK,
            at_risk: DEFAULT_AT_RISK,
            min_elapsed_fraction: DEFAULT_MIN_ELAPSED_FRACTION,
            on_track_color: Color::from_hex(DEFAULT_COLOR_ON_TRACK),
            at_risk_color: Color::from_hex(DEFAULT_COLOR_AT_RISK),
            over_color: Color::from_hex(DEFAULT_COLOR_OVER),
        }
    }
}

impl Settings {
    /// Build from raw settings values, falling back per field. Thresholds must
    /// be positive and ordered, or the bands would be unreachable.
    pub fn sanitized(
        enabled: bool,
        on_track: f64,
        at_risk: f64,
        min_elapsed_fraction: f64,
        on_track_color: &str,
        at_risk_color: &str,
        over_color: &str,
    ) -> Self {
        let defaults = Self::default();
        let (on_track, at_risk) = if on_track > 0.0 && at_risk > on_track {
            (on_track, at_risk)
        } else {
            (defaults.on_track, defaults.at_risk)
        };

        Self {
            enabled,
            on_track,
            at_risk,
            min_elapsed_fraction: if (0.0..1.0).contains(&min_elapsed_fraction) {
                min_elapsed_fraction
            } else {
                defaults.min_elapsed_fraction
            },
            on_track_color: Color::try_from_hex(on_track_color).unwrap_or(defaults.on_track_color),
            at_risk_color: Color::try_from_hex(at_risk_color).unwrap_or(defaults.at_risk_color),
            over_color: Color::try_from_hex(over_color).unwrap_or(defaults.over_color),
        }
    }
}

/// Consumption rate relative to the time left in the window, on the same scale
/// as the Claude Code statusline: 100 means "exactly on track to reach 100% at
/// the reset", above means burning faster than the window allows.
pub fn pace(
    section: &UsageSection,
    window: Duration,
    now: SystemTime,
    settings: &Settings,
) -> Option<f64> {
    if !settings.enabled {
        return None;
    }

    let resets_at = section.resets_at?;
    let remaining = resets_at.duration_since(now).ok()?.as_secs_f64();
    let total = window.as_secs_f64();
    if remaining >= total {
        return None;
    }

    // Below the floor a couple of percent spent right after a reset would
    // divide by almost nothing and paint everything red.
    let elapsed = (total - remaining).max(total * settings.min_elapsed_fraction);
    Some(section.percentage * total / elapsed)
}

fn band(pace: Option<f64>, settings: &Settings) -> Option<Band> {
    match pace? {
        pace if pace < settings.on_track => Some(Band::OnTrack),
        pace if pace < settings.at_risk => Some(Band::AtRisk),
        _ => Some(Band::Over),
    }
}

pub fn pace_color(pace: Option<f64>, settings: &Settings, fallback: Color) -> Color {
    match band(pace, settings) {
        Some(Band::OnTrack) => settings.on_track_color,
        Some(Band::AtRisk) => settings.at_risk_color,
        Some(Band::Over) => settings.over_color,
        None => fallback,
    }
}

/// Ink for text drawn on top of `pace_color`. A light band cannot carry white
/// text, so the ink follows the luminance of the band it sits on.
pub fn pace_ink(pace: Option<f64>, settings: &Settings, fallback: Color) -> Color {
    match band(pace, settings) {
        Some(_) => {
            let fill = pace_color(pace, settings, fallback);
            if is_light(fill) {
                Color::from_hex("#111111")
            } else {
                Color::from_hex("#FFFFFF")
            }
        }
        None => fallback,
    }
}

fn is_light(color: Color) -> bool {
    let luminance =
        0.299 * color.r as f64 + 0.587 * color.g as f64 + 0.114 * color.b as f64;
    luminance > 150.0
}

pub fn section_color(
    section: &UsageSection,
    window: Duration,
    settings: &Settings,
    fallback: Color,
) -> Color {
    pace_color(
        pace(section, window, SystemTime::now(), settings),
        settings,
        fallback,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn section(percentage: f64, remaining: Duration) -> UsageSection {
        UsageSection {
            percentage,
            resets_at: Some(SystemTime::UNIX_EPOCH + remaining),
        }
    }

    fn pace_at(percentage: f64, remaining_secs: u64) -> Option<f64> {
        pace(
            &section(percentage, Duration::from_secs(remaining_secs)),
            SESSION_WINDOW,
            SystemTime::UNIX_EPOCH,
            &Settings::default(),
        )
    }

    #[test]
    fn matches_statusline_formula_midway_through_the_window() {
        assert_eq!(pace_at(50.0, 9_000), Some(100.0));
        assert_eq!(pace_at(25.0, 9_000), Some(50.0));
    }

    #[test]
    fn clamps_elapsed_time_right_after_a_reset() {
        assert_eq!(pace_at(1.0, 17_880), Some(10.0));
        assert_eq!(pace_at(40.0, 17_880), Some(400.0));
    }

    #[test]
    fn thresholds_follow_the_statusline_bands() {
        let settings = Settings::default();
        assert_eq!(band(Some(84.9), &settings), Some(Band::OnTrack));
        assert_eq!(band(Some(85.0), &settings), Some(Band::AtRisk));
        assert_eq!(band(Some(114.9), &settings), Some(Band::AtRisk));
        assert_eq!(band(Some(115.0), &settings), Some(Band::Over));
    }

    #[test]
    fn a_light_band_takes_a_dark_ink() {
        let settings = Settings::default();
        let black = Color::new(0, 0, 0);
        let on_amber = pace_ink(Some(100.0), &settings, black);
        let on_green = pace_ink(Some(10.0), &settings, black);
        let on_red = pace_ink(Some(200.0), &settings, black);
        assert_eq!((on_amber.r, on_amber.g, on_amber.b), (0x11, 0x11, 0x11));
        assert_eq!((on_green.r, on_green.g, on_green.b), (0xFF, 0xFF, 0xFF));
        assert_eq!((on_red.r, on_red.g, on_red.b), (0xFF, 0xFF, 0xFF));
    }

    #[test]
    fn disabling_the_setting_falls_back_everywhere() {
        let settings = Settings {
            enabled: false,
            ..Settings::default()
        };
        let unknown = pace(
            &section(90.0, Duration::from_secs(9_000)),
            SESSION_WINDOW,
            SystemTime::UNIX_EPOCH,
            &settings,
        );
        assert!(unknown.is_none());

        let fallback = Color::new(0xD9, 0x77, 0x57);
        let color = pace_color(unknown, &settings, fallback);
        assert_eq!((color.r, color.g, color.b), (0xD9, 0x77, 0x57));
    }

    #[test]
    fn falls_back_when_the_reset_time_is_unknown() {
        let settings = Settings::default();
        let unknown = UsageSection {
            percentage: 90.0,
            resets_at: None,
        };
        assert!(pace(&unknown, SESSION_WINDOW, SystemTime::UNIX_EPOCH, &settings).is_none());

        let fallback = Color::new(0xD9, 0x77, 0x57);
        let color = section_color(&unknown, SESSION_WINDOW, &settings, fallback);
        assert_eq!((color.r, color.g, color.b), (0xD9, 0x77, 0x57));
    }

    #[test]
    fn ignores_a_reset_further_away_than_the_window() {
        assert_eq!(pace_at(50.0, 20_000), None);
    }

    #[test]
    fn rejects_settings_that_would_make_a_band_unreachable() {
        let broken = Settings::sanitized(true, 200.0, 100.0, 5.0, "nope", "#GGG", "#C4402F");
        let defaults = Settings::default();
        assert_eq!(broken.on_track, defaults.on_track);
        assert_eq!(broken.at_risk, defaults.at_risk);
        assert_eq!(broken.min_elapsed_fraction, defaults.min_elapsed_fraction);
        assert_eq!(
            (broken.on_track_color.r, broken.on_track_color.g),
            (defaults.on_track_color.r, defaults.on_track_color.g)
        );
        assert_eq!(
            (broken.over_color.r, broken.over_color.g, broken.over_color.b),
            (0xC4, 0x40, 0x2F)
        );
    }

    #[test]
    fn accepts_custom_thresholds_and_colours() {
        let custom = Settings::sanitized(true, 50.0, 90.0, 0.25, "#123456", "#abcdef", "#000000");
        assert_eq!(custom.on_track, 50.0);
        assert_eq!(custom.at_risk, 90.0);
        assert_eq!(custom.min_elapsed_fraction, 0.25);
        assert_eq!(band(Some(60.0), &custom), Some(Band::AtRisk));
        assert_eq!(
            (
                custom.on_track_color.r,
                custom.on_track_color.g,
                custom.on_track_color.b
            ),
            (0x12, 0x34, 0x56)
        );
    }
}
