---
name: AI Usage Monitor
description: A glass cockpit in the Windows taskbar. Every usage window is an engine tape, and a magenta bug marks where the clock says you should be.
colors:
  pace-on-track: "#3F9142"
  pace-at-risk: "#E8A33C"
  pace-over: "#C4402F"
  night-bug: "#D86BD3"
  day-bug: "#A8309F"
  night-over-text: "#D37063"
  day-at-risk-text: "#976A27"
  day-over-text: "#AC3829"
  hot-ink: "#FFFFFF"
  night-plate: "#0A0C0F"
  night-hover: "#16191D"
  night-track: "#22262B"
  night-rule: "#22262B"
  night-bezel: "#2A2E33"
  night-box-line: "#454B52"
  night-dim: "#8B929A"
  night-ink: "#E8EAED"
  day-plate: "#FBFBFA"
  day-hover: "#EEF0F2"
  day-track: "#DCDFE3"
  day-rule: "#E1E4E8"
  day-bezel: "#C4C8CD"
  day-box-line: "#9AA0A6"
  day-dim: "#5A6169"
  day-ink: "#1B1F24"
typography:
  widget-value:
    fontFamily: "Bahnschrift SemiBold SemiConden, Bahnschrift SemiBold, Segoe UI Semibold, Segoe UI"
    fontSize: "12px"
    fontWeight: 600
    letterSpacing: "normal"
  widget-countdown:
    fontFamily: "Bahnschrift SemiCondensed, Bahnschrift, Segoe UI"
    fontSize: "12px"
    fontWeight: 400
    letterSpacing: "normal"
  flyout-title:
    fontFamily: "Bahnschrift SemiBold SemiConden, Bahnschrift SemiBold, Segoe UI Semibold, Segoe UI"
    fontSize: "13px"
    fontWeight: 600
    letterSpacing: "0.08em"
  flyout-label:
    fontFamily: "Bahnschrift SemiBold SemiConden, Bahnschrift SemiBold, Segoe UI Semibold, Segoe UI"
    fontSize: "11px"
    fontWeight: 600
    letterSpacing: "0.08em"
  flyout-readout:
    fontFamily: "Bahnschrift SemiBold SemiConden, Bahnschrift SemiBold, Segoe UI Semibold, Segoe UI"
    fontSize: "14px"
    fontWeight: 600
    letterSpacing: "normal"
  flyout-alert:
    fontFamily: "Bahnschrift SemiBold SemiConden, Bahnschrift SemiBold, Segoe UI Semibold, Segoe UI"
    fontSize: "12px"
    fontWeight: 600
    letterSpacing: "normal"
  flyout-reset:
    fontFamily: "Bahnschrift SemiCondensed, Bahnschrift, Segoe UI"
    fontSize: "12px"
    fontWeight: 400
    letterSpacing: "normal"
  flyout-countdown:
    fontFamily: "Bahnschrift SemiCondensed, Bahnschrift, Segoe UI"
    fontSize: "11px"
    fontWeight: 400
    letterSpacing: "normal"
  flyout-legend:
    fontFamily: "Segoe UI Variable Text, Segoe UI"
    fontSize: "12px"
    fontWeight: 400
    letterSpacing: "normal"
rounded:
  readout: "2px"
  alert-frame: "3px"
  plate: "4px"
spacing:
  readout-pad: "3px"
  box-gap: "4px"
  code-gap: "5px"
  tape-gap: "6px"
  plate-pad-x: "7px"
  col-gap: "10px"
  fly-pad-y: "12px"
  fly-anchor-gap: "12px"
  fly-pad-x: "16px"
  group-gap: "20px"
