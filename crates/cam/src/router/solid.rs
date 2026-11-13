//! From a solid part's surface to the operations that carve it out of a block.
//!
//! The surface is the part as the tool sees it from above, in work coordinates. The block is the
//! part's extent grown sideways by a margin and upwards by the stock above it; the cuts stop a
//! skin above the part's lowest point, so the part stays attached to what is left of the block
//! under it. Three operations, in the order they are cut:
//!
//! 1. roughing with a flat end mill, level by level, leaving a little on the part;
//! 2. the walls, level by level round the part with the same cutter, to size;
//! 3. finishing passes side by side over the whole part, with a ball end mill when one is
//!    given (the scallops it leaves are set by its stepover), else with the roughing cutter.
//!
//! Speeds and feeds are [`feeds`](super::feeds)' for the machine, as they are for plate parts.
//! Roughing and the walls come down into material at half the ramp feed: the surface roughing
//! plunges straight down, so the roughing cutter must cut at its centre.
//!
//! A vertical tool reaches what it can see from above: an undercut, or the far side of the
//! part, is left for the part to be turned over.

use serde::{Deserialize, Serialize};
use solidcore_record::Vec2;

use crate::router::auto::Planned;
use crate::router::feeds::{CutKind, FeedRequest, Material, Recipe, Tier, recipe};
use crate::router::{Issue, MachineProfile};
use crate::surface_ops::{Boundary, ParallelParams, RoughingParams, WaterlineParams, parallel, roughing, waterline};
use crate::{Feeds, Heights, Operation, Result, Surface, Tool, ToolKind, invalid};

/// How a solid part is carved from its block.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct SolidCut {
    pub material: Material,
    #[serde(default)]
    pub tier: Tier,
    /// How far beyond the part's sides the cutter's centre goes (mm): the block is at least
    /// this much larger than the part, plus the cutter's radius, on every side.
    pub margin: f64,
    /// Stock above the part's highest point (mm). The block's top is work Z of that point plus
    /// this.
    pub stock_top: f64,
    /// Left on the part by roughing for the walls and the finishing passes (mm).
    pub finish_stock: f64,
    /// Left uncut above the part's lowest point (mm), so the part is not cut free of the block.
    pub skin: f64,
    /// Distance between finishing passes (mm); left out, a tenth of a ball end mill's diameter
    /// or a third of a flat one's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finish_stepover: Option<f64>,
    /// Height the tool travels at between cuts, above the block's top (mm).
    pub clearance: f64,
    /// Height rapids stop at above the block's top (mm).
    pub retract: f64,
    pub coolant: bool,
}

impl SolidCut {
    /// The usual cut: the cutter goes a diameter of 6 mm beyond the part, 0.5 mm of stock above
    /// it, 0.3 mm left for finishing, a 0.5 mm skin under it, travel 5 mm above the block.
    pub fn new(material: Material) -> SolidCut {
        SolidCut {
            material,
            tier: Tier::Standard,
            margin: 6.0,
            stock_top: 0.5,
            finish_stock: 0.3,
            skin: 0.5,
            finish_stepover: None,
            clearance: 5.0,
            retract: 1.5,
            coolant: true,
        }
    }

    fn validate(&self) -> Result<()> {
        let within = |v: f64, lo: f64, hi: f64| v.is_finite() && v >= lo && v <= hi;
        if !within(self.margin, 0.0, 1000.0) {
            return invalid("the margin round the part must be 0 to 1000 mm");
        }
        if !within(self.stock_top, 0.0, 500.0) {
            return invalid("the stock above the part must be 0 to 500 mm");
        }
        if !within(self.finish_stock, 0.0, 5.0) {
            return invalid("the stock left for finishing must be 0 to 5 mm");
        }
        if !within(self.skin, 0.0, 500.0) {
            return invalid("the skin left under the part must be 0 to 500 mm");
        }
        if self.finish_stepover.is_some_and(|s| !within(s, 0.01, 100.0)) {
            return invalid("the finishing stepover must be 0.01 to 100 mm");
        }
        if !(self.retract > 0.0) || !(self.clearance >= self.retract) || !(self.clearance <= 1000.0) {
            return invalid("the clearance height must be at least the retract height, which must be above 0");
        }
        Ok(())
    }
}

/// Levels no deeper than `deepest` that share `total` evenly.
fn even_stepdown(total: f64, deepest: f64) -> f64 {
    let passes = (total / deepest.max(1e-6) - 1e-9).ceil().max(1.0);
    total / passes
}

