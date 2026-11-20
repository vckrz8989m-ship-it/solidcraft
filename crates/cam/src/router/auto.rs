//! From a part's outline to the operations that cut it out of plate.
//!
//! A part is one or more regions: an outline with openings in it. The openings are cut first,
//! nearest first, while the plate still holds the part: each is cleared in passes that leave
//! a little on its wall, then the wall is finished in one pass (two when it is more than two
//! cutter diameters deep). The outline is cut last, the same way: round the part in depth
//! passes down a ramp, a little off the wall, then a finishing pass. Every cut is climb
//! milling, goes through the plate by a little, and takes its speed and feeds from
//! [`feeds`](super::feeds) for the machine.
//!
//! The outside leaves holding tabs ([`Hold::Tabs`], the default), so that the part does not
//! come loose under the cutter: three to eight round it by its size, where both passes leave
//! them. An outline too small for them is cut without, and the plan says so.
//!
//! Pockets ([`Pocket`]) are recesses cut to a depth, not through: they are cut before the
//! openings, cleared and then finished round their walls and islands as an opening is.
//!
//! A region lying in an opening of another is the material that opening removes, not a part
//! of its own (a sketch of a plate with a hole gives both the plate and the hole's disc).
//!
//! A round opening the cutter does not fit is not cut, but its centre is marked: a shallow
//! plunge ([`MARK_DEPTH`]) for a drill to start in once the part is off the machine.
//!
//! A round opening may be given a thread ([`Threading`]): it is then cut at the thread's bore
//! size whatever was drawn, deeper than the plate so that the thread can run out of the bottom,
//! and the thread is milled last, with the thread mill, as operations of their own: a second
//! program, run once the thread mill is in the spindle and zeroed.
//!
//! What cannot be cut as asked is said, not guessed: an opening the cutter does not fit is
//! left alone and reported, and so are flutes shorter than the plate is thick.

use serde::{Deserialize, Serialize};
use solidcore_record::{Loop2, Region2, Seg2, Vec2, Vec3};
use solidcraft_geom::prelude::*;

use crate::helical::{CHIP_ROOM_RATIO, ThreadParams, thread_mill};
use crate::router::feeds::{CutKind, FeedRequest, Material, Recipe, Tier, recipe};
use crate::router::threads::{FIT_RANGE, Thread, mm, thread_feed};
use crate::router::{Issue, MachineProfile};
use crate::{
    CamError, ContourOptions, ContourParams, Cut, Feeds, Heights, Move, Operation, PocketParams, Result, Side, TabLayout, Tabs, Tool, ToolKind,
    Toolpath, contour, contour_with, invalid, offset_regions, pocket, verify,
};

/// The most regions and openings one part may have.
const MAX_LOOPS: usize = 2000;
/// How deep the centre of a round opening too small for the cutter is marked (mm), and never
/// more than half the plate.
pub const MARK_DEPTH: f64 = 0.5;
/// Chord error allowed on curved boundaries (mm).
const TOLERANCE: f64 = 0.005;
/// How far from a round opening's centre a thread may be named and still mean that opening (mm).
pub const HOLE_TOLERANCE: f64 = 0.01;
/// Largest coordinate of a thread's hole (mm).
const MAX_COORD: f64 = 1.0e6;

/// What keeps the part in place while its outside is cut through.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Hold {
    /// Tabs are left round the outside, to cut or break once the plate is off the machine.
    #[default]
    Tabs,
    /// Nothing is left: screws through the part's openings, tape or vacuum hold it.
    None,
}

/// How a part is cut from its plate.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct PartCut {
    pub material: Material,
    /// Thickness of the plate (mm).
    pub thickness: f64,
    #[serde(default)]
    pub tier: Tier,
    /// Left on the walls by roughing for the finishing pass (mm).
    pub finish_stock: f64,
    /// How far below the plate the cuts go (mm), into the spoilboard.
    pub break_through: f64,
    /// Steepest descent into material (radians).
    pub ramp_angle: f64,
    /// Height the tool travels at between cuts, above the plate's top (mm).
    pub clearance: f64,
    /// Height rapids stop at above the plate's top (mm).
    pub retract: f64,
    /// The plate's top in work coordinates (mm).
    pub top: f64,
    pub coolant: bool,
    #[serde(default)]
    pub hold: Hold,
}

