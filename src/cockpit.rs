//! Glass Cockpit: the widget, the flyout and the tray badges, drawn as one
//! instrument panel.
//!
//! THESIS: every usage window is an engine tape. The fill is what was spent, a
//! magenta bug is where the clock says you should be, and the gap between them
//! is the pace. Refuses the category's label, progress bar and percentage row.
//! OWN-WORLD: a black glass plate (a pale plate in day mode), Bahnschrift
//! SemiCondensed, boxed tabular readouts, one-pixel bezels, pace green, amber
//! and brick red, and magenta for the bug and nothing else.
//! STORY: a glance says which provider is burning ahead of its clock and when
//! it resets; a click opens every window, its exact reset and a projection.
//! FIRST VIEWPORT: a 40 px plate in the taskbar, one row per provider: code,
//! 5-hour tape with its bug, 7-day hairline under it, boxed %, countdown. Only
//! the hottest red readout is reversed.
//! FORM: glass cockpit (EICAS), 4th of 7 grounded directions, seed af696b2e.
//! FINISH: unreviewed and undocumented is unfinished; this build ends with the finish review, the verdict, DESIGN.md, and every shipping raster carrying its provenance

use std::ffi::c_void;
use std::sync::OnceLock;

use windows::core::PCWSTR;
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::GdiPlus;

use crate::native_interop::{self, Color};
use crate::pace::{self, Band};

// --- palette ------------------------------------------------------------------

/// Every colour the panel uses. Night is the black glass; day is the same
/// instrument lit for a light taskbar. The pace fills are the user's, from the
/// settings file; only the text variants are derived, for contrast.
#[derive(Clone, Copy, Debug)]
pub struct Palette {
    pub plate: Color,
    pub bezel: Color,
    pub track: Color,
    pub ink: Color,
    pub dim: Color,
    pub rule: Color,
    pub box_line: Color,
    pub bug: Color,
    pub hover: Color,
    fills: [Color; 3],
    texts: [Color; 3],
}

impl Palette {
    pub fn new(dark: bool, pace: &pace::Settings) -> Self {
        let fills = [pace.on_track_color, pace.at_risk_color, pace.over_color];
        let white = Color::new(0xFF, 0xFF, 0xFF);
        let black = Color::new(0, 0, 0);
        if dark {
            let ink = Color::from_hex("#E8EAED");
            Self {
                plate: Color::from_hex("#0A0C0F"),
                bezel: Color::from_hex("#2A2E33"),
                track: Color::from_hex("#22262B"),
                ink,
                dim: Color::from_hex("#8B929A"),
                rule: Color::from_hex("#22262B"),
                box_line: Color::from_hex("#454B52"),
                bug: Color::from_hex("#D86BD3"),
                hover: Color::from_hex("#16191D"),
                fills,
                texts: [ink, fills[1], mix(fills[2], white, 0.25)],
            }
        } else {
            let ink = Color::from_hex("#1B1F24");
            Self {
                plate: Color::from_hex("#FBFBFA"),
                bezel: Color::from_hex("#C4C8CD"),
                track: Color::from_hex("#DCDFE3"),
                ink,
                dim: Color::from_hex("#5A6169"),
                rule: Color::from_hex("#E1E4E8"),
                box_line: Color::from_hex("#9AA0A6"),
                bug: Color::from_hex("#A8309F"),
                hover: Color::from_hex("#EEF0F2"),
                fills,
                // Darkened only as far as 4.5:1 on the plate needs, so amber
                // still reads as amber and not as brown.
                texts: [ink, mix(fills[1], black, 0.35), mix(fills[2], black, 0.12)],
            }
        }
    }

    /// Fill of a gauge. Without a band (pace colouring off, or no reset time
    /// to judge by) the tape is plain instrument ink.
    pub fn fill(&self, band: Option<Band>) -> Color {
        band.map_or(self.ink, |band| self.fills[band as usize])
    }

    /// Ink of a readout. The on-track band stays neutral: green numbers would
    /// only add noise to a value that needs no attention.
    pub fn text(&self, band: Option<Band>) -> Color {
        band.map_or(self.ink, |band| self.texts[band as usize])
    }

    pub fn hot_fill(&self) -> Color {
        self.fills[Band::Over as usize]
    }

    pub fn hot_ink(&self) -> Color {
        if is_light(self.hot_fill()) {
            Color::from_hex("#111111")
        } else {
            Color::new(0xFF, 0xFF, 0xFF)
        }
    }

    /// Line around a readout: neutral unless the value needs attention.
    fn box_line_for(&self, band: Option<Band>) -> Color {
        match band {
            Some(Band::AtRisk | Band::Over) => self.text(band),
            _ => self.box_line,
        }
    }
}

fn mix(a: Color, b: Color, t: f64) -> Color {
    let channel = |a: u8, b: u8| (a as f64 + (b as f64 - a as f64) * t).round() as u8;
    Color::new(channel(a.r, b.r), channel(a.g, b.g), channel(a.b, b.b))
}

fn is_light(color: Color) -> bool {
    0.299 * color.r as f64 + 0.587 * color.g as f64 + 0.114 * color.b as f64 > 150.0
}

// --- what gets drawn --------------------------------------------------------

/// One tape. `fraction` is `None` for a window nobody reported: the tape is
/// drawn hollow rather than claiming a zero.
#[derive(Clone, Debug, Default)]
pub struct Gauge {
    pub fraction: Option<f64>,
    pub band: Option<Band>,
    pub bug: Option<f64>,
}

#[derive(Clone, Debug, Default)]
pub struct Readout {
    pub text: String,
    pub band: Option<Band>,
    /// The single most at-risk value on this surface: the only one reversed.
    pub hot: bool,
}

#[derive(Clone, Debug)]
pub struct WidgetRow {
    pub code: String,
    /// The provider could not be read: its code is struck through.
    pub failed: bool,
    pub gauge: Gauge,
    /// The 7-day hairline under the 5-hour tape, when rows are per provider.
    pub sub: Option<Gauge>,
    pub readout: Readout,
    pub countdown: String,
}

/// The plate answers the pointer the way taskbar buttons do: lit on hover,
/// and lit more while pressed or while its flyout is open.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PlateState {
    #[default]
    Rest,
    Hover,
    Active,
}

#[derive(Clone, Debug)]
pub struct WidgetModel {
    pub rows: Vec<WidgetRow>,
    pub plate: PlateState,
    pub palette: Palette,
    /// The widest countdowns this language and format can print, so the column
    /// does not change width as the minutes tick.
    pub countdown_samples: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct FlyoutColumn {
    pub label: String,
    pub gauge: Gauge,
    pub readout: Readout,
    pub reset: String,
    pub countdown: String,
}

#[derive(Clone, Debug)]
pub struct FlyoutGroup {
    pub name: String,
    /// Why the provider could not be read, in words; `None` when it was.
    pub cause: Option<String>,
    pub columns: Vec<FlyoutColumn>,
}

#[derive(Clone, Debug)]
pub struct Alert {
    pub who: String,
    pub text: String,
    pub band: Option<Band>,
}

#[derive(Clone, Debug)]
pub struct FlyoutModel {
    pub title: String,
    pub updated: String,
    pub alerts: Vec<Alert>,
    pub groups: Vec<FlyoutGroup>,
    pub legend: String,
    pub buttons: [String; 2],
    pub palette: Palette,
}

/// A tray icon: the same boxed readout as the widget's, at icon size.
#[derive(Clone, Debug)]
pub struct Badge {
    pub text: String,
    pub band: Option<Band>,
    pub hot: bool,
}

// --- painter ----------------------------------------------------------------

#[derive(Clone, Copy)]
pub enum Face {
    /// Readouts, codes and labels.
    Value,
    /// Countdowns and reset times.
    Text,
    /// The one line of prose, the legend.
    Prose,
}

#[derive(Clone, Copy)]
pub enum Align {
    Left,
    Center,
    Right,
}

static GDIPLUS_READY: OnceLock<bool> = OnceLock::new();

/// Starts GDI+ once. The session is deliberately never shut down: the process
/// ends with it, and tearing it down while a paint might be in flight buys
/// nothing.
fn ensure_gdiplus() -> bool {
    *GDIPLUS_READY.get_or_init(|| unsafe {
        let input = GdiPlus::GdiplusStartupInput {
            GdiplusVersion: 1,
            DebugEventCallback: 0,
            SuppressBackgroundThread: false.into(),
            SuppressExternalCodecs: false.into(),
        };
        let mut token: usize = 0;
        GdiPlus::GdiplusStartup(&mut token, &input, std::ptr::null_mut()).0 == 0
    })
}

fn argb(colour: Color) -> u32 {
    0xFF00_0000 | (colour.r as u32) << 16 | (colour.g as u32) << 8 | colour.b as u32
}

/// Draws into a 32-bit premultiplied bitmap through GDI+, so every edge and
/// glyph is anti-aliased into the alpha channel: the widget composites over
/// whatever the taskbar shows, light, dark or translucent.
pub struct Painter {
    graphics: *mut GdiPlus::GpGraphics,
    bitmap: *mut GdiPlus::GpBitmap,
    families: [*mut GdiPlus::GpFontFamily; 3],
    format: *mut GdiPlus::GpStringFormat,
    _scratch: Vec<u32>,
}

impl Painter {
    /// Wraps the pixel buffer of a top-down 32-bit DIB of `width` by `height`.
    pub fn new(bits: *mut c_void, width: i32, height: i32) -> Option<Self> {
        Self::wrap(bits, width, height, Vec::new())
    }

