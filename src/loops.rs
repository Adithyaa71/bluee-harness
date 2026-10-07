//! Loops - work bluee does on its own, on a schedule, without being asked.
//!
//! A loop is a **Markdown file in `loops/`**, deliberately the same shape as a
//! skill (§4f-d.4): frontmatter for the schedule, prose for the instruction.
//! That choice is the whole point of this module. Adithya asked for "the
//! easiest way for building loops ... as i will customize it manually later",
//! and the easiest thing to customise is a text file he can open in any editor,
//! diff, and version-control - not a UI form writing JSON he never sees.
//!
//! ## What a loop is not
//!
//! A loop does **not** get its own turn implementation. It calls
//! `Agent::turn_with`, the same function the CLI, the dashboard websocket and
//! the voice loop drive (§11). A second turn loop would be a second set of
//! bugs, and its tool calls would not land in the event log the same way.
//!
//! ## Why loop output is automatically useful
//!
//! A loop runs in a real session, so every word it says and every tool it calls
//! is appended to the event log - which is the source of truth (§4a), which the
//! reducer turns into vector chunks and graph edges. **Nothing extra is needed
//! to make what a loop notices searchable later.** That falls out of §4a rather
//! than being built.
//!
//! ## Cost, which is the real risk here
//!
//! Every other subsystem in this harness spends money only when Adithya types
//! something. A loop spends it while he is asleep. Three guards, all of them
//! deliberate:
//!
//! 1. **No schedule means it never runs.** A file with no `every:` or `at:` is
//!    `Manual` and costs nothing until you press the button. Dropping a file
//!    into `loops/` cannot start a meter.
//! 2. **`max_runs` per day, default 24**, enforced against persisted state so a
//!    restart loop cannot be used to get around it.
//! 3. **`servers:` scopes the toolset per loop.** §12f measured this as the
//!    single biggest cost lever: 129 tools is ~22,300 prompt tokens *per turn*
//!    before anything is said, and a loop that only reads memory needs none of
//!    UACC's 70. An unscoped nightly loop is the most expensive line in the
//!    file it is written in.
//!
//! State lives in `data/loops.json` - last run and today's count per loop - so
//! a restart resumes the schedule instead of re-firing everything at once.

use anyhow::{Context, Result};
use chrono::{DateTime, Datelike, Local, NaiveTime};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crate::agent::Agent;
use crate::config::Config;
use crate::mcp::McpRegistry;
use crate::vision::VisionState;

/// How a loop decides it is due.
#[derive(Debug, Clone, PartialEq)]
pub enum Trigger {
    /// No schedule. Runs only when a person asks it to. **The default**, and
    /// the reason an unfinished loop file cannot cost anything.
    Manual,
    /// Fixed interval since the last run.
    Every(Duration),
    /// Once a day at a wall-clock time, local.
    DailyAt(NaiveTime),
    /// Once per harness start. For "catch me up on what changed while I was
    /// away", which is a real want and a terrible interval trigger.
    OnStartup,
}

impl Trigger {
    /// Human-readable, for the CLI and the UI. Kept next to the parser so the
    /// two cannot drift.
    pub fn describe(&self) -> String {
        match self {
            Trigger::Manual => "manual".into(),
            Trigger::Every(d) => format!("every {}", humanise(*d)),
            Trigger::DailyAt(t) => format!("daily at {}", t.format("%H:%M")),
            Trigger::OnStartup => "on startup".into(),
        }
    }
}

fn humanise(d: Duration) -> String {
    let s = d.as_secs();
    if s % 86_400 == 0 {
        format!("{}d", s / 86_400)
    } else if s % 3_600 == 0 {
        format!("{}h", s / 3_600)
    } else if s % 60 == 0 {
        format!("{}m", s / 60)
    } else {
        format!("{s}s")
    }
}

/// One loop, as parsed from one file.
#[derive(Debug, Clone)]
pub struct Loop {
    pub name: String,
    pub description: String,
    pub trigger: Trigger,
    pub enabled: bool,
    /// Which MCP servers this loop may use. `None` means every server, which is
    /// almost never what an unattended loop wants - see the cost note above.
    pub servers: Option<Vec<String>>,
    pub max_runs: u32,
    /// Minimum time between runs of an `on: startup` loop. Without it every
    /// launch of the app - including every rebuild-and-restart - was a paid
    /// run: three in one afternoon of testing (CLAUDE.md §56). `min_gap:` in
    /// the frontmatter; defaults to 4h for startup loops.
    pub min_gap: Option<std::time::Duration>,
    /// The instruction sent to the model. Everything after the frontmatter.
    pub prompt: String,
    pub path: PathBuf,
}

