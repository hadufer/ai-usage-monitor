# Product

<!-- impeccable:product-schema 1 -->

## Platform

windows

Native Win32 desktop app (Rust, `windows` crate). No web layer. The design language it renders into is the Windows 10/11 shell: taskbar, notification area, flyouts, context menus.

## Users

Windows developers who already use Claude Code (CLI or app) and often Codex, sometimes Google Antigravity, on a paid plan with rolling usage limits. They run several agent sessions in parallel and keep asking the same question: how close am I to the limit, and when does it reset? They glance at the taskbar mid-task; they do not want to open a terminal, a browser, or `/usage` to find out.

## Product Purpose

A taskbar-resident monitor that answers "am I burning too fast, and when does it reset?" at a glance, for every enabled provider, without leaving the current task. Success is a user who never hits a limit by surprise and never has to type `/usage`.

## Positioning

- Coloured by consumption pace, not raw percentage: `pace = percentage * window / elapsed`, elapsed clamped to a tenth of the window. 40% used with four hours left reads differently from 40% with twenty minutes left.
- Lives inside the Windows taskbar itself, not in a floating window or a browser tab.
- Several providers side by side (Claude Code, Codex, Antigravity) from one widget, driven by one provider table.
- Reads the credentials the CLIs already have; no backend, no telemetry.

## Operating Context

- Always on screen, in the taskbar next to the notification area, on the primary monitor by default; draggable along the taskbar and onto another monitor's taskbar.
- Glanced at between prompts, a few seconds at a time, often while several Claude Code sessions run.
- A click on the widget opens the usage panel (a flyout) with every window, its exact reset time (Windows regional format, time zone aware) and a projection when the limit would land first; it replaced the hover tooltip. Dragging the widget moves it. Right-click opens the native context menu (refresh, models, frequency, language, start with Windows, settings, updates, exit). Left-click on the tray icon toggles the widget; hovering a tray icon shows its tooltip.
- One tray icon per enabled provider, badged with its 5-hour percentage.
- Follows the Windows light/dark setting and per-monitor DPI.

## Capabilities and Constraints

- Data per provider: 5-hour session window and 7-day window (percentage + reset time); Claude additionally reports a per-model weekly limit (`weekly_scoped`, label taken from the API, e.g. "Fable"). A window nobody reported shows `--`, never an invented `0%`.
- Failure per provider (no data, expired token) must be said in words or a mark, not in red: red belongs to pace.
- Height is bounded by the taskbar (widget is 46 logical px today, Windows 11 taskbar is 48); width competes with pinned apps and must stay small.
- Rendering: GDI + GDI+ into a per-pixel-alpha layered window, composited over the taskbar; fallback non-embedded window path exists.
- Pace thresholds and band colours are user-configurable in `settings.json` (defaults `#3F9142` / `#E8A33C` / `#C4402F`, 85 / 115); pace colouring can be turned off for flat brand colours.
- `detailed_time` switches the countdown between nearest hour (`4h`) and hours+minutes (`3h59m`).
- 11 UI languages; provider names and short labels are localized.
- Refresh interval 1 min to 1 hour; a 1-second countdown timer keeps the reset time live.

## Brand Commitments

- Pace colours (green / amber / brick red) stay, and the three bands must keep differing in lightness as well as hue, for colour-blind readers.
- The countdown to reset stays visible without hovering.
- Provider accents in use today: Claude `#D97757`, Codex black/white by theme, Antigravity `#4285F4`.
- Fork of CodeZeno/Claude-Code-Usage-Monitor; name "Claude Code Usage Monitor"; MIT.

## Evidence on Hand

- `.github/widget.png`: the current widget (rows 5h / 7d / Fable, 5-segment bars, `5% · 7m`).
- `x-post.png`, `linkedin-post.png`: launch visuals showing the widget at large scale.
- No user research, testimonials or usage statistics exist; do not invent any.

## Product Principles

1. The glance is the product: the answer must land in under a second, from across the desk.
2. Pace before percentage: how fast you are burning matters more than how much is gone.
3. Say absence honestly: `--` for unreported, a word for failure, never a fake zero.
4. Earn every taskbar pixel: width is borrowed from the user's pinned apps.
5. Belong to the shell: it should look like Windows made it, not like an overlay.

## Accessibility & Inclusion

Pace bands must stay distinguishable under common colour-vision deficiencies (lightness difference, not hue alone). Text must stay legible at 100% scaling on both light and dark taskbars.
