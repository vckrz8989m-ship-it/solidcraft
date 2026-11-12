//! A router's side of CAM: what one machine can do, and the checks a program for it must pass.
//!
//! The toolpath operations know nothing of a machine. This module holds a [`MachineProfile`]
//! (a gantry router with a high-speed spindle under a grblHAL controller is the case it was
//! written for), the post a profile writes its programs with, starting feeds and speeds for a
//! cutter and material on that machine ([`feeds`]), and two checks: one of a planned
//! [`Program`] against the machine ([`check_program`], [`check_travel`]) and one of the written
//! text for the codes and moves that stop such a controller ([`check_text`]).

use serde::{Deserialize, Serialize};

use crate::{CamError, Move, Post, Program, Result, SafeRetract, invalid};

pub mod feeds;

/// The largest value any length, feed or speed of a profile may have: far beyond any machine,
/// small enough that arithmetic on it stays finite.
const MAX_VALUE: f64 = 1.0e6;

/// A spindle driven by a frequency inverter. Below `base_rpm` its torque is taken as constant,
/// so the power it can give falls in step with its speed.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Spindle {
    /// Rated power (kW), at `base_rpm` and above.
    pub power_kw: f64,
    /// The slowest speed it may be run at.
    pub rpm_min: f64,
    pub rpm_max: f64,
    /// The speed at the inverter's rated frequency, where rated power is reached.
    pub base_rpm: f64,
    /// The share of electrical power that reaches the cutter (0 to 1).
    pub efficiency: f64,
    /// How long it takes to reach speed (s); written as a dwell after it starts. 0 when the
    /// controller waits for the spindle itself.
    pub spinup_seconds: f64,
}

impl Spindle {
    /// `rpm` brought inside the spindle's range.
    pub fn clamp(&self, rpm: f64) -> f64 {
        rpm.clamp(self.rpm_min, self.rpm_max)
    }

    /// Power at the cutter (kW) at `rpm`.
    pub fn power_at(&self, rpm: f64) -> f64 {
        self.power_kw * (rpm / self.base_rpm).clamp(0.0, 1.0) * self.efficiency
    }

    /// Torque at the cutter (N·m) at and below `base_rpm`.
    pub fn torque(&self) -> f64 {
        self.power_kw * 1000.0 / (2.0 * std::f64::consts::PI * self.base_rpm / 60.0) * self.efficiency
    }

    fn validate(&self) -> Result<()> {
        let positive = [self.power_kw, self.rpm_min, self.rpm_max, self.base_rpm];
        if positive.iter().any(|v| !(*v > 0.0) || !(*v <= MAX_VALUE)) {
            return invalid("the spindle's power and speeds must be above 0");
        }
        if self.rpm_min > self.rpm_max {
            return invalid("the spindle's lowest speed is above its highest");
        }
        if !(self.efficiency > 0.0) || !(self.efficiency <= 1.0) {
            return invalid("the spindle's efficiency must be above 0 and at most 1");
        }
        if !(self.spinup_seconds >= 0.0) || !(self.spinup_seconds <= 600.0) {
            return invalid("the spindle's run-up time must be 0 to 600 s");
        }
        Ok(())
    }
}

/// What a program needs to know of the machine it will run on.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MachineProfile {
    pub name: String,
    /// Travel of X, Y and Z from the homed position (mm), when it has been measured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub travel: Option<[f64; 3]>,
    /// The fastest feed the machine holds while cutting (mm/min).
    pub max_feed: f64,
    /// Acceleration of the slower of X and Y (mm/s²).
    pub acceleration: f64,
    pub spindle: Spindle,
    /// How far each axis backs off its switch after homing (mm). The axis rests there, so a
    /// move in machine coordinates to less than this from 0 runs into the switch.
    pub homing_pulloff: f64,
    /// Where the tool is taken to be clear of everything, in machine coordinates (mm, below 0).
    pub safe_z: f64,
}