/// Persisted per-loop bookkeeping. Separate from the file so editing a loop
/// never resets its schedule, and so `loops/` stays hand-editable without
/// carrying machine state.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LoopState {
    pub last_run: Option<DateTime<Local>>,
    /// Ordinal day the counter refers to, so "today" survives a restart.
    #[serde(default)]
    pub count_day: i32,
    #[serde(default)]
    pub count: u32,
    #[serde(default)]
    pub last_ok: Option<bool>,
    #[serde(default)]
    pub last_note: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StateFile {
    #[serde(default)]
    pub loops: BTreeMap<String, LoopState>,
}

impl StateFile {
    pub fn load(path: &Path) -> Self {
        // This file IS the spending cap: an unreadable one used to load as
        // "never run", which re-armed every loop. Measured: a copy written by
        // Windows PowerShell (which prepends a UTF-8 byte-order mark) fired
        // `catch-up` despite a day's runs being recorded in it. Strip the BOM;
        // and if it still does not parse, say so rather than reset quietly.
        let Ok(text) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        match serde_json::from_str(text.trim_start_matches('\u{feff}')) {
            Ok(s) => s,
            Err(e) => {
                eprintln!(
                    "[loops] {} does not parse ({e}) - loop history treated as empty",
                    path.display()
                );
                Self::default()
            }
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).ok();
        }
        std::fs::write(path, serde_json::to_string_pretty(self)?)
            .with_context(|| format!("writing {}", path.display()))
    }
}

/// Parse a loop file: `---` frontmatter, then the prompt.
///
/// Hand-parsed rather than pulling in a YAML crate. The grammar is six keys of
/// `key: value` and this keeps loop files honest - if a key needs nesting to
/// express, it is too complicated to belong in a schedule header.
pub fn parse(path: &Path, text: &str) -> Result<Loop> {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "unnamed".into());

    let text = text.trim_start_matches('\u{feff}');
    let mut head = String::new();
    let body;

    if let Some(rest) = text.strip_prefix("---") {
        // Split on the closing fence at the start of a line.
        let rest = rest.trim_start_matches(['\r', '\n']);
        match rest.find("\n---") {
            Some(i) => {
                head = rest[..i].to_string();
                body = rest[i + 4..].trim_start_matches(['\r', '\n']).to_string();
            }
            None => body = rest.to_string(),
        }
    } else {
        body = text.to_string();
    }

    let mut name = stem;
    let mut description = String::new();
    let mut trigger = Trigger::Manual;
    let mut enabled = true;
    let mut servers = None;
    let mut max_runs = 24u32;
    let mut min_gap = None;

    for line in head.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        let k = k.trim().to_ascii_lowercase();
        let v = v.trim().trim_matches('"').trim_matches('\'').trim();
        if v.is_empty() {
            continue;
        }
        match k.as_str() {
            "name" => name = v.to_string(),
            "description" | "desc" => description = v.to_string(),
            "every" => {
                trigger = parse_duration(v)
                    .map(Trigger::Every)
                    .with_context(|| format!("{}: cannot read `every: {v}`", path.display()))?
            }
            "at" => {
                trigger = parse_time(v)
                    .map(Trigger::DailyAt)
                    .with_context(|| format!("{}: cannot read `at: {v}`", path.display()))?
            }
            "on" => {
                if v.eq_ignore_ascii_case("startup") {
                    trigger = Trigger::OnStartup;
                }
            }
            "enabled" => enabled = !matches!(v, "false" | "no" | "0"),
            "servers" => {
                // `servers: []` means no MCP servers at all - a real choice,
                // kept distinct from unset, exactly as the workspace tick-boxes
                // do (§25).
                let inner = v.trim_start_matches('[').trim_end_matches(']');
                let list: Vec<String> = inner
                    .split(',')
                    .map(|s| s.trim().trim_matches('"').trim_matches('\'').to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
                servers = Some(list);
            }
            "max_runs" | "max-runs" => max_runs = v.parse().unwrap_or(24),
            "min_gap" | "min-gap" | "cooldown" => {
                min_gap = Some(
                    parse_duration(v)
                        .with_context(|| format!("{}: cannot read `min_gap: {v}`", path.display()))?,
                )
            }
            _ => {}
        }
    }

    let min_gap = min_gap.or(if trigger == Trigger::OnStartup {
        Some(Duration::from_secs(4 * 3600))
    } else {
        None
    });

    Ok(Loop {
        name,
        description,
        trigger,
        enabled,
        servers,
        max_runs,
        min_gap,
        prompt: body.trim().to_string(),
        path: path.to_path_buf(),
    })
}

