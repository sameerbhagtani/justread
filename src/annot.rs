//! Freehand annotations, stored outside the PDF in the user's data directory.
//!
//! Each PDF gets one JSON file named after the BLAKE3 hash of its bytes, so
//! annotations follow the document even if it is renamed or moved.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Stroke {
    pub page: usize,
    pub color: [u8; 4],
    /// Line width in PDF points.
    pub width: f32,
    /// Points in PDF page units (1/72 inch), independent of zoom.
    pub points: Vec<[f32; 2]>,
}

#[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Annotations {
    pub strokes: Vec<Stroke>,
}

pub fn key(pdf_bytes: &[u8]) -> String {
    blake3::hash(pdf_bytes).to_hex().to_string()
}

fn path_for(key: &str) -> Option<PathBuf> {
    let dirs = directories::ProjectDirs::from("", "", "justread")?;
    Some(
        dirs.data_dir()
            .join("annotations")
            .join(format!("{key}.json")),
    )
}

impl Annotations {
    pub fn load(key: &str) -> Self {
        path_for(key)
            .map(|p| Self::load_from(&p))
            .unwrap_or_default()
    }

    pub fn save(&self, key: &str) -> std::io::Result<()> {
        match path_for(key) {
            Some(p) => self.save_to(&p),
            None => Err(std::io::Error::other("no data directory on this system")),
        }
    }

    fn load_from(path: &Path) -> Self {
        std::fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    /// Writes to a temp file and renames it, so a crash never leaves a half-written file.
    /// With no strokes left the file is removed instead, so only annotated PDFs have one.
    fn save_to(&self, path: &Path) -> std::io::Result<()> {
        if self.strokes.is_empty() {
            return match std::fs::remove_file(path) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
                _ => Ok(()),
            };
        }
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec(self)?)?;
        std::fs::rename(&tmp, path)
    }

    /// Index of the topmost stroke on `page` passing within `radius` of `pos`.
    pub fn hit(&self, page: usize, pos: [f32; 2], radius: f32) -> Option<usize> {
        self.strokes.iter().rposition(|s| {
            let near = |a, b| dist_to_segment(pos, a, b) <= radius + s.width / 2.0;
            s.page == page
                && match s.points.as_slice() {
                    // A click without movement leaves a single point, which has no segments.
                    [p] => near(*p, *p),
                    points => points.windows(2).any(|w| near(w[0], w[1])),
                }
        })
    }
}

fn dist_to_segment(p: [f32; 2], a: [f32; 2], b: [f32; 2]) -> f32 {
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let len2 = dx * dx + dy * dy;
    let t = if len2 == 0.0 {
        0.0
    } else {
        (((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / len2).clamp(0.0, 1.0)
    };
    let (cx, cy) = (a[0] + t * dx, a[1] + t * dy);
    ((p[0] - cx).powi(2) + (p[1] - cy).powi(2)).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stroke(page: usize, points: &[[f32; 2]]) -> Stroke {
        Stroke {
            page,
            color: [229, 72, 77, 255],
            width: 2.0,
            points: points.to_vec(),
        }
    }

    fn temp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("justread-{}-{name}.json", std::process::id()))
    }

    #[test]
    fn save_then_load_round_trips() {
        let annots = Annotations {
            strokes: vec![
                stroke(4, &[[10.0, 10.0], [100.0, 10.0]]),
                stroke(0, &[[5.0, 5.0]]),
            ],
        };
        let path = temp_path("round-trip");
        annots.save_to(&path).unwrap();
        assert_eq!(Annotations::load_from(&path), annots);
        assert!(!path.with_extension("json.tmp").exists());
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn removing_every_stroke_deletes_the_file() {
        let path = temp_path("emptied");
        let mut annots = Annotations {
            strokes: vec![stroke(0, &[[1.0, 1.0], [2.0, 2.0]])],
        };
        annots.save_to(&path).unwrap();
        assert!(path.exists());

        annots.strokes.clear();
        annots.save_to(&path).unwrap();
        assert!(!path.exists());
        annots.save_to(&path).unwrap(); // already gone is fine
    }

    #[test]
    fn missing_or_corrupt_file_loads_empty() {
        assert_eq!(
            Annotations::load_from(&temp_path("missing")),
            Annotations::default()
        );

        let path = temp_path("corrupt");
        std::fs::write(&path, "not json").unwrap();
        assert_eq!(Annotations::load_from(&path), Annotations::default());
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn key_depends_only_on_content() {
        assert_eq!(key(b"same"), key(b"same"));
        assert_ne!(key(b"same"), key(b"other"));
    }

    #[test]
    fn hit_finds_strokes_near_the_pointer() {
        let annots = Annotations {
            strokes: vec![stroke(4, &[[10.0, 10.0], [100.0, 10.0]])],
        };
        assert_eq!(annots.hit(4, [50.0, 12.0], 2.0), Some(0));
        assert_eq!(
            annots.hit(4, [102.5, 10.0], 2.0),
            Some(0),
            "past the end, within reach"
        );
        assert_eq!(annots.hit(4, [50.0, 30.0], 2.0), None);
        assert_eq!(annots.hit(3, [50.0, 10.0], 2.0), None, "other page");
    }

    #[test]
    fn hit_prefers_the_topmost_stroke() {
        let line = [[0.0, 0.0], [100.0, 0.0]];
        let annots = Annotations {
            strokes: vec![stroke(0, &line), stroke(0, &line)],
        };
        assert_eq!(annots.hit(0, [50.0, 0.0], 1.0), Some(1));
    }

    #[test]
    fn hit_finds_single_point_strokes() {
        let annots = Annotations {
            strokes: vec![stroke(0, &[[20.0, 20.0]])],
        };
        assert_eq!(annots.hit(0, [21.0, 21.0], 2.0), Some(0));
        assert_eq!(annots.hit(0, [40.0, 40.0], 2.0), None);
    }
}
