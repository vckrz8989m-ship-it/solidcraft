//! Starting feeds and speeds for a cutter and a material on one machine.
//!
//! The chip is what is chosen: a real chip thickness for the cutter's size and the material,
//! from which the feed follows once the spindle speed is fixed. A cut narrower than half the
//! cutter makes a thinner chip than its feed per tooth (the cutter's edge leaves the material
//! before it has swept its full bite), so the feed is raised to keep the chip; a cut deeper
//! than the cutter is wide gets a smaller chip, a quarter less at two diameters and half at
//! three. The spindle's range and its power at the chosen speed, the machine's fastest feed
//! and the tightest arc it follows at the feed are held against the result, and what does not
//! fit is said in [`Recipe::warnings`].
//!
//! The chip thicknesses for aluminium follow the cutter maker's published chip loads for
//! single-flute cutters in aluminium on routers, with their low end taken as this table's high
//! one: a light gantry is not the machine those were measured on. The other materials are
//! estimates. Every recipe is a place to start a test cut from, not a promise.

use std::f64::consts::PI;

use serde::{Deserialize, Serialize};

use crate::router::MachineProfile;
use crate::{Result, Tool, invalid};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Material {
    /// Wrought aluminium that cuts cleanly: 6061, 6082, 7075.
    Aluminum,
    /// Gummy aluminium: cast plate, 5052, 1100.
    AluminumSoft,
    Brass,
    Acrylic,
    /// HDPE, UHMW, acetal.
    SoftPlastic,
    Hardwood,
    /// Softwood, plywood, MDF.
    Softwood,
}

/// Chip thickness (mm) of a single-flute cutter up to a diameter: careful, standard, bold.
type Band = (f64, [f64; 3]);

struct Data {
    /// Surface speed aimed for (m/min).
    surface_speed: f64,
    /// Specific cutting force (N/mm²).
    cutting_force: f64,
    bands: &'static [Band],
    /// Share of the single-flute chip each tooth of a 1, 2 and 3-or-more flute cutter takes.
    per_flute: [f64; 3],
    /// Below this the edge rubs instead of cutting.
    thinnest_chip: f64,
    /// Share of the feed a ramp or helix is fed at.
    ramp: f64,
}

impl Material {
    fn data(self) -> Data {
        match self {
            Material::Aluminum => Data {
                surface_speed: 350.0,
                cutting_force: 800.0,
                bands: &[
                    (3.5, [0.025, 0.040, 0.060]),
                    (5.0, [0.035, 0.050, 0.080]),
                    (7.0, [0.050, 0.075, 0.110]),
                    (10.0, [0.060, 0.100, 0.150]),
                    (f64::INFINITY, [0.080, 0.120, 0.180]),
                ],
                per_flute: [1.0, 0.75, 0.60],
                thinnest_chip: 0.020,
                ramp: 0.5,
            },
            Material::AluminumSoft => Data {
                surface_speed: 300.0,
                cutting_force: 750.0,
                bands: &[
                    (3.5, [0.030, 0.045, 0.060]),
                    (5.0, [0.040, 0.055, 0.080]),
                    (7.0, [0.055, 0.080, 0.110]),
                    (10.0, [0.070, 0.100, 0.140]),
                    (f64::INFINITY, [0.080, 0.120, 0.170]),
                ],
                per_flute: [1.0, 0.75, 0.60],
                thinnest_chip: 0.025,
                ramp: 0.5,
            },
            Material::Brass => Data {
                surface_speed: 200.0,
                cutting_force: 780.0,
                bands: &[
                    (3.5, [0.015, 0.025, 0.040]),
                    (5.0, [0.020, 0.035, 0.050]),
                    (7.0, [0.030, 0.045, 0.070]),
                    (f64::INFINITY, [0.040, 0.060, 0.090]),
                ],
                per_flute: [1.0, 0.85, 0.75],
                thinnest_chip: 0.012,
                ramp: 0.5,
            },
            Material::Acrylic => Data {
                surface_speed: 180.0,
                cutting_force: 150.0,
                bands: &[(3.5, [0.080, 0.110, 0.150]), (7.0, [0.120, 0.180, 0.250]), (f64::INFINITY, [0.200, 0.280, 0.380])],
                per_flute: [1.0, 0.7, 0.5],
                thinnest_chip: 0.060,
                ramp: 0.5,
            },
            Material::SoftPlastic => Data {
                surface_speed: 170.0,
                cutting_force: 100.0,
                bands: &[(3.5, [0.080, 0.120, 0.160]), (7.0, [0.130, 0.200, 0.280]), (f64::INFINITY, [0.200, 0.300, 0.400])],
                per_flute: [1.0, 0.7, 0.5],
                thinnest_chip: 0.060,
                ramp: 0.6,
            },
            Material::Hardwood => Data {
                surface_speed: 450.0,
                cutting_force: 60.0,
                bands: &[(3.5, [0.050, 0.080, 0.110]), (7.0, [0.090, 0.140, 0.200]), (f64::INFINITY, [0.130, 0.200, 0.280])],
                per_flute: [1.2, 1.0, 0.8],
                thinnest_chip: 0.040,
                ramp: 0.4,
            },
            Material::Softwood => Data {
                surface_speed: 500.0,
                cutting_force: 40.0,
                bands: &[(3.5, [0.060, 0.100, 0.140]), (7.0, [0.120, 0.180, 0.260]), (f64::INFINITY, [0.180, 0.260, 0.360])],
                per_flute: [1.2, 1.0, 0.8],
                thinnest_chip: 0.050,
                ramp: 0.4,
            },
        }
    }

