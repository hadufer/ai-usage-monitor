use std::time::{Duration, SystemTime};

use crate::models::UsageSection;
use crate::native_interop::Color;

pub const SESSION_WINDOW: Duration = Duration::from_secs(5 * 60 * 60);
pub const WEEKLY_WINDOW: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// Fraction of the window that must have elapsed before the pace is taken at
/// face value. Below it the elapsed time is clamped, otherwise a couple of
/// percent burned right after a reset would divide by almost nothing.
const MIN_ELAPSED_FRACTION: f64 = 0.1;

const PACE_ON_TRACK: f64 = 85.0;
const PACE_AT_RISK: f64 = 115.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Band {
    OnTrack,
    AtRisk,
    Over,
}

fn band(pace: Option<f64>) -> Option<Band> {
    match pace? {
        pace if pace < PACE_ON_TRACK => Some(Band::OnTrack),
        pace if pace < PACE_AT_RISK => Some(Band::AtRisk),
        _ => Some(Band::Over),
    }
}

/// Consumption rate relative to the time left in the window, on the same scale
/// as the Claude Code statusline: 100 means "exactly on track to reach 100% at
/// the reset", above means burning faster than the window allows.
pub fn pace(section: &UsageSection, window: Duration, now: SystemTime) -> Option<f64> {
    let resets_at = section.resets_at?;
    let remaining = resets_at.duration_since(now).ok()?.as_secs_f64();
    let total = window.as_secs_f64();
    if remaining >= total {
        return None;
    }

    let elapsed = (total - remaining).max(total * MIN_ELAPSED_FRACTION);
    Some(section.percentage * total / elapsed)
}

pub fn pace_color(pace: Option<f64>, fallback: Color) -> Color {
    match band(pace) {
        Some(Band::OnTrack) => Color::from_hex("#3F9142"),
        Some(Band::AtRisk) => Color::from_hex("#E8A33C"),
        Some(Band::Over) => Color::from_hex("#C4402F"),
        None => fallback,
    }
}

/// Ink for text drawn on top of `pace_color`. The at-risk amber is too light to
/// carry white text, so it takes a dark ink instead.
pub fn pace_ink(pace: Option<f64>, fallback: Color) -> Color {
    match band(pace) {
        Some(Band::AtRisk) => Color::from_hex("#111111"),
        Some(_) => Color::from_hex("#FFFFFF"),
        None => fallback,
    }
}

pub fn section_color(section: &UsageSection, window: Duration, fallback: Color) -> Color {
    pace_color(pace(section, window, SystemTime::now()), fallback)
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
        assert_eq!(band(Some(84.9)), Some(Band::OnTrack));
        assert_eq!(band(Some(85.0)), Some(Band::AtRisk));
        assert_eq!(band(Some(114.9)), Some(Band::AtRisk));
        assert_eq!(band(Some(115.0)), Some(Band::Over));

        let green = pace_color(Some(84.9), Color::new(0, 0, 0));
        let amber = pace_color(Some(85.0), Color::new(0, 0, 0));
        let red = pace_color(Some(115.0), Color::new(0, 0, 0));
        assert_eq!((green.r, green.g, green.b), (0x3F, 0x91, 0x42));
        assert_eq!((amber.r, amber.g, amber.b), (0xE8, 0xA3, 0x3C));
        assert_eq!((red.r, red.g, red.b), (0xC4, 0x40, 0x2F));
    }

    #[test]
    fn the_amber_band_takes_a_dark_ink() {
        let on_amber = pace_ink(Some(100.0), Color::new(0, 0, 0));
        let on_green = pace_ink(Some(10.0), Color::new(0, 0, 0));
        let on_red = pace_ink(Some(200.0), Color::new(0, 0, 0));
        assert_eq!((on_amber.r, on_amber.g, on_amber.b), (0x11, 0x11, 0x11));
        assert_eq!((on_green.r, on_green.g, on_green.b), (0xFF, 0xFF, 0xFF));
        assert_eq!((on_red.r, on_red.g, on_red.b), (0xFF, 0xFF, 0xFF));
    }

    #[test]
    fn falls_back_when_the_reset_time_is_unknown() {
        let unknown = UsageSection {
            percentage: 90.0,
            resets_at: None,
        };
        assert!(pace(&unknown, SESSION_WINDOW, SystemTime::UNIX_EPOCH).is_none());

        let fallback = Color::new(0xD9, 0x77, 0x57);
        let color = section_color(&unknown, SESSION_WINDOW, fallback);
        assert_eq!((color.r, color.g, color.b), (0xD9, 0x77, 0x57));
    }

    #[test]
    fn ignores_a_reset_further_away_than_the_window() {
        assert_eq!(pace_at(50.0, 20_000), None);
    }
}