    /// A painter over a scratch pixel, for measuring text before the real
    /// bitmap's size is known.
    pub fn measuring() -> Option<Self> {
        let mut scratch = vec![0u32; 1];
        let bits = scratch.as_mut_ptr() as *mut c_void;
        Self::wrap(bits, 1, 1, scratch)
    }

    fn wrap(bits: *mut c_void, width: i32, height: i32, scratch: Vec<u32>) -> Option<Self> {
        if bits.is_null() || width <= 0 || height <= 0 || !ensure_gdiplus() {
            return None;
        }

        unsafe {
            let mut bitmap: *mut GdiPlus::GpBitmap = std::ptr::null_mut();
            if GdiPlus::GdipCreateBitmapFromScan0(
                width,
                height,
                width * 4,
                // PixelFormat32bppPARGB, from Gdipluspixelformats.h.
                0x000E_200B,
                Some(bits as *const u8),
                &mut bitmap,
            )
            .0 != 0
                || bitmap.is_null()
            {
                return None;
            }

            let mut graphics: *mut GdiPlus::GpGraphics = std::ptr::null_mut();
            if GdiPlus::GdipGetImageGraphicsContext(bitmap as *mut GdiPlus::GpImage, &mut graphics).0
                != 0
                || graphics.is_null()
            {
                GdiPlus::GdipDisposeImage(bitmap as *mut GdiPlus::GpImage);
                return None;
            }
            let _ = GdiPlus::GdipSetSmoothingMode(graphics, GdiPlus::SmoothingModeAntiAlias);
            // Integer coordinates land on pixel edges, so rectangles stay crisp.
            let _ = GdiPlus::GdipSetPixelOffsetMode(graphics, GdiPlus::PixelOffsetModeHalf);
            let _ = GdiPlus::GdipSetTextRenderingHint(
                graphics,
                GdiPlus::TextRenderingHintAntiAliasGridFit,
            );

            // Typographic, so measured widths carry no GDI+ padding; no wrap and
            // no clip, so a string never breaks or vanishes in a tight box.
            let mut typographic: *mut GdiPlus::GpStringFormat = std::ptr::null_mut();
            let mut format: *mut GdiPlus::GpStringFormat = std::ptr::null_mut();
            if GdiPlus::GdipStringFormatGetGenericTypographic(&mut typographic).0 != 0
                || GdiPlus::GdipCloneStringFormat(typographic, &mut format).0 != 0
                || format.is_null()
            {
                GdiPlus::GdipDeleteGraphics(graphics);
                GdiPlus::GdipDisposeImage(bitmap as *mut GdiPlus::GpImage);
                return None;
            }
            let _ = GdiPlus::GdipSetStringFormatFlags(
                format,
                GdiPlus::StringFormatFlagsNoWrap.0
                    | GdiPlus::StringFormatFlagsNoClip.0
                    | GdiPlus::StringFormatFlagsMeasureTrailingSpaces.0,
            );
            let _ = GdiPlus::GdipSetStringFormatLineAlign(format, GdiPlus::StringAlignmentCenter);

            // Bahnschrift is the DIN face Windows ships since 10 1709. Its
            // named instances are families of their own to GDI+, truncated to
            // 31 characters; older systems fall back to Segoe UI.
            let families = [
                family(&[
                    "Bahnschrift SemiBold SemiConden",
                    "Bahnschrift SemiBold",
                    "Segoe UI Semibold",
                    "Segoe UI",
                ]),
                family(&["Bahnschrift SemiCondensed", "Bahnschrift", "Segoe UI"]),
                family(&["Segoe UI Variable Text", "Segoe UI"]),
            ];

            Some(Self {
                graphics,
                bitmap,
                families,
                format,
                _scratch: scratch,
            })
        }
    }

    fn brush(&self, colour: Color) -> Option<*mut GdiPlus::GpBrush> {
        let mut brush: *mut GdiPlus::GpSolidFill = std::ptr::null_mut();
        unsafe {
            if GdiPlus::GdipCreateSolidFill(argb(colour), &mut brush).0 != 0 || brush.is_null() {
                return None;
            }
        }
        Some(brush as *mut GdiPlus::GpBrush)
    }

    pub fn fill_rect(&self, x: i32, y: i32, width: i32, height: i32, colour: Color) {
        if width <= 0 || height <= 0 {
            return;
        }
        let Some(brush) = self.brush(colour) else { return };
        unsafe {
            let _ = GdiPlus::GdipFillRectangle(
                self.graphics,
                brush,
                x as f32,
                y as f32,
                width as f32,
                height as f32,
            );
            GdiPlus::GdipDeleteBrush(brush);
        }
    }

    pub fn fill_round(&self, x: i32, y: i32, width: i32, height: i32, radius: f32, colour: Color) {
        let Some(path) = round_path(x as f32, y as f32, width as f32, height as f32, radius) else {
            return;
        };
        if let Some(brush) = self.brush(colour) {
            unsafe {
                let _ = GdiPlus::GdipFillPath(self.graphics, brush, path);
                GdiPlus::GdipDeleteBrush(brush);
            }
        }
        unsafe {
            GdiPlus::GdipDeletePath(path);
        }
    }

    /// A rounded outline drawn inside the rectangle, `line` pixels thick.
    pub fn stroke_round(
        &self,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        radius: f32,
        line: f32,
        colour: Color,
    ) {
        let inset = line / 2.0;
        let Some(path) = round_path(
            x as f32 + inset,
            y as f32 + inset,
            width as f32 - line,
            height as f32 - line,
            (radius - inset).max(0.0),
        ) else {
            return;
        };
        unsafe {
            let mut pen: *mut GdiPlus::GpPen = std::ptr::null_mut();
            if GdiPlus::GdipCreatePen1(argb(colour), line, GdiPlus::UnitPixel, &mut pen).0 == 0
                && !pen.is_null()
            {
                let _ = GdiPlus::GdipDrawPath(self.graphics, pen, path);
                GdiPlus::GdipDeletePen(pen);
            }
            GdiPlus::GdipDeletePath(path);
        }
    }

    pub fn triangle(&self, points: [(f32, f32); 3], colour: Color) {
        let Some(brush) = self.brush(colour) else { return };
        let points = points.map(|(x, y)| GdiPlus::PointF { X: x, Y: y });
        unsafe {
            let _ = GdiPlus::GdipFillPolygon(
                self.graphics,
                brush,
                points.as_ptr(),
                points.len() as i32,
                GdiPlus::FillModeAlternate,
            );
            GdiPlus::GdipDeleteBrush(brush);
        }
    }

    fn font(&self, face: Face, px: f32) -> Option<*mut GdiPlus::GpFont> {
        let family = self.families[face as usize];
        if family.is_null() {
            return None;
        }
        let mut font: *mut GdiPlus::GpFont = std::ptr::null_mut();
        unsafe {
            if GdiPlus::GdipCreateFont(
                family,
                px,
                GdiPlus::FontStyleRegular.0,
                GdiPlus::UnitPixel,
                &mut font,
            )
            .0 != 0
                || font.is_null()
            {
                return None;
            }
        }
        Some(font)
    }