impl PartCut {
    /// The usual cut: 0.2 mm left for finishing, 0.2 mm through, a 3° ramp, travel 5 mm above
    /// the plate, whose top is work Z 0.
    pub fn new(material: Material, thickness: f64) -> PartCut {
        PartCut {
            material,
            thickness,
            tier: Tier::Standard,
            finish_stock: 0.2,
            break_through: 0.2,
            ramp_angle: 3.0f64.to_radians(),
            clearance: 5.0,
            retract: 0.5,
            top: 0.0,
            coolant: true,
            hold: Hold::Tabs,
        }
    }

    fn validate(&self) -> Result<()> {
        if !(self.thickness > 0.0) || !(self.thickness <= 500.0) {
            return invalid("the plate's thickness must be above 0 and at most 500 mm");
        }
        if !(self.finish_stock >= 0.0) || !(self.finish_stock <= 5.0) {
            return invalid("the stock left for finishing must be 0 to 5 mm");
        }
        if !(self.break_through >= 0.0) || !(self.break_through <= 5.0) {
            return invalid("the depth cut below the plate must be 0 to 5 mm");
        }
        if !(self.ramp_angle >= 0.0) || !(self.ramp_angle <= std::f64::consts::FRAC_PI_2) {
            return invalid("the ramp angle must be 0° to 90°");
        }
        if !(self.retract > 0.0) || !(self.clearance >= self.retract) || !(self.clearance <= 1000.0) {
            return invalid("the clearance height must be at least the retract height, which must be above 0");
        }
        if !self.top.is_finite() || self.top.abs() > 1.0e6 {
            return invalid("the plate's top must be finite");
        }
        Ok(())
    }
}

/// Recesses of one depth: each region is cleared to `depth` below the plate's top, its holes
/// left standing as islands.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Pocket {
    pub regions: Vec<Region2>,
    /// Depth below the plate's top (mm), less than the plate's thickness.
    pub depth: f64,
}

/// The operations of a part, in the order they are cut, and what could not be done as asked.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Planned {
    pub operations: Vec<Operation>,
    pub issues: Vec<Issue>,
}

/// A point to measure the way to a loop from: a full circle's centre, else the mean of where
/// its pieces start.
fn middle(lp: &Loop2) -> Vec2 {
    if let [Seg2::Arc { center, .. }] = lp.segs.as_slice() {
        return *center;
    }
    let n = lp.segs.len().max(1) as f64;
    let sum = lp.segs.iter().map(Seg2::start).fold(Vec2 { x: 0.0, y: 0.0 }, |a, p| Vec2 { x: a.x + p.x, y: a.y + p.y });
    Vec2 { x: sum.x / n, y: sum.y / n }
}

/// The centre and radius of a loop that is one full circle.
fn full_circle(lp: &Loop2) -> Option<(Vec2, f64)> {
    match lp.segs.as_slice() {
        [Seg2::Arc { center, radius, sweep, .. }] if (sweep.abs() - std::f64::consts::TAU).abs() < 1e-9 => Some((*center, *radius)),
        _ => None,
    }
}

/// The tabs an outline is left on: one about every 100 mm, three to eight, as long as the
/// cutter is wide (5 mm at least), 1 mm high in metal and 2 mm in wood and plastics, and never
/// more than half the cut.
fn tabs_for(outline: &Loop2, tool: &Tool, material: Material, total: f64) -> Tabs {
    let around: f64 = outline.segs.iter().map(Seg2::length).sum();
    let count = if around.is_finite() { (around / 100.0).round().clamp(3.0, 8.0) as usize } else { 3 };
    let metal = matches!(material, Material::Aluminum | Material::AluminumSoft | Material::Brass);
    Tabs {
        layout: TabLayout::Count(count),
        width: tool.diameter.max(5.0),
        height: f64::min(if metal { 1.0 } else { 2.0 }, 0.5 * total),
        ramp_angle: 30.0f64.to_radians(),
    }
}

