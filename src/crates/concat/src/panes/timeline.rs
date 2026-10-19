// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Jareer and Concat contributors

//! The timeline's view: where it is looking, how close, with which tool,
//! and what the lanes know about a track that the document does not.
//!
//! The pane owns the scroll, the zoom, the tool, the snap and pan
//! switches, and each lane's lock and height. It also knows how wide the
//! lanes are on screen, which Slint reports as they change, and from
//! that it says which clips are worth publishing: the ones that intersect
//! the visible window plus one screen either side, so a long cut costs
//! the window what is on screen and not what is in the document.
//!
//! The gestures - a clip dragged, trimmed or razored, a picture moved on
//! the stage - stay with the controller: one gesture spans the lanes and
//! the stage, and an echo of the document, and moves next.

use std::collections::HashMap;

use crate::studio::Studio;
use crate::ui::{TimelineTool, TrackSize};

/// The closest the timeline zooms: half a millisecond a pixel.
pub const ZOOM_IN_LIMIT: f32 = 0.000_5;
/// The farthest for a short cut: a second and a half a pixel. A longer one
/// may go farther; see `TimelinePane::zoom_out_limit`.
pub const ZOOM_OUT_LIMIT: f32 = 1.5;
/// One step of the menu's zoom, as a factor.
const ZOOM_STEP: f32 = 1.4;

/// What the window knows about a lane that the document does not: whether
/// it is locked, and how tall to draw it.
#[derive(Clone, Copy)]
pub struct LaneView {
    pub locked: bool,
    pub size: TrackSize,
}

impl Default for LaneView {
    fn default() -> Self {
        Self {
            locked: false,
            size: TrackSize::Auto,
        }
    }
}

/// Everything that can happen to the view.
#[derive(Clone, Debug)]
pub enum TimelineMsg {
    ToolChanged(TimelineTool),
    PanChanged(bool),
    SnapChanged(bool),
    /// The menu's Snap row: the other way round.
    SnapToggled,
    /// The magnetic timeline, from the tray's button or the Settings
    /// switch. A preference, not view state: it is remembered.
    /// https://github.com/jub0t/Concat/issues/106
    MagneticChanged(bool),
    /// The menu's Magnetic row: the other way round.
    MagneticToggled,
    /// The tray's preview axis button: the monitor follows the pointer.
    /// A preference, remembered.
    TrimFollowChanged(bool),
    /// The tray's meter button: the VU meters shown or put away.
    MetersShownChanged(bool),
    PreviewAxisChanged(bool),
    /// The button's menu: whether the sound under the pointer plays too.
    PreviewAxisAudioChanged(bool),
    /// The pointer is over the lanes at this many seconds.
    Hovered(f32),
    /// The pointer left the lanes.
    HoverEnded,
    /// The lanes scrolled sideways, to this many seconds from the start.
    Scrolled(f32),
    /// The wheel or a pinch: by a factor, about an instant; an anchor
    /// below zero keeps the left edge where it is.
    Zoomed {
        factor: f32,
        anchor: f32,
    },
    /// The whole cut across this many pixels.
    ZoomToFit(f32),
    ZoomIn,
    ZoomOut,
    /// The lanes are this wide on screen now, in logical pixels.
    Resized(f32),
    /// A project opened: the view starts at the beginning, nothing locked.
    Reset,
}

