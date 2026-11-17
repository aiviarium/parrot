//! Measuring state detection against labelled recordings.
//!
//! A truth file (`<cast>.truth.toml`) says which state a person sees on the screen over the
//! whole recording, as spans `[start, state]` (`none` for no state, `*` for a stretch that is
//! not scored), and which state each marker should show. [`detect`] replays the cast through
//! a [`Tracker`] with a profile; [`compare`] scores the detected timeline against the truth;
//! [`Summary`] adds up casts.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::automate::Tracker;
use crate::profile::{Hooks, Profile};
use parrot_term::cast::{Cast, Replay, ReplayConfig};
use parrot_term::settle::SettleConfig;

/// Seconds after a true transition within which a late (or before which an early) detection
/// counts as timing, not as a wrong state.
pub const DEFAULT_TOLERANCE: f64 = 1.0;
/// Markers are checked up to this long after their time (but before the true state changes):
/// one settle interval of [`SettleConfig::for_automation`] plus margin.
pub const MARKER_DELAY: f64 = 0.35;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Truth {
    /// The cast, relative to the truth file.
    pub cast: String,
    /// The profile, relative to the truth file.
    pub profile: String,
    /// Not looked at while writing the profile's rules.
    #[serde(default)]
    pub holdout: bool,
    /// `[start seconds, state]`, in order; each state lasts until the next start.
    pub spans: Vec<(f64, String)>,
    /// The state each marker should show.
    #[serde(default)]
    pub markers: BTreeMap<String, String>,
    pub notes: Option<String>,
}

impl Truth {
    pub fn load(path: &Path) -> Result<(Truth, PathBuf), String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let t: Truth = toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        for w in t.spans.windows(2) {
            if w[1].0 < w[0].0 {
                return Err(format!("{}: spans out of order at {}", path.display(), w[1].0));
            }
        }
        Ok((t, path.parent().unwrap_or(Path::new(".")).to_path_buf()))
    }

    /// The true state at `t`.
    pub fn at(&self, t: f64) -> &str {
        self.spans
            .iter()
            .rev()
            .find(|(s, _)| *s <= t)
            .map_or("none", |(_, s)| s.as_str())
    }
}

/// The detected state over a replay.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Detected {
    /// `(time, state)` whenever the current state changed.
    pub changes: Vec<(f64, Option<String>)>,
    pub frames: usize,
    pub end: f64,
    pub unrecognised_after: f64,
}

impl Detected {
    pub fn at(&self, t: f64) -> Option<&str> {
        self.changes
            .iter()
            .rev()
            .find(|(s, _)| *s <= t)
            .and_then(|(_, s)| s.as_deref())
    }
}