components:
  plate:
    backgroundColor: "{colors.night-plate}"
    rounded: "{rounded.plate}"
    height: "40px"
    padding: "0 7px"
  plate-hover:
    backgroundColor: "{colors.night-hover}"
  plate-active:
    backgroundColor: "{colors.night-hover}"
  readout:
    textColor: "{colors.night-ink}"
    typography: "{typography.widget-value}"
    rounded: "{rounded.readout}"
    padding: "0 3px"
    height: "15px"
  readout-at-risk:
    textColor: "{colors.pace-at-risk}"
  readout-over:
    textColor: "{colors.night-over-text}"
  readout-hot:
    backgroundColor: "{colors.pace-over}"
    textColor: "{colors.hot-ink}"
  tape:
    backgroundColor: "{colors.night-track}"
    width: "62px"
    height: "5px"
  vertical-tape:
    backgroundColor: "{colors.night-track}"
    width: "10px"
    height: "92px"
  flyout:
    backgroundColor: "{colors.night-plate}"
    padding: "12px 16px"
  flyout-readout:
    textColor: "{colors.night-ink}"
    typography: "{typography.flyout-readout}"
    rounded: "{rounded.readout}"
    height: "18px"
    width: "36px"
  flyout-button:
    textColor: "{colors.night-ink}"
    typography: "{typography.flyout-label}"
    rounded: "{rounded.readout}"
    height: "26px"
    padding: "0 10px"
  flyout-button-hover:
    backgroundColor: "{colors.night-hover}"
  flyout-button-pressed:
    backgroundColor: "{colors.night-track}"
  tray-badge:
    backgroundColor: "{colors.night-plate}"
    textColor: "{colors.night-ink}"
    rounded: "2px"
    width: "16px"
    height: "16px"
---

# Design System: AI Usage Monitor

## Overview

**Creative North Star: "The Glass Cockpit"**

The monitor is drawn as one instrument panel in the manner of an EICAS engine display: a black glass plate set into the taskbar (a pale plate when the taskbar is light), with an engine tape for every usage window. The fill shows what has been spent. A magenta bug shows where the clock says you should be. The gap between the two is the pace, so the reading is geometric: fill divided by bug is the pace, exactly, because the bug sits at the same clamped elapsed fraction that `pace.rs` divides by (`pace::elapsed_fraction`, floor `DEFAULT_MIN_ELAPSED_FRACTION` = 0.1). The direction contract lives in the opening comment of `src/cockpit.rs`, and the confirmed brief in `.impeccable/shape-brief-glass-cockpit.md`.

The panel is dense and quiet. It sets out to replace the category's usual kit (a text label, a segmented progress bar, a row of percentages) with instrument marks: boxed tabular readouts, one-pixel frames, quarter ticks cut through the tape, and a single pointer. Colour means pace and nothing else. Green, amber and brick red are the three bands. Magenta is the bug. Every other state is said with a mark or a word. Hierarchy in the widget comes from weight, frame and reversal, never from a second type size. The whole system is drawn by one GDI+ painter (`cockpit::Painter`) into per-pixel-alpha bitmaps, so the widget, the click-open flyout and the tray badges are the same instrument at three sizes.

It belongs to the Windows shell. It uses Bahnschrift, the DIN face Windows has shipped since 10 1709, lights up the way a taskbar button lights up, gets its rounded corners and drop shadow from the shell, follows the system light/dark setting, and has no motion except the one gesture the shell's own "Show animations" switch controls. The brief rejects the aviation costume: screws, glows, gradients and drop shadows inside the widget.

**Key Characteristics:**
- One instrument, three surfaces: taskbar widget, click-open flyout, tray badges, all from `src/cockpit.rs`.
- Pace is geometry first (fill against bug) and hue second (band colour).
- Two palettes (night, day), chosen by the Windows theme; the pace fills are user-configurable, and the text variants are derived from them.
- Boxed readouts show bare numbers with no `%` sign; the box is the unit.
- Absence and failure are marks and words, never red.
- The only motion is the reset drain, and it is skipped when Windows animations are off.

## Colors

A near-black (or near-white) neutral instrument ramp carrying three pace hues and one reserved magenta. Every value is defined in `Palette::new` (`src/cockpit.rs`), and the pace defaults are in `src/pace.rs` (`DEFAULT_COLOR_*`).

### Primary
The pace bands. These are the only hues that mean anything, and they are the user's: `settings.json` can override all three (`pace::Settings::sanitized`). The fills are the same in both themes.
- **Runway Green** (pace-on-track): fills a tape whose pace is under the on-track threshold (default 85). It appears only as a fill. An on-track readout stays in instrument ink.
- **Caution Amber** (pace-at-risk): fills a tape between 85 and 115. At night the readout text and its frame use the same amber.
- **Brick Warning Red** (pace-over): fills a tape at 115 or above, and is the ground of the one reversed readout (see The One Hot Readout Rule).