/// The timeline's view.
pub struct TimelinePane {
    /// Seconds from the start of the cut at the lanes' left edge.
    pub scroll_left: f32,
    pub seconds_per_pixel: f32,
    pub tool: TimelineTool,
    pub snap: bool,
    /// Hand tool: wheel and drag pan time instead of scrolling the stack.
    pub pan_mode: bool,
    pub lane_view: HashMap<String, LaneView>,
    /// The lanes' width on screen, in logical pixels; zero until Slint has
    /// said, and then every clip is published.
    pub width: f32,
    /// A project just opened and the lanes have not yet shown the whole
    /// cut. Cleared by the first fit, so a later resize keeps the zoom.
    fit_pending: bool,
    /// Keep re-fitting on every Resized until [`Self::end_fit_hold`]: set
    /// when the desk will maximize after open, so the first (pre-maximize)
    /// width is not the final auto-fit.
    pub fit_hold: bool,
    /// Bumped when a project opens, so the lane stack scrolls back to the
    /// top. The pane draws that scroll; Rust only says that it should.
    pub stack_home: i32,
    /// The playhead is held at the middle of the lanes and the cut moves
    /// under it: the phone's timeline, where a finger drags the lanes to
    /// scrub. `scroll_left` is then a fact about the playhead - half a
    /// screen before it, negative at the start of the cut, which is what
    /// puts the first clip's head at the middle - and the view follows every
    /// seek, every zoom and every resize. Off, the view is its own: it pages
    /// after the playhead and scrolls where it is put, as on a desk.
    pub centred: bool,
}

impl Default for TimelinePane {
    fn default() -> Self {
        Self {
            scroll_left: 0.0,
            seconds_per_pixel: 0.05,
            tool: TimelineTool::Select,
            snap: true,
            pan_mode: false,
            lane_view: HashMap::new(),
            width: 0.0,
            fit_pending: false,
            fit_hold: false,
            stack_home: 0,
            centred: false,
        }
    }
}

impl TimelinePane {
    /// Applies one message. The studio is the rest of the window; while
    /// this runs the studio's copy of the pane is a blank it must not read.
    pub fn update(&mut self, msg: TimelineMsg, studio: &mut Studio) {
        match msg {
            TimelineMsg::ToolChanged(tool) => self.tool = tool,
            TimelineMsg::PanChanged(on) => self.pan_mode = on,
            TimelineMsg::SnapChanged(on) => self.snap = on,
            TimelineMsg::SnapToggled => self.snap = !self.snap,
            TimelineMsg::MagneticChanged(on) => {
                studio.prefs.magnetic = on;
                studio.prefs.save(&studio.host.dirs);
            }
            TimelineMsg::MagneticToggled => {
                studio.prefs.magnetic = !studio.prefs.magnetic;
                studio.prefs.save(&studio.host.dirs);
            }
            TimelineMsg::TrimFollowChanged(on) => {
                studio.prefs.trim_follow = on;
                studio.prefs.save(&studio.host.dirs);
            }
            TimelineMsg::MetersShownChanged(on) => {
                studio.prefs.vu_meters = Some(on);
                studio.prefs.save(&studio.host.dirs);
                studio.wake_meters();
            }
            TimelineMsg::PreviewAxisChanged(on) => {
                studio.prefs.preview_axis = on;
                studio.prefs.save(&studio.host.dirs);
                if !on {
                    studio.end_hover();
                }
            }
            TimelineMsg::PreviewAxisAudioChanged(on) => {
                studio.prefs.preview_axis_audio = on;
                studio.prefs.save(&studio.host.dirs);
            }
            TimelineMsg::Hovered(seconds) => studio.hover(seconds),
            TimelineMsg::HoverEnded => studio.end_hover(),
            // A centred view may be scrolled off its playhead - a clip
            // dragged to the lanes' edge scrolls them - and comes back to it
            // at the next seek; the floor at zero is the desk's, where the
            // start of the cut is the left edge and nothing is before it.
            TimelineMsg::Scrolled(seconds) => self.scrolled(seconds),
            TimelineMsg::Zoomed { factor, anchor } => {
                let anchor = if anchor < 0.0 {
                    self.default_anchor(studio)
                } else {
                    anchor
                };
                let limit = self.zoom_out_limit(studio.duration());
                self.zoom(factor, anchor, limit);
                self.recentre(studio.playhead);
            }
            TimelineMsg::ZoomToFit(width) => {
                if width > 1.0 {
                    self.fit(studio.duration(), width);
                    self.recentre(studio.playhead);
                }
            }
            TimelineMsg::ZoomIn => {
                let anchor = self.default_anchor(studio);
                let limit = self.zoom_out_limit(studio.duration());
                self.zoom(1.0 / ZOOM_STEP, anchor, limit);
                self.recentre(studio.playhead);
            }
            TimelineMsg::ZoomOut => {
                let anchor = self.default_anchor(studio);
                let limit = self.zoom_out_limit(studio.duration());
                self.zoom(ZOOM_STEP, anchor, limit);
                self.recentre(studio.playhead);
            }
            TimelineMsg::Resized(width) => {
                self.resized_to(width, studio.duration());
                self.recentre(studio.playhead);
            }
            TimelineMsg::Reset => {
                self.lane_view.clear();
                self.opened(studio.duration());
                self.recentre(studio.playhead);
            }
        }
    }