    /// Width of a string, measured by the engine that will draw it.
    pub fn measure(&self, text: &str, face: Face, px: f32) -> i32 {
        self.measure_f(text, face, px).ceil() as i32
    }

    /// Width of a string set with extra space after every character but the
    /// last, `tracking` being a fraction of the em, as instrument legends are.
    pub fn measure_tracked(&self, text: &str, face: Face, px: f32, tracking: f32) -> i32 {
        if !trackable(text) {
            return self.measure(text, face, px);
        }
        self.tracked_advances(text, face, px, tracking)
            .last()
            .map_or(0, |(x, width)| (x + width).ceil() as i32)
    }

    /// Where each character starts and how wide it is. Set character by
    /// character, which GDI+ has no letter-spacing option to do for us.
    fn tracked_advances(&self, text: &str, face: Face, px: f32, tracking: f32) -> Vec<(f32, f32)> {
        let mut x = 0.0;
        text.chars()
            .map(|ch| {
                let width = self.measure_f(ch.encode_utf8(&mut [0; 4]), face, px);
                let start = x;
                x += width + tracking * px;
                (start, width)
            })
            .collect()
    }

    /// Tracked capitals in a box, aligned as a whole.
    #[allow(clippy::too_many_arguments)]
    pub fn text_tracked(
        &self,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        text: &str,
        face: Face,
        px: f32,
        colour: Color,
        align: Align,
        tracking: f32,
    ) {
        // The glyph-by-glyph call below does no font fallback, and Bahnschrift
        // has no CJK: those labels go through DrawString, which falls back,
        // and are set solid, as those scripts are anyway.
        if !trackable(text) {
            self.text(x, y, width, height, text, face, px, colour, align);
            return;
        }
        let advances = self.tracked_advances(text, face, px, tracking);
        let total = advances.last().map_or(0.0, |(x, w)| x + w);
        let left = match align {
            Align::Left => x as f32,
            Align::Center => x as f32 + (width as f32 - total) / 2.0,
            Align::Right => (x + width) as f32 - total,
        };
        let family = self.families[face as usize];
        let Some(font) = self.font(face, px) else { return };
        let Some(brush) = self.brush(colour) else {
            unsafe {
                GdiPlus::GdipDeleteFont(font);
            }
            return;
        };
        unsafe {
            // The same baseline a one-line DrawString centred in this box
            // would use: the line box centred, the ascent down from its top.
            let style = GdiPlus::FontStyleRegular.0;
            let (mut em, mut ascent, mut spacing) = (0u16, 0u16, 0u16);
            let _ = GdiPlus::GdipGetEmHeight(family, style, &mut em);
            let _ = GdiPlus::GdipGetCellAscent(family, style, &mut ascent);
            let _ = GdiPlus::GdipGetLineSpacing(family, style, &mut spacing);
            let em = em.max(1) as f32;
            let baseline = y as f32 + (height as f32 - px * spacing as f32 / em) / 2.0
                + px * ascent as f32 / em;

            // Every glyph at its own position in one call: laying out each
            // letter as a string of its own let GDI+ shrink and lift some.
            let wide: Vec<u16> = text.encode_utf16().collect();
            let positions: Vec<GdiPlus::PointF> = text
                .chars()
                .zip(&advances)
                .flat_map(|(ch, (start, _))| {
                    let point = GdiPlus::PointF {
                        X: (left + start).round(),
                        Y: baseline.round(),
                    };
                    std::iter::repeat(point).take(ch.len_utf16())
                })
                .collect();
            let _ = GdiPlus::GdipDrawDriverString(
                self.graphics,
                wide.as_ptr(),
                wide.len() as i32,
                font,
                brush,
                positions.as_ptr(),
                GdiPlus::DriverStringOptionsCmapLookup.0,
                std::ptr::null(),
            );
            GdiPlus::GdipDeleteBrush(brush);
            GdiPlus::GdipDeleteFont(font);
        }
    }
    fn measure_f(&self, text: &str, face: Face, px: f32) -> f32 {
        if text.is_empty() {
            return 0.0;
        }
        let Some(font) = self.font(face, px) else { return 0.0 };
        let wide: Vec<u16> = text.encode_utf16().collect();
        let layout = GdiPlus::RectF {
            X: 0.0,
            Y: 0.0,
            Width: 4096.0,
            Height: 256.0,
        };
        let mut bounds = GdiPlus::RectF::default();
        let mut fitted = 0i32;
        let mut lines = 0i32;
        unsafe {
            let _ = GdiPlus::GdipSetStringFormatAlign(self.format, GdiPlus::StringAlignmentNear);
            let _ = GdiPlus::GdipMeasureString(
                self.graphics,
                PCWSTR::from_raw(wide.as_ptr()),
                wide.len() as i32,
                font,
                &layout,
                self.format as *const GdiPlus::GpStringFormat,
                &mut bounds,
                &mut fitted,
                &mut lines,
            );
            GdiPlus::GdipDeleteFont(font);
        }
        bounds.Width
    }

    /// One line of text in a box, centred vertically.
    #[allow(clippy::too_many_arguments)]
    pub fn text(
        &self,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        text: &str,
        face: Face,
        px: f32,
        colour: Color,
        align: Align,
    ) {
        self.text_f(x as f32, y as f32, width as f32, height as f32, text, face, px, colour, align);
    }