/// The operations that carve `surface` out of a block with `rough` (a flat end mill) and
/// `finish` (the finishing cutter; `None`: the roughing cutter finishes too) on `machine`.
pub fn plan_solid(surface: &Surface, rough: &Tool, finish: Option<&Tool>, cut: &SolidCut, machine: &MachineProfile) -> Result<Planned> {
    cut.validate()?;
    machine.validate()?;
    rough.validate()?;
    if rough.kind != ToolKind::FlatEndmill {
        return invalid("the roughing cutter must be a flat end mill");
    }
    let finisher = finish.unwrap_or(rough);
    finisher.validate()?;
    if !matches!(finisher.kind, ToolKind::FlatEndmill | ToolKind::BallEndmill | ToolKind::BullNose) {
        return invalid("the finishing cutter must be a flat, ball or bull nose end mill");
    }
    let bounds = surface.bounds();
    if surface.is_empty() || bounds.is_empty() {
        return invalid("the part has no surface to cut");
    }
    let (lo, hi) = (bounds.min, bounds.max);
    if ![lo.x, lo.y, lo.z, hi.x, hi.y, hi.z].iter().all(|v| v.is_finite() && v.abs() <= 1.0e5) {
        return invalid("the part must lie within 100 m of the work zero");
    }
    let top = hi.z + cut.stock_top;
    let height = hi.z - lo.z;
    let mut plan = Planned::default();
    if cut.skin >= height {
        plan.issues.push(Issue::error(
            None,
            format!("a skin of {:.2} mm leaves nothing of a part {height:.2} mm high to cut: give a smaller one", cut.skin),
        ));
        return Ok(plan);
    }
    let floor = lo.z + cut.skin;
    let depth = top - floor;
    for (tool, what) in [(rough, "roughing"), (finisher, "finishing")] {
        if depth > tool.flute_length {
            plan.issues.push(Issue::error(
                None,
                format!(
                    "the cut is {depth:.2} mm deep and the {what} cutter has {:.2} mm of flute: use a longer cutter, or leave a thicker skin",
                    tool.flute_length
                ),
            ));
            return Ok(plan);
        }
    }
    let ask = |tool: &Tool, kind: CutKind| -> Result<Recipe> {
        let mut request = FeedRequest::new(cut.material, tool.diameter, tool.flutes, kind);
        request.tier = cut.tier;
        request.flute_length = Some(tool.flute_length);
        recipe(&request, machine)
    };
    let (slot, clear, walls, fine) =
        (ask(rough, CutKind::Slot)?, ask(rough, CutKind::Pocket)?, ask(rough, CutKind::Finish)?, ask(finisher, CutKind::Finish)?);
    for r in [&slot, &clear, &walls, &fine] {
        for w in &r.warnings {
            if !plan.issues.iter().any(|i| i.message == *w) {
                plan.issues.push(Issue::warning(None, w.clone()));
            }
        }
    }
    let heights = Heights { clearance: top + cut.clearance, retract: top + cut.retract, top, bottom: floor };
    // The cutter's centre goes `margin` beyond the part; the finishing cutter only as far as
    // it must to come down the part's sides.
    let rect =
        |beyond: f64| Boundary { rect: Some([Vec2::new(lo.x - beyond, lo.y - beyond), Vec2::new(hi.x + beyond, hi.y + beyond)]), region: None };
    let margin = cut.margin.max(rough.radius() + cut.finish_stock);
    let add = |plan: &mut Planned, name: &str, tool: &Tool, rpm: f64, path: crate::Toolpath| {
        if !path.moves.is_empty() {
            plan.operations.push(Operation { name: name.to_string(), tool: tool.clone(), spindle_rpm: rpm, coolant: cut.coolant, path });
        }
    };

    let roughed = roughing(
        surface,
        rough,
        &RoughingParams {
            heights,
            feeds: Feeds { cut: slot.feed, plunge: 0.5 * slot.ramp_feed },
            stepdown: even_stepdown(depth, slot.depth),
            stepover: clear.width.min(rough.diameter),
            angle: 0.0,
            boundary: rect(margin),
            stock_to_leave: cut.finish_stock,
            step: 0.25 * rough.diameter,
            tolerance: 0.02,
            zigzag: true,
        },
    )?;
    add(&mut plan, "Rough", rough, slot.rpm, roughed);

    let around = waterline(
        surface,
        rough,
        &WaterlineParams {
            heights,
            feeds: Feeds { cut: walls.feed, plunge: 0.5 * walls.ramp_feed },
            stepdown: even_stepdown(depth, (0.1 * rough.diameter).max(0.2)),
            boundary: rect(margin),
            stock_to_leave: 0.0,
            step: (0.1 * rough.diameter).clamp(0.2, 1.0),
            tolerance: 0.01,
            climb: true,
        },
    )?;
    add(&mut plan, "Walls", rough, walls.rpm, around);

    let ball = finisher.kind == ToolKind::BallEndmill;
    let stepover = cut.finish_stepover.unwrap_or(if ball { 0.1 } else { 0.33 } * finisher.diameter).min(finisher.diameter);
    let over = parallel(
        surface,
        finisher,
        &ParallelParams {
            heights,
            feeds: Feeds { cut: fine.feed, plunge: 0.5 * fine.ramp_feed },
            stepover,
            angle: 0.0,
            boundary: rect(finisher.radius()),
            stock_to_leave: 0.0,
            step: 0.25 * finisher.diameter,
            tolerance: 0.01,
            zigzag: true,
            descent_limit: crate::surface_ops::DEFAULT_DESCENT_LIMIT,
            descent_feed: None,
        },
    )?;
    add(&mut plan, "Finish", finisher, fine.rpm, over);

    if cut.skin > 0.0 {
        plan.issues
            .push(Issue::warning(None, format!("the part stays on a {:.2} mm skin of the block under it: saw or cut it free afterwards", cut.skin)));
    } else {
        plan.issues.push(Issue::warning(None, "with no skin left the part is cut free of the block: nothing holds it at the end"));
    }
    if !ball && finish.is_none() {
        plan.issues.push(Issue::warning(None, "a flat end mill finishes slopes and curves in steps: a ball end mill follows them"));
    }
    Ok(plan)
}