/// Depth passes no deeper than `deepest` that share `total` evenly.
fn even_stepdown(total: f64, deepest: f64) -> f64 {
    let passes = (total / deepest.max(1e-6) - 1e-9).ceil().max(1.0);
    total / passes
}

/// What lies in an opening of another of `regions` is that opening's waste (or, of a pocket,
/// the island's own top): the regions that are not.
fn outermost(regions: &[Region2]) -> Vec<&Region2> {
    let nested = |b: &Region2| {
        let p = b.interior_point();
        regions.iter().any(|a| !std::ptr::eq(a, b) && a.outer.contains(p) && !a.contains(p))
    };
    regions.iter().filter(|r| !nested(r)).collect()
}

/// A recess to cut: an opening through the plate or a pocket to its depth.
struct Recess {
    name: String,
    region: Region2,
    depth: f64,
    /// What it is called in a message.
    kind: &'static str,
    /// The thread milled in it afterwards, when it is a hole to thread.
    thread: Option<Thread>,
}

/// `recesses` in the order they are cut: each the nearest to the one before it, from `at`,
/// which is left at the last.
fn nearest_first(mut recesses: Vec<Recess>, at: &mut Vec2) -> Vec<Recess> {
    let mut out = Vec::with_capacity(recesses.len());
    while !recesses.is_empty() {
        let from = *at;
        let dist = |r: &Recess| {
            let p = middle(&r.region.outer);
            (p.x - from.x).powi(2) + (p.y - from.y).powi(2)
        };
        let nearest = recesses.iter().enumerate().min_by(|a, b| dist(a.1).total_cmp(&dist(b.1))).map_or(0, |(k, _)| k);
        let next = recesses.swap_remove(nearest);
        *at = middle(&next.region.outer);
        out.push(next);
    }
    out
}

/// The operations that cut `regions` out of plate with `tool` on `machine`.
pub fn plan_part(regions: &[Region2], tool: &Tool, cut: &PartCut, machine: &MachineProfile) -> Result<Planned> {
    plan_part_with(regions, &[], tool, cut, machine)
}

/// [`plan_part`] with `pockets` cut into the part first.
pub fn plan_part_with(regions: &[Region2], pockets: &[Pocket], tool: &Tool, cut: &PartCut, machine: &MachineProfile) -> Result<Planned> {
    plan_part_threaded(regions, pockets, None, tool, cut, machine)
}

/// A round opening of the part to mill a thread in, named by its centre.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ThreadedHole {
    pub center: Vec2,
    pub thread: Thread,
}

/// The threads of a part and what mills them.
#[derive(Clone, Debug, PartialEq)]
pub struct Threading {
    pub holes: Vec<ThreadedHole>,
    /// The thread mill: single-form, its reach kept as its flute length.
    pub tool: Tool,
    /// Added to each thread's major diameter (mm): above 0 cuts deeper and loosens the thread.
    pub fit: f64,
}

impl Threading {
    fn validate(&self) -> Result<()> {
        self.tool.validate()?;
        if self.tool.kind != ToolKind::ThreadMill {
            return invalid(format!("thread milling needs a thread mill, not a {}", self.tool.kind.label()));
        }
        if !(self.fit >= FIT_RANGE.0) || !(self.fit <= FIT_RANGE.1) {
            return invalid(format!("the thread's fit must be {} to {} mm", FIT_RANGE.0, FIT_RANGE.1));
        }
        if self.holes.len() > MAX_LOOPS {
            return Err(CamError::TooLarge(format!("more than {MAX_LOOPS} threads in a part")));
        }
        for hole in &self.holes {
            let (c, t) = (hole.center, hole.thread);
            if !c.x.is_finite() || !c.y.is_finite() || c.x.abs() > MAX_COORD || c.y.abs() > MAX_COORD {
                return invalid("a thread's hole must be at a finite point");
            }
            if !(t.pitch >= 0.1) || !(t.pitch <= 10.0) || !(t.major >= 1.0) || !(t.major <= 100.0) || !(t.bore() >= 0.5 * t.major) {
                return invalid(format!("the {} thread's diameter and pitch are not a screw thread's", t.name));
            }
        }
        Ok(())
    }
}