/// Replays `cast` through a tracker with `profile`.
pub fn detect(cast: &Cast, profile: &Profile, settle: SettleConfig) -> Detected {
    let replay = Replay::with_config(
        cast,
        ReplayConfig {
            settle,
            ..Default::default()
        },
    );
    let mut tracker = Tracker::new(Some(profile.clone()), Hooks::new());
    let mut d = Detected {
        end: cast.duration(),
        unrecognised_after: profile.unrecognised_after().as_secs_f64(),
        ..Default::default()
    };
    let mut last: Option<Option<String>>;
    // A live automation evaluates the screen at once, before any output.
    tracker.update(0.0, &parrot_term::screen::Screen::blank(cast.header.size()));
    d.changes.push((0.0, tracker.state().map(str::to_string)));
    last = Some(d.changes[0].1.clone());
    for f in replay.frames() {
        tracker.update(f.time, &f.screen);
        d.frames += 1;
        let s = tracker.state().map(str::to_string);
        if last.as_ref() != Some(&s) {
            d.changes.push((f.time, s.clone()));
            last = Some(s);
        }
    }
    d
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct StateScore {
    /// Seconds the truth shows this state.
    pub truth: f64,
    /// Seconds it was detected.
    pub detected: f64,
    /// Seconds both.
    pub both: f64,
}

impl StateScore {
    pub fn precision(&self) -> Option<f64> {
        (self.detected > 0.0).then(|| self.both / self.detected)
    }
    pub fn recall(&self) -> Option<f64> {
        (self.truth > 0.0).then(|| self.both / self.truth)
    }
}

/// A stretch where the detected state differs from the truth.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Interval {
    pub start: f64,
    pub end: f64,
    pub truth: String,
    pub detected: String,
    /// Within the tolerance of a true transition, showing the state on its other side: late
    /// or early detection rather than a wrong state.
    pub timing: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Transition {
    pub time: f64,
    pub from: String,
    pub to: String,
    /// Detected minus true time; negative when detection was early. `None`: never detected
    /// while the true state lasted.
    pub latency: Option<f64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MarkerCheck {
    pub marker: String,
    pub time: f64,
    pub expected: String,
    pub detected: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CastReport {
    pub cast: String,
    pub profile: String,
    pub holdout: bool,
    pub duration: f64,
    pub scored: f64,
    pub frames: usize,
    pub states: BTreeMap<String, StateScore>,
    pub intervals: Vec<Interval>,
    pub transitions: Vec<Transition>,
    pub markers: Vec<MarkerCheck>,
    /// Times `unrecognised` would have triggered, and how many of them while the truth showed
    /// a state.
    pub unrecognised: usize,
    pub unrecognised_false: usize,
}

fn name(s: Option<&str>) -> String {
    s.unwrap_or("none").to_string()
}

/// Scores a detected timeline against the truth.
pub fn compare(truth: &Truth, det: &Detected, cast: &Cast, tol: f64) -> CastReport {
    let end = det.end.max(truth.spans.last().map_or(0.0, |s| s.0));
    let mut points: Vec<f64> = truth.spans.iter().map(|s| s.0).collect();
    points.extend(det.changes.iter().map(|c| c.0));
    points.push(0.0);
    points.push(end);
    points.retain(|&p| p <= end);
    points.sort_by(f64::total_cmp);
    points.dedup();
    // The start of the cast counts as a transition from `none`.
    let mut truth_changes: Vec<(f64, String, String)> = truth
        .spans
        .first()
        .filter(|s| s.1 != "none")
        .map(|s| (s.0, "none".to_string(), s.1.clone()))
        .into_iter()
        .collect();
    // After a `*` stretch, a change counts from the state before it.
    let mut prev: Option<&str> = truth.spans.first().map(|s| s.1.as_str());
    for w in truth.spans.windows(2) {
        if w[0].1 != "*" {
            prev = Some(&w[0].1);
        }
        let from = prev.unwrap_or("none");
        if w[1].1 != w[0].1 && w[1].1 != from {
            truth_changes.push((w[1].0, from.to_string(), w[1].1.clone()));
        }
    }
    let mut r = CastReport {
        cast: truth.cast.clone(),
        profile: truth.profile.clone(),
        holdout: truth.holdout,
        duration: end,
        frames: det.frames,
        ..Default::default()
    };
    let mut current: Option<Interval> = None;
    for w in points.windows(2) {
        let (a, b) = (w[0], w[1]);
        // Truth times are rounded to the millisecond.
        if b - a < 0.001 {
            continue;
        }
        let mid = (a + b) / 2.0;
        let t = truth.at(mid).to_string();
        let d = name(det.at(mid));
        if t == "*" {
            if let Some(i) = current.take() {
                r.intervals.push(i);
            }
            continue;
        }
        let len = b - a;
        r.scored += len;
        r.states.entry(t.clone()).or_default().truth += len;
        r.states.entry(d.clone()).or_default().detected += len;
        if t == d {
            r.states.entry(t.clone()).or_default().both += len;
            if let Some(i) = current.take() {
                r.intervals.push(i);
            }
            continue;
        }
        match &mut current {
            Some(i) if i.truth == t && i.detected == d && (i.end - a).abs() < 1e-9 => i.end = b,
            _ => {
                if let Some(i) = current.take() {
                    r.intervals.push(i);
                }
                current = Some(Interval {
                    start: a,
                    end: b,
                    truth: t,
                    detected: d,
                    timing: false,
                });
            }
        }
    }
    if let Some(i) = current.take() {
        r.intervals.push(i);
    }
    for i in &mut r.intervals {
        let late = truth_changes.iter().any(|(tc, from, to)| {
            *to == i.truth && *from == i.detected && i.start >= tc - 1e-6 && i.end <= tc + tol
        });
        let early = truth_changes.iter().any(|(tc, from, to)| {
            *from == i.truth && *to == i.detected && i.end <= tc + 1e-6 && i.start >= tc - tol
        });
        i.timing = late || early;
    }
    for (k, (tc, from, to)) in truth_changes.iter().enumerate() {
        if to == "*" {
            continue;
        }
        let until = truth_changes.get(k + 1).map_or(end, |n| n.0);
        // Already showing at the true time: since when (negative latency). Otherwise the
        // first switch to it while the true state lasts.
        let latency = if name(det.at(*tc)) == *to {
            det.changes
                .iter()
                .rev()
                .find(|(t, _)| *t <= *tc)
                .map(|(t, _)| t - tc)
        } else {
            det.changes
                .iter()
                .find(|(t, s)| *t > *tc && *t < until && name(s.as_deref()) == *to)
                .map(|(t, _)| t - tc)
        };
        r.transitions.push(Transition {
            time: *tc,
            from: from.clone(),
            to: to.clone(),
            latency,
        });
    }
    let markers: BTreeMap<String, f64> = cast.markers().into_iter().map(|(t, l)| (l, t)).collect();
    for (m, expected) in &truth.markers {
        if expected == "*" {
            continue;
        }
        if let Some(&t) = markers.get(m) {
            // As late as possible while the true state lasts, up to MARKER_DELAY: detection
            // needs a settled frame after the screen changed.
            let next = truth_changes
                .iter()
                .map(|c| c.0)
                .find(|&c| c > t)
                .unwrap_or(f64::INFINITY);
            let check = (t + MARKER_DELAY).min(next - 0.02).max(t);
            r.markers.push(MarkerCheck {
                marker: m.clone(),
                time: t,
                expected: expected.clone(),
                detected: name(det.at(check)),
            });
        } else {
            r.markers.push(MarkerCheck {
                marker: m.clone(),
                time: f64::NAN,
                expected: expected.clone(),
                detected: "(no such marker)".into(),
            });
        }
    }
    // Unrecognised: stretches with no detected state longer than the threshold.
    let mut none_start: Option<f64> = None;
    let mut bounds: Vec<(f64, Option<String>)> = det.changes.clone();
    bounds.push((end, Some("(end)".into())));
    if bounds.first().is_none_or(|b| b.0 > 0.0) {
        none_start = Some(0.0);
    }
    for (t, s) in bounds {
        match (&none_start, s.is_none()) {
            (None, true) => none_start = Some(t),
            (Some(st), false) => {
                if t - st >= det.unrecognised_after {
                    r.unrecognised += 1;
                    let fire = st + det.unrecognised_after;
                    let tr = truth.at(fire);
                    if tr != "none" && tr != "*" {
                        r.unrecognised_false += 1;
                    }
                }
                none_start = None;
            }
            _ => {}
        }
    }
    r
}

/// Totals over several casts.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Summary {
    pub casts: usize,
    pub scored: f64,
    pub frames: usize,
    pub states: BTreeMap<String, StateScore>,
    pub wrong: usize,
    pub wrong_seconds: f64,
    pub wrong_longest: f64,
    pub timing: usize,
    pub timing_seconds: f64,
    pub transitions: usize,
    pub missed: usize,
    pub latencies: Vec<f64>,
    pub markers: usize,
    pub markers_failed: usize,
    pub unrecognised: usize,
    pub unrecognised_false: usize,
}

impl Summary {
    pub fn add(&mut self, r: &CastReport) {
        self.casts += 1;
        self.scored += r.scored;
        self.frames += r.frames;
        for (k, v) in &r.states {
            let e = self.states.entry(k.clone()).or_default();
            e.truth += v.truth;
            e.detected += v.detected;
            e.both += v.both;
        }
        for i in &r.intervals {
            let len = i.end - i.start;
            if i.timing {
                self.timing += 1;
                self.timing_seconds += len;
            } else {
                self.wrong += 1;
                self.wrong_seconds += len;
                self.wrong_longest = self.wrong_longest.max(len);
            }
        }
        for t in &r.transitions {
            self.transitions += 1;
            match t.latency {
                Some(l) => self.latencies.push(l),
                None => self.missed += 1,
            }
        }
        self.markers += r.markers.len();
        self.markers_failed += r.markers.iter().filter(|m| m.expected != m.detected).count();
        self.unrecognised += r.unrecognised;
        self.unrecognised_false += r.unrecognised_false;
    }

    /// Mean, median, 90th percentile and maximum detection latency.
    pub fn latency_stats(&self) -> Option<(f64, f64, f64, f64)> {
        if self.latencies.is_empty() {
            return None;
        }
        let mut v = self.latencies.clone();
        v.sort_by(f64::total_cmp);
        let mean = v.iter().sum::<f64>() / v.len() as f64;
        let q = |p: f64| v[((v.len() - 1) as f64 * p).round() as usize];
        Some((mean, q(0.5), q(0.9), *v.last().unwrap()))
    }

    pub fn to_text(&self, title: &str) -> String {
        use std::fmt::Write;
        let mut s = String::new();
        let _ = writeln!(
            s,
            "{title}: {} casts, {:.1} s scored, {} settled frames",
            self.casts, self.scored, self.frames
        );
        let _ = writeln!(
            s,
            "  {:<20} {:>8} {:>8} {:>9} {:>7}",
            "state", "truth s", "det s", "precision", "recall"
        );
        let pct = |x: Option<f64>| x.map_or("-".to_string(), |v| format!("{:.1}%", v * 100.0));
        for (k, v) in &self.states {
            let _ = writeln!(
                s,
                "  {:<20} {:>8.1} {:>8.1} {:>9} {:>7}",
                k,
                v.truth,
                v.detected,
                pct(v.precision()),
                pct(v.recall())
            );
        }
        let agree: f64 = self.states.values().map(|v| v.both).sum();
        let _ = writeln!(
            s,
            "  agreement {:.1}% of scored time",
            if self.scored > 0.0 {
                agree / self.scored * 100.0
            } else {
                0.0
            }
        );
        let _ = writeln!(
            s,
            "  wrong intervals: {} ({:.2} s total, longest {:.2} s); timing intervals (within tolerance of a transition): {} ({:.2} s)",
            self.wrong, self.wrong_seconds, self.wrong_longest, self.timing, self.timing_seconds
        );
        match self.latency_stats() {
            Some((mean, med, p90, max)) => {
                let _ = writeln!(
                    s,
                    "  transitions: {} ({} missed); latency mean {:.3} s, median {:.3} s, p90 {:.3} s, max {:.3} s",
                    self.transitions, self.missed, mean, med, p90, max
                );
            }
            None => {
                let _ = writeln!(s, "  transitions: {} ({} missed)", self.transitions, self.missed);
            }
        }
        let _ = writeln!(
            s,
            "  markers: {} checked, {} wrong; unrecognised would have triggered {} times ({} while a state was showing)",
            self.markers, self.markers_failed, self.unrecognised, self.unrecognised_false
        );
        s
    }
}

impl CastReport {
    pub fn to_text(&self) -> String {
        use std::fmt::Write;
        let mut s = String::new();
        let _ = writeln!(
            s,
            "{}{} ({:.1} s, {} frames)",
            self.cast,
            if self.holdout { " [held out]" } else { "" },
            self.duration,
            self.frames
        );
        for i in &self.intervals {
            let _ = writeln!(
                s,
                "  {} {:>7.3}..{:<7.3} ({:.3} s) truth {} detected {}",
                if i.timing { "timing" } else { "WRONG " },
                i.start,
                i.end,
                i.end - i.start,
                i.truth,
                i.detected
            );
        }
        for t in &self.transitions {
            match t.latency {
                Some(l) => {
                    let _ = writeln!(s, "  {:>7.3} {} → {}: {:+.3} s", t.time, t.from, t.to, l);
                }
                None => {
                    let _ = writeln!(s, "  {:>7.3} {} → {}: MISSED", t.time, t.from, t.to);
                }
            }
        }
        for m in &self.markers {
            if m.expected != m.detected {
                let _ = writeln!(
                    s,
                    "  marker {} at {:.3}: expected {}, detected {}",
                    m.marker, m.time, m.expected, m.detected
                );
            }
        }
        if self.unrecognised > 0 {
            let _ = writeln!(
                s,
                "  unrecognised would trigger {} times ({} false)",
                self.unrecognised, self.unrecognised_false
            );
        }
        s
    }
}

/// Loads a truth file and its cast and profile, replays and scores it.
pub fn measure_file(path: &Path, settle: SettleConfig, tol: f64) -> Result<CastReport, String> {
    let (truth, dir) = Truth::load(path)?;
    let cast = Cast::load(dir.join(&truth.cast)).map_err(|e| format!("{}: {e}", truth.cast))?;
    let profile = Profile::load(dir.join(&truth.profile)).map_err(|e| e.to_string())?;
    let det = detect(&cast, &profile, settle);
    let mut r = compare(&truth, &det, &cast, tol);
    r.profile = profile.id().to_string();
    Ok(r)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scores_a_timeline() {
        let truth = Truth {
            cast: "x".into(),
            profile: "p".into(),
            holdout: false,
            spans: vec![
                (0.0, "starting".into()),
                (2.0, "idle".into()),
                (5.0, "working".into()),
            ],
            markers: BTreeMap::new(),
            notes: None,
        };
        let det = Detected {
            changes: vec![
                (0.5, Some("starting".into())),
                (2.3, Some("idle".into())),
                (3.0, Some("working".into())),
                (3.5, Some("idle".into())),
                (5.2, Some("working".into())),
            ],
            frames: 5,
            end: 8.0,
            unrecognised_after: 10.0,
        };
        let cast = Cast::parse("{\"version\": 2, \"width\": 10, \"height\": 2}\n").unwrap();
        let r = compare(&truth, &det, &cast, 1.0);
        let idle = &r.states["idle"];
        assert!((idle.truth - 3.0).abs() < 1e-9);
        assert!((idle.both - 2.2).abs() < 1e-9);
        let wrong: Vec<_> = r.intervals.iter().filter(|i| !i.timing).collect();
        assert_eq!(wrong.len(), 1);
        assert_eq!(wrong[0].detected, "working");
        assert_eq!(r.transitions.len(), 3);
        assert!((r.transitions[0].latency.unwrap() - 0.5).abs() < 1e-9);
        assert!((r.transitions[1].latency.unwrap() - 0.3).abs() < 1e-9);
        assert!((r.transitions[2].latency.unwrap() - 0.2).abs() < 1e-9);
    }
}
