//! A project's plan - `<repo>/.bluee/plan.md` - read for the progress view.
//!
//! The point is that LOOKING at where a project stands costs nothing: bluee
//! keeps this file current as it works (the `project-plan` skill tells it
//! how), and the Playground parses it directly. Opening a repo a week later
//! shows the phase, the progress and what is next without a single model call.
//!
//! Plain Markdown on purpose, so Adithya can edit it by hand too:
//!
//! ```text
//! # Invoice SaaS
//! > Small-business invoicing with Stripe
//!
//! ## Phase 1: Foundations
//! - [x] repo and CI
//! - [~] auth                       <- in progress ([-] works too)
//! - [ ] database schema
//!
//! ## Next
//! - finish auth
//!
//! ## Waiting on you
//! - pick the pricing tiers
//!
//! ## Decisions
//! - Postgres, not Mongo
//!
//! ## Log
//! - 2026-10-09: auth started
//! ```
//!
//! Every `##` heading that is not one of the named sections is a phase.

use serde::Serialize;

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum State {
    Done,
    Doing,
    Todo,
}

#[derive(Debug, Clone, Serialize)]
pub struct Task {
    pub text: String,
    pub state: State,
}

#[derive(Debug, Clone, Serialize)]
pub struct Phase {
    pub title: String,
    pub tasks: Vec<Task>,
    pub done: usize,
    pub total: usize,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct Plan {
    pub title: String,
    pub goal: String,
    pub phases: Vec<Phase>,
    /// Index into `phases` of the first one not finished, if any.
    pub current: Option<usize>,
    pub done: usize,
    pub total: usize,
    pub next: Vec<String>,
    pub waiting: Vec<String>,
    pub decisions: Vec<String>,
    /// Newest last, as written.
    pub log: Vec<String>,
}

fn section(title: &str) -> Option<&'static str> {
    let t = title.trim().trim_end_matches(':').to_lowercase();
    match t.as_str() {
        "next" | "next up" | "what's next" | "what next" | "todo next" => Some("next"),
        "waiting on you" | "waiting" | "blocked" | "questions" | "open questions" | "needs you" => Some("waiting"),
        "decisions" | "decided" => Some("decisions"),
        "log" | "progress log" | "history" | "changelog" => Some("log"),
        "notes" => Some("notes"),
        _ => None,
    }
}

fn bullet(line: &str) -> Option<&str> {
    let l = line.trim_start();
    l.strip_prefix("- ").or_else(|| l.strip_prefix("* ")).map(str::trim)
}

fn task(text: &str) -> Option<Task> {
    let (state, rest) = if let Some(r) = text.strip_prefix("[x]").or_else(|| text.strip_prefix("[X]")) {
        (State::Done, r)
    } else if let Some(r) = text.strip_prefix("[~]").or_else(|| text.strip_prefix("[-]")).or_else(|| text.strip_prefix("[/]")) {
        (State::Doing, r)
    } else if let Some(r) = text.strip_prefix("[ ]") {
        (State::Todo, r)
    } else {
        return None;
    };
    Some(Task { text: rest.trim().to_string(), state })
}

pub fn parse(md: &str) -> Plan {
    let mut p = Plan::default();
    // Where bullets go: a phase index, a named section, or nowhere.
    enum At { Nowhere, Phase(usize), Sec(&'static str) }
    let mut at = At::Nowhere;
    for raw in md.trim_start_matches('\u{feff}').lines() {
        let line = raw.trim_end();
        if let Some(h) = line.strip_prefix("# ") {
            if p.title.is_empty() {
                p.title = h.trim().to_string();
            }
            continue;
        }
        if let Some(g) = line.strip_prefix('>') {
            if p.goal.is_empty() && p.phases.is_empty() {
                p.goal = g.trim().to_string();
            }
            continue;
        }
        if let Some(h) = line.strip_prefix("## ") {
            at = match section(h) {
                Some(s) => At::Sec(s),
                None => {
                    p.phases.push(Phase { title: h.trim().to_string(), tasks: vec![], done: 0, total: 0 });
                    At::Phase(p.phases.len() - 1)
                }
            };
            continue;
        }
        let Some(b) = bullet(line) else { continue };
        match &at {
            At::Phase(i) => {
                if let Some(t) = task(b) {
                    p.phases[*i].tasks.push(t);
                }
            }
            At::Sec(s) => {
                let text = task(b).map(|t| t.text).unwrap_or_else(|| b.to_string());
                match *s {
                    "next" => p.next.push(text),
                    "waiting" => p.waiting.push(text),
                    "decisions" => p.decisions.push(text),
                    "log" => p.log.push(text),
                    _ => {}
                }
            }
            At::Nowhere => {}
        }
    }
    for ph in &mut p.phases {
        ph.total = ph.tasks.len();
        ph.done = ph.tasks.iter().filter(|t| t.state == State::Done).count();
        p.total += ph.total;
        p.done += ph.done;
    }
    p.current = p.phases.iter().position(|ph| ph.total == 0 || ph.done < ph.total);
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\u{feff}# Invoice SaaS\n> Small-business invoicing\n\n\
        ## Phase 1: Foundations\n- [x] repo\n- [x] CI\n\n\
        ## Phase 2: Core\n- [x] invoices\n- [~] auth\n- [ ] payments\n\n\
        ## Phase 3: Launch\n- [ ] landing page\n\n\
        ## Next\n- finish auth\n\n## Waiting on you\n- [ ] pricing tiers\n\n\
        ## Decisions\n- Postgres\n\n## Log\n- 2026-10-09: auth started\n";

    #[test]
    fn reads_phases_progress_and_sections() {
        let p = parse(SAMPLE);
        assert_eq!(p.title, "Invoice SaaS");
        assert_eq!(p.goal, "Small-business invoicing");
        assert_eq!(p.phases.len(), 3, "named sections are not phases");
        assert_eq!((p.phases[1].done, p.phases[1].total), (1, 3));
        assert_eq!(p.phases[1].tasks[1].state, State::Doing);
        assert_eq!((p.done, p.total), (3, 6));
        assert_eq!(p.current, Some(1), "phase 2 is where work is");
        assert_eq!(p.next, ["finish auth"]);
        assert_eq!(p.waiting, ["pricing tiers"], "a checkbox in a section is just its text");
        assert_eq!(p.decisions, ["Postgres"]);
        assert_eq!(p.log.len(), 1);
    }

    #[test]
    fn a_finished_plan_has_no_current_phase() {
        let p = parse("# X\n## One\n- [x] a\n");
        assert_eq!(p.current, None);
        assert_eq!((p.done, p.total), (1, 1));
    }

    #[test]
    fn plain_bullets_in_a_phase_are_not_tasks() {
        let p = parse("## Phase\n- just a note\n- [ ] real task\n");
        assert_eq!(p.phases[0].total, 1);
    }
}