impl MachineProfile {
    /// A 3-axis gantry router on 5 mm ball screws with a 2.2 kW, 12,000 to 24,000 rpm
    /// water-cooled spindle, under grblHAL on a Flexi-HAL board. Feed and acceleration are
    /// cautious until the machine has been measured, and the travel is not known.
    pub fn flexihal_router() -> MachineProfile {
        MachineProfile {
            name: "flexihal-router".into(),
            travel: None,
            max_feed: 4000.0,
            acceleration: 400.0,
            spindle: Spindle { power_kw: 2.2, rpm_min: 12000.0, rpm_max: 24000.0, base_rpm: 24000.0, efficiency: 0.8, spinup_seconds: 8.0 },
            homing_pulloff: 2.0,
            safe_z: -5.0,
        }
    }

    /// The names [`MachineProfile::builtin`] knows.
    pub const BUILTIN: &'static [&'static str] = &["flexihal-router"];

    /// The built-in machine called `name`.
    pub fn builtin(name: &str) -> Option<MachineProfile> {
        match name.trim().to_ascii_lowercase().as_str() {
            "flexihal-router" | "flexihal" => Some(MachineProfile::flexihal_router()),
            _ => None,
        }
    }

    pub fn validate(&self) -> Result<()> {
        if self.name.is_empty() || self.name.len() > 64 || self.name.chars().any(char::is_control) {
            return invalid("a machine's name must be 1 to 64 printable characters");
        }
        if let Some(travel) = self.travel
            && travel.iter().any(|v| !(*v > 0.0) || !(*v <= MAX_VALUE))
        {
            return invalid("a machine's travel must be above 0 on every axis");
        }
        if !(self.max_feed > 0.0) || !(self.max_feed <= MAX_VALUE) {
            return invalid("a machine's fastest feed must be above 0");
        }
        if !(self.acceleration > 0.0) || !(self.acceleration <= MAX_VALUE) {
            return invalid("a machine's acceleration must be above 0");
        }
        if !(self.homing_pulloff >= 0.0) || !(self.homing_pulloff <= 100.0) {
            return invalid("a machine's homing pull-off must be 0 to 100 mm");
        }
        if !(self.safe_z < -self.homing_pulloff) || !(self.safe_z >= -MAX_VALUE) {
            return invalid("a machine's safe Z must be below its homing pull-off: nearer 0 the move runs into the switch");
        }
        self.spindle.validate()
    }

    /// The post this machine's programs are written with: [`Post::grblhal`], with the tool
    /// taken to the safe Z in machine coordinates at the start, before a tool change and at
    /// the end, and a dwell for the spindle to reach speed.
    pub fn post(&self) -> Result<Post> {
        self.validate()?;
        let mut post = Post::grblhal();
        post.safe_retract =
            Some(SafeRetract { lines: vec![format!("G53 G0 Z{}", plain_number(self.safe_z))], at_start: true, at_tool_change: true, at_end: true });
        if self.spindle.spinup_seconds > 0.0 {
            post.spindle_on.push(format!("G4 P{}", plain_number(self.spindle.spinup_seconds)));
        }
        post.validate()?;
        Ok(post)
    }

    /// The fastest feed (mm/min) the machine holds round an arc of `radius` mm: its
    /// acceleration is what turns the tool, so the speed is `sqrt(acceleration × radius)`.
    pub fn arc_feed_limit(&self, radius: f64) -> f64 {
        if !(radius > 0.0) {
            return 0.0;
        }
        ((self.acceleration * radius).sqrt() * 60.0).min(self.max_feed)
    }

    /// The smallest arc radius (mm) the machine follows at `feed` mm/min.
    pub fn min_arc_radius(&self, feed: f64) -> f64 {
        let v = feed.max(0.0) / 60.0;
        v * v / self.acceleration
    }
}

/// A number as a controller reads it: at most three decimals, no trailing zeros.
fn plain_number(x: f64) -> String {
    let s = format!("{x:.3}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" || s.is_empty() { "0".into() } else { s.into() }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// The program must not be run as it is.
    Error,
    /// The program runs, but not as it was planned.
    Warning,
}

/// Something a check found.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Issue {
    pub severity: Severity,
    /// The operation it was found in, or the line of the written text (`line 12`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
    pub message: String,
}

impl Issue {
    fn error(at: Option<String>, message: impl Into<String>) -> Issue {
        Issue { severity: Severity::Error, at, message: message.into() }
    }

    fn warning(at: Option<String>, message: impl Into<String>) -> Issue {
        Issue { severity: Severity::Warning, at, message: message.into() }
    }
}