The three bands must differ in lightness as well as hue, for colour-blind readers. The test `day_and_night_keep_the_bands_apart_in_lightness` enforces this (amber more than 20 luminance units above green, green more than 5 above red, and the at-risk and over text more than 60 away from the plate).

### Secondary
- **Bug Magenta** (night-bug) and **Deep Bug Magenta** (day-bug): the bug triangle on every tape, plus the one copy of it that keys the legend in the flyout footer. Nothing else.

### Tertiary
Text variants derived from the pace fills in `Palette::new`, so readouts reach contrast without a separate palette:
- **Night Over Text** (night-over-text): the red fill mixed 25% toward white (`mix(fills[2], white, 0.25)`), for red numerals and frames on black glass. Night at-risk text uses the amber fill itself.
- **Day Amber Text** (day-at-risk-text): amber mixed 35% toward black. The code comment says it is darkened "only as far as 4.5:1 on the plate needs, so amber still reads as amber and not as brown".
- **Day Over Text** (day-over-text): red mixed 12% toward black.
- **Hot Ink** (hot-ink): numerals on the reversed red box. `Palette::hot_ink` switches to `#111111` if a user-chosen red is light (BT.601 luma above 150). With the default red it is white.

When the user supplies custom fills, these hexes are recomputed with the same mixes, so treat them as formulas, not constants.

### Neutral
Night (black glass) and day (lit plate) share one role table:
- **Black Glass / Lit Plate** (night-plate / day-plate): the widget plate, the flyout ground, the tray badge ground, and the colour the quarter ticks are cut in.
- **Lit Glass** (night-hover / day-hover): the plate on hover and while active, and a flyout button on hover.
- **Unlit Tape** (night-track / day-track): the unfilled tape, and a flyout button while pressed.
- **Panel Rule** (night-rule / day-rule): the flyout's horizontal rules and the frame around the alerts. At night it has the same value as the track.
- **Bezel** (night-bezel / day-bezel): the plate's one-pixel rim at rest, and the flyout's DWM window border (`DWMWA_BORDER_COLOR`).
- **Readout Frame** (night-box-line / day-box-line): the frame of a neutral readout, the plate rim on hover, the vertical tape's tick marks, and a flyout button's outline at rest.
- **Legend Grey** (night-dim / day-dim): provider codes, countdowns, group names, column labels, the "updated" stamp, the legend sentence, the strike line, the rim of the active plate, and a flyout button's outline on hover. It is also the outline of a neutral tray badge, because the Readout Frame is too faint at 16 px.
- **Instrument Ink** (night-ink / day-ink): readout numerals, flyout title, reset times, the cause of a failure, button labels, the focus ring, the pointer at every tape's fill end, and the tape fill itself when pace colouring is off.

The non-embedded fallback window (`window.rs::paint`) paints the plate over an opaque ground (`#1C1C1C` night, `#F3F3F3` day), because that window has no per-pixel alpha. This is a fallback ground, not a palette role.

### Named Rules
**The One Hot Readout Rule.** On each surface (widget rows, flyout columns, tray icons, each counted separately), only values in the red band compete, and only the one with the highest pace is reversed: a red box with Hot Ink. Other red values get red numerals and a red frame. Amber gets amber numerals and an amber frame. On-track values stay neutral. If nothing is red, nothing is reversed. See `heat_of` and `hottest` in `src/window.rs`.

**The Bug Magenta Rule.** Magenta is reserved for the bug. The footer legend draws the bug itself to key it; nothing else on any surface may be magenta.

**The Quiet Green Rule.** On-track readouts use Instrument Ink, not green (`Palette::text`). Green numerals would add noise to a value that needs no attention, so green lives only in fills.

## Typography

**Value Face:** Bahnschrift SemiBold SemiCondensed (GDI+ family name `Bahnschrift SemiBold SemiConden`, truncated to 31 characters), falling back to Bahnschrift SemiBold, Segoe UI Semibold, Segoe UI
**Text Face:** Bahnschrift SemiCondensed, falling back to Bahnschrift, Segoe UI
**Prose Face:** Segoe UI Variable Text, falling back to Segoe UI