    /// Whether the chip thicknesses come from published chip loads, not from an estimate.
    pub fn is_sourced(self) -> bool {
        matches!(self, Material::Aluminum)
    }
}

/// How hard to cut: a first cut on an unproven machine, the usual cut, or one for a machine
/// that has shown it can take it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    Careful,
    #[default]
    Standard,
    Bold,
}

impl Tier {
    fn index(self) -> usize {
        match self {
            Tier::Careful => 0,
            Tier::Standard => 1,
            Tier::Bold => 2,
        }
    }
}

/// The kind of cut, which sets how wide and deep the cutter is in the material.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CutKind {
    /// The full width of the cutter, as round a profile cut from plate.
    Slot,
    /// Passes four tenths of the cutter apart.
    Pocket,
    /// A narrow, deep cut with the engagement held down.
    Adaptive,
    /// A light pass along a wall left by roughing.
    Finish,
    /// A helix down a hole; the depth is the helix's pitch.
    Bore,
}

struct Engagement {
    /// Width as a share of the diameter, by tier; `None` for a finishing pass.
    width: Option<[f64; 3]>,
    /// Depth as a share of the diameter, by tier; `None` for a helix.
    depth: Option<[f64; 3]>,
    /// Share of the material's chip thickness.
    chip: f64,
}

impl CutKind {
    fn engagement(self) -> Engagement {
        match self {
            CutKind::Slot => Engagement { width: Some([1.0, 1.0, 1.0]), depth: Some([0.15, 0.25, 0.50]), chip: 0.8 },
            CutKind::Pocket => Engagement { width: Some([0.40, 0.40, 0.45]), depth: Some([0.25, 0.50, 1.00]), chip: 1.0 },
            CutKind::Adaptive => Engagement { width: Some([0.10, 0.20, 0.25]), depth: Some([0.50, 1.00, 1.50]), chip: 1.0 },
            CutKind::Finish => Engagement { width: None, depth: Some([1.0, 2.0, 2.0]), chip: 0.6 },
            CutKind::Bore => Engagement { width: Some([1.0, 1.0, 1.0]), depth: None, chip: 0.7 },
        }
    }
}

/// What to work a recipe out for. The optional values replace the ones the kind of cut would
/// choose.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct FeedRequest {
    pub material: Material,
    /// Cutter diameter (mm).
    pub diameter: f64,
    pub flutes: u32,
    pub kind: CutKind,
    #[serde(default)]
    pub tier: Tier,
    /// A spindle speed to hold to (brought inside the spindle's range).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rpm: Option<f64>,
    /// Width of cut (mm).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<f64>,
    /// Depth of cut (mm); for a bore, the helix's pitch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depth: Option<f64>,
    /// The cutter's flute length (mm): the depth never exceeds it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flute_length: Option<f64>,
}