/// How much faster than the machine can turn an arc's feed may be before it is reported.
const ARC_FEED_MARGIN: f64 = 1.25;

/// A planned program against the machine: spindle speeds inside the spindle's range, feeds the
/// machine can hold, and arcs it can follow at their feed. An arc tighter than that is cut
/// slower than planned, so its chip is thinner than the feeds were chosen for.
pub fn check_program(program: &Program, machine: &MachineProfile) -> Result<Vec<Issue>> {
    machine.validate()?;
    let mut issues = Vec::new();
    for op in &program.operations {
        let at = || Some(op.name.clone());
        let spindle = &machine.spindle;
        if !(op.spindle_rpm >= spindle.rpm_min) || !(op.spindle_rpm <= spindle.rpm_max) {
            issues.push(Issue::error(
                at(),
                format!(
                    "spindle speed {} rpm is outside the spindle's {} to {} rpm",
                    plain_number(op.spindle_rpm),
                    plain_number(spindle.rpm_min),
                    plain_number(spindle.rpm_max)
                ),
            ));
        }
        let mut fastest = 0.0f64;
        let mut tight = 0usize;
        let mut tightest: Option<(f64, f64)> = None;
        let mut from = None;
        for m in &op.path.moves {
            match *m {
                Move::Feed { feed, .. } | Move::PlaneArc { feed, .. } => fastest = fastest.max(feed),
                Move::Arc { to, center, feed, .. } => {
                    fastest = fastest.max(feed);
                    let radius = ((to.x - center.x).powi(2) + (to.y - center.y).powi(2)).sqrt();
                    if from.is_some() && feed > machine.arc_feed_limit(radius) * ARC_FEED_MARGIN {
                        tight += 1;
                        if tightest.is_none_or(|(r, _)| radius < r) {
                            tightest = Some((radius, feed));
                        }
                    }
                }
                Move::Rapid { .. } | Move::Dwell { .. } => {}
            }
            if let Some(to) = m.target() {
                from = Some(to);
            }
        }
        if fastest > machine.max_feed {
            issues.push(Issue::warning(
                at(),
                format!(
                    "feed {} mm/min is above the machine's {} mm/min: it will cut slower, with a thinner chip",
                    plain_number(fastest),
                    plain_number(machine.max_feed)
                ),
            ));
        }
        if let Some((radius, feed)) = tightest {
            issues.push(Issue::warning(
                at(),
                format!(
                    "{tight} arcs are tighter than the machine follows at their feed (the tightest: radius {} mm at {} mm/min, where it holds {} mm/min)",
                    plain_number(radius),
                    plain_number(feed),
                    plain_number(machine.arc_feed_limit(radius))
                ),
            ));
        }
    }
    Ok(issues)
}

/// Whether every position of the program is inside the machine's travel, with the work
/// origin at `work_origin` in machine coordinates. A homed machine counts down from 0 at the
/// switches, so each axis runs from minus its travel to 0. Nothing is reported for a machine
/// whose travel is not known.
pub fn check_travel(program: &Program, machine: &MachineProfile, work_origin: [f64; 3]) -> Result<Vec<Issue>> {
    machine.validate()?;
    if work_origin.iter().any(|v| !v.is_finite() || v.abs() > MAX_VALUE) {
        return invalid("the work origin must be finite");
    }
    let Some(travel) = machine.travel else { return Ok(Vec::new()) };
    let mut low = [f64::INFINITY; 3];
    let mut high = [f64::NEG_INFINITY; 3];
    for op in &program.operations {
        for (p, _) in op.path.points(0.01) {
            for (i, v) in [p.x, p.y, p.z].into_iter().enumerate() {
                if let (Some(lo), Some(hi)) = (low.get_mut(i), high.get_mut(i)) {
                    *lo = lo.min(v);
                    *hi = hi.max(v);
                }
            }
        }
    }
    let mut issues = Vec::new();
    for (i, axis) in ["X", "Y", "Z"].into_iter().enumerate() {
        let (Some(lo), Some(hi), Some(origin), Some(reach)) = (low.get(i), high.get(i), work_origin.get(i), travel.get(i)) else {
            continue;
        };
        if !lo.is_finite() {
            continue;
        }
        let (lo, hi) = (lo + origin, hi + origin);
        if lo < -reach || hi > 0.0 {
            issues.push(Issue::error(
                None,
                format!(
                    "{axis} runs from {} to {} in machine coordinates, outside the machine's {} to 0",
                    plain_number(lo),
                    plain_number(hi),
                    plain_number(-reach)
                ),
            ));
        }
    }
    Ok(issues)
}

