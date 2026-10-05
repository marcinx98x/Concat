// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Jareer and Concat contributors

//! Where a clip may sit in the stack: pictures laid over the video
//! (stills, titles, shapes) and looks at the top, the video in the middle,
//! the sound at the foot.
//!
//! A lane has no type of its own; what is on it is its type. An empty lane
//! takes anything the lanes around it allow. Rows count from the top of the
//! panel, the way the lanes are drawn; `new_lane_index` alone answers in
//! the model's bottom-first order, since that is what inserting a track
//! takes.

use concat_project::model::ClipKind as ModelKind;

use crate::ui::ClipKind as UiKind;

/// What sort of thing a clip is, as far as its place in the stack goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Group {
    /// A still, a title or a shape: drawn over the video.
    Graphic,
    /// A look laid over a span: over the video too, on lanes of its own.
    Effect,
    Video,
    Audio,
}

impl Group {
    pub fn of_model(kind: ModelKind) -> Self {
        match kind {
            ModelKind::Video => Self::Video,
            ModelKind::Audio => Self::Audio,
            ModelKind::Image | ModelKind::Text | ModelKind::Shape => Self::Graphic,
            ModelKind::Layer => Self::Effect,
        }
    }

    pub fn of_ui(kind: UiKind) -> Self {
        match kind {
            UiKind::Video => Self::Video,
            UiKind::Audio => Self::Audio,
            UiKind::Image | UiKind::Text | UiKind::Shape => Self::Graphic,
            UiKind::Filter => Self::Effect,
        }
    }

    /// Top of the stack first. Graphics and looks share the top: they may
    /// interleave, but never share a lane.
    fn rank(self) -> u8 {
        match self {
            Self::Graphic | Self::Effect => 0,
            Self::Video => 1,
            Self::Audio => 2,
        }
    }
}

/// Whether `group` may sit on `row`, given what every row holds: the lane
/// is empty or holds the same group, nothing above it belongs lower in the
/// stack, and nothing below it higher.
pub fn valid_row(groups: &[Option<Group>], row: i32, group: Group) -> bool {
    let Ok(at) = usize::try_from(row) else {
        return false;
    };
    let Some(here) = groups.get(at) else {
        return false;
    };
    if here.is_some_and(|held| held != group) {
        return false;
    }
    let rank = group.rank();
    groups[..at]
        .iter()
        .flatten()
        .all(|above| above.rank() <= rank)
        && groups[at + 1..]
            .iter()
            .flatten()
            .all(|below| below.rank() >= rank)
}

/// The row a moved clip lands on: the first row allowed walking back from
/// `target` to `origin`, so a drag past the edge of the clip's zone stops
/// at that edge. `origin` when nothing on the way is allowed - a clip can
/// always stay where it is.
pub fn toward(groups: &[Option<Group>], target: i32, origin: i32, group: Group) -> i32 {
    let step = if target > origin { -1 } else { 1 };
    let mut row = target;
    while row != origin {
        if valid_row(groups, row, group) {
            return row;
        }
        row += step;
    }
    origin
}

/// Where a new lane for `group` goes when no row allows it, as an index
/// into the model's bottom-first tracks: sound at the very bottom, graphics
/// and looks at the very top, video just over the top-most video lane - or
/// the top-most sound lane when there is no video.
pub fn new_lane_index(groups: &[Option<Group>], group: Group) -> usize {
    let count = groups.len();
    let top_most = |wanted: Group| {
        groups
            .iter()
            .position(|held| *held == Some(wanted))
            .map(|row| count - row)
    };
    match group {
        Group::Audio => 0,
        Group::Graphic | Group::Effect => count,
        Group::Video => top_most(Group::Video)
            .or_else(|| top_most(Group::Audio))
            .unwrap_or(0),
    }
}

/// Where something added without a drop lands: on a lane, by row, or on a
/// new lane, by index into the model's bottom-first tracks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Landing {
    Row(i32),
    NewLane(usize),
}