    #[allow(clippy::too_many_arguments)]
    fn text_f(
        &self,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        text: &str,
        face: Face,
        px: f32,
        colour: Color,
        align: Align,
    ) {
        if text.is_empty() || width <= 0.0 {
            return;
        }
        let Some(font) = self.font(face, px) else { return };
        let Some(brush) = self.brush(colour) else {
            unsafe {
                GdiPlus::GdipDeleteFont(font);
            }
            return;
        };
        let wide: Vec<u16> = text.encode_utf16().collect();
        let layout = GdiPlus::RectF {
            X: x,
            Y: y,
            Width: width,
            Height: height,
        };
        let alignment = match align {
            Align::Left => GdiPlus::StringAlignmentNear,
            Align::Center => GdiPlus::StringAlignmentCenter,
            Align::Right => GdiPlus::StringAlignmentFar,
        };
        unsafe {
            let _ = GdiPlus::GdipSetStringFormatAlign(self.format, alignment);
            let _ = GdiPlus::GdipDrawString(
                self.graphics,
                PCWSTR::from_raw(wide.as_ptr()),
                wide.len() as i32,
                font,
                &layout,
                self.format as *const GdiPlus::GpStringFormat,
                brush,
            );
            GdiPlus::GdipDeleteBrush(brush);
            GdiPlus::GdipDeleteFont(font);
        }
    }
}

impl Drop for Painter {
    fn drop(&mut self) {
        unsafe {
            GdiPlus::GdipDeleteStringFormat(self.format);
            for family in self.families {
                if !family.is_null() {
                    GdiPlus::GdipDeleteFontFamily(family);
                }
            }
            GdiPlus::GdipDeleteGraphics(self.graphics);
            GdiPlus::GdipDisposeImage(self.bitmap as *mut GdiPlus::GpImage);
        }
    }
}

/// Latin, Greek and Cyrillic: the scripts Bahnschrift draws itself, and the
/// only ones letter-spaced.
fn trackable(text: &str) -> bool {
    text.chars().all(|ch| (ch as u32) < 0x0530)
}

fn family(names: &[&str]) -> *mut GdiPlus::GpFontFamily {
    for name in names {
        let wide = native_interop::wide_str(name);
        let mut family: *mut GdiPlus::GpFontFamily = std::ptr::null_mut();
        let status = unsafe {
            GdiPlus::GdipCreateFontFamilyFromName(
                PCWSTR::from_raw(wide.as_ptr()),
                std::ptr::null_mut(),
                &mut family,
            )
        };
        if status.0 == 0 && !family.is_null() {
            return family;
        }
    }
    std::ptr::null_mut()
}

fn round_path(x: f32, y: f32, width: f32, height: f32, radius: f32) -> Option<*mut GdiPlus::GpPath> {
    if width <= 0.0 || height <= 0.0 {
        return None;
    }
    unsafe {
        let mut path: *mut GdiPlus::GpPath = std::ptr::null_mut();
        if GdiPlus::GdipCreatePath(GdiPlus::FillModeAlternate, &mut path).0 != 0 || path.is_null() {
            return None;
        }
        let d = (radius * 2.0).min(width).min(height);
        if d <= 0.0 {
            let _ = GdiPlus::GdipAddPathRectangle(path, x, y, width, height);
        } else {
            let _ = GdiPlus::GdipAddPathArc(path, x, y, d, d, 180.0, 90.0);
            let _ = GdiPlus::GdipAddPathArc(path, x + width - d, y, d, d, 270.0, 90.0);
            let _ = GdiPlus::GdipAddPathArc(path, x + width - d, y + height - d, d, d, 0.0, 90.0);
            let _ = GdiPlus::GdipAddPathArc(path, x, y + height - d, d, d, 90.0, 90.0);
            let _ = GdiPlus::GdipClosePathFigure(path);
        }
        Some(path)
    }
}

// --- shared pieces ----------------------------------------------------------

/// Logical pixels (designed at 96 DPI) to device pixels at scale `k`.
fn px(k: f32, value: f32) -> i32 {
    (value * k).round() as i32
}

fn hairline(k: f32) -> i32 {
    px(k, 1.0).max(1)
}

fn filled_width(fraction: f64, width: i32) -> i32 {
    if fraction <= 0.0 {
        return 0;
    }
    // A sliver of use still shows as a sliver, not as nothing.
    ((fraction.min(1.0) * width as f64).round() as i32).max(1)
}

/// The boxed value: outlined, in the band's ink; reversed when it is the hot one.
#[allow(clippy::too_many_arguments)]
fn readout_box(
    p: &Painter,
    pal: &Palette,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    readout: &Readout,
    font_px: f32,
    k: f32,
    align: Align,
) {
    let radius = 2.0 * k;
    let ink = if readout.hot {
        p.fill_round(x, y, width, height, radius, pal.hot_fill());
        pal.hot_ink()
    } else {
        p.stroke_round(
            x,
            y,
            width,
            height,
            radius,
            hairline(k) as f32,
            pal.box_line_for(readout.band),
        );
        pal.text(readout.band)
    };
    let pad = px(k, 3.0);
    p.text(
        x + pad,
        y,
        width - pad * 2,
        height,
        &readout.text,
        Face::Value,
        font_px,
        ink,
        align,
    );
}

// --- widget -----------------------------------------------------------------

const PLATE_H: f32 = 40.0;
const PAD_X: f32 = 7.0;
const CODE_GAP: f32 = 5.0;
const TAPE_W: f32 = 62.0;
const TAPE_GAP: f32 = 6.0;
const BOX_GAP: f32 = 4.0;

/// Column widths, measured once per render so the plate fits its content.
pub struct WidgetLayout {
    pub width: i32,
    code_w: i32,
    box_w: i32,
    countdown_w: i32,
    font_px: f32,
}

fn widget_font(rows: usize, k: f32) -> f32 {
    // One size for every letter on the plate; three rows get a smaller one.
    if rows <= 2 {
        12.0 * k
    } else {
        10.0 * k
    }
}

pub fn widget_layout(p: &Painter, model: &WidgetModel, k: f32) -> WidgetLayout {
    let font_px = widget_font(model.rows.len(), k);
    let code_w = model
        .rows
        .iter()
        .map(|row| p.measure(&row.code, Face::Value, font_px))
        .max()
        .unwrap_or(0);
    let box_w = ["100", "--"]
        .iter()
        .map(|text| p.measure(text, Face::Value, font_px))
        .max()
        .unwrap_or(0)
        + px(k, 3.0) * 2;
    let countdown_w = model
        .countdown_samples
        .iter()
        .chain(model.rows.iter().map(|row| &row.countdown))
        .map(|text| p.measure(text, Face::Text, font_px))
        .max()
        .unwrap_or(0);

    WidgetLayout {
        width: px(k, PAD_X) * 2
            + code_w
            + px(k, CODE_GAP)
            + px(k, TAPE_W)
            + px(k, TAPE_GAP)
            + box_w
            + px(k, BOX_GAP)
            + countdown_w,
        code_w,
        box_w,
        countdown_w,
        font_px,
    }
}

/// The plate, centred on the taskbar: `height` is the widget window's, which is
/// bottom-anchored, so the plate sits a pixel above the window's own centre.
pub fn paint_widget(p: &Painter, model: &WidgetModel, layout: &WidgetLayout, k: f32, height: i32) {
    // Hover and open light the glass the way the shell lights a taskbar
    // button; the tapes' track stays darker than either, so it never dissolves.
    let (glass, rim) = match model.plate {
        PlateState::Rest => (model.palette.plate, model.palette.bezel),
        PlateState::Hover => (model.palette.hover, model.palette.box_line),
        PlateState::Active => (model.palette.hover, model.palette.dim),
    };
    let pal = &Palette {
        plate: glass,
        ..model.palette
    };
    let plate_h = px(k, PLATE_H).min(height - px(k, 2.0)).max(1);
    let plate_top = ((height - plate_h) / 2 - px(k, 1.0)).max(0);
    let line = hairline(k);
    p.fill_round(0, plate_top, layout.width, plate_h, 4.0 * k, glass);
    p.stroke_round(0, plate_top, layout.width, plate_h, 4.0 * k, line as f32, rim);

    let n = model.rows.len().max(1) as i32;
    let (row_h, gap) = if n <= 2 {
        (px(k, 15.0), px(k, 3.0))
    } else {
        (px(k, 11.0), px(k, 1.0))
    };
    let total = row_h * n + gap * (n - 1);
    let first_y = plate_top + (plate_h - total) / 2;

    for (index, row) in model.rows.iter().enumerate() {
        let y = first_y + index as i32 * (row_h + gap);
        let mut x = px(k, PAD_X);

        p.text(x, y, layout.code_w, row_h, &row.code, Face::Value, layout.font_px, pal.dim, Align::Left);
        if row.failed {
            // Said as a mark rather than a colour: red belongs to the pace.
            let struck = p.measure(&row.code, Face::Value, layout.font_px);
            p.fill_rect(x - line, y + row_h / 2, struck + line * 2, line, pal.dim);
        }
        x += layout.code_w + px(k, CODE_GAP);

        widget_tape(p, pal, x, y, px(k, TAPE_W), row_h, row, n <= 2, k);
        x += px(k, TAPE_W) + px(k, TAPE_GAP);

        readout_box(p, pal, x, y, layout.box_w, row_h, &row.readout, layout.font_px, k, Align::Right);
        x += layout.box_w + px(k, BOX_GAP);

        p.text(x, y, layout.countdown_w, row_h, &row.countdown, Face::Text, layout.font_px, pal.dim, Align::Left);
    }
}

#[allow(clippy::too_many_arguments)]
fn widget_tape(
    p: &Painter,
    pal: &Palette,
    x: i32,
    y: i32,
    width: i32,
    row_h: i32,
    row: &WidgetRow,
    roomy: bool,
    k: f32,
) {
    let line = hairline(k);
    let bug_h = px(k, if roomy { 4.0 } else { 3.0 });
    let bar_h = px(
        k,
        match (row.sub.is_some(), roomy) {
            (true, true) => 5.0,
            (true, false) => 4.0,
            (false, true) => 7.0,
            (false, false) => 6.0,
        },
    );
    let sub_h = px(k, 2.0);
    let total = bug_h + bar_h + if row.sub.is_some() { line + sub_h } else { 0 };
    let top = y + (row_h - total) / 2;
    let bar_y = top + bug_h;

    let gauge = &row.gauge;
    p.fill_rect(x, bar_y, width, bar_h, pal.track);
    if let Some(fraction) = gauge.fraction {
        p.fill_rect(x, bar_y, filled_width(fraction, width), bar_h, pal.fill(gauge.band));
    }
    // Quarter marks cut through track and fill alike, like the rule on a tape.
    for quarter in [0.25, 0.5, 0.75] {
        let tick_x = x + (quarter * width as f64).round() as i32;
        p.fill_rect(tick_x, bar_y, line, bar_h, pal.plate);
    }
    // The pointer: where the fill ends, in ink, so the reading never rests on
    // hue alone (amber on a pale track is too close in lightness to trust).
    if let Some(fraction) = gauge.fraction.filter(|fraction| *fraction > 0.0) {
        let end = x + filled_width(fraction, width);
        p.fill_rect(end - line, bar_y, line, bar_h, pal.ink);
    }
    if let (Some(bug), Some(_)) = (gauge.bug, gauge.fraction) {
        let bug_x = x as f32 + (bug.min(1.0) * width as f64) as f32;
        let half = 3.0 * k;
        p.triangle(
            [
                (bug_x - half, top as f32),
                (bug_x + half, top as f32),
                (bug_x, (top + bug_h) as f32),
            ],
            pal.bug,
        );
    }

    if let Some(sub) = &row.sub {
        let sub_y = bar_y + bar_h + line;
        p.fill_rect(x, sub_y, width, sub_h, pal.track);
        if let Some(fraction) = sub.fraction.filter(|fraction| *fraction > 0.0) {
            let filled = filled_width(fraction, width);
            p.fill_rect(x, sub_y, filled, sub_h, pal.fill(sub.band));
            p.fill_rect(x + filled - line, sub_y, line, sub_h, pal.ink);
        }
    }
}

// --- flyout -----------------------------------------------------------------

const FLY_PAD_X: f32 = 16.0;
const FLY_PAD_Y: f32 = 12.0;
const COL_MIN_W: f32 = 56.0;
const COL_GAP: f32 = 10.0;
const GROUP_GAP: f32 = 20.0;
const VTAPE_H: f32 = 92.0;
const VTAPE_W: f32 = 10.0;
const BUTTON_H: f32 = 26.0;
/// Letter-spacing of the panel's capital legends, as a fraction of the em.
const TRACK: f32 = 0.08;

/// Where everything in the flyout goes, computed before the window is sized
/// and reused for hit-testing the two buttons.
pub struct FlyoutLayout {
    pub width: i32,
    pub height: i32,
    pub buttons: [RECT; 2],
    col_w: Vec<i32>,
    group_w: Vec<i32>,
    who_w: i32,
}

/// The panel's type sizes. Unlike the widget it has room for steps: the title
/// one up from the legends, readouts one up from the alerts.
struct FlyFonts {
    title: f32,
    label: f32,
    alert: f32,
    big: f32,
    reset: f32,
    small: f32,
    legend: f32,
}

fn fly_fonts(k: f32) -> FlyFonts {
    FlyFonts {
        title: 13.0 * k,
        label: 11.0 * k,
        alert: 12.0 * k,
        big: 14.0 * k,
        reset: 12.0 * k,
        small: 11.0 * k,
        legend: 12.0 * k,
    }
}

fn group_width(p: &Painter, group: &FlyoutGroup, col_w: i32, f: &FlyFonts, k: f32) -> i32 {
    let count = group.columns.len().max(1) as i32;
    let name_w = p.measure_tracked(&group.name, Face::Value, f.label, TRACK)
        + group.cause.as_ref().map_or(0, |cause| {
            px(k, 12.0) + p.measure_tracked(cause, Face::Value, f.label, TRACK)
        });
    (col_w * count + px(k, COL_GAP) * (count - 1)).max(name_w)
}

pub fn flyout_layout(p: &Painter, model: &FlyoutModel, k: f32) -> FlyoutLayout {
    let f = fly_fonts(k);

    let col_w: Vec<i32> = model
        .groups
        .iter()
        .map(|group| {
            group
                .columns
                .iter()
                .map(|column| {
                    [
                        p.measure_tracked(&column.label, Face::Value, f.label, TRACK),
                        p.measure(&column.reset, Face::Text, f.reset),
                        p.measure(&column.countdown, Face::Text, f.small),
                        p.measure(&column.readout.text, Face::Value, f.big) + px(k, 10.0),
                    ]
                    .into_iter()
                    .max()
                    .unwrap_or(0)
                        + px(k, 8.0)
                })
                .max()
                .unwrap_or(0)
                .max(px(k, COL_MIN_W))
        })
        .collect();
    let group_w: Vec<i32> = model
        .groups
        .iter()
        .zip(&col_w)
        .map(|(group, &width)| group_width(p, group, width, &f, k))
        .collect();
    let groups_w = group_w.iter().sum::<i32>()
        + px(k, GROUP_GAP) * (model.groups.len() as i32 - 1).max(0);

    let header_w = p.measure_tracked(&model.title, Face::Value, f.title, TRACK)
        + px(k, 24.0)
        + p.measure_tracked(&model.updated, Face::Value, f.label, TRACK);

    let who_w = model
        .alerts
        .iter()
        .map(|alert| p.measure(&alert.who, Face::Value, f.alert))
        .max()
        .unwrap_or(0);
    let alerts_w = model
        .alerts
        .iter()
        .map(|alert| p.measure(&alert.text, Face::Value, f.alert))
        .max()
        .map_or(0, |text_w| who_w + px(k, 14.0) + text_w + px(k, 20.0));

    let button_w: Vec<i32> = model
        .buttons
        .iter()
        .map(|label| p.measure_tracked(label, Face::Value, f.label, TRACK) + px(k, 20.0))
        .collect();
    let footer_w = px(k, 12.0)
        + p.measure(&model.legend, Face::Prose, f.legend)
        + px(k, 18.0)
        + button_w.iter().sum::<i32>()
        + px(k, 8.0);

    let content_w = groups_w.max(header_w).max(alerts_w).max(footer_w);
    let width = content_w + px(k, FLY_PAD_X) * 2;

    let alerts_h = if model.alerts.is_empty() {
        px(k, 14.0)
    } else {
        px(k, 10.0) + px(k, 16.0) + px(k, 18.0) * model.alerts.len() as i32 + px(k, 12.0)
    };
    let column_h = px(k, 12.0) + px(k, 6.0) + px(k, VTAPE_H) + px(k, 8.0) + px(k, 18.0)
        + px(k, 5.0) + px(k, 14.0) + px(k, 2.0) + px(k, 13.0);
    let groups_h = px(k, 12.0) + px(k, 7.0) + hairline(k) + px(k, 10.0) + column_h;
    let footer_h = px(k, 12.0) + hairline(k) + px(k, 9.0) + px(k, BUTTON_H);
    let height = px(k, FLY_PAD_Y) * 2 + px(k, 16.0) + alerts_h + groups_h + footer_h;

    let button_top = height - px(k, FLY_PAD_Y) - px(k, BUTTON_H);
    let mut right = width - px(k, FLY_PAD_X);
    let mut buttons = [RECT::default(); 2];
    for index in (0..2).rev() {
        buttons[index] = RECT {
            left: right - button_w[index],
            top: button_top,
            right,
            bottom: button_top + px(k, BUTTON_H),
        };
        right -= button_w[index] + px(k, 8.0);
    }

    FlyoutLayout {
        width,
        height,
        buttons,
        col_w,
        group_w,
        who_w,
    }
}

/// `hover` and `pressed` follow the mouse; `focus` is set only once the
/// keyboard has been used, as Windows does, and draws the ring.
pub fn paint_flyout(
    p: &Painter,
    model: &FlyoutModel,
    layout: &FlyoutLayout,
    k: f32,
    hover: Option<usize>,
    pressed: Option<usize>,
    focus: Option<usize>,
) {
    let pal = &model.palette;
    let f = fly_fonts(k);
    let line = hairline(k);
    let left = px(k, FLY_PAD_X);
    let right = layout.width - px(k, FLY_PAD_X);
    let content_w = right - left;

    p.fill_rect(0, 0, layout.width, layout.height, pal.plate);

    // The page's name in ink, one step up; what labels rather than measures
    // stays dim.
    let mut y = px(k, FLY_PAD_Y);
    let header_h = px(k, 16.0);
    p.text_tracked(left, y, content_w, header_h, &model.title, Face::Value, f.title, pal.ink, Align::Left, TRACK);
    p.text_tracked(left, y, content_w, header_h, &model.updated, Face::Value, f.label, pal.dim, Align::Right, TRACK);
    y += header_h;

    if model.alerts.is_empty() {
        y += px(k, 14.0);
    } else {
        y += px(k, 10.0);
        let box_h = px(k, 16.0) + px(k, 18.0) * model.alerts.len() as i32;
        p.stroke_round(left, y, content_w, box_h, 3.0 * k, line as f32, pal.rule);
        let mut line_y = y + px(k, 8.0);
        for alert in &model.alerts {
            let ink = pal.text(alert.band);
            let x = left + px(k, 10.0);
            p.text(x, line_y, layout.who_w, px(k, 18.0), &alert.who, Face::Value, f.alert, ink, Align::Left);
            let text_x = x + layout.who_w + px(k, 14.0);
            p.text(text_x, line_y, right - text_x - px(k, 10.0), px(k, 18.0), &alert.text, Face::Value, f.alert, ink, Align::Left);
            line_y += px(k, 18.0);
        }
        y += box_h + px(k, 12.0);
    }

    let mut group_x = left;
    for ((group, &col_w), &group_w) in model.groups.iter().zip(&layout.col_w).zip(&layout.group_w) {
        let label_h = px(k, 12.0);
        p.text_tracked(group_x, y, group_w, label_h, &group.name, Face::Value, f.label, pal.dim, Align::Left, TRACK);
        if let Some(cause) = &group.cause {
            // Struck like the widget's code, and the reason said in words.
            let name_w = p.measure_tracked(&group.name, Face::Value, f.label, TRACK);
            p.fill_rect(group_x - line, y + label_h / 2, name_w + line * 2, line, pal.dim);
            p.text_tracked(group_x, y, group_w, label_h, cause, Face::Value, f.label, pal.ink, Align::Right, TRACK);
        }
        let rule_y = y + label_h + px(k, 7.0);
        p.fill_rect(group_x, rule_y, group_w, line, pal.rule);

        let mut col_x = group_x;
        for column in &group.columns {
            flyout_column(p, pal, col_x, rule_y + line + px(k, 10.0), col_w, column, k, &f);
            col_x += col_w + px(k, COL_GAP);
        }
        group_x += group_w + px(k, GROUP_GAP);
    }

    // Footer: the legend that makes the bug readable, and the two actions.
    let button_top = layout.buttons[0].top;
    let rule_y = button_top - px(k, 9.0) - line;
    p.fill_rect(left, rule_y, content_w, line, pal.rule);

    let legend_mid = button_top + px(k, BUTTON_H) / 2;
    p.triangle(
        [
            (left as f32, legend_mid as f32),
            ((left + px(k, 6.0)) as f32, (legend_mid - px(k, 4.0)) as f32),
            ((left + px(k, 6.0)) as f32, (legend_mid + px(k, 4.0)) as f32),
        ],
        pal.bug,
    );
    let legend_x = left + px(k, 12.0);
    p.text(
        legend_x,
        button_top,
        layout.buttons[0].left - legend_x,
        px(k, BUTTON_H),
        &model.legend,
        Face::Prose,
        f.legend,
        pal.dim,
        Align::Left,
    );

    for (index, rect) in layout.buttons.iter().enumerate() {
        let (w, h) = (rect.right - rect.left, rect.bottom - rect.top);
        if pressed == Some(index) {
            p.fill_round(rect.left, rect.top, w, h, 2.0 * k, pal.track);
        } else if hover == Some(index) {
            p.fill_round(rect.left, rect.top, w, h, 2.0 * k, pal.hover);
        }
        let outline = if hover == Some(index) { pal.dim } else { pal.box_line };
        p.stroke_round(rect.left, rect.top, w, h, 2.0 * k, line as f32, outline);
        if focus == Some(index) {
            let gap = px(k, 2.0) + line;
            p.stroke_round(
                rect.left - gap,
                rect.top - gap,
                w + gap * 2,
                h + gap * 2,
                4.0 * k,
                line as f32,
                pal.ink,
            );
        }
        p.text_tracked(rect.left, rect.top, w, h, &model.buttons[index], Face::Value, f.label, pal.ink, Align::Center, TRACK);
    }
}

#[allow(clippy::too_many_arguments)]
fn flyout_column(
    p: &Painter,
    pal: &Palette,
    x: i32,
    y: i32,
    width: i32,
    column: &FlyoutColumn,
    k: f32,
    f: &FlyFonts,
) {
    let line = hairline(k);
    let mut y = y;
    p.text_tracked(x, y, width, px(k, 12.0), &column.label, Face::Value, f.label, pal.dim, Align::Center, TRACK);
    y += px(k, 12.0) + px(k, 6.0);

    // Vertical tape: ticks on the left, the bug on the right pointing in.
    let tape_h = px(k, VTAPE_H);
    let tape_w = px(k, VTAPE_W);
    let tape_x = x + (width - tape_w) / 2;
    p.fill_rect(tape_x, y, tape_w, tape_h, pal.track);
    let gauge = &column.gauge;
    if let Some(fraction) = gauge.fraction.filter(|fraction| *fraction > 0.0) {
        let filled = filled_width(fraction, tape_h);
        p.fill_rect(tape_x, y + tape_h - filled, tape_w, filled, pal.fill(gauge.band));
        // The pointer, in ink: the reading never rests on hue alone.
        p.fill_rect(tape_x, y + tape_h - filled, tape_w, line, pal.ink);
    }
    for (step, long) in [(0.0, true), (0.25, false), (0.5, true), (0.75, false), (1.0, true)] {
        let tick_y = y + tape_h - (step * tape_h as f64).round() as i32 - if step >= 1.0 { 0 } else { line };
        let length = px(k, if long { 7.0 } else { 5.0 });
        p.fill_rect(tape_x - px(k, 2.0) - length, tick_y, length, line, pal.box_line);
    }
    if let (Some(bug), Some(_)) = (gauge.bug, gauge.fraction) {
        let bug_y = (y + tape_h) as f32 - (bug.min(1.0) * tape_h as f64) as f32;
        let tip = (tape_x + tape_w + px(k, 1.0)) as f32;
        p.triangle(
            [
                (tip, bug_y),
                (tip + 6.0 * k, bug_y - 4.0 * k),
                (tip + 6.0 * k, bug_y + 4.0 * k),
            ],
            pal.bug,
        );
    }
    y += tape_h + px(k, 8.0);

    let box_w = px(k, 36.0).max(p.measure(&column.readout.text, Face::Value, f.big) + px(k, 10.0));
    readout_box(
        p,
        pal,
        x + (width - box_w) / 2,
        y,
        box_w,
        px(k, 18.0),
        &column.readout,
        f.big,
        k,
        Align::Center,
    );
    y += px(k, 18.0) + px(k, 5.0);

    p.text(x, y, width, px(k, 14.0), &column.reset, Face::Text, f.reset, pal.ink, Align::Center);
    y += px(k, 14.0) + px(k, 2.0);
    p.text(x, y, width, px(k, 13.0), &column.countdown, Face::Text, f.small, pal.dim, Align::Center);
}

// --- tray badge -------------------------------------------------------------

/// Paints a badge onto a `size` square buffer with straight (not premultiplied)
/// alpha, which is what an icon's colour bitmap carries.
pub fn paint_badge(bits: *mut c_void, size: i32, badge: &Badge, pal: &Palette) -> bool {
    let Some(p) = Painter::new(bits, size, size) else {
        return false;
    };
    let line = (size as f32 / 16.0).round().max(1.0);
    let radius = size as f32 / 8.0;
    let ink = if badge.hot {
        p.fill_round(0, 0, size, size, radius, pal.hot_fill());
        pal.hot_ink()
    } else {
        // Every badge keeps its box: the plate alone barely differs from the
        // taskbar, and the outline is what makes it a readout.
        p.fill_round(0, 0, size, size, radius, pal.plate);
        let outline = match badge.band {
            Some(Band::AtRisk | Band::Over) => pal.text(badge.band),
            // The widget's neutral line is too faint at 16 px.
            _ => pal.dim,
        };
        p.stroke_round(0, 0, size, size, radius, line, outline);
        pal.text(badge.band)
    };

    // Largest size that fits inside the outline, in half-pixel steps at 16 px.
    // Three characters ("100") give up the inner margin rather than shrink
    // to a smudge; they may touch the outline, never cross it.
    let margin = if badge.text.chars().count() < 3 { size / 10 } else { 0 };
    let room = size - (line as i32) * 2 - margin;
    let step = (size as f32 / 32.0).max(0.5);
    // Capped by height too: numerals at 0.8 of the square touch its edge, and
    // Bahnschrift's capitals stand taller than its numerals.
    let all_digits = badge.text.chars().all(|ch| ch.is_ascii_digit());
    let mut font_px = size as f32 * if all_digits { 0.72 } else { 0.62 };
    while font_px > 6.0 && p.measure(&badge.text, Face::Value, font_px) > room {
        font_px -= step;
    }
    p.text(0, 0, size, size, &badge.text, Face::Value, font_px, ink, Align::Center);
    drop(p);

    let pixels = unsafe { std::slice::from_raw_parts_mut(bits as *mut u32, (size * size) as usize) };
    for pixel in pixels.iter_mut() {
        let alpha = *pixel >> 24;
        if alpha > 0 && alpha < 255 {
            let unpremultiply = |shift: u32| ((((*pixel >> shift) & 0xFF) * 255 / alpha).min(255)) << shift;
            *pixel = (alpha << 24) | unpremultiply(16) | unpremultiply(8) | unpremultiply(0);
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn day_and_night_keep_the_bands_apart_in_lightness() {
        // The README's promise: the three bands differ in lightness as well as
        // hue, so they survive the common colour-vision deficiencies. The text
        // variants are the ones that change per theme; check both.
        let settings = pace::Settings::default();
        let luminance = |c: Color| 0.2126 * c.r as f64 + 0.7152 * c.g as f64 + 0.0722 * c.b as f64;
        for dark in [true, false] {
            let pal = Palette::new(dark, &settings);
            let fills: Vec<f64> = [Band::OnTrack, Band::AtRisk, Band::Over]
                .map(|band| luminance(pal.fill(Some(band))))
                .to_vec();
            assert!(fills[1] - fills[0] > 20.0 && fills[0] - fills[2] > 5.0);
            let plate = luminance(pal.plate);
            for band in [Band::AtRisk, Band::Over] {
                assert!((luminance(pal.text(Some(band))) - plate).abs() > 60.0);
            }
        }
    }

    #[test]
    fn the_pointer_reads_against_the_track_in_both_themes() {
        // A fill can sit close to its track in lightness (amber on the pale
        // day track is about 1.6:1), so the reading rests on the ink pointer
        // at the fill's end, which must clear 3:1 on its own.
        let contrast = |a: Color, b: Color| {
            let lum = |c: Color| {
                let lin = |v: u8| {
                    let v = v as f64 / 255.0;
                    if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
                };
                0.2126 * lin(c.r) + 0.7152 * lin(c.g) + 0.0722 * lin(c.b)
            };
            let (hi, lo) = if lum(a) > lum(b) { (lum(a), lum(b)) } else { (lum(b), lum(a)) };
            (hi + 0.05) / (lo + 0.05)
        };
        for dark in [true, false] {
            let pal = Palette::new(dark, &pace::Settings::default());
            assert!(contrast(pal.ink, pal.track) >= 3.0);
            for band in [Band::OnTrack, Band::AtRisk, Band::Over] {
                assert!(contrast(pal.ink, pal.fill(Some(band))) >= 1.5, "pointer visible on the fill too");
            }
        }
    }

    #[test]
    fn a_sliver_of_use_still_shows() {
        assert_eq!(filled_width(0.0, 62), 0);
        assert_eq!(filled_width(0.001, 62), 1);
        assert_eq!(filled_width(1.5, 62), 62);
    }

    // --- previews ---------------------------------------------------------
    //
    // `cargo test render_previews -- --ignored` draws every state the panel
    // has into target/cockpit-preview, at 100% and 200%, on the taskbar colours
    // Windows uses. Synthetic data: the same example the design mocks used.

    fn gauge(pct: f64, band: Option<Band>, bug: Option<f64>) -> Gauge {
        Gauge {
            fraction: Some(pct / 100.0),
            band,
            bug,
        }
    }

    fn readout(text: &str, band: Option<Band>, hot: bool) -> Readout {
        Readout {
            text: text.to_string(),
            band,
            hot,
        }
    }

    fn claude_row(code: &str) -> WidgetRow {
        WidgetRow {
            code: code.to_string(),
            failed: false,
            gauge: gauge(62.0, Some(Band::AtRisk), Some(0.567)),
            sub: Some(gauge(31.0, Some(Band::OnTrack), None)),
            readout: readout("62", Some(Band::AtRisk), false),
            countdown: "2h10m".to_string(),
        }
    }

    fn codex_row() -> WidgetRow {
        WidgetRow {
            code: "CX".to_string(),
            failed: false,
            gauge: gauge(71.0, Some(Band::Over), Some(0.267)),
            sub: Some(gauge(12.0, Some(Band::OnTrack), None)),
            readout: readout("71", Some(Band::Over), true),
            countdown: "3h40m".to_string(),
        }
    }

    fn failed_row() -> WidgetRow {
        WidgetRow {
            code: "AG".to_string(),
            failed: true,
            gauge: Gauge::default(),
            sub: Some(Gauge::default()),
            readout: readout("--", None, false),
            countdown: String::new(),
        }
    }

    fn widget(rows: Vec<WidgetRow>, dark: bool) -> WidgetModel {
        WidgetModel {
            rows,
            plate: PlateState::Rest,
            palette: Palette::new(dark, &pace::Settings::default()),
            countdown_samples: vec!["59m59s".into(), "23h59m".into(), "6d23h".into()],
        }
    }

    fn column(label: &str, g: Gauge, r: Readout, reset: &str, countdown: &str) -> FlyoutColumn {
        FlyoutColumn {
            label: label.to_string(),
            gauge: g,
            readout: r,
            reset: reset.to_string(),
            countdown: countdown.to_string(),
        }
    }

    fn flyout(dark: bool) -> FlyoutModel {
        FlyoutModel {
            title: "USAGE".into(),
            updated: "UPDATED 14:31".into(),
            alerts: vec![
                Alert {
                    who: "CODEX 5H".into(),
                    text: "LIMIT IN ~33m AT THIS PACE".into(),
                    band: Some(Band::Over),
                },
                Alert {
                    who: "CLAUDE 5H".into(),
                    text: "LIMIT IN ~1h44m AT THIS PACE".into(),
                    band: Some(Band::AtRisk),
                },
            ],
            groups: vec![
                FlyoutGroup {
                    name: "CLAUDE".into(),
                    cause: None,
                    columns: vec![
                        column("5H", gauge(62.0, Some(Band::AtRisk), Some(0.567)), readout("62", Some(Band::AtRisk), false), "16:42", "2h10m"),
                        column("7D", gauge(31.0, Some(Band::OnTrack), Some(0.417)), readout("31", Some(Band::OnTrack), false), "Fri 16:32", "4d02h"),
                        column("FABLE", gauge(18.0, Some(Band::OnTrack), Some(0.417)), readout("18", Some(Band::OnTrack), false), "Fri 16:32", "4d02h"),
                    ],
                },
                FlyoutGroup {
                    name: "CODEX".into(),
                    cause: None,
                    columns: vec![
                        column("5H", gauge(71.0, Some(Band::Over), Some(0.267)), readout("71", Some(Band::Over), true), "18:12", "3h40m"),
                        column("7D", gauge(12.0, Some(Band::OnTrack), Some(0.22)), readout("12", Some(Band::OnTrack), false), "Sat 01:32", "5d11h"),
                    ],
                },
                FlyoutGroup {
                    name: "ANTIGRAVITY".into(),
                    cause: Some("SIGN IN AGAIN".into()),
                    columns: vec![
                        column("5H", Gauge::default(), readout("--", None, false), "", ""),
                        column("7D", Gauge::default(), readout("--", None, false), "", ""),
                    ],
                },
            ],
            legend: "Where the clock says you should be".into(),
            buttons: ["REFRESH".into(), "SETTINGS".into()],
            palette: Palette::new(dark, &pace::Settings::default()),
        }
    }

    fn save_png(pixels: &mut [u32], width: i32, height: i32, path: &std::path::Path) {
        unsafe {
            let mut bitmap: *mut GdiPlus::GpBitmap = std::ptr::null_mut();
            GdiPlus::GdipCreateBitmapFromScan0(width, height, width * 4, 0x000E_200B, Some(pixels.as_ptr() as *const u8), &mut bitmap);
            let png = windows::core::GUID::from_u128(0x557cf406_1a04_11d3_9a73_0000f81ef32e);
            let name = native_interop::wide_str(&path.to_string_lossy());
            GdiPlus::GdipSaveImageToFile(bitmap as *mut GdiPlus::GpImage, PCWSTR::from_raw(name.as_ptr()), &png, std::ptr::null());
            GdiPlus::GdipDisposeImage(bitmap as *mut GdiPlus::GpImage);
        }
    }

    /// Draws `paint` over a taskbar-coloured ground, padded, into a PNG.
    fn preview(name: &str, width: i32, height: i32, ground: u32, paint: impl Fn(&Painter)) {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/cockpit-preview");
        std::fs::create_dir_all(&dir).unwrap();
        let mut pixels = vec![ground; (width * height) as usize];
        {
            let painter = Painter::new(pixels.as_mut_ptr() as *mut c_void, width, height).unwrap();
            paint(&painter);
        }
        save_png(&mut pixels, width, height, &dir.join(format!("{name}.png")));
    }

    #[test]
    #[ignore = "writes preview PNGs to target/cockpit-preview; run by hand"]
    fn render_previews() {
        // The app runs per-monitor aware; render under the same rules.
        unsafe {
            let _ = windows::Win32::UI::HiDpi::SetProcessDpiAwarenessContext(
                windows::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
            );
        }
        for (theme, dark, ground) in [("night", true, 0xFF1C_1C1C), ("day", false, 0xFFE9_EEF4)] {
            for k in [1.0f32, 2.0] {
                let scale = k as i32;
                let height = px(k, 46.0);
                let cases = [
                    ("two", vec![claude_row("CL"), codex_row()]),
                    ("three", vec![claude_row("CL"), codex_row(), failed_row()]),
                    (
                        "single",
                        vec![
                            WidgetRow { sub: None, ..claude_row("5H") },
                            WidgetRow {
                                sub: None,
                                gauge: gauge(31.0, Some(Band::OnTrack), Some(0.417)),
                                readout: readout("31", Some(Band::OnTrack), false),
                                countdown: "4d02h".into(),
                                ..claude_row("7D")
                            },
                            WidgetRow {
                                sub: None,
                                gauge: gauge(18.0, Some(Band::OnTrack), Some(0.417)),
                                readout: readout("18", Some(Band::OnTrack), false),
                                countdown: "4d02h".into(),
                                ..claude_row("FABLE")
                            },
                        ],
                    ),
                ];
                let open = WidgetModel {
                    plate: PlateState::Active,
                    ..widget(vec![claude_row("CL"), codex_row()], dark)
                };
                let cases = cases.into_iter().map(|(case, rows)| (case, widget(rows, dark))).chain([("open", open)]);
                for (case, model) in cases {
                    let measure = Painter::measuring().unwrap();
                    let layout = widget_layout(&measure, &model, k);
                    let pad = 8 * scale;
                    preview(&format!("widget-{case}-{theme}@{scale}x"), layout.width + pad * 2, height + pad * 2, ground, |p| {
                        // Paint in a sub-rectangle by drawing onto a shifted copy.
                        let mut pixels = vec![0u32; (layout.width * height) as usize];
                        {
                            let inner = Painter::new(pixels.as_mut_ptr() as *mut c_void, layout.width, height).unwrap();
                            paint_widget(&inner, &model, &layout, k, height);
                        }
                        blit(p, &pixels, layout.width, height, pad, pad);
                    });
                }

                let model = flyout(dark);
                let measure = Painter::measuring().unwrap();
                let layout = flyout_layout(&measure, &model, k);
                preview(&format!("flyout-{theme}@{scale}x"), layout.width, layout.height, ground, |p| {
                    paint_flyout(p, &model, &layout, k, Some(0), None, Some(1));
                });

                // The README's picture: the two providers the widget is best
                // at, one of them running ahead of its clock.
                let mut readme = flyout(dark);
                readme.groups.truncate(2);
                let layout = flyout_layout(&measure, &readme, k);
                preview(&format!("flyout-readme-{theme}@{scale}x"), layout.width, layout.height, ground, |p| {
                    paint_flyout(p, &readme, &layout, k, None, None, None);
                });

                // Scripts Bahnschrift does not cover must still read: CJK
                // falls back through DrawString, Cyrillic stays tracked.
                for (lang, title, updated, buttons, cause) in [
                    ("ja", "使用量", "14:31 に更新", ["更新", "設定"], "再ログインが必要"),
                    ("ru", "ИСПОЛЬЗОВАНИЕ", "ОБНОВЛЕНО 14:31", ["ОБНОВИТЬ", "ПАРАМЕТРЫ"], "ВОЙДИТЕ СНОВА"),
                ] {
                    let mut model = flyout(dark);
                    model.title = title.into();
                    model.updated = updated.into();
                    model.buttons = buttons.map(String::from);
                    model.groups[2].cause = Some(cause.into());
                    let measure = Painter::measuring().unwrap();
                    let layout = flyout_layout(&measure, &model, k);
                    preview(&format!("flyout-{lang}-{theme}@{scale}x"), layout.width, layout.height, ground, |p| {
                        paint_flyout(p, &model, &layout, k, None, None, None);
                    });
                }
            }

            // Tray badges at the sizes the notification area asks for at 100%,
            // 125% and 150%, one row each, magnified 4x without smoothing.
            let badges = [
                Badge { text: "62".into(), band: Some(Band::AtRisk), hot: false },
                Badge { text: "71".into(), band: Some(Band::Over), hot: true },
                Badge { text: "18".into(), band: Some(Band::OnTrack), hot: false },
                Badge { text: "100".into(), band: Some(Band::Over), hot: false },
                Badge { text: "CX".into(), band: None, hot: false },
            ];
            let pal = Palette::new(dark, &pace::Settings::default());
            let zoom = 4;
            let sizes = [16, 20, 24];
            let cell = 24 * zoom + 16;
            preview(&format!("badges-{theme}"), cell * badges.len() as i32, cell * sizes.len() as i32, ground, |p| {
                unsafe {
                    let _ = GdiPlus::GdipSetInterpolationMode(p.graphics, GdiPlus::InterpolationModeNearestNeighbor);
                }
                for (row, size) in sizes.into_iter().enumerate() {
                    for (index, badge) in badges.iter().enumerate() {
                        let mut pixels = vec![0u32; (size * size) as usize];
                        paint_badge(pixels.as_mut_ptr() as *mut c_void, size, badge, &pal);
                        // Back to premultiplied for the preview blend.
                        for pixel in pixels.iter_mut() {
                            let a = *pixel >> 24;
                            let channel = |shift: u32| (((*pixel >> shift) & 0xFF) * a / 255) << shift;
                            *pixel = (a << 24) | channel(16) | channel(8) | channel(0);
                        }
                        blit_scaled(p, &pixels, size, size, 8 + index as i32 * cell, 8 + row as i32 * cell, zoom);
                    }
                }
            });
        }
    }

    fn blit_scaled(p: &Painter, pixels: &[u32], width: i32, height: i32, x: i32, y: i32, zoom: i32) {
        unsafe {
            let mut source: *mut GdiPlus::GpBitmap = std::ptr::null_mut();
            GdiPlus::GdipCreateBitmapFromScan0(width, height, width * 4, 0x000E_200B, Some(pixels.as_ptr() as *const u8), &mut source);
            GdiPlus::GdipDrawImageRectI(p.graphics, source as *mut GdiPlus::GpImage, x, y, width * zoom, height * zoom);
            GdiPlus::GdipDisposeImage(source as *mut GdiPlus::GpImage);
        }
    }

    /// Composites a premultiplied buffer onto the painter's bitmap.
    fn blit(p: &Painter, pixels: &[u32], width: i32, height: i32, x: i32, y: i32) {
        unsafe {
            let mut source: *mut GdiPlus::GpBitmap = std::ptr::null_mut();
            GdiPlus::GdipCreateBitmapFromScan0(width, height, width * 4, 0x000E_200B, Some(pixels.as_ptr() as *const u8), &mut source);
            GdiPlus::GdipDrawImageRectI(p.graphics, source as *mut GdiPlus::GpImage, x, y, width, height);
            GdiPlus::GdipDisposeImage(source as *mut GdiPlus::GpImage);
        }
    }
}