/// The longest text [`check_text`] reads: a program beyond it is refused, not scanned.
const MAX_TEXT: usize = 64 << 20;

/// The written text against what stops a grblHAL controller or the machine under it: cutter
/// compensation and tool length words it does not take, a move in machine coordinates into a
/// homing switch, and a spindle speed outside the spindle's range. Arcs written with a radius
/// word are reported as well: they cannot describe a full circle and lose accuracy near a half
/// turn. This reads any text, not only what this crate wrote.
pub fn check_text(text: &str, machine: &MachineProfile) -> Result<Vec<Issue>> {
    machine.validate()?;
    if text.len() > MAX_TEXT {
        return Err(CamError::TooLarge("a program of more than 64 MB".into()));
    }
    let mut issues = Vec::new();
    for (n, raw) in text.lines().enumerate() {
        let at = || Some(format!("line {}", n + 1));
        let words = words(raw);
        let has = |letter: char, value: f64| words.iter().any(|(l, v)| *l == letter && (*v - value).abs() < 1e-9);
        let word = |letter: char| words.iter().find(|(l, _)| *l == letter).map(|(_, v)| *v);
        for (code, why) in [
            (41.0, "cutter compensation (G41) is not a grblHAL code: offset the path instead"),
            (42.0, "cutter compensation (G42) is not a grblHAL code: offset the path instead"),
            (43.0, "a tool length offset from a tool table (G43) needs a build with a tool table"),
            (49.0, "G49 cancels the tool length offset the controller keeps across its tool changes"),
        ] {
            if has('G', code) {
                issues.push(Issue::error(at(), why));
            }
        }
        if has('G', 53.0)
            && let Some(z) = word('Z')
            && z > -machine.homing_pulloff
        {
            issues.push(Issue::error(
                at(),
                format!(
                    "a move in machine coordinates to Z{} runs into the homing switch: the axis rests {} mm below it",
                    plain_number(z),
                    plain_number(machine.homing_pulloff)
                ),
            ));
        }
        if let Some(s) = word('S')
            && s != 0.0
            && (s < machine.spindle.rpm_min || s > machine.spindle.rpm_max)
        {
            issues.push(Issue::error(
                at(),
                format!(
                    "spindle speed S{} is outside the spindle's {} to {} rpm",
                    plain_number(s),
                    plain_number(machine.spindle.rpm_min),
                    plain_number(machine.spindle.rpm_max)
                ),
            ));
        }
        if (has('G', 2.0) || has('G', 3.0)) && word('R').is_some() {
            issues.push(Issue::warning(at(), "an arc written with a radius (R): write its centre (I, J) instead"));
        }
    }
    Ok(issues)
}

/// The words of one line (a letter and its number), comments left out. What cannot be read as
/// a word is passed over: this is a check for known traps, not a parser.
fn words(line: &str) -> Vec<(char, f64)> {
    let mut code = String::with_capacity(line.len());
    let mut depth = 0usize;
    for c in line.chars() {
        match c {
            ';' if depth == 0 => break,
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            _ if depth == 0 => code.push(c.to_ascii_uppercase()),
            _ => {}
        }
    }
    let mut out = Vec::new();
    let mut chars = code.chars().peekable();
    while let Some(c) = chars.next() {
        if !c.is_ascii_alphabetic() {
            continue;
        }
        let mut number = String::new();
        while let Some(d) = chars.peek().copied() {
            if d.is_ascii_digit() || d == '.' || ((d == '-' || d == '+') && number.is_empty()) {
                number.push(d);
                chars.next();
            } else if d == ' ' && number.is_empty() {
                chars.next();
            } else {
                break;
            }
        }
        if let Ok(v) = number.parse::<f64>()
            && v.is_finite()
        {
            out.push((c, v));
        }
    }
    out
}