    /// A project opened. The view returns to the start of the cut and, once
    /// the lanes have a width, shows the whole of it. A later resize does
    /// not fit again: that zoom is the one the tray's buttons then change.
    ///
    /// Width is cleared so a leftover size from a previous project cannot
    /// fit early and clear `fit_pending` before the new desk reports its
    /// real lane width (Reset runs before `on_start` flips). `fit_hold` is
    /// left alone: the opener sets it when a maximize will follow.
    fn opened(&mut self, _duration: f32) {
        self.scroll_left = 0.0;
        self.width = 0.0;
        self.fit_pending = true;
        self.stack_home = self.stack_home.wrapping_add(1);
    }

    /// The lanes are `width` pixels wide. The first time after [`opened`]
    /// that the width is real, the cut is fitted; after that only the
    /// width is kept — unless [`Self::fit_hold`], which refits every resize
    /// until [`Self::end_fit_hold`].
    pub fn resized_to(&mut self, width: f32, duration: f32) {
        self.width = width.max(0.0);
        self.fit_now(duration);
    }

    /// Fits the cut when a project open is still waiting on a width.
    fn fit_now(&mut self, duration: f32) {
        if self.fit_pending && self.width > 1.0 {
            self.fit(duration, self.width);
            if !self.fit_hold {
                self.fit_pending = false;
            }
        }
    }

    /// Maximize (or the phone's first layout) has settled: one last fit at
    /// the current width, then stop auto-fitting on resize.
    pub fn end_fit_hold(&mut self, duration: f32) {
        self.fit_hold = false;
        self.fit_pending = true;
        self.fit_now(duration);
    }

    /// The lanes scrolled to `seconds` at their left edge. See `Scrolled`.
    fn scrolled(&mut self, seconds: f32) {
        self.scroll_left = if self.centred {
            seconds
        } else {
            seconds.max(0.0)
        };
    }

    /// Puts the playhead at the middle of the lanes, when the view is the
    /// centred kind; nothing otherwise, and nothing until the lanes have
    /// said how wide they are. Called after every move of the playhead and
    /// every change of the view's scale or size, so the two never come
    /// apart.
    pub fn recentre(&mut self, playhead: f32) {
        if self.centred && self.width > 0.0 {
            self.scroll_left = playhead - self.width * self.seconds_per_pixel / 2.0;
        }
    }

    /// The anchor point for a zoom that has no pointer position (e.g. keyboard
    /// shortcut or tray buttons): the playhead if on screen, else the center of
    /// the visible timeline.
    fn default_anchor(&self, studio: &Studio) -> f32 {
        let playhead = f64::from(studio.playhead) as f32;
        let screen = self.width * self.seconds_per_pixel;
        if screen > 0.0 && (self.scroll_left..=self.scroll_left + screen).contains(&playhead) {
            playhead
        } else if screen > 0.0 {
            self.scroll_left + screen / 2.0
        } else {
            playhead.max(0.0)
        }
    }

    /// The farthest the view may zoom out on a cut `duration` seconds long:
    /// far enough that the whole cut takes about half the lanes, and never
    /// less than `ZOOM_OUT_LIMIT`. A fixed limit left a long cut that no
    /// zoom, the fit button included, could get onto one screen.
    fn zoom_out_limit(&self, duration: f32) -> f32 {
        if self.width > 0.0 {
            ZOOM_OUT_LIMIT.max(2.0 * duration.max(1.0) * 1.05 / self.width)
        } else {
            ZOOM_OUT_LIMIT
        }
    }