/// Where a clip added at the playhead goes, given what every row holds
/// and which rows are `free` - unlocked, with room over the clip's span.
///
/// A lane of its own group first, then an empty lane where its group
/// belongs; pictures over the video take the one nearest the video, video
/// and sound the top-most. With no room on any, a new lane: over
/// everything for pictures and looks, under the bottom-most video lane for
/// video, under everything for sound - so a second video stacks under the
/// first and a second sound under the sound, and the user drags it to
/// another lane of its kind if that is where it belongs.
pub fn landing(groups: &[Option<Group>], free: &[bool], group: Group) -> Landing {
    let over_video = matches!(group, Group::Graphic | Group::Effect);
    let last_held = groups.iter().rposition(|held| *held == Some(group));
    let preference = |row: i32| {
        let at = row as usize;
        if groups[at] == Some(group) {
            0
        } else if over_video || last_held.is_none_or(|last| at > last) {
            1
        } else {
            2
        }
    };
    let candidates = (0..groups.len() as i32).filter(|&row| {
        free.get(row as usize).copied().unwrap_or(false) && valid_row(groups, row, group)
    });
    let best = if over_video {
        candidates.min_by_key(|&row| (preference(row), -row))
    } else {
        candidates.min_by_key(|&row| (preference(row), row))
    };
    best.map_or_else(
        || Landing::NewLane(added_lane_index(groups, group)),
        Landing::Row,
    )
}

/// Where a clip dropped from the library goes: onto `row`, the lane under
/// the pointer, when there is one, its group may sit there and `row_free`
/// says the clip fits in the room the pointer is over. Anywhere else -
/// between lanes, off the stack, over another kind's lane, over clips it
/// would cover - on a new lane at its group's edge, at the same moment, so
/// a drop never pushes what is already on a lane along it.
pub fn drop_landing(
    groups: &[Option<Group>],
    row: Option<i32>,
    row_free: bool,
    group: Group,
) -> Landing {
    match row {
        Some(row) if row_free && valid_row(groups, row, group) => Landing::Row(row),
        _ => Landing::NewLane(added_lane_index(groups, group)),
    }
}

/// The new lanes moved clips that would cover others take, each clip
/// given as its group and span. Clips of a group share a lane while they
/// do not overlap one another; past that the group takes another. Returns
/// the new lane of each clip, counted in the order they are made, and the
/// index each is inserted at - worked out against the stack as every
/// earlier one has already gone in, which is the order they are applied.
pub fn fresh_lanes(
    groups: &[Option<Group>],
    clashing: &[(Group, f64, f64)],
) -> (Vec<usize>, Vec<usize>) {
    let mut stack = groups.to_vec();
    /// A lane being made: its group, the spans on it, where it goes in.
    type Fresh = (Group, Vec<(f64, f64)>, usize);
    let mut lanes: Vec<Fresh> = Vec::new();
    let mut of_clip = Vec::with_capacity(clashing.len());
    for &(group, start, end) in clashing {
        let shared = lanes.iter().position(|(held, spans, _)| {
            *held == group && spans.iter().all(|&(from, to)| end <= from || to <= start)
        });
        let lane = shared.unwrap_or_else(|| {
            let index = added_lane_index(&stack, group);
            stack.insert(stack.len() - index, Some(group));
            lanes.push((group, Vec::new(), index));
            lanes.len() - 1
        });
        lanes[lane].1.push((start, end));
        of_clip.push(lane);
    }
    (
        of_clip,
        lanes.into_iter().map(|(_, _, index)| index).collect(),
    )
}