**Character:** a condensed DIN for every number, code and legend, as on a panel placard, with the shell's own text face kept for the one sentence of prose. Weight separates value from text: SemiBold for readouts, codes and labels (`Face::Value`), Regular for countdowns and reset times (`Face::Text`). The faces are selected in `Painter::wrap` and drawn anti-aliased with grid fitting, in a typographic string format with no wrap and no clip.

### Hierarchy
- **Widget value** (Value, 12 px, untracked): provider or window code (Legend Grey) and readout numerals. At three rows it drops to 10 px (`widget_font`).
- **Widget countdown** (Text, 12 px, or 10 px at three rows, untracked): time to reset, in Legend Grey. Its column is as wide as the widest countdown the current language and format can print (`countdown_samples`), so it does not change width as the minutes tick.
- **Flyout title** (Value, 13 px, +0.08 em, capitals): "USAGE", in ink. It is the only step above the labels.
- **Flyout readout** (Value, 14 px, untracked): the boxed number under each vertical tape.
- **Flyout alert** (Value, 12 px, capitals, untracked): projection lines, coloured by band.
- **Flyout reset / countdown** (Text, 12 px ink / 11 px dim): exact local reset time (Windows regional format) and the detailed countdown.
- **Flyout label** (Value, 11 px, +0.08 em, capitals): the updated stamp, group names, failure causes, column labels, button labels.
- **Legend** (Prose, 12 px, sentence case, Legend Grey): the single line "Where the clock says you should be".

Sizes are logical pixels at 96 DPI, multiplied by the monitor scale `k` (`fly_fonts`, `widget_font`).

### Named Rules
**The One Size Rule.** Every letter on the widget plate is one size: 12 px, or 10 px when three rows share the plate. It is untracked. Hierarchy comes from weight, frame and reversal. Only the flyout has room for type steps.

**The Tracked Legend Rule.** The flyout's capital legends are letter-spaced +0.08 em (`TRACK`), glyph by glyph, because GDI+ has no letter-spacing option. This applies to Latin, Greek and Cyrillic only (`trackable`: every code point below U+0530). CJK labels fall back through DrawString, which does font fallback where the glyph path cannot, and are set solid, as those scripts normally are.

## Layout

All geometry is in logical pixels, rounded to device pixels by `px(k, v)`. A hairline is at least one device pixel (`hairline`). Integer coordinates land on pixel edges (`PixelOffsetModeHalf`), so rectangles stay crisp.

**Widget.** The window is 46 px tall (`WIDGET_HEIGHT`, capped by the taskbar). It is bottom-anchored and draggable along any taskbar. The plate is 40 px (`PLATE_H`), centred one pixel above the window's centre. Its width follows its content (`widget_layout`): `PAD_X` 7 px on both sides, then the code column, `CODE_GAP` 5, the 62 px tape (`TAPE_W`), `TAPE_GAP` 6, the readout box (as wide as "100" or "--" plus 3 px on each side), `BOX_GAP` 4, then the countdown column. Rows are 15 px with 3 px gaps for one or two rows, and 11 px with 1 px gaps for three.
- Two or three providers get one row each: code `CL` / `CX` / `AG` (never translated), the 5-hour tape, and the 7-day hairline under it.
- A single provider gets one row per window instead, coded `5H` / `7D` / the per-model name in capitals, with no hairline.

**Flyout.** It is a content-sized popup centred on the widget, `GAP` 12 px above it (or below, for a top taskbar), and clamped 12 px inside the monitor's work area (`flyout.rs::place`). Padding is 16 px horizontal and 12 px vertical. From top to bottom:
- a 16 px header row (title left, updated stamp right);
- an alerts frame (10 px inset, 18 px per line, at most three lines, soonest limit first), or 14 px of air when there are none;
- one group per provider, `GROUP_GAP` 20 px apart. Each group has a name, a rule 7 px below it, then columns `COL_GAP` 10 px apart and at least `COL_MIN_W` 56 px wide;
- a footer rule 9 px above the 26 px buttons, with the legend at left and Refresh and Settings at right, 8 px apart.

### Named Rules
**The One Scale Rule.** Every tape is one 0–100 scale with quarter ticks. On the widget tape, 1 px cuts in the plate colour at 25, 50 and 75 run through track and fill alike. On the flyout's vertical tape, ticks sit 2 px left of the tape: long (7 px) at 0, 50 and 100, short (5 px) at 25 and 75, in the Readout Frame colour.