fn parse_duration(v: &str) -> Option<Duration> {
    let v = v.trim();
    let (num, unit) = v.split_at(v.find(|c: char| c.is_alphabetic())?);
    let n: u64 = num.trim().parse().ok()?;
    let secs = match unit.trim().to_ascii_lowercase().as_str() {
        "s" | "sec" | "secs" | "second" | "seconds" => n,
        "m" | "min" | "mins" | "minute" | "minutes" => n * 60,
        "h" | "hr" | "hrs" | "hour" | "hours" => n * 3_600,
        "d" | "day" | "days" => n * 86_400,
        _ => return None,
    };
    // A sub-minute loop is almost certainly a typo, and a typo that bills.
    Some(Duration::from_secs(secs.max(60)))
}

fn parse_time(v: &str) -> Option<NaiveTime> {
    let (h, m) = v.trim().split_once(':')?;
    NaiveTime::from_hms_opt(h.trim().parse().ok()?, m.trim().parse().ok()?, 0)
}

/// Read every `.md` in `loops/`. A file that fails to parse is reported and
/// skipped rather than taking the others down - the same call §Phase 0 makes
/// about a broken MCP server.
pub fn load_all(dir: &Path) -> (Vec<Loop>, Vec<String>) {
    let mut out = Vec::new();
    let mut errs = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (out, errs);
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        // The folder documents itself, and that file is not a loop. Also skip
        // anything prefixed `_`, which is the conventional way to park a draft
        // next to the real ones.
        let stem = path.file_stem().unwrap_or_default().to_string_lossy();
        if stem.eq_ignore_ascii_case("readme") || stem.starts_with('_') {
            continue;
        }
        match std::fs::read_to_string(&path) {
            Ok(text) => match parse(&path, &text) {
                Ok(l) => out.push(l),
                Err(e) => errs.push(format!("{}: {e:#}", path.display())),
            },
            Err(e) => errs.push(format!("{}: {e}", path.display())),
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    (out, errs)
}

/// Is this loop due right now?
///
/// Should an `on: startup` loop fire on this launch?
///
/// It used to be an unconditional yes, which skipped `due` - and `due` is
/// where the daily cap lives, so startup loops had no cap at all. Now the cap
/// applies, and so does `min_gap`: a restart ten minutes after the last run is
/// not a new "start of the day".
fn startup_due(l: &Loop, st: &LoopState, now: DateTime<Local>) -> bool {
    if st.count_day == now.ordinal() as i32 && st.count >= l.max_runs {
        return false;
    }
    match (l.min_gap, st.last_run) {
        (Some(gap), Some(last)) => {
            now.signed_duration_since(last).to_std().map(|d| d >= gap).unwrap_or(false)
        }
        _ => true,
    }
}

/// `startup` is handled by the caller (it fires once, when the runner starts),
/// so it is never due on a tick.
fn due(l: &Loop, st: &LoopState, now: DateTime<Local>) -> bool {
    if !l.enabled {
        return false;
    }
    // The daily cap is checked before the schedule so a misconfigured interval
    // cannot spend past it.
    if st.count_day == now.ordinal() as i32 && st.count >= l.max_runs {
        return false;
    }
    match &l.trigger {
        Trigger::Manual | Trigger::OnStartup => false,
        Trigger::Every(d) => match st.last_run {
            None => true,
            Some(last) => now.signed_duration_since(last).to_std().unwrap_or_default() >= *d,
        },
        Trigger::DailyAt(t) => {
            let past = now.time() >= *t;
            let ran_today = st
                .last_run
                .map(|l| l.ordinal() == now.ordinal() && l.year() == now.year())
                .unwrap_or(false);
            past && !ran_today
        }
    }
}

/// Run one loop: a real session, the real turn loop, the real event log.
///
/// Returns the assistant's final text, which is what the UI shows and what the
/// state file records a one-line trace of.
pub async fn run_once(
    cfg: &Config,
    registry: Arc<McpRegistry>,
    vision: Arc<VisionState>,
    l: &Loop,
) -> Result<String> {
    let mut agent = Agent::new(cfg, registry, vision).await?;

    // Scope the toolset before the first turn, not after - the schemas are paid
    // on the request that carries them.
    agent.set_allowed_servers(l.servers.clone());

    // Titled so a loop run is obvious in the Sessions page and never looks like
    // something Adithya said.
    agent.set_title(&format!("loop: {}", l.name)).ok();

    let events = agent.turn_with(&l.prompt, None).await;
    agent.end("loop finished");

    let mut reply = String::new();
    let mut failed = None;
    for e in events {
        match e {
            crate::agent::TurnEvent::Reply { text } => reply = text,
            crate::agent::TurnEvent::Error { message } => failed = Some(message),
            _ => {}
        }
    }
    if let Some(m) = failed {
        anyhow::bail!("{m}");
    }
    Ok(reply)
}

/// The background scheduler. One task, one 30-second tick.
///
/// Deliberately re-reads `loops/` every tick rather than watching for file
/// changes: editing a loop should take effect without a restart, and a file
/// watcher would be a dependency and a race for no benefit at this cadence.
/// Note that a save is **not** a trigger - a loop fires on its schedule, never
/// because it was edited, or writing one would bill on every keystroke.
pub async fn run_scheduler(cfg: Config, registry: Arc<McpRegistry>, vision: Arc<VisionState>) {
    let dir = cfg.loops_dir.clone();
    let state_path = cfg.data_dir.join("loops.json");
    let mut fired_startup = false;

    loop {
        let (loops, errs) = load_all(&dir);
        for e in &errs {
            eprintln!("loop config: {e}");
        }

        let now = Local::now();
        let mut state = StateFile::load(&state_path);

        for l in &loops {
            // Read the state out before touching it: the save below needs the
            // map, so holding a mutable borrow of one entry across it does not
            // compile - and copying three small fields is cheaper than the
            // dance required to keep the borrow alive.
            let st = state.loops.get(&l.name).cloned().unwrap_or_default();

            let is_due = if !fired_startup && l.trigger == Trigger::OnStartup && l.enabled {
                startup_due(l, &st, now)
            } else {
                due(l, &st, now)
            };
            if !is_due {
                continue;
            }

            // Reserve the slot before running. A loop that crashes mid-turn
            // must still count, or a reliably-failing loop retries forever.
            let count = if st.count_day == now.ordinal() as i32 {
                st.count + 1
            } else {
                1
            };
            {
                let e = state.loops.entry(l.name.clone()).or_default();
                e.count = count;
                e.count_day = now.ordinal() as i32;
                e.last_run = Some(now);
            }
            state.save(&state_path).ok();

            let name = l.name.clone();
            let result = run_once(&cfg, registry.clone(), vision.clone(), l).await;

            let mut state2 = StateFile::load(&state_path);
            let e = state2.loops.entry(name.clone()).or_default();
            // Carry the counters forward: the run may have taken minutes and
            // the file on disk is the one just written above.
            e.last_run = Some(now);
            e.count_day = now.ordinal() as i32;
            e.count = count;
            match &result {
                Ok(text) => {
                    e.last_ok = Some(true);
                    e.last_note = text.chars().take(200).collect();
                    println!("loop {name}: ok");
                }
                Err(err) => {
                    e.last_ok = Some(false);
                    e.last_note = format!("{err:#}").chars().take(200).collect();
                    eprintln!("loop {name}: {err:#}");
                }
            }
            state2.save(&state_path).ok();
            state = state2;
        }

        fired_startup = true;
        tokio::time::sleep(Duration::from_secs(30)).await;
    }
}

#[cfg(test)]
mod startup_tests {
    use super::*;

    #[test]
    fn startup_respects_gap_and_cap() {
        let l = parse(Path::new("c.md"), "---\nname: c\non: startup\nmax_runs: 2\n---\nx").unwrap();
        assert_eq!(l.min_gap, Some(std::time::Duration::from_secs(4 * 3600)));
        let now = Local::now();
        let fresh = LoopState::default();
        assert!(startup_due(&l, &fresh, now), "never run: fire");

        let recent = LoopState { last_run: Some(now - chrono::Duration::minutes(10)), count_day: now.ordinal() as i32, count: 1, ..Default::default() };
        assert!(!startup_due(&l, &recent, now), "ran 10 minutes ago: wait");

        let old = LoopState { last_run: Some(now - chrono::Duration::hours(5)), count_day: now.ordinal() as i32, count: 1, ..Default::default() };
        assert!(startup_due(&l, &old, now), "ran 5h ago: fire");

        let capped = LoopState { last_run: Some(now - chrono::Duration::hours(5)), count_day: now.ordinal() as i32, count: 2, ..Default::default() };
        assert!(!startup_due(&l, &capped, now), "daily cap reached");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(text: &str) -> Loop {
        parse(Path::new("loops/demo.md"), text).unwrap()
    }

    #[test]
    fn no_schedule_means_manual() {
        // The load-bearing default: a loop file that forgot to say when it runs
        // must never run on its own.
        let l = p("---\ndescription: hi\n---\nDo a thing.");
        assert_eq!(l.trigger, Trigger::Manual);
        assert!(!due(&l, &LoopState::default(), Local::now()));
    }

    #[test]
    fn reads_frontmatter() {
        let l = p("---\nname: nightly\nevery: 6h\nservers: [kuzu_graph, snarevec]\nmax_runs: 4\n---\nReview the day.");
        assert_eq!(l.name, "nightly");
        assert_eq!(l.trigger, Trigger::Every(Duration::from_secs(21_600)));
        assert_eq!(
            l.servers,
            Some(vec!["kuzu_graph".into(), "snarevec".into()])
        );
        assert_eq!(l.max_runs, 4);
        assert_eq!(l.prompt, "Review the day.");
    }

    #[test]
    fn empty_server_list_is_not_unset() {
        // `[]` means "no tools" and `unset` means "every tool". Collapsing them
        // is the §25 bug in a new place.
        assert_eq!(p("---\nservers: []\n---\nx").servers, Some(vec![]));
        assert_eq!(p("---\nname: a\n---\nx").servers, None);
    }

    #[test]
    fn sub_minute_intervals_are_clamped() {
        // `every: 5s` is a typo that bills. Treat it as a minute.
        assert_eq!(parse_duration("5s"), Some(Duration::from_secs(60)));
        assert_eq!(parse_duration("90s"), Some(Duration::from_secs(90)));
    }

    #[test]
    fn daily_cap_beats_the_schedule() {
        let l = p("---\nevery: 1m\nmax_runs: 2\n---\nx");
        let now = Local::now();
        let st = LoopState {
            last_run: None,
            count_day: now.ordinal() as i32,
            count: 2,
            ..Default::default()
        };
        assert!(!due(&l, &st, now), "cap must hold even when interval is due");
    }

    #[test]
    fn readme_is_not_a_loop() {
        // The folder documents itself. Listing its README as a loop is noise,
        // and a `_draft.md` parked next to the real ones should stay parked.
        let dir = std::env::temp_dir().join("bluee-loops-test");
        std::fs::create_dir_all(&dir).unwrap();
        for f in ["README.md", "_draft.md", "real.md"] {
            std::fs::write(dir.join(f), "---
every: 1h
---
x").unwrap();
        }
        let (loops, errs) = load_all(&dir);
        std::fs::remove_dir_all(&dir).ok();
        assert!(errs.is_empty());
        let names: Vec<_> = loops.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(names, vec!["real"]);
    }

    #[test]
    fn disabled_never_runs() {
        let l = p("---\nevery: 1m\nenabled: false\n---\nx");
        assert!(!due(&l, &LoopState::default(), Local::now()));
    }

    #[test]
    fn daily_at_runs_once_per_day() {
        use chrono::Timelike;
        let l = p("---\nat: 09:00\n---\nx");
        let now = Local::now()
            .with_hour(10)
            .unwrap()
            .with_minute(0)
            .unwrap();
        assert!(due(&l, &LoopState::default(), now));
        let ran = LoopState {
            last_run: Some(now),
            ..Default::default()
        };
        assert!(!due(&l, &ran, now), "already ran today");
    }
}