    /// The whole cut across lanes `width` pixels wide, from its start.
    fn fit(&mut self, duration: f32, width: f32) {
        let span = duration.max(1.0) * 1.05;
        let limit = ZOOM_OUT_LIMIT.max(2.0 * span / width);
        self.seconds_per_pixel = (span / width).clamp(ZOOM_IN_LIMIT, limit);
        self.scroll_left = 0.0;
    }

    /// Zooms by a factor about an instant, so the instant under the pointer
    /// stays under it; an anchor below zero holds the left edge instead.
    fn zoom(&mut self, factor: f32, anchor: f32, out_limit: f32) {
        let before = self.seconds_per_pixel;
        if !(factor.is_finite() && factor > 0.0) {
            return;
        }
        let after = (before * factor).clamp(ZOOM_IN_LIMIT, out_limit.max(ZOOM_IN_LIMIT));
        self.seconds_per_pixel = after;
        if anchor >= 0.0 {
            let left = anchor - (anchor - self.scroll_left) * (after / before);
            self.scroll_left = if self.centred { left } else { left.max(0.0) };
        }
    }

    /// The span of the cut worth publishing: what the lanes show, plus one
    /// screen either side so a scroll never waits on a publish. None until
    /// the lanes have said how wide they are, which means everything.
    pub fn published_span(&self) -> Option<(f32, f32)> {
        if self.width <= 0.0 {
            return None;
        }
        let screen = self.width * self.seconds_per_pixel;
        Some((self.scroll_left - screen, self.scroll_left + 2.0 * screen))
    }

