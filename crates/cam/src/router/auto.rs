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
//! A region lying in an opening of another is the material that opening removes, not a part
//! of its own (a sketch of a plate with a hole gives both the plate and the hole's disc).
//!
//! What cannot be cut as asked is said, not guessed: an opening the cutter does not fit is
//! left alone and reported, and so are flutes shorter than the plate is thick.

use serde::{Deserialize, Serialize};
use solidcore_record::{Loop2, Region2, Seg2, Vec2};
use solidcraft_geom::prelude::*;

use crate::router::feeds::{CutKind, FeedRequest, Material, Recipe, Tier, recipe};
use crate::router::{Issue, MachineProfile};
use crate::{
    CamError, ContourOptions, ContourParams, Cut, Feeds, Heights, Operation, PocketParams, Result, Side, TabLayout, Tabs, Tool, contour,
    contour_with, invalid, offset_regions, pocket, verify,
};

/// The most regions and openings one part may have.
const MAX_LOOPS: usize = 2000;
/// Chord error allowed on curved boundaries (mm).
const TOLERANCE: f64 = 0.005;

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

/// The operations that cut `regions` out of plate with `tool` on `machine`.
pub fn plan_part(regions: &[Region2], tool: &Tool, cut: &PartCut, machine: &MachineProfile) -> Result<Planned> {
    cut.validate()?;
    machine.validate()?;
    tool.validate()?;
    if regions.is_empty() {
        return invalid("a part needs at least one closed outline");
    }
    if regions.iter().map(|r| 1 + r.holes.len()).sum::<usize>() > MAX_LOOPS {
        return Err(CamError::TooLarge(format!("a part of more than {MAX_LOOPS} outlines and openings")));
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
    let heights = Heights { clearance: cut.top + cut.clearance, retract: cut.top + cut.retract, top: cut.top, bottom: cut.top - total };
    let passes =
        |r: &Recipe, deepest: f64| Cut { heights, feeds: Feeds { cut: r.feed, plunge: r.ramp_feed }, stepdown: even_stepdown(total, deepest) };
    let wall = |side: Side, stock: f64| ContourParams { side, climb: true, stock_to_leave: stock, tolerance: TOLERANCE, ramp_angle: cut.ramp_angle };

    // What lies in another region's opening is that opening's waste.
    let in_opening = |b: &Region2| {
        let p = b.interior_point();
        regions.iter().any(|a| !std::ptr::eq(a, b) && a.outer.contains(p) && !a.contains(p))
    };
    let parts: Vec<&Region2> = regions.iter().filter(|r| !in_opening(r)).collect();
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
    for r in [&slot, &clear, &finish] {
        for w in &r.warnings {
            if !plan.issues.iter().any(|i| i.message == *w) {
                plan.issues.push(Issue::warning(None, w.clone()));
            }
        }
    }
    let add = |plan: &mut Planned, name: String, rpm: f64, path: crate::Toolpath| -> Result<()> {
        verify(&path, &heights)?;
        plan.operations.push(Operation { name, tool: tool.clone(), spindle_rpm: rpm, coolant: cut.coolant, path });
        Ok(())
    };

    // Openings first, each from the one before it.
    let mut openings: Vec<(String, Region2)> = Vec::new();
    for (i, region) in parts.iter().enumerate() {
        for (j, hole) in region.holes.iter().enumerate() {
            let outer = hole.ccw();
            let name = if parts.len() > 1 { format!("Part {} opening {}", i + 1, j + 1) } else { format!("Opening {}", j + 1) };
            openings.push((name, Region2 { outer, holes: vec![] }));
        }
    }
    let mut at = Vec2 { x: 0.0, y: 0.0 };
    while !openings.is_empty() {
        let dist = |p: Vec2| (p.x - at.x).powi(2) + (p.y - at.y).powi(2);
        let nearest =
            openings.iter().enumerate().min_by(|a, b| dist(middle(&a.1.1.outer)).total_cmp(&dist(middle(&b.1.1.outer)))).map_or(0, |(k, _)| k);
        let (name, opening) = openings.swap_remove(nearest);
        at = middle(&opening.outer);
        let one = std::slice::from_ref(&opening);
        if offset_regions(one, d / 2.0, TOLERANCE)?.is_empty() {
            plan.issues.push(Issue::warning(
                Some(name),
                format!("the {d:.3} mm cutter does not fit this opening: it is left uncut, for a smaller cutter or a drill"),
            ));
            continue;
        }
        if offset_regions(one, d / 2.0 + cut.finish_stock, TOLERANCE)?.is_empty() {
            // No room to rough and finish: the wall is cut in depth passes as a slot is.
            let path = contour(one, tool, &wall(Side::Inside, 0.0), &passes(&slot, slot.depth))?;
            add(&mut plan, name, slot.rpm, path)?;
            continue;
        }
        let params = PocketParams {
            stepover: clear.width.min(d),
            climb: true,
            stock_to_leave: cut.finish_stock,
            tolerance: TOLERANCE,
            ramp_angle: cut.ramp_angle,
        };
        let rough = pocket(one, tool, &params, &passes(&clear, clear.depth))?;
        add(&mut plan, format!("{name} rough"), clear.rpm, rough)?;
        let done = contour(one, tool, &wall(Side::Inside, 0.0), &passes(&finish, finish.depth))?;
        add(&mut plan, format!("{name} finish"), finish.rpm, done)?;
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
        add(&mut plan, format!("{name} rough"), slot.rpm, rough)?;
        add(&mut plan, format!("{name} finish"), finish.rpm, done)?;
    }
    Ok(plan)
}