impl FeedRequest {
    pub fn new(material: Material, diameter: f64, flutes: u32, kind: CutKind) -> FeedRequest {
        FeedRequest { material, diameter, flutes, kind, tier: Tier::Standard, rpm: None, width: None, depth: None, flute_length: None }
    }

    fn validate(&self) -> Result<()> {
        if !(self.diameter > 0.0) || !(self.diameter <= 100.0) {
            return invalid("a cutter's diameter must be above 0 and at most 100 mm");
        }
        if self.flutes == 0 || self.flutes > 12 {
            return invalid("a cutter has 1 to 12 flutes");
        }
        for (v, what) in
            [(self.rpm, "spindle speed"), (self.width, "width of cut"), (self.depth, "depth of cut"), (self.flute_length, "flute length")]
        {
            if let Some(v) = v
                && (!(v > 0.0) || !(v <= 1.0e6))
            {
                return invalid(format!("the {what} must be above 0"));
            }
        }
        Ok(())
    }
}

/// Feeds and speeds for one cut, and what they ask of the machine.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Recipe {
    pub rpm: f64,
    /// Surface speed at that rpm (m/min).
    pub surface_speed: f64,
    /// The chip's real thickness (mm).
    pub chip_thickness: f64,
    /// How much a cut this narrow thins the chip: the feed per tooth is the chip thickness
    /// times this (never taken above 2).
    pub chip_thinning: f64,
    pub feed_per_tooth: f64,
    /// Cutting feed (mm/min), to the nearest 10.
    pub feed: f64,
    /// Feed down a ramp or helix (mm/min), to the nearest 10.
    pub ramp_feed: f64,
    /// Width of cut (mm).
    pub width: f64,
    /// Depth of cut (mm); for a bore, the helix's pitch.
    pub depth: f64,
    /// Material removed (cm³/min).
    pub removal_rate: f64,
    /// Power the cut takes at the cutter (kW).
    pub cut_power: f64,
    /// Power the spindle has at the cutter at this rpm (kW).
    pub spindle_power: f64,
    /// Force along the cut at the cutter's edge (N).
    pub force: f64,
    /// The tightest arc the machine follows at the feed (mm).
    pub min_arc_radius: f64,
    /// Whether the chip thickness comes from published chip loads ([`Material::is_sourced`]).
    pub sourced: bool,
    pub warnings: Vec<String>,
}

impl Recipe {
    /// Writes the recipe into a tool's own speeds and feeds, so operations that read them from
    /// the tool use it.
    pub fn apply(&self, tool: &mut Tool) {
        tool.spindle_rpm = Some(self.rpm);
        tool.feed = Some(self.feed);
        tool.plunge_feed = Some(self.ramp_feed);
        tool.stepdown = Some(self.depth);
        tool.stepover = Some(self.width);
    }
}

/// How much thinner than the feed per tooth the chip is when the cut is `width` wide: 1 at
/// half the diameter and beyond.
pub fn chip_thinning(diameter: f64, width: f64) -> f64 {
    if !(width > 0.0) || width >= diameter / 2.0 {
        return 1.0;
    }
    diameter / (2.0 * (diameter * width - width * width).sqrt())
}

fn depth_share(diameter: f64, depth: f64) -> f64 {
    let ratio = depth / diameter;
    if ratio <= 1.0 { 1.0 } else { (1.0 - 0.25 * (ratio - 1.0)).max(0.5) }
}

fn to_ten(x: f64) -> f64 {
    (x / 10.0).round() * 10.0
}