**The Sliver Rule.** Any positive use shows at least one pixel of fill (`filled_width`). A sliver of use is never drawn as nothing.

## Elevation & Depth

The system is flat. Depth inside the widget comes from tone: the plate is darker (or, by day, lighter) than the taskbar, the track is a step off the plate, and a one-pixel rim closes the edge. The widget carries no shadow, glow or gradient. The flyout is the one elevated surface, and its elevation is the shell's: the window-class drop shadow (`CS_DROPSHADOW`), Windows 11 rounded corners (`DWMWCP_ROUND`), and a DWM border in the Bezel colour. The painter draws none of this.

### Named Rules
**The Borrowed Depth Rule.** Only the shell lifts anything. Painted surfaces stay flat, and the one floating panel gets its shadow and corners from DWM, so it looks like a Windows flyout and not like an overlay.

## Shapes

Small, near-square radii and one-pixel strokes, drawn inside the shape (`stroke_round` insets by half the line):
- the plate: 4 px radius, 1 px rim;
- readout boxes and flyout buttons: 2 px radius;
- the alerts frame: 3 px radius;
- the keyboard focus ring: 4 px radius;
- tray badges: size / 8 radius (2 px at 16 px), outline size / 16 (1 px at 16 px);
- the flyout window: whatever corners DWM gives it.

Tapes and hairlines are square-ended rectangles. The bug is a solid triangle: in the widget it points down from above the tape (6 px wide, 4 px tall, or 3 px tall at three rows), and in the flyout it points in from the right of the vertical tape (6 px deep, 8 px tall, 1 px off the tape).

## Components

### Widget plate
A taskbar button with an instrument behind the glass.
- **Structure:** code, tape, boxed readout, countdown, per row (`paint_widget`).
- **States:** Rest has the plate fill and a Bezel rim. Hover has Lit Glass and a Readout Frame rim. Active has Lit Glass and a Legend Grey rim, and applies while pressed (and not dragging) or while its flyout is open (`widget_model`). The tape track stays darker than all three, so it never dissolves into a lit plate.
- **Interaction:** the whole plate is the drag handle. A move past `SM_CXDRAG` / `SM_CYDRAG` drags it, including onto another monitor's taskbar, and hides the flyout. A release without a drag toggles the flyout. The widget has no hover tooltip; the flyout replaced it.

### Engine tape (widget)
- **Track:** Unlit Tape, 62 px wide. It is 5 px tall above a hairline (4 px at three rows), or 7 / 6 px tall with no hairline.
- **Fill:** the band colour, or Instrument Ink when there is no band (pace colouring off, or no reset time to judge by).
- **Pointer:** a 1 px Instrument Ink line at the fill's end.
- **Bug:** a magenta triangle above the tape at the elapsed fraction. It is drawn only when the window was reported.
- **7-day hairline:** 2 px, 1 px below the tape, track plus band fill. It has no ticks and no bug, and carries the same ink pointer at its fill end.
- **Unreported window:** a hollow tape (track and ticks only, no fill, no bug), the readout `--`, and an empty countdown.

### Boxed readout
The unit of the whole system, shared by the widget, flyout and tray (`readout_box`).
- **Neutral / on-track:** Instrument Ink numerals in a 1 px Readout Frame.
- **At-risk / over:** band-text numerals, with the frame in the same band text colour.
- **Hot:** a filled Brick Warning Red box with Hot Ink numerals. One per surface.
- **Content:** a bare whole number (`62`), or `--` when unreported. There is no `%` sign.
- **Size:** in the widget, row height, right-aligned, 3 px inner pad. In the flyout, 18 px tall, centred, at least 36 px wide, or the text plus 10 px.