/// A round opening of a part, as a plan names it.
#[derive(Clone, Debug, PartialEq)]
pub struct RoundHole {
    pub name: String,
    pub center: Vec2,
    pub diameter: f64,
}

/// What opening `j` of part `i` is called, of `parts` parts.
fn opening_name(parts: usize, i: usize, j: usize) -> String {
    if parts > 1 { format!("Part {} opening {}", i + 1, j + 1) } else { format!("Opening {}", j + 1) }
}

/// The round openings of the part `regions` draw, named as [`plan_part`] names them: the
/// holes a thread can be milled in.
pub fn round_holes(regions: &[Region2]) -> Result<Vec<RoundHole>> {
    if regions.iter().map(|r| 1 + r.holes.len()).sum::<usize>() > MAX_LOOPS {
        return Err(CamError::TooLarge(format!("a part of more than {MAX_LOOPS} outlines and openings")));
    }
    let parts = outermost(regions);
    let mut out = Vec::new();
    for (i, region) in parts.iter().enumerate() {
        for (j, hole) in region.holes.iter().enumerate() {
            if let Some((center, radius)) = full_circle(hole).filter(|(c, r)| c.x.is_finite() && c.y.is_finite() && r.is_finite() && *r > 0.0) {
                out.push(RoundHole { name: opening_name(parts.len(), i, j), center, diameter: 2.0 * radius });
            }
        }
    }
    Ok(out)
}

/// An opening that is given a thread: which it is, its centre and diameter as drawn, and the
/// thread.
struct Tapped {
    part: usize,
    hole: usize,
    center: Vec2,
    drawn: f64,
    thread: Thread,
}

/// Points round a hole's wall that are looked at to see what a wider hole would cut into.
const WALL_SAMPLES: usize = 72;

/// A part's outline and openings as polygons, made once for the holes bored wider than drawn.
struct Walls {
    outline: solidcraft_geom::Ring<Vec2>,
    openings: Vec<solidcraft_geom::Ring<Vec2>>,
}

impl Walls {
    fn of(region: &Region2) -> Walls {
        Walls { outline: region.outer.polyline(1e-3), openings: region.holes.iter().map(|h| h.polyline(1e-3)).collect() }
    }

    /// Whether a hole of `diameter` about `center` stays in the part and out of its openings
    /// other than opening `own`: looked at round the hole's wall.
    fn hold(&self, own: usize, center: Vec2, diameter: f64) -> bool {
        (0..WALL_SAMPLES).all(|k| {
            let p = center + Vec2::from_angle(std::f64::consts::TAU * k as f64 / WALL_SAMPLES as f64) * (0.5 * diameter);
            self.outline.contains(p) && !self.openings.iter().enumerate().any(|(j, other)| j != own && other.contains(p))
        })
    }
}