/// The recipe for `request` on `machine`.
pub fn recipe(request: &FeedRequest, machine: &MachineProfile) -> Result<Recipe> {
    request.validate()?;
    machine.validate()?;
    let data = request.material.data();
    let engagement = request.kind.engagement();
    let tier = request.tier.index();
    let d = request.diameter;
    let spindle = &machine.spindle;
    let mut warnings = Vec::new();
    let pick = |values: [f64; 3]| values.get(tier).copied().unwrap_or(values[1]);

    let ideal = data.surface_speed * 1000.0 / (PI * d);
    let rpm = match request.rpm {
        Some(asked) => {
            let held = spindle.clamp(asked);
            if held != asked {
                warnings.push(format!("{asked:.0} rpm is outside the spindle's range: {held:.0} rpm is used"));
            }
            held
        }
        None => spindle.clamp((spindle.clamp(ideal) / 500.0).round() * 500.0),
    };
    let surface_speed = PI * d * rpm / 1000.0;
    if request.rpm.is_none() && ideal < spindle.rpm_min {
        warnings.push(format!("a cutter this large wants {ideal:.0} rpm, below the spindle's slowest: use it for light cuts only"));
    }

    let width = match (request.width, engagement.width) {
        (Some(w), _) => w,
        (None, Some(shares)) => pick(shares) * d,
        (None, None) => (0.035 * d).clamp(0.1, 0.3),
    }
    .min(d);
    let mut depth = match (request.depth, engagement.depth) {
        (Some(z), _) => z,
        (None, Some(shares)) => pick(shares) * d,
        (None, None) => (0.1 * d).clamp(0.3, 1.0),
    };
    if let Some(flute) = request.flute_length
        && depth > flute
    {
        warnings.push(format!("a depth of {depth:.2} mm is more than the {flute:.2} mm of flute: {flute:.2} mm is used"));
        depth = flute;
    }

    let flutes = f64::from(request.flutes);
    let per_flute = match request.flutes {
        1 => data.per_flute[0],
        2 => data.per_flute[1],
        _ => data.per_flute[2],
    };
    let band = data.bands.iter().find(|(up_to, _)| d <= *up_to).or(data.bands.last()).map_or([0.05; 3], |(_, chips)| *chips);
    let mut chip = (pick(band) * engagement.chip * per_flute * depth_share(d, depth)).max(data.thinnest_chip);
    let thinning = chip_thinning(d, width);
    let used = thinning.min(2.0);
    let mut feed_per_tooth = chip * used;
    let mut feed = rpm * flutes * feed_per_tooth;
    if feed > machine.max_feed {
        warnings.push(format!(
            "the feed for this chip, {feed:.0} mm/min, is more than the machine holds: {:.0} mm/min is used, with a thinner chip",
            machine.max_feed
        ));
        feed = machine.max_feed;
        feed_per_tooth = feed / (rpm * flutes);
        chip = feed_per_tooth / used;
    }
    if chip < data.thinnest_chip * 0.999 {
        warnings.push(format!(
            "the chip, {chip:.3} mm, is thinner than the {:.3} mm at which this material stops cutting cleanly: lower the spindle speed",
            data.thinnest_chip
        ));
    }

    let removal_rate = width * depth * feed / 1000.0;
    let small_chip = chip < 0.05 && data.cutting_force > 300.0;
    let cutting_force = data.cutting_force * if small_chip { 1.5 } else { 1.0 };
    let cut_power = removal_rate * cutting_force / 60000.0;
    let spindle_power = spindle.power_at(rpm);
    let force = if surface_speed > 0.0 { cut_power * 1000.0 / (surface_speed / 60.0) } else { 0.0 };
    if cut_power > spindle_power {
        warnings.push(format!(
            "the cut takes about {cut_power:.2} kW and the spindle has {spindle_power:.2} kW at {rpm:.0} rpm: cut narrower or shallower"
        ));
    }
    if force > 150.0 {
        warnings.push(format!("about {force:.0} N at the cutter is a lot for a gantry: expect it to bend, and cut narrower or shallower"));
    }
    let min_arc_radius = machine.min_arc_radius(feed);
    if request.kind == CutKind::Adaptive && min_arc_radius > 0.35 * d {
        warnings.push(format!(
            "the machine needs arcs of at least {min_arc_radius:.1} mm radius to hold {feed:.0} mm/min: tighter turns are cut slower, with a thinner chip"
        ));
    }

    Ok(Recipe {
        rpm,
        surface_speed,
        chip_thickness: chip,
        chip_thinning: thinning,
        feed_per_tooth,
        feed: to_ten(feed),
        ramp_feed: to_ten(feed * data.ramp),
        width,
        depth,
        removal_rate,
        cut_power,
        spindle_power,
        force,
        min_arc_radius,
        sourced: request.material.is_sourced(),
        warnings,
    })
}