/// [`new_lane_index`], but video goes under the bottom-most video lane
/// rather than over the top-most: a clip added stacks under what is there.
pub fn added_lane_index(groups: &[Option<Group>], group: Group) -> usize {
    let count = groups.len();
    match group {
        Group::Video => groups
            .iter()
            .rposition(|held| *held == Some(Group::Video))
            .map_or_else(|| new_lane_index(groups, group), |row| count - 1 - row),
        _ => new_lane_index(groups, group),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Group::*;

    /// Room on a lane of its own kind is taken; with none, a new lane at
    /// the kind's edge: pictures on top, video under the video, sound at
    /// the foot.
    #[test]
    fn an_added_clip_takes_room_in_its_kind_or_a_new_lane_at_its_edge() {
        let groups = vec![Some(Graphic), Some(Video), Some(Video), Some(Audio)];
        let all = [true; 4];
        let none = [false; 4];
        assert_eq!(landing(&groups, &all, Video), Landing::Row(1));
        assert_eq!(
            landing(&groups, &[true, false, true, true], Video),
            Landing::Row(2)
        );
        assert_eq!(landing(&groups, &all, Audio), Landing::Row(3));
        assert_eq!(landing(&groups, &all, Graphic), Landing::Row(0));
        // Rows 0..3 top-first are track indices 3..0: under the bottom-most
        // video (row 2, index 1) is index 1.
        assert_eq!(landing(&groups, &none, Video), Landing::NewLane(1));
        assert_eq!(landing(&groups, &none, Audio), Landing::NewLane(0));
        assert_eq!(landing(&groups, &none, Graphic), Landing::NewLane(4));
        assert_eq!(landing(&groups, &none, Effect), Landing::NewLane(4));
    }

    /// A graphic, video and three sound lanes, top to bottom: a drop that
    /// does not fit where it is let go lands on a new lane at its kind's
    /// edge - a graphic over everything, video under the video, sound
    /// under the third sound lane - and one that fits stays put.
    #[test]
    fn a_drop_lands_where_its_kind_belongs_unless_it_fits_where_let_go() {
        let groups = vec![
            Some(Graphic),
            Some(Video),
            Some(Audio),
            Some(Audio),
            Some(Audio),
        ];
        assert_eq!(
            drop_landing(&groups, Some(4), true, Graphic),
            Landing::NewLane(5),
            "a graphic over sound goes to the top"
        );
        assert_eq!(
            drop_landing(&groups, Some(0), false, Graphic),
            Landing::NewLane(5)
        );
        assert_eq!(
            drop_landing(&groups, Some(0), true, Graphic),
            Landing::Row(0)
        );
        // Row 1 is track index 3: a new lane under it is index 3.
        assert_eq!(
            drop_landing(&groups, Some(1), false, Video),
            Landing::NewLane(3)
        );
        assert_eq!(
            drop_landing(&groups, None, true, Video),
            Landing::NewLane(3)
        );
        assert_eq!(
            drop_landing(&groups, Some(3), true, Video),
            Landing::NewLane(3)
        );
        assert_eq!(drop_landing(&groups, Some(1), true, Video), Landing::Row(1));
        assert_eq!(
            drop_landing(&groups, Some(2), false, Audio),
            Landing::NewLane(0)
        );
        assert_eq!(
            drop_landing(&groups, Some(0), true, Audio),
            Landing::NewLane(0)
        );
        assert_eq!(drop_landing(&groups, Some(3), true, Audio), Landing::Row(3));
    }

    /// Moved clips that would cover others: two videos over each other take
    /// a lane each, stacked under the video; the sound one goes under
    /// everything; two videos clear of each other share a lane.
    #[test]
    fn moved_clips_that_clash_take_new_lanes_at_their_kinds_edge() {
        let groups = vec![Some(Graphic), Some(Video), Some(Audio)];
        let (of_clip, indices) = fresh_lanes(
            &groups,
            &[(Video, 0.0, 5.0), (Video, 2.0, 6.0), (Audio, 0.0, 5.0)],
        );
        assert_eq!(of_clip, [0, 1, 2]);
        assert_eq!(indices, [1, 1, 0]);
        let (of_clip, indices) = fresh_lanes(&groups, &[(Video, 0.0, 2.0), (Video, 3.0, 5.0)]);
        assert_eq!(of_clip, [0, 0]);
        assert_eq!(indices, [1]);
        let (of_clip, indices) = fresh_lanes(&groups, &[(Graphic, 0.0, 2.0), (Graphic, 1.0, 3.0)]);
        assert_eq!(of_clip, [0, 1]);
        assert_eq!(indices, [3, 4]);
    }

    /// An empty lane where the kind belongs is used before a new one is
    /// made; one on the wrong side of the video is not.
    #[test]
    fn an_added_clip_takes_an_empty_lane_where_its_kind_belongs() {
        let fresh = vec![None];
        assert_eq!(landing(&fresh, &[true], Video), Landing::Row(0));
        assert_eq!(landing(&fresh, &[true], Audio), Landing::Row(0));
        // Video busy: the empty lane under it, not the one over it.
        let groups = vec![None, Some(Video), None, Some(Audio)];
        let free = [true, false, true, false];
        assert_eq!(landing(&groups, &free, Video), Landing::Row(2));
        assert_eq!(landing(&groups, &free, Graphic), Landing::Row(0));
        assert_eq!(landing(&groups, &free, Audio), Landing::Row(2));
        // A locked or busy empty lane is no room at all.
        assert_eq!(
            landing(&[Some(Video), None], &[false, false], Audio),
            Landing::NewLane(0)
        );
    }

    /// Top to bottom: a graphic, an empty lane, video, sound, an empty lane.
    fn stack() -> Vec<Option<Group>> {
        vec![Some(Graphic), None, Some(Video), Some(Audio), None]
    }

    #[test]
    fn sound_never_rises_over_the_video() {
        let groups = stack();
        assert!(!valid_row(&groups, 0, Audio));
        assert!(!valid_row(&groups, 1, Audio));
        assert!(!valid_row(&groups, 2, Audio));
        assert!(valid_row(&groups, 3, Audio));
        assert!(valid_row(&groups, 4, Audio));
        // Dragged from its lane to the top, it stops on the top-most row
        // it may have.
        assert_eq!(toward(&groups, 0, 3, Audio), 3);
        assert_eq!(toward(&groups, 0, 4, Audio), 3);
    }

    #[test]
    fn a_graphic_never_sinks_under_the_video() {
        let groups = stack();
        assert!(valid_row(&groups, 0, Graphic));
        assert!(valid_row(&groups, 1, Graphic));
        assert!(!valid_row(&groups, 2, Graphic));
        assert!(!valid_row(&groups, 4, Graphic));
        assert_eq!(toward(&groups, 4, 0, Graphic), 1);
    }

    #[test]
    fn a_look_keeps_off_a_graphic_lane() {
        let groups = stack();
        assert!(!valid_row(&groups, 0, Effect));
        assert!(valid_row(&groups, 1, Effect));
    }

    #[test]
    fn an_empty_lane_between_video_and_sound_takes_either_but_no_graphic() {
        let groups = vec![Some(Video), None, Some(Audio)];
        assert!(valid_row(&groups, 1, Video));
        assert!(valid_row(&groups, 1, Audio));
        assert!(!valid_row(&groups, 1, Graphic));
    }

    #[test]
    fn a_drop_with_nowhere_to_go_says_so_and_where_a_lane_belongs() {
        let groups = vec![Some(Graphic), Some(Video)];
        assert_eq!(new_lane_index(&groups, Audio), 0, "under everything");
        assert_eq!(new_lane_index(&groups, Effect), 2, "over everything");
        // Video goes just over the top-most video lane: row 1 is track
        // index 0, so the new lane is index 1, under the graphic.
        assert_eq!(new_lane_index(&groups, Video), 1);
        assert_eq!(new_lane_index(&[Some(Graphic), Some(Audio)], Video), 1);
        assert_eq!(new_lane_index(&[Some(Graphic)], Video), 0);
    }

    #[test]
    fn rows_off_the_stack_are_never_allowed() {
        let groups = stack();
        assert!(!valid_row(&groups, -1, Graphic));
        assert!(!valid_row(&groups, 5, Audio));
    }
}