    /// Whether a clip at `start` for `duration` seconds falls in a span;
    /// every clip does when there is no span.
    pub fn shows(span: Option<(f32, f32)>, start: f32, duration: f32) -> bool {
        match span {
            None => true,
            Some((from, to)) => start + duration.max(0.0) >= from && start <= to,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane(width: f32, scroll_left: f32, seconds_per_pixel: f32) -> TimelinePane {
        TimelinePane {
            width,
            scroll_left,
            seconds_per_pixel,
            ..TimelinePane::default()
        }
    }

    #[test]
    fn the_span_is_a_screen_either_side_of_the_view() {
        // 1000 px at 0.05 s/px is a 50 s screen, looking at 100..150.
        let (from, to) = pane(1000.0, 100.0, 0.05).published_span().expect("a span");
        assert!(
            (from - 50.0).abs() < 1e-3 && (to - 200.0).abs() < 1e-3,
            "{from}..{to}"
        );
        // Before the lanes have a width, everything is published.
        assert!(pane(0.0, 100.0, 0.05).published_span().is_none());
        assert!(pane(-5.0, 100.0, 0.05).published_span().is_none());
    }

    #[test]
    fn a_clip_is_shown_when_any_of_it_is_in_the_span() {
        let span = Some((50.0, 250.0));
        assert!(TimelinePane::shows(span, 40.0, 10.0), "ends on the edge");
        assert!(TimelinePane::shows(span, 250.0, 5.0), "starts on the edge");
        assert!(TimelinePane::shows(span, 0.0, 1000.0), "covers the span");
        assert!(!TimelinePane::shows(span, 0.0, 9.0));
        assert!(!TimelinePane::shows(span, 251.0, 100.0));
        // A negative duration is no duration, not a clip that reaches back.
        assert!(!TimelinePane::shows(span, 40.0, -100.0));
        assert!(TimelinePane::shows(None, 1e9, 0.0), "no span, everything");
    }

    #[test]
    fn a_centred_view_keeps_the_playhead_in_the_middle() {
        let mut view = pane(400.0, 0.0, 0.05);
        view.centred = true;
        // A 400 px screen at 0.05 s/px is 20 s; the playhead at 3 s puts
        // the left edge 7 s before the start of the cut.
        view.recentre(3.0);
        assert!(
            (view.scroll_left + 7.0).abs() < 1e-6,
            "{}",
            view.scroll_left
        );
        assert!(((3.0 - view.scroll_left) / view.seconds_per_pixel - 200.0).abs() < 1e-3);
        // A zoom about the playhead keeps it where it is: the middle.
        view.zoom(0.5, 3.0, ZOOM_OUT_LIMIT);
        assert!(((3.0 - view.scroll_left) / view.seconds_per_pixel - 200.0).abs() < 1e-3);
        // A scroll may take the view off its playhead, before zero included.
        view.scrolled(-4.0);
        assert_eq!(view.scroll_left, -4.0);
        // Not centred, the desk's floor holds.
        view.centred = false;
        view.recentre(3.0);
        assert_eq!(view.scroll_left, -4.0);
        view.scrolled(-4.0);
        assert_eq!(view.scroll_left, 0.0);
    }

    /// What the lanes' pinch sends: Slint's scale since the fingers landed,
    /// turned into the step since the last update.
    fn pinch(view: &mut TimelinePane, scales: &[f32], anchor: f32) {
        let mut last = 1.0;
        for &scale in scales {
            view.zoom(last / scale, anchor, ZOOM_OUT_LIMIT);
            view.recentre(3.0);
            last = scale;
        }
    }

    #[test]
    fn a_pinch_zooms_by_its_whole_scale_about_the_fingers() {
        // On a desk: the instant between the fingers stays between them.
        let mut view = pane(1000.0, 10.0, 0.1);
        let before = (20.0 - view.scroll_left) / view.seconds_per_pixel;
        pinch(&mut view, &[1.1, 1.3, 1.25, 2.0], 20.0);
        assert!(
            (view.seconds_per_pixel - 0.05).abs() < 1e-6,
            "{}",
            view.seconds_per_pixel
        );
        let after = (20.0 - view.scroll_left) / view.seconds_per_pixel;
        assert!((before - after).abs() < 1e-2, "{before} {after}");
        // Spread past the closest zoom and back: held at the limit, and
        // turning back zooms out at once rather than after a dead stretch.
        pinch(&mut view, &[1e4, 5e3], 20.0);
        assert!(view.seconds_per_pixel > ZOOM_IN_LIMIT);
        assert!((view.seconds_per_pixel - 2.0 * ZOOM_IN_LIMIT).abs() < 1e-6);

        // On a phone: whatever the fingers' middle, the playhead stays at
        // the lanes' middle, as the zoom buttons keep it.
        let mut phone = pane(400.0, 0.0, 0.05);
        phone.centred = true;
        phone.recentre(3.0);
        pinch(&mut phone, &[0.8, 0.5], 9.0);
        assert!((phone.seconds_per_pixel - 0.1).abs() < 1e-6);
        assert!(((3.0 - phone.scroll_left) / phone.seconds_per_pixel - 200.0).abs() < 1e-3);
    }

    #[test]
    fn zoom_keeps_the_anchor_under_the_pointer_and_stays_in_range() {
        let mut view = pane(1000.0, 10.0, 0.1);
        // The instant 20 s is at pixel 100; zooming in about it keeps it there.
        view.zoom(0.5, 20.0, ZOOM_OUT_LIMIT);
        assert!((view.seconds_per_pixel - 0.05).abs() < 1e-6);
        assert!((view.scroll_left - 15.0).abs() < 1e-6);
        assert!(((20.0 - view.scroll_left) / view.seconds_per_pixel - 100.0).abs() < 1e-3);
        // The limits hold, and a scroll never goes negative.
        view.zoom(1e9, -1.0, ZOOM_OUT_LIMIT);
        assert_eq!(view.seconds_per_pixel, ZOOM_OUT_LIMIT);
        view.zoom(1e-9, 0.0, ZOOM_OUT_LIMIT);
        assert_eq!(view.seconds_per_pixel, ZOOM_IN_LIMIT);
        assert!(view.scroll_left >= 0.0);
        // Nonsense factors change nothing.
        let before = view.seconds_per_pixel;
        view.zoom(f32::NAN, 0.0, ZOOM_OUT_LIMIT);
        view.zoom(0.0, 0.0, ZOOM_OUT_LIMIT);
        view.zoom(-2.0, 0.0, ZOOM_OUT_LIMIT);
        assert_eq!(view.seconds_per_pixel, before);
    }

    #[test]
    fn a_long_cut_can_be_zoomed_out_until_it_all_fits_on_screen() {
        let mut view = pane(1000.0, 0.0, 0.05);
        let two_hours = 7200.0;
        let limit = view.zoom_out_limit(two_hours);
        assert!(limit > ZOOM_OUT_LIMIT, "{limit}");
        view.zoom(1e9, -1.0, limit);
        assert!(
            view.width * view.seconds_per_pixel >= two_hours,
            "all the way out shows the whole cut"
        );
        view.fit(two_hours, view.width);
        assert!(
            view.width * view.seconds_per_pixel >= two_hours,
            "and so does fit"
        );
        assert_eq!(view.scroll_left, 0.0);
    }

    #[test]
    fn opening_fits_the_whole_cut_and_a_later_resize_keeps_that_zoom() {
        let mut view = pane(1000.0, 40.0, 0.05);
        let duration = 7200.0;
        view.opened(duration);
        // Stale width must not fit early — wait for the new desk's report.
        assert_eq!(view.width, 0.0);
        assert!(view.fit_pending);
        assert!((view.seconds_per_pixel - 0.05).abs() < 1e-6);
        view.resized_to(1000.0, duration);
        assert!(
            view.width * view.seconds_per_pixel >= duration,
            "{}",
            view.seconds_per_pixel
        );
        assert_eq!(view.scroll_left, 0.0);
        let zoom = view.seconds_per_pixel;
        view.resized_to(800.0, duration);
        assert_eq!(view.seconds_per_pixel, zoom);
        assert_eq!(view.width, 800.0);
    }

    #[test]
    fn a_fit_on_open_waits_until_the_lanes_have_a_width() {
        let mut view = pane(0.0, 10.0, 0.05);
        view.opened(120.0);
        assert!((view.seconds_per_pixel - 0.05).abs() < 1e-6);
        assert!(view.fit_pending);
        view.resized_to(500.0, 120.0);
        assert!(view.width * view.seconds_per_pixel + 1e-3 >= 120.0 * 1.05);
        assert!(!view.fit_pending);
        let zoom = view.seconds_per_pixel;
        view.resized_to(700.0, 120.0);
        assert_eq!(view.seconds_per_pixel, zoom);
    }

    #[test]
    fn a_fit_hold_refits_until_released_after_maximize() {
        let mut view = pane(0.0, 0.0, 0.05);
        let duration = 120.0;
        view.opened(duration);
        view.fit_hold = true;
        view.resized_to(400.0, duration);
        assert!(view.fit_pending, "hold keeps pending");
        let small = view.seconds_per_pixel;
        view.resized_to(800.0, duration);
        assert!(view.fit_pending);
        assert!(
            (view.seconds_per_pixel - small).abs() > 1e-6,
            "refit on the larger desk"
        );
        let fitted = {
            let mut expected = pane(800.0, 0.0, 0.05);
            expected.fit(duration, 800.0);
            expected.seconds_per_pixel
        };
        assert!((view.seconds_per_pixel - fitted).abs() < 1e-6);
        view.end_fit_hold(duration);
        assert!(!view.fit_hold);
        assert!(!view.fit_pending);
        let zoom = view.seconds_per_pixel;
        view.resized_to(600.0, duration);
        assert_eq!(view.seconds_per_pixel, zoom, "later resize keeps zoom");
        assert_eq!(view.width, 600.0);
    }

    #[test]
    fn a_short_cut_keeps_the_fixed_zoom_out_limit() {
        let view = pane(1000.0, 0.0, 0.05);
        assert_eq!(view.zoom_out_limit(60.0), ZOOM_OUT_LIMIT);
        assert_eq!(pane(0.0, 0.0, 0.05).zoom_out_limit(7200.0), ZOOM_OUT_LIMIT);
    }
}