### Flyout
It opens on a click on the widget and closes on Escape, on focus loss, or on a second click. A 300 ms reopen guard stops the closing click from reopening it.
- **Alerts:** a framed list of projections ("LIMIT IN ~33m AT THIS PACE"), each prefixed by the provider and window. The whole line is in the band's text colour. A projection appears only when the limit would land before the reset (`pace::time_to_limit`), computed from the same clamped elapsed time as the pace.
- **Column:** tracked label, 10×92 px vertical tape filled from the bottom with an ink pointer on top, ticks at left, bug at right, then the boxed readout, the exact reset time in ink, and the detailed countdown in grey.
- **Failed provider:** the group name is struck through with a 1 px Legend Grey line, and the cause is written in ink at right ("SIGN IN AGAIN" when the sign-in was refused, "NO DATA" otherwise). Its columns show hollow tapes and `--`.
- **Buttons (Refresh, Settings):** 26 px tall, tracked 11 px capitals in ink, 1 px Readout Frame outline. Hover fills with Lit Glass and changes the outline to Legend Grey. Pressed fills with Unlit Tape. Keyboard focus draws a 1 px ink ring 3 px outside the button, but only after a key has been pressed, as Windows does. Tab and the arrow keys switch between the two buttons, and Enter or Space presses the focused one.

### Tray badge
The widget's readout at icon size (`paint_badge`), drawn at `SM_CXSMICON` so the outline lands on whole pixels.
- **Content:** the 5-hour value. Before any data arrives it shows the provider code, except Claude, which keeps the app icon.
- **Style:** Lit Plate or Black Glass ground. The outline is Legend Grey when neutral and band text when at risk or over. Hot badges are reversed under the same one-per-surface rule.
- **Fit:** numerals start at 0.72 of the square (letters at 0.62) and shrink in half-pixel steps until they fit. "100" gives up the inner margin rather than shrink to a smudge.

### Named Rules
**The Marks, Not Hues Rule.** A non-pace state is never a colour:
- an unreported window is a hollow tape plus `--`;
- a provider that could not be read is a struck code (in the widget) or a struck name plus the cause in words (in the flyout).

Red belongs to the pace.

**The Ink Pointer Rule.** Every tape carries a 1 px Instrument Ink pointer at its fill end, so the reading never rests on hue alone. This covers the widget's main tape, its 7-day hairline, and each flyout vertical tape. Amber on the pale day track is only about 1.6:1. The test `the_pointer_reads_against_the_track_in_both_themes` requires ink to clear 3:1 against the track in both themes.

**The Only Motion Rule.** The reset drain is the only animation. When a window rolls over (its reset jumps more than 30 minutes later and less has been spent), the fill runs from the old value to the new one over 250 ms (`DRAIN_DURATION`). It uses an exponential ease-out, `1 − 2^(−10t)`, at 16 ms frames (`TIMER_ANIM`). It is skipped entirely when Windows "Show animations" is off (`SPI_GETCLIENTAREAANIMATION`). The bug moves once a minute and the fill moves on each poll, both without transition.

## Do's and Don'ts

### Do:
- **Do** draw every new surface through `cockpit::Painter` with the `Palette` roles, in both night and day. Check the night/day pair at 1x and 2x (`cargo test render_previews -- --ignored`).
- **Do** keep the widget to one type size: 12 px, or 10 px at three rows, untracked.
- **Do** track flyout capitals +0.08 em for Latin, Greek and Cyrillic only, and set CJK solid through the DrawString fallback.
- **Do** reverse only the single hottest red readout on each surface, and nothing when no value is red.
- **Do** put a 1 px ink pointer at the end of every tape fill, and keep every tape on the 0–100 scale with quarter ticks.
- **Do** say absence as `--` and a hollow tape, and failure as a struck code or name plus the cause in words.
- **Do** give the plate exactly three states, Rest, Hover and Active, lit the way the shell lights a taskbar button.
- **Do** derive band text colours from the user's fills with the `mix` rules, so custom pace colours stay legible.
- **Do** size columns from the widest string they can print, so nothing reflows as values tick.

### Don't:
- **Don't** use magenta for anything but the bug and its legend key.
- **Don't** use provider brand colours in the widget: Claude's `#D97757` orange collides with Caution Amber. Providers are told apart by their codes.
- **Don't** use red, or any band hue, for a failure, a missing value or an error. Red belongs to the pace.
- **Don't** show an invented `0` for a window nobody reported.
- **Don't** add the aviation costume: decorative bezels beyond the one-pixel rim, screws, glows, gradients or drop shadows inside the widget.
- **Don't** add motion beyond the reset drain, and never animate when Windows animations are off.
- **Don't** colour on-track numerals green, or add a `%` sign inside a readout box.
- **Don't** bring back a hover tooltip on the widget. The flyout, opened by a click, is the detail view.
