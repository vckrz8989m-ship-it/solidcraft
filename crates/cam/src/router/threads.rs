//! Screw threads a router mills into plate: the sizes offered, the hole each is milled in, and
//! starting feeds for a single-form thread mill.
//!
//! A thread is milled in a hole the end mill has bored: the major diameter less one pitch
//! across (the tap drill rule), so the thread mill cuts half a pitch deep on the radius. The
//! thread goes right through the plate. For it to run out of the bottom the thread mill's tip
//! goes [`Thread::below`] under the plate, and the hole is bored [`Thread::bored_below`] under
//! it, so the thread mill's end never meets material no cutter has cleared.

use crate::router::MachineProfile;
use crate::router::feeds::{Material, Tier};
use crate::{Result, Tool, ToolKind, invalid};

/// A thread size: its name, major (nominal) diameter and pitch (mm).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Thread {
    pub name: &'static str,
    pub major: f64,
    pub pitch: f64,
}

const fn unified(name: &'static str, major: f64, per_inch: f64) -> Thread {
    Thread { name, major, pitch: 25.4 / per_inch }
}

/// The threads offered, unified then metric (coarse pitches).
pub const THREADS: &[Thread] = &[
    unified("10-24", 4.826, 24.0),
    unified("10-32", 4.826, 32.0),
    unified("1/4-20", 6.35, 20.0),
    unified("1/4-28", 6.35, 28.0),
    unified("5/16-18", 7.9375, 18.0),
    unified("5/16-24", 7.9375, 24.0),
    unified("3/8-16", 9.525, 16.0),
    unified("3/8-24", 9.525, 24.0),
    Thread { name: "M5", major: 5.0, pitch: 0.8 },
    Thread { name: "M6", major: 6.0, pitch: 1.0 },
    Thread { name: "M8", major: 8.0, pitch: 1.25 },
    Thread { name: "M10", major: 10.0, pitch: 1.5 },
];

/// Fit is the diameter added to a thread's major diameter (mm): this far under and over.
pub const FIT_RANGE: (f64, f64) = (-0.2, 0.5);
/// The surface speed a thread mill is run at where the spindle goes that slow (m/min).
const SURFACE_SPEED: f64 = 120.0;
/// The chip a tooth of a thread mill takes at its edge, as a share of the tool's diameter.
const CHIP_PER_DIAMETER: f64 = 0.004;
/// Feed down the bored hole to the thread's start (mm/min).
const PLUNGE: f64 = 300.0;
/// Room a thread mill needs between it and the hole's wall on each side (mm).
pub const WALL_GAP: f64 = crate::helical::MIN_WALL_GAP;

/// The thread called `name`, whatever its case and spacing; a metric one also with its pitch
/// (`M6x1`).
pub fn find(name: &str) -> Option<Thread> {
    let asked: String = name.chars().filter(|c| !c.is_whitespace()).collect::<String>().to_ascii_uppercase();
    if asked.is_empty() || asked.len() > 16 {
        return None;
    }
    THREADS.iter().copied().find(|t| {
        let own = t.name.to_ascii_uppercase();
        asked == own
            || (own.starts_with('M')
                && asked.strip_prefix(&format!("{own}X")).and_then(|p| p.parse::<f64>().ok()).is_some_and(|p| (p - t.pitch).abs() < 1e-9))
    })
}

impl Thread {
    /// The diameter of the hole the thread is milled in (mm): the major diameter less a pitch.
    pub fn bore(&self) -> f64 {
        self.major - self.pitch
    }

    /// How far below the plate the thread mill's tip goes (mm), for the thread to run out.
    pub fn below(&self) -> f64 {
        self.pitch + 0.5
    }

    /// How far below the plate the hole is bored (mm): 0.5 mm under the thread mill's tip.
    pub fn bored_below(&self) -> f64 {
        self.pitch + 1.0
    }

    /// The largest thread mill that goes down the hole with room (mm).
    pub fn widest_mill(&self) -> f64 {
        self.bore() - 2.0 * WALL_GAP
    }
}

/// How a thread is milled: speed, the tool centre's feed, and the passes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ThreadFeed {
    pub rpm: f64,
    /// Feed of the tool's centre round the hole (mm/min), to the nearest 5.
    pub feed: f64,
    /// Feed down the bored hole to the thread's start (mm/min).
    pub plunge: f64,
    pub radial_passes: u32,
    pub spring_pass: bool,
}

/// Starting feeds for milling `thread`, cut `fit` mm over its major diameter, with `tool` (a
/// thread mill that fits the thread's hole). The chip is set at the cutting edge; the tool's
/// centre goes round a smaller circle than the edge cuts, so its feed is that much lower.
/// Metals take two radial passes up to a 1 mm pitch and three above it, plastics and wood one
/// fewer, the careful tier one more, and all a spring pass. A starting point for a test hole.
pub fn thread_feed(thread: &Thread, fit: f64, tool: &Tool, material: Material, tier: Tier, machine: &MachineProfile) -> Result<ThreadFeed> {
    tool.validate()?;
    machine.validate()?;
    if tool.kind != ToolKind::ThreadMill {
        return invalid(format!("thread milling needs a thread mill, not a {}", tool.kind.label()));
    }
    if !(fit >= FIT_RANGE.0) || !(fit <= FIT_RANGE.1) {
        return invalid(format!("the thread's fit must be {} to {} mm", FIT_RANGE.0, FIT_RANGE.1));
    }
    if !(tool.diameter <= thread.widest_mill()) {
        return invalid(format!(
            "the ⌀{} mm thread mill does not fit the ⌀{} mm hole of a {} thread: use one of at most ⌀{} mm",
            mm(tool.diameter),
            mm(thread.bore()),
            thread.name,
            mm(thread.widest_mill())
        ));
    }
    let rpm = machine.spindle.clamp(SURFACE_SPEED * 1000.0 / (std::f64::consts::PI * tool.diameter));
    let share = match tier {
        Tier::Careful => 0.6,
        Tier::Standard => 1.0,
        Tier::Bold => 1.3,
    };
    let edge = rpm * f64::from(tool.flutes) * CHIP_PER_DIAMETER * tool.diameter * share;
    let across = thread.major + fit;
    let centre = edge * (across - tool.diameter) / across;
    let feed = ((centre / 5.0).round() * 5.0).clamp(10.0, machine.max_feed);
    let metal = matches!(material, Material::Aluminum | Material::AluminumSoft | Material::Brass);
    let radial_passes = u32::from(metal) + u32::from(thread.pitch > 1.0) + u32::from(tier == Tier::Careful) + 1;
    Ok(ThreadFeed { rpm, feed, plunge: PLUNGE.min(machine.max_feed), radial_passes, spring_pass: true })
}

/// A length as messages show it: at most three decimals.
pub(crate) fn mm(v: f64) -> String {
    let s = format!("{v:.3}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" { "0".into() } else { s.to_string() }
}