/// [`plan_part_with`] with `threading`: its holes are bored to their threads' size, and the
/// threads are milled last, with the thread mill.
pub fn plan_part_threaded(
    regions: &[Region2],
    pockets: &[Pocket],
    threading: Option<&Threading>,
    tool: &Tool,
    cut: &PartCut,
    machine: &MachineProfile,
) -> Result<Planned> {
    cut.validate()?;
    machine.validate()?;
    tool.validate()?;
    if regions.is_empty() {
        return invalid("a part needs at least one closed outline");
    }
    let loops = |rs: &[Region2]| rs.iter().map(|r| 1 + r.holes.len()).sum::<usize>();
    if loops(regions) + pockets.iter().map(|p| loops(&p.regions)).sum::<usize>() > MAX_LOOPS {
        return Err(CamError::TooLarge(format!("a part of more than {MAX_LOOPS} outlines, openings and pockets")));
    }
    if pockets.iter().any(|p| !(p.depth > 0.0) || !p.depth.is_finite()) {
        return invalid("a pocket's depth must be above 0");
    }
    let threading = threading.filter(|t| !t.holes.is_empty());
    if let Some(t) = threading {
        t.validate()?;
    }
    let d = tool.diameter;
    let ask = |kind: CutKind| -> Result<Recipe> {
        let mut request = FeedRequest::new(cut.material, d, tool.flutes, kind);
        request.tier = cut.tier;
        request.flute_length = Some(tool.flute_length);
        recipe(&request, machine)
    };
    let (slot, clear, finish) = (ask(CutKind::Slot)?, ask(CutKind::Pocket)?, ask(CutKind::Finish)?);
    let total = cut.thickness + cut.break_through;
    let levels = |depth: f64| Heights { clearance: cut.top + cut.clearance, retract: cut.top + cut.retract, top: cut.top, bottom: cut.top - depth };
    let passes_to = |r: &Recipe, deepest: f64, depth: f64| Cut {
        heights: levels(depth),
        feeds: Feeds { cut: r.feed, plunge: r.ramp_feed },
        stepdown: even_stepdown(depth, deepest),
    };
    let passes = |r: &Recipe, deepest: f64| passes_to(r, deepest, total);
    let wall = |side: Side, stock: f64| ContourParams { side, climb: true, stock_to_leave: stock, tolerance: TOLERANCE, ramp_angle: cut.ramp_angle };

    let parts = outermost(regions);
    if parts.is_empty() {
        return invalid("a part needs an outline that is not inside another's opening");
    }

    let mut plan = Planned::default();
    if total > tool.flute_length {
        plan.issues.push(Issue::error(
            None,
            format!("the cut is {total:.2} mm deep and the cutter has {:.2} mm of flute: use a longer cutter", tool.flute_length),
        ));
        return Ok(plan);
    }
    // Each thread must name a round opening of the part, which the cutters can bore and thread.
    let mut tapped: Vec<Tapped> = Vec::new();
    if let Some(t) = threading {
        for asked in &t.holes {
            let found = parts.iter().enumerate().find_map(|(i, region)| {
                region
                    .holes
                    .iter()
                    .enumerate()
                    .find_map(|(j, lp)| full_circle(lp).filter(|(c, _)| c.dist(asked.center) <= HOLE_TOLERANCE).map(|(c, r)| (i, j, c, 2.0 * r)))
            });
            let Some((part, hole, center, drawn)) = found else {
                plan.issues.push(Issue::error(
                    None,
                    format!(
                        "there is no round hole at X {} Y {} for the {} thread: choose the holes to thread again",
                        mm(asked.center.x),
                        mm(asked.center.y),
                        asked.thread.name
                    ),
                ));
                return Ok(plan);
            };
            if tapped.iter().any(|h| h.part == part && h.hole == hole) {
                return invalid(format!("two threads are given for the hole at X {} Y {}", mm(center.x), mm(center.y)));
            }
            tapped.push(Tapped { part, hole, center, drawn, thread: asked.thread });
        }
        let mut walls: Vec<Option<Walls>> = parts.iter().map(|_| None).collect();
        for Tapped { part, hole, center, drawn, thread } in &tapped {
            let (bored, reach) = (cut.thickness + thread.bored_below(), cut.thickness + thread.below());
            // A hole bored wider than it was drawn cuts where nothing was drawn to be cut.
            let wider = thread.bore() > drawn + 2.0 * HOLE_TOLERANCE;
            let holds = !wider
                || match (walls.get_mut(*part), parts.get(*part)) {
                    (Some(slot), Some(region)) => slot.get_or_insert_with(|| Walls::of(region)).hold(*hole, *center, thread.bore()),
                    _ => false,
                };
            let stop = if !holds {
                Some(format!(
                    "the Ø{} mm hole of a {} thread at X {} Y {} is wider than the Ø{} mm hole drawn there, and would cut through the part's \
                     edge or into another opening: choose the thread for another hole, or move the hole",
                    mm(thread.bore()),
                    thread.name,
                    mm(center.x),
                    mm(center.y),
                    mm(*drawn)
                ))
            } else if !(d <= thread.widest_cutter()) {
                Some(format!(
                    "the {} mm cutter does not fit the Ø{} mm hole of a {} thread with room for its chips: use a cutter of at most {} mm, or \
                     take the thread off the hole",
                    mm(d),
                    mm(thread.bore()),
                    thread.name,
                    mm(thread.widest_cutter())
                ))
            } else if bored > tool.flute_length {
                Some(format!(
                    "the hole of a {} thread is bored {} mm deep and the cutter has {} mm of flute: use a longer cutter",
                    thread.name,
                    mm(bored),
                    mm(tool.flute_length)
                ))
            } else if !(t.tool.diameter <= thread.widest_mill()) {
                Some(format!(
                    "the Ø{} mm thread mill does not fit the Ø{} mm hole of a {} thread: use one of at most Ø{} mm",
                    mm(t.tool.diameter),
                    mm(thread.bore()),
                    thread.name,
                    mm(thread.widest_mill())
                ))
            } else if reach > t.tool.flute_length {
                Some(format!(
                    "a {} thread through this plate needs a thread mill with {} mm of reach, and this one has {} mm: use a longer one",
                    thread.name,
                    mm(reach),
                    mm(t.tool.flute_length)
                ))
            } else {
                None
            };
            if let Some(message) = stop {
                plan.issues.push(Issue::error(None, message));
                return Ok(plan);
            }
        }
    }
    for r in [&slot, &clear, &finish] {
        for w in &r.warnings {
            if !plan.issues.iter().any(|i| i.message == *w) {
                plan.issues.push(Issue::warning(None, w.clone()));
            }
        }
    }
    let add = |plan: &mut Planned, name: String, rpm: f64, path: crate::Toolpath, depth: f64| -> Result<()> {
        verify(&path, &levels(depth))?;
        plan.operations.push(Operation { name, tool: tool.clone(), spindle_rpm: rpm, coolant: cut.coolant, path });
        Ok(())
    };

    // Pockets while the plate is whole, then the openings, each from the one before it.
    let mut wanted: Vec<Recess> = Vec::new();
    let several = pockets.iter().map(|p| outermost(&p.regions).len()).sum::<usize>() > 1;
    for pocket in pockets {
        for region in outermost(&pocket.regions) {
            let name = if several { format!("Pocket {}", wanted.len() + 1) } else { "Pocket".to_string() };
            if pocket.depth >= cut.thickness {
                plan.issues.push(Issue::error(
                    Some(name),
                    format!(
                        "a pocket {:.2} mm deep goes through the {:.2} mm plate: draw it in the outline's sketch, where it is an opening",
                        pocket.depth, cut.thickness
                    ),
                ));
                return Ok(plan);
            }
            if !parts.iter().any(|part| part.contains(region.interior_point())) {
                plan.issues.push(Issue::warning(Some(name.clone()), "this pocket is not inside the part's outline"));
            }
            let region = Region2 { outer: region.outer.ccw(), holes: region.holes.clone() };
            wanted.push(Recess { name, region, depth: pocket.depth, kind: "pocket", thread: None });
        }
    }
    let mut at = Vec2 { x: 0.0, y: 0.0 };
    let mut recesses = nearest_first(wanted, &mut at);
    let mut openings: Vec<Recess> = Vec::new();
    for (i, region) in parts.iter().enumerate() {
        for (j, hole) in region.holes.iter().enumerate() {
            let name = opening_name(parts.len(), i, j);
            let recess = match tapped.iter().find(|h| h.part == i && h.hole == j) {
                // A hole to thread is cut at the thread's bore size, deep enough for the thread
                // mill to run out of the plate.
                Some(h) => Recess {
                    name,
                    region: Region2 { outer: Loop2::circle(h.center, 0.5 * h.thread.bore()), holes: vec![] },
                    depth: total.max(cut.thickness + h.thread.bored_below()),
                    kind: "opening",
                    thread: Some(h.thread),
                },
                None => Recess { name, region: Region2 { outer: hole.ccw(), holes: vec![] }, depth: total, kind: "opening", thread: None },
            };
            openings.push(recess);
        }
    }
    recesses.extend(nearest_first(openings, &mut at));
    for Recess { name, region, depth, kind, thread } in recesses {
        let one = std::slice::from_ref(&region);
        if offset_regions(one, d / 2.0, TOLERANCE)?.is_empty() {
            if let Some(thread) = thread {
                // Checked before anything was planned; a hole that still cannot be bored stops the plan.
                plan.operations.clear();
                plan.issues.push(Issue::error(Some(name), format!("the {} mm cutter does not fit the hole of the {} thread", mm(d), thread.name)));
                return Ok(plan);
            }
            // A round opening is marked at its centre for a drill; anything else is left alone.
            if let (true, Some((centre, across))) = (depth >= total, full_circle(&region.outer)) {
                let mark = MARK_DEPTH.min(0.5 * cut.thickness);
                let h = levels(mark);
                let at = |z: f64| Vec3::new(centre.x, centre.y, z);
                let path = Toolpath {
                    moves: vec![
                        Move::Rapid { to: at(h.clearance) },
                        Move::Rapid { to: at(h.retract) },
                        Move::Feed { to: at(h.bottom), feed: 0.5 * slot.ramp_feed },
                        Move::Rapid { to: at(h.clearance) },
                    ],
                };
                plan.issues.push(Issue::warning(
                    Some(name.clone()),
                    format!(
                        "the {d:.3} mm cutter does not fit this {:.3} mm hole: its centre is marked {mark:.1} mm deep, to drill it afterwards",
                        2.0 * across
                    ),
                ));
                add(&mut plan, format!("{name} mark"), slot.rpm, path, mark)?;
                continue;
            }
            plan.issues.push(Issue::warning(
                Some(name),
                format!("the {d:.3} mm cutter does not fit this {kind}: it is left uncut, for a smaller cutter or a drill"),
            ));
            continue;
        }
        if offset_regions(one, d / 2.0 + cut.finish_stock, TOLERANCE)?.is_empty() {
            // No room to rough and finish: the wall is cut in depth passes as a slot is.
            let path = contour(one, tool, &wall(Side::Inside, 0.0), &passes_to(&slot, slot.depth, depth))?;
            add(&mut plan, name, slot.rpm, path, depth)?;
            continue;
        }
        let params = PocketParams {
            stepover: clear.width.min(d),
            climb: true,
            stock_to_leave: cut.finish_stock,
            tolerance: TOLERANCE,
            ramp_angle: cut.ramp_angle,
        };
        let rough = pocket(one, tool, &params, &passes_to(&clear, clear.depth, depth))?;
        add(&mut plan, format!("{name} rough"), clear.rpm, rough, depth)?;
        let done = contour(one, tool, &wall(Side::Inside, 0.0), &passes_to(&finish, finish.depth, depth))?;
        add(&mut plan, format!("{name} finish"), finish.rpm, done, depth)?;
    }

    // Outlines last: once one is through, nothing but tabs, screws or tape holds the part.
    let loose = "nothing holds the part once its outside is cut through: fix it down with screws through its openings or with tape";
    for (i, region) in parts.iter().enumerate() {
        let name = if parts.len() > 1 { format!("Part {} outside", i + 1) } else { "Outside".to_string() };
        let outline = [Region2 { outer: region.outer.clone(), holes: vec![] }];
        let both = |tabs: Option<Tabs>| -> Result<(crate::Toolpath, crate::Toolpath)> {
            let opts = ContourOptions { tabs, ..ContourOptions::default() };
            let never = |_: Vec2, _: Vec2, _: f64| false;
            let rough = contour_with(&outline, tool, &wall(Side::Outside, cut.finish_stock), &opts, &passes(&slot, slot.depth), &never)?;
            let done = contour_with(&outline, tool, &wall(Side::Outside, 0.0), &opts, &passes(&finish, finish.depth), &never)?;
            Ok((rough, done))
        };
        let (rough, done) = match cut.hold {
            Hold::None => {
                plan.issues.push(Issue::warning(Some(name.clone()), loose));
                both(None)?
            }
            Hold::Tabs => match both(Some(tabs_for(&region.outer, tool, cut.material, total))) {
                Ok(paths) => paths,
                // An outline with no room for tabs is still cut, and said to be loose.
                Err(_) => {
                    plan.issues.push(Issue::warning(Some(name.clone()), format!("this outline is too small for holding tabs: {loose}")));
                    both(None)?
                }
            },
        };
        add(&mut plan, format!("{name} rough"), slot.rpm, rough, total)?;
        add(&mut plan, format!("{name} finish"), finish.rpm, done, total)?;
    }

    // Threads last, a size at a time, each hole from the one before: another cutter's work.
    if let Some(t) = threading {
        let mut sizes: Vec<Thread> = Vec::new();
        for h in &tapped {
            if !sizes.contains(&h.thread) {
                sizes.push(h.thread);
            }
        }
        let mut at = Vec2 { x: 0.0, y: 0.0 };
        for thread in sizes {
            let mut left: Vec<Vec2> = tapped.iter().filter(|h| h.thread == thread).map(|h| h.center).collect();
            let mut centres = Vec::with_capacity(left.len());
            while !left.is_empty() {
                let nearest = left.iter().enumerate().min_by(|a, b| a.1.dist(at).total_cmp(&b.1.dist(at))).map_or(0, |(k, _)| k);
                at = left.swap_remove(nearest);
                centres.push(at);
            }
            let feed = thread_feed(&thread, t.fit, &t.tool, cut.material, cut.tier, machine)?;
            let (across, rise) = (thread.major + t.fit, 0.5 * thread.pitch);
            // The thread starts half a pitch above the plate and runs out below it. From hole to
            // hole the thread mill crosses the plate at the clearance height, as every other cut
            // travels: over a clamp or a screw head, not just above the plate.
            let travel = cut.clearance.max(rise + 0.5);
            let heights = Heights {
                clearance: cut.top + travel,
                retract: cut.top + travel,
                top: cut.top + rise,
                bottom: cut.top - cut.thickness - thread.below(),
            };
            let params = ThreadParams {
                heights,
                feeds: Feeds { cut: feed.feed, plunge: feed.plunge },
                pitch: thread.pitch,
                diameter: across,
                internal: true,
                right_hand: true,
                climb: true,
                depth: Some(0.5 * (across - thread.bore())),
                radial_passes: feed.radial_passes,
                spring_pass: feed.spring_pass,
            };
            let milled = thread_mill(&centres, &t.tool, &params)?;
            let n = centres.len();
            let name = format!("Thread {}, {n} hole{}", thread.name, if n == 1 { "" } else { "s" });
            for w in milled.warnings {
                plan.issues.push(Issue::warning(Some(name.clone()), w));
            }
            // What the cutter does for the thread is said of the part (its own program is told);
            // what the thread mill does, of the thread operation.
            let below = thread.bored_below().max(cut.break_through);
            plan.issues.push(Issue::warning(
                None,
                format!(
                    "{} bored Ø{} mm whatever was drawn, and {} mm below the plate, into the spoilboard, for the thread to run out of the bottom",
                    if n == 1 {
                        format!("the hole of the {} thread is", thread.name)
                    } else {
                        format!("the {n} holes of the {} threads are", thread.name)
                    },
                    mm(thread.bore()),
                    mm(below)
                ),
            ));
            if thread.bore() < CHIP_ROOM_RATIO * d {
                plan.issues.push(Issue::warning(
                    None,
                    format!(
                        "the {} mm cutter has little room for chips in the Ø{} mm hole of a {} thread: clear them with air, or use a smaller cutter",
                        mm(d),
                        mm(thread.bore()),
                        thread.name
                    ),
                ));
            }
            plan.issues.push(Issue::warning(
                Some(name.clone()),
                format!(
                    "the thread mill goes {} mm below the plate, in holes the cutter's program bores {} mm below it: run it only after that program",
                    mm(thread.below()),
                    mm(below)
                ),
            ));
            plan.operations.push(Operation { name, tool: t.tool.clone(), spindle_rpm: feed.rpm, coolant: cut.coolant, path: milled.path });
        }
    }
    Ok(plan)
}
