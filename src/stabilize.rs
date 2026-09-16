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
