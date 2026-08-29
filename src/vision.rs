//! Screen understanding (§6) - three modes, never continuous VLM by default.
//!
//! The gate is the important part. In OFF and PASSIVE the `analyze_screen_vlm`
//! tool is **not in the toolset at all** - the model cannot reach for vision
//! because it does not know vision exists. That is stronger than telling it not
//! to, and it means the cost and latency of a VLM call can never happen in a
//! session where you did not authorise it.
//!
//! PASSIVE is text-only: UACC's accessibility read, no model involved, cheap
//! and local. It is the default because it gives useful context for nothing.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::RwLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VisionMode {
    /// No capture at all.
    Off,
    /// Periodic accessibility read, text only. No model, no cost.
    Passive,
    /// Unlocks the VLM path - both the manual ask and the agentic tool.
    Active,
}

impl VisionMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Passive => "passive",
            Self::Active => "active",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "off" => Some(Self::Off),
            "passive" => Some(Self::Passive),
            "active" => Some(Self::Active),
            _ => None,
        }
    }
    /// Whether the model is offered `analyze_screen_vlm` this session.
    pub fn vlm_allowed(&self) -> bool {
        matches!(self, Self::Active)
    }
    pub fn captures(&self) -> bool {
        !matches!(self, Self::Off)
    }
}

#[derive(Serialize, Deserialize)]
struct Persisted {
    mode: VisionMode,
    #[serde(default = "default_interval")]
    interval_secs: u64,
}

fn default_interval() -> u64 {
    45
}

pub struct VisionState {
    mode: RwLock<VisionMode>,
    interval: AtomicU64,
    path: PathBuf,
    /// Hash of the last capture, so unchanged screens are not written again.
    /// §8 names "passive capture writing low-value noise into memory" as a real
    /// risk to retrieval quality - this is the mitigation.
    last: AtomicU64,
    captured: AtomicUsize,
    skipped: AtomicUsize,
}

impl VisionState {
    pub fn load(data_dir: &std::path::Path) -> Self {
        let path = data_dir.join("vision.json");
        let p: Persisted = std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or(Persisted {
                // §6: passive is the default, and it is what runs unless you
                // change it.
                mode: VisionMode::Passive,
                interval_secs: default_interval(),
            });
        Self {
            mode: RwLock::new(p.mode),
            interval: AtomicU64::new(p.interval_secs.clamp(10, 600)),
            path,
            last: AtomicU64::new(0),
            captured: AtomicUsize::new(0),
            skipped: AtomicUsize::new(0),
        }
    }

    pub fn mode(&self) -> VisionMode {
        *self.mode.read().unwrap()
    }

    pub fn interval(&self) -> u64 {
        self.interval.load(Ordering::Relaxed)
    }

    pub fn set(&self, mode: VisionMode, interval: Option<u64>) -> Result<()> {
        *self.mode.write().unwrap() = mode;
        if let Some(i) = interval {
            self.interval.store(i.clamp(10, 600), Ordering::Relaxed);
        }
        let p = Persisted {
            mode,
            interval_secs: self.interval(),
        };
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).ok();
        }
        std::fs::write(&self.path, serde_json::to_string_pretty(&p)?)?;
        Ok(())
    }

    /// True if this text is different enough from the last capture to be worth
    /// recording.
    pub fn is_new(&self, text: &str) -> bool {
        let h = hash(text);
        let prev = self.last.swap(h, Ordering::Relaxed);
        if prev == h {
            self.skipped.fetch_add(1, Ordering::Relaxed);
            false
        } else {
            self.captured.fetch_add(1, Ordering::Relaxed);
            true
        }
    }

    pub fn stats(&self) -> (usize, usize) {
        (
            self.captured.load(Ordering::Relaxed),
            self.skipped.load(Ordering::Relaxed),
        )
    }
}

fn hash(s: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

/// Pull the human-readable screen map out of whatever shape UACC returned.
///
/// UACC hands back its payload as a JSON *string* inside the result, so this
/// digs one level in rather than trusting a fixed shape.
pub fn extract_screen_text(v: &serde_json::Value) -> Option<String> {
    fn from_obj(o: &serde_json::Value) -> Option<String> {
        o.get("text_map")
            .and_then(|t| t.as_str())
            .map(str::to_string)
    }
    if let Some(t) = from_obj(v) {
        return Some(t);
    }
    // {"result": "<json string>"}
    if let Some(inner) = v.get("result").and_then(|r| r.as_str()) {
        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(inner) {
            if let Some(t) = from_obj(&parsed) {
                return Some(t);
            }
        }
    }
    None
}

/// Pull base64 image data out of a UACC screenshot result.
pub fn extract_image_b64(v: &serde_json::Value) -> Option<String> {
    fn scan(v: &serde_json::Value) -> Option<String> {
        match v {
            serde_json::Value::Object(o) => {
                if let Some(d) = o.get("data").and_then(|d| d.as_str()) {
                    if d.len() > 512 {
                        return Some(d.to_string());
                    }
                }
                o.values().find_map(scan)
            }
            serde_json::Value::Array(a) => a.iter().find_map(scan),
            _ => None,
        }
    }
    scan(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gate_is_active_only() {
        assert!(!VisionMode::Off.vlm_allowed());
        assert!(!VisionMode::Passive.vlm_allowed());
        assert!(VisionMode::Active.vlm_allowed());
        assert!(!VisionMode::Off.captures());
        assert!(VisionMode::Passive.captures());
    }

    #[test]
    fn identical_screens_are_skipped() {
        let dir = std::env::temp_dir().join(format!("vis-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let v = VisionState::load(&dir);
        assert!(v.is_new("window: terminal"));
        assert!(!v.is_new("window: terminal"));
        assert!(v.is_new("window: browser"));
        let (kept, skipped) = v.stats();
        assert_eq!((kept, skipped), (2, 1));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn digs_text_map_out_of_uacc_shape() {
        let wrapped = serde_json::json!({
            "result": "{\"success\": true, \"text_map\": \"Screen: 1920x1080\"}"
        });
        assert_eq!(
            extract_screen_text(&wrapped).as_deref(),
            Some("Screen: 1920x1080")
        );
    }
}
