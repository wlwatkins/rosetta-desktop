//! Temporal smoothing for OCR results.
//!
//! Tesseract re-runs layout analysis from scratch on every frame, so two scans of
//! visually identical content can disagree: a line is merged with its neighbour,
//! or its confidence dips below the floor and it disappears. Rendered straight to
//! the overlay that reads as translations flickering in and out.
//!
//! Lines are tracked in *screen* coordinates so they survive the region being
//! dragged, and a line that goes missing is held for a few frames before being
//! retired.

use crate::geom::Rect;
use crate::worker::Item;

/// How many consecutive scans a line may be absent before it is dropped.
const GRACE: u32 = 4;
/// Box overlap above which two scans are considered to describe the same line.
const MATCH_IOU: f32 = 0.35;

struct Tracked {
    /// Screen coordinates, so a moving region does not invalidate the match.
    rect: Rect,
    source: String,
    text: String,
    misses: u32,
}

#[derive(Default)]
pub struct Stabilizer {
    tracked: Vec<Tracked>,
}

impl Stabilizer {
    pub fn clear(&mut self) {
        self.tracked.clear();
    }

    /// Fold a fresh scan into the tracked set and return what should be drawn.
    /// `items` and the result are both region-relative; `region` is in screen
    /// coordinates.
    pub fn update(&mut self, region: Rect, items: Vec<Item>) -> Vec<Item> {
        let fresh: Vec<Tracked> = items
            .into_iter()
            .map(|i| Tracked {
                rect: i.rect.offset_by(region.x, region.y),
                source: i.source,
                text: i.text,
                misses: 0,
            })
            .collect();

        let mut claimed = vec![false; fresh.len()];

        for t in self.tracked.iter_mut() {
            // Same text wins over mere overlap: it survives a line shifting a few
            // pixels when the surrounding layout reflows.
            let mut best: Option<(usize, f32)> = None;
            for (i, f) in fresh.iter().enumerate() {
                if claimed[i] {
                    continue;
                }
                let iou = t.rect.iou(&f.rect);
                let score = if f.source == t.source { iou + 1.0 } else { iou };
                if score > MATCH_IOU && best.map(|(_, b)| score > b).unwrap_or(true) {
                    best = Some((i, score));
                }
            }
            match best {
                Some((i, _)) => {
                    claimed[i] = true;
                    t.rect = fresh[i].rect;
                    t.source = fresh[i].source.clone();
                    t.text = fresh[i].text.clone();
                    t.misses = 0;
                }
                None => t.misses += 1,
            }
        }

        for (i, f) in fresh.into_iter().enumerate() {
            if !claimed[i] {
                self.tracked.push(f);
            }
        }

        // Retire anything that has been missing too long or has been dragged out
        // of the region entirely.
        self.tracked
            .retain(|t| t.misses <= GRACE && t.rect.intersects(&region));

        self.tracked
            .iter()
            .map(|t| Item {
                rect: t.rect.offset_by(-region.x, -region.y),
                source: t.source.clone(),
                text: t.text.clone(),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REGION: Rect = Rect { x: 100, y: 100, w: 400, h: 200 };

    fn item(x: i32, y: i32, text: &str) -> Item {
        Item {
            rect: Rect::new(x, y, 120, 20),
            source: format!("src:{text}"),
            text: text.to_string(),
        }
    }

    fn texts(items: &[Item]) -> Vec<String> {
        let mut t: Vec<String> = items.iter().map(|i| i.text.clone()).collect();
        t.sort();
        t
    }

    #[test]
    fn first_scan_passes_everything_through() {
        let mut s = Stabilizer::default();
        let out = s.update(REGION, vec![item(10, 10, "one"), item(10, 40, "two")]);
        assert_eq!(texts(&out), vec!["one", "two"]);
    }

    #[test]
    fn a_line_missing_from_one_scan_is_held() {
        let mut s = Stabilizer::default();
        s.update(REGION, vec![item(10, 10, "one"), item(10, 40, "two")]);
        // Tesseract loses the second line for a frame.
        let out = s.update(REGION, vec![item(10, 10, "one")]);
        assert_eq!(texts(&out), vec!["one", "two"], "the dropped line should survive one miss");
    }

    #[test]
    fn a_line_missing_for_too_long_is_retired() {
        let mut s = Stabilizer::default();
        s.update(REGION, vec![item(10, 10, "one"), item(10, 40, "two")]);
        let mut out = Vec::new();
        for _ in 0..=GRACE {
            out = s.update(REGION, vec![item(10, 10, "one")]);
        }
        assert_eq!(texts(&out), vec!["one"], "after GRACE misses it should be gone");
    }

    #[test]
    fn an_empty_scan_does_not_blank_the_overlay() {
        // This is the flicker case: OCR finds nothing for a frame.
        let mut s = Stabilizer::default();
        s.update(REGION, vec![item(10, 10, "one"), item(10, 40, "two")]);
        let out = s.update(REGION, vec![]);
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn a_line_that_reappears_resets_its_grace() {
        let mut s = Stabilizer::default();
        s.update(REGION, vec![item(10, 10, "one"), item(10, 40, "two")]);
        for _ in 0..GRACE {
            s.update(REGION, vec![item(10, 10, "one")]);
        }
        // It comes back, so the counter restarts and it survives another run.
        s.update(REGION, vec![item(10, 10, "one"), item(10, 40, "two")]);
        let mut out = Vec::new();
        for _ in 0..GRACE {
            out = s.update(REGION, vec![item(10, 10, "one")]);
        }
        assert_eq!(texts(&out), vec!["one", "two"]);
    }

    #[test]
    fn a_shifted_box_is_matched_not_duplicated() {
        let mut s = Stabilizer::default();
        s.update(REGION, vec![item(10, 10, "one")]);
        // Same line, nudged a few pixels by a different layout pass.
        let out = s.update(REGION, vec![item(13, 12, "one")]);
        assert_eq!(out.len(), 1, "a small shift must not create a second entry");
        assert_eq!(out[0].rect, Rect::new(13, 12, 120, 20));
    }

    #[test]
    fn a_new_translation_at_the_same_place_replaces_the_old() {
        let mut s = Stabilizer::default();
        s.update(REGION, vec![item(10, 10, "before")]);
        let out = s.update(REGION, vec![item(10, 10, "after")]);
        assert_eq!(texts(&out), vec!["after"]);
    }

    #[test]
    fn lines_track_the_region_as_it_moves() {
        let mut s = Stabilizer::default();
        s.update(REGION, vec![item(10, 10, "one")]);

        // The box is dragged 50px right; the text has not moved on screen, so
        // its region-relative x must drop by the same 50.
        let moved = Rect::new(REGION.x + 50, REGION.y, REGION.w, REGION.h);
        let out = s.update(moved, vec![]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].rect.x, 10 - 50);
    }

    #[test]
    fn a_line_dragged_out_of_the_region_is_dropped() {
        let mut s = Stabilizer::default();
        s.update(REGION, vec![item(10, 10, "one")]);
        // Move the box far away; nothing held can still intersect it.
        let elsewhere = Rect::new(2000, 2000, 100, 100);
        let out = s.update(elsewhere, vec![]);
        assert!(out.is_empty());
    }

    #[test]
    fn clear_forgets_everything() {
        let mut s = Stabilizer::default();
        s.update(REGION, vec![item(10, 10, "one")]);
        s.clear();
        assert!(s.update(REGION, vec![]).is_empty());
    }

    #[test]
    fn identical_text_wins_over_a_merely_overlapping_box() {
        let mut s = Stabilizer::default();
        s.update(REGION, vec![item(10, 10, "alpha")]);
        // Two candidates overlap the tracked box; the one with the same source
        // text should claim it, leaving the other as a new entry.
        let out = s.update(
            REGION,
            vec![item(12, 12, "beta"), item(14, 14, "alpha")],
        );
        assert_eq!(out.len(), 2);
        assert_eq!(texts(&out), vec!["alpha", "beta"]);
    }
}
