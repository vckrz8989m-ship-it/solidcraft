//! The Cut Out Part dialog (`cam.cut_out`): the sketch that is the part's outline, the plate
//! (material and thickness) and the cutter, then a file dialog for the G-code. A second sketch
//! may give pockets: its closed shapes are cut to a depth before the openings. Check plans the
//! cuts on a copy of the design, lists them with what the plan warns of and draws their paths
//! over the part ([`show`]): cuts in the accent colour, moves between them faintly. Nothing is
//! written or kept until Save G-code. The values start from the part the design last cut out,
//! else from the last program saved in any design; the cutters used before are offered by name
//! (`6 mm · 1F · 18 mm`: diameter, flutes, flute length), kept in the preferences ([`Memory`]).
//!
//! The part's round holes are listed (`cam.holes`), each with a thread to choose. Once one has
//! a thread the dialog asks for the thread mill (those used before by name, each with the fit
//! it was last used at), and Save G-code writes two files: the cuts, then the threads.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use solidcraft_engine::doc::FeatureKind;
use solidcraft_engine::doc::cam::CamOperation;

use crate::SolveApp;
use crate::dialogs::{Kind, combo, field, form_row};
use crate::selection::SelInput;
use solidcore_record::Vec3;
use solidcraft_engine::doc::cam::CamGeometry;
use solidcraft_theme::Tokens;

/// The materials, as `cam.cut_out` names them, and their labels.
pub(crate) const MATERIALS: [(&str, &str); 7] = [
    ("aluminum", "Aluminium 6061"),
    ("aluminum_soft", "Aluminium, soft or cast"),
    ("brass", "Brass"),
    ("acrylic", "Acrylic"),
    ("soft_plastic", "HDPE and soft plastics"),
    ("hardwood", "Hardwood"),
    ("softwood", "Softwood, plywood and MDF"),
];
/// How hard to cut, as `cam.cut_out` names it, and its label.
pub(crate) const TIERS: [(&str, &str); 3] = [("careful", "Careful (first cut)"), ("standard", "Standard"), ("bold", "Bold (proven machine)")];
pub(crate) const FLUTES: [&str; 4] = ["1", "2", "3", "4"];
/// What holds the part while its outside is cut through, as `cam.cut_out` names it, and its label.
const HOLDS: [(&str, &str); 2] = [("tabs", "Holding tabs"), ("none", "None (screws or tape)")];
/// The setup the dialog's parts are kept in.
const SETUP: &str = "Router";
/// Most points of a plan's paths drawn in a frame: past it, every few are.
const MAX_DRAWN: usize = 60_000;

/// Most cutters remembered.
const MAX_CUTTERS: usize = 12;
/// Most holes listed with a thread to choose.
const MAX_HOLE_ROWS: usize = 40;

/// A thread mill as the dialog's fields hold it, with the fit it was last used at.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ThreadCutter {
    pub diameter: String,
    /// Number of flutes, 1 to 4.
    pub flutes: usize,
    pub reach: String,
    /// Added to the threads' major diameter: above 0 loosens them.
    #[serde(default)]
    pub fit: String,
}

impl ThreadCutter {
    /// `4.572 mm · 3F · 12 mm`: diameter, flutes, reach.
    pub fn label(&self) -> String {
        format!("{} · {}F · {}", self.diameter.trim(), self.flutes, self.reach.trim())
    }

    fn same(&self, other: &ThreadCutter) -> bool {
        self.diameter.trim() == other.diameter.trim() && self.flutes == other.flutes && self.reach.trim() == other.reach.trim()
    }
}

/// A round hole of the part, as `cam.holes` lists it.
#[derive(Clone, Debug, PartialEq)]
pub struct Hole {
    pub name: String,
    /// Its centre in the setup's XY (mm).
    pub at: [f64; 2],
    pub diameter: f64,
}

/// A hole's centre to the micrometre: what a thread choice is kept by.
fn spot(at: [f64; 2]) -> [i64; 2] {
    [(at[0] * 1000.0).round() as i64, (at[1] * 1000.0).round() as i64]
}

/// A cutter as the dialog's fields hold it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Cutter {
    pub diameter: String,
    /// Number of flutes, 1 to 4.
    pub flutes: usize,
    pub flute_length: String,
}

impl Cutter {
    /// `6 mm · 1F · 18 mm`: diameter, flutes, flute length.
    pub fn label(&self) -> String {
        format!("{} · {}F · {}", self.diameter.trim(), self.flutes, self.flute_length.trim())
    }

    fn same(&self, other: &Cutter) -> bool {
        self.diameter.trim() == other.diameter.trim() && self.flutes == other.flutes && self.flute_length.trim() == other.flute_length.trim()
    }
}

/// What the dialog remembers between designs (the `cut_out_memory` preference): the cutters
/// used, newest first, and the last program's plate and choices.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Memory {
    #[serde(default)]
    pub cutters: Vec<Cutter>,
    #[serde(default)]
    pub material: String,
    #[serde(default)]
    pub thickness: String,
    #[serde(default)]
    pub tier: String,
    #[serde(default)]
    pub hold: String,
    #[serde(default)]
    pub pocket_depth: String,
    /// The thread mills used, newest first.
    #[serde(default)]
    pub thread_mills: Vec<ThreadCutter>,
}

impl Memory {
    /// What the preference holds; a text that is not a memory is none.
    pub fn read(text: &str) -> Memory {
        let mut memory: Memory = serde_json::from_str(text).unwrap_or_default();
        memory.cutters.retain(|c| {
            (1..=FLUTES.len()).contains(&c.flutes) && !c.diameter.trim().is_empty() && c.diameter.len() <= 64 && c.flute_length.len() <= 64
        });
        memory.cutters.truncate(MAX_CUTTERS);
        memory.thread_mills.retain(|c| {
            (1..=FLUTES.len()).contains(&c.flutes)
                && !c.diameter.trim().is_empty()
                && c.diameter.len() <= 64
                && c.reach.len() <= 64
                && c.fit.len() <= 64
        });
        memory.thread_mills.truncate(MAX_CUTTERS);
        memory
    }
}

/// A run of a planned path in the model: a cut, or the move to the next one.
#[derive(Clone, Debug, PartialEq)]
pub struct Run {
    pub cutting: bool,
    pub points: Vec<[f64; 3]>,
}

/// The dialog's values.
#[derive(Clone, Debug, PartialEq)]
pub struct CutOut {
    /// The outline's sketch (a feature id; 0 while the design has none).
    pub sketch: u64,
    /// Index into [`MATERIALS`].
    pub material: usize,
    pub thickness: String,
    pub diameter: String,
    /// Index into [`FLUTES`].
    pub flutes: usize,
    pub flute_length: String,
    /// Index into [`TIERS`].
    pub tier: usize,
    /// Index into [`HOLDS`].
    pub hold: usize,
    /// The cutters offered by name: those remembered, then the design's own.
    pub cutters: Vec<Cutter>,
    /// The cutter chosen among them; past their end, a new one typed in the fields.
    pub cutter: usize,
    /// The sketch whose closed shapes are pockets (a feature id; 0: none).
    pub pocket_sketch: u64,
    pub pocket_depth: String,
    /// The part's round holes, read for `holes_of` (the outline's sketch and the design).
    pub holes: Vec<Hole>,
    pub holes_of: String,
    /// The threads `cam.holes` offers, by name.
    pub thread_names: Vec<String>,
    /// The threads chosen: a hole's centre to the micrometre, and the thread's name.
    pub threads: Vec<([i64; 2], String)>,
    /// The thread mills offered by name, and the one chosen; past their end, a new one typed.
    pub thread_mills: Vec<ThreadCutter>,
    pub thread_mill: usize,
    pub tm_diameter: String,
    /// Index into [`FLUTES`].
    pub tm_flutes: usize,
    pub tm_reach: String,
    pub tm_fit: String,
    /// The paths of the plan Check last made, drawn over the part.
    pub paths: Vec<Run>,
    /// What Check last found, a line each; the flag says the plan was refused.
    pub report: Vec<String>,
    pub refused: bool,
    /// The values and the design the report was made for: any other and it is not shown.
    pub report_of: String,
}

impl CutOut {
    /// The thread chosen for the hole at `at`.
    pub fn thread_of(&self, at: [f64; 2]) -> Option<&str> {
        let key = spot(at);
        self.threads.iter().find(|t| t.0 == key).map(|t| t.1.as_str())
    }

    /// Give the hole at `at` the thread called `thread`; an empty name takes its thread off.
    pub fn set_thread(&mut self, at: [f64; 2], thread: &str) {
        let key = spot(at);
        self.threads.retain(|t| t.0 != key);
        if !thread.trim().is_empty() {
            self.threads.push((key, thread.trim().to_owned()));
        }
    }

    /// The thread mill in the fields.
    fn typed_mill(&self) -> ThreadCutter {
        ThreadCutter {
            diameter: self.tm_diameter.trim().to_owned(),
            flutes: self.tm_flutes + 1,
            reach: self.tm_reach.trim().to_owned(),
            fit: self.tm_fit.trim().to_owned(),
        }
    }
}

/// The round holes of the part `sketch` draws and the threads offered for them: what
/// `cam.holes` replies on a copy of the design. None while the sketch gives no part.
fn holes(app: &SolveApp, sketch: u64) -> (Vec<Hole>, Vec<String>) {
    if sketch == 0 {
        return (Vec::new(), Vec::new());
    }
    let asked = [("cam.holes".to_string(), json!({"sketch": sketch, "setup": SETUP}))];
    let Ok(preview) = app.session.preview(&asked) else { return (Vec::new(), Vec::new()) };
    let Some(reply) = preview.replies.first() else { return (Vec::new(), Vec::new()) };
    let found = reply["holes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|h| {
            let (x, y, diameter) = (h["at"][0].as_f64()?, h["at"][1].as_f64()?, h["diameter"].as_f64()?);
            Some(Hole { name: h["name"].as_str()?.to_owned(), at: [x, y], diameter })
        })
        .collect();
    let names = reply["threads"].as_array().into_iter().flatten().filter_map(|t| t.as_str().map(str::to_owned)).collect();
    (found, names)
}

/// A number as a row's label shows it: at most three decimals.
fn short(v: f64) -> String {
    format!("{}", (v * 1000.0).round() / 1000.0)
}

/// The design's sketches, oldest first: (feature id, name).
fn sketches(app: &SolveApp) -> Vec<(u64, String)> {
    app.session
        .doc
        .features
        .iter()
        .filter(|f| matches!(f.kind, FeatureKind::Sketch { .. }) && !f.suppressed)
        .map(|f| (f.id, f.name.clone()))
        .collect()
}

/// The sketch a part's outline is most likely in: the one whose closed shapes cover the most
/// (a pocket's or a hole pattern's sketch lies inside the outline's), the newest of equals.
fn largest_sketch(app: &SolveApp) -> u64 {
    let state = app.session.model.state();
    let covered = |id: u64| state.sketch(id).map_or(0.0, |s| s.profiles.iter().map(|p| p.area.abs()).filter(|a| a.is_finite()).sum::<f64>());
    sketches(app).iter().rev().map(|s| (s.0, covered(s.0))).fold((0, -1.0), |best, next| if next.1 > best.1 { next } else { best }).0
}

fn mm(v: f64) -> String {
    format!("{} mm", (v * 1000.0).round() / 1000.0)
}

pub fn start(app: &SolveApp, id: &str) -> Option<(Kind, Vec<SelInput>)> {
    if id != "cam.cut_out" {
        return None;
    }
    let mut k = CutOut {
        sketch: largest_sketch(app),
        material: 0,
        thickness: "6 mm".into(),
        diameter: "6 mm".into(),
        flutes: 0,
        flute_length: "18 mm".into(),
        tier: 1,
        hold: 0,
        cutters: Vec::new(),
        cutter: 0,
        pocket_sketch: 0,
        pocket_depth: "2 mm".into(),
        holes: Vec::new(),
        holes_of: String::new(),
        thread_names: Vec::new(),
        threads: Vec::new(),
        thread_mills: Vec::new(),
        thread_mill: 0,
        tm_diameter: "3.429 mm".into(),
        tm_flutes: 2,
        tm_reach: "12 mm".into(),
        tm_fit: "0 mm".into(),
        paths: Vec::new(),
        report: Vec::new(),
        refused: false,
        report_of: String::new(),
    };
    // The last program saved in any design gives the plate, the choices and the cutters known.
    let memory = Memory::read(&app.preferences.cut_out_memory);
    k.material = MATERIALS.iter().position(|m| m.0 == memory.material).unwrap_or(k.material);
    k.tier = TIERS.iter().position(|t| t.0 == memory.tier).unwrap_or(k.tier);
    k.hold = HOLDS.iter().position(|h| h.0 == memory.hold).unwrap_or(k.hold);
    if !memory.thickness.trim().is_empty() {
        k.thickness = memory.thickness.clone();
    }
    if !memory.pocket_depth.trim().is_empty() {
        k.pocket_depth = memory.pocket_depth.clone();
    }
    k.cutters = memory.cutters;
    k.thread_mills = memory.thread_mills;
    if let Some(first) = k.thread_mills.first() {
        (k.tm_diameter, k.tm_flutes, k.tm_reach, k.tm_fit) = (first.diameter.clone(), first.flutes - 1, first.reach.clone(), first.fit.clone());
    }
    // A thread mill is not a cutter to cut the part out with.
    for t in app.session.doc.cam.tools.iter().filter(|t| json!(t.kind).as_str() != Some("thread_mill")) {
        let cutter = Cutter { diameter: mm(t.diameter), flutes: t.flutes as usize, flute_length: mm(t.flute_length) };
        if (1..=FLUTES.len()).contains(&cutter.flutes) && !k.cutters.iter().any(|c| c.same(&cutter)) && k.cutters.len() < 2 * MAX_CUTTERS {
            k.cutters.push(cutter);
        }
    }
    if let Some(first) = k.cutters.first() {
        (k.diameter, k.flutes, k.flute_length) = (first.diameter.clone(), first.flutes - 1, first.flute_length.clone());
    }
    // The part cut out last gives the plate, the cutter and, while they are still there, its sketches.
    let cam = &app.session.doc.cam;
    let last = cam.setup(SETUP).and_then(|s| s.operations.iter().rev().find(|op| matches!(op, CamOperation::Part { .. })));
    if let Some(CamOperation::Part { tool, material, thickness, tier, hold, geometry, pockets, threads, thread_tool, thread_fit, .. }) = last {
        // Its threads, while their holes are still there (the holes are read as the rows are drawn).
        k.threads = threads.iter().map(|t| (spot(t.at), t.thread.clone())).collect();
        if let Some(t) = thread_tool.and_then(|n| cam.tool(n)).filter(|_| !threads.is_empty()) {
            k.tm_diameter = mm(t.diameter);
            k.tm_reach = mm(t.flute_length);
            k.tm_flutes = FLUTES.iter().position(|f| f.parse::<u32>().is_ok_and(|n| n == t.flutes)).unwrap_or(k.tm_flutes);
            k.tm_fit = thread_fit.as_str().to_owned();
        }
        let drawn = |g: &CamGeometry| match g {
            CamGeometry::Sketch { sketch, .. } => sketches(app).iter().find(|s| s.0 == *sketch).map(|s| s.0),
            _ => None,
        };
        if let Some(outline) = drawn(geometry) {
            k.sketch = outline;
        }
        if let Some((sketch, depth)) = pockets.iter().find_map(|p| drawn(&p.geometry).map(|s| (s, p.depth.as_str()))) {
            k.pocket_sketch = sketch;
            k.pocket_depth = depth.to_owned();
        }
        let word = |v: Value| v.as_str().map(str::to_owned).unwrap_or_default();
        let (material, tier, hold) = (word(json!(material)), word(json!(tier)), word(json!(hold)));
        k.material = MATERIALS.iter().position(|m| m.0 == material).unwrap_or(k.material);
        k.tier = TIERS.iter().position(|t| t.0 == tier).unwrap_or(k.tier);
        k.hold = HOLDS.iter().position(|h| h.0 == hold).unwrap_or(k.hold);
        k.thickness = thickness.as_str().to_owned();
        if let Some(t) = cam.tool(*tool) {
            k.diameter = mm(t.diameter);
            k.flute_length = mm(t.flute_length);
            k.flutes = FLUTES.iter().position(|f| f.parse::<u32>().is_ok_and(|n| n == t.flutes)).unwrap_or(k.flutes);
        }
    }
    // The cutter in the fields is chosen by name where it is one of those offered.
    let typed = Cutter { diameter: k.diameter.clone(), flutes: k.flutes + 1, flute_length: k.flute_length.clone() };
    k.cutter = k.cutters.iter().position(|c| c.same(&typed)).unwrap_or(k.cutters.len());
    let typed = k.typed_mill();
    k.thread_mill = k.thread_mills.iter().position(|c| c.same(&typed)).unwrap_or(k.thread_mills.len());
    Some((Kind::CutOut(k), Vec::new()))
}

/// The memory with this program's cutter first and its plate and choices as the last.
pub fn remembered(old: &str, k: &CutOut) -> String {
    let mut memory = Memory::read(old);
    let cutter = Cutter { diameter: k.diameter.trim().to_owned(), flutes: k.flutes + 1, flute_length: k.flute_length.trim().to_owned() };
    memory.cutters.retain(|c| !c.same(&cutter));
    memory.cutters.insert(0, cutter);
    memory.cutters.truncate(MAX_CUTTERS);
    memory.material = MATERIALS.get(k.material).map_or("", |m| m.0).to_owned();
    memory.tier = TIERS.get(k.tier).map_or("", |t| t.0).to_owned();
    memory.hold = HOLDS.get(k.hold).map_or("", |h| h.0).to_owned();
    memory.thickness = k.thickness.trim().to_owned();
    memory.pocket_depth = k.pocket_depth.trim().to_owned();
    if !k.threads.is_empty() {
        let mill = k.typed_mill();
        memory.thread_mills.retain(|c| !c.same(&mill));
        memory.thread_mills.insert(0, mill);
        memory.thread_mills.truncate(MAX_CUTTERS);
    }
    serde_json::to_string(&memory).unwrap_or_default()
}

/// What `cam.cut_out` is asked.
fn params(k: &CutOut) -> Result<Value, String> {
    if k.sketch == 0 {
        return Err("Draw the part's outline in a sketch first.".into());
    }
    let text = |value: &str, what: &str| {
        let value = value.trim();
        if value.is_empty() { Err(format!("{what} is empty")) } else { Ok(value.to_owned()) }
    };
    let mut asked = json!({
        "sketch": k.sketch,
        "setup": SETUP,
        "material": MATERIALS.get(k.material).map_or("aluminum", |m| m.0),
        "thickness": text(&k.thickness, "Plate thickness")?,
        "tool_diameter": text(&k.diameter, "Cutter diameter")?,
        "flutes": k.flutes + 1,
        "flute_length": text(&k.flute_length, "Flute length")?,
        "tier": TIERS.get(k.tier).map_or("standard", |t| t.0),
        "hold": HOLDS.get(k.hold).map_or("tabs", |h| h.0),
    });
    if k.pocket_sketch != 0 && k.pocket_sketch != k.sketch {
        asked["pockets"] = json!([{"sketch": k.pocket_sketch, "depth": text(&k.pocket_depth, "Pocket depth")?}]);
    }
    // The threads chosen, each by its hole's centre as `cam.holes` told it. One whose hole is
    // not there now is asked for all the same, so that the plan says so and is refused: a
    // thread is never dropped without a word.
    let threads: Vec<Value> = k
        .threads
        .iter()
        .map(|(spot_of, thread)| {
            let at = k.holes.iter().find(|h| spot(h.at) == *spot_of).map_or([spot_of[0] as f64 / 1000.0, spot_of[1] as f64 / 1000.0], |h| h.at);
            json!({"at": at, "thread": thread})
        })
        .collect();
    if !threads.is_empty() {
        asked["threads"] = json!(threads);
        asked["thread_mill_diameter"] = json!(text(&k.tm_diameter, "Thread mill diameter")?);
        asked["thread_mill_flutes"] = json!(k.tm_flutes + 1);
        asked["thread_mill_reach"] = json!(text(&k.tm_reach, "Thread mill reach")?);
        asked["thread_fit"] = json!(text(&k.tm_fit, "Thread fit (+ looser)")?);
    }
    Ok(asked)
}

/// A reply's paths as runs: each cut, and each move between cuts, a line of its own.
pub(crate) fn runs(reply: &Value) -> Vec<Run> {
    let mut out: Vec<Run> = Vec::new();
    for path in reply["paths"].as_array().into_iter().flatten() {
        let mut last: Option<[f64; 3]> = None;
        for p in path["points"].as_array().into_iter().flatten() {
            let (Some(x), Some(y), Some(z)) = (p[0].as_f64(), p[1].as_f64(), p[2].as_f64()) else { continue };
            let (at, cutting) = ([x, y, z], p[3].as_f64() == Some(1.0));
            match out.last_mut() {
                Some(run) if run.cutting == cutting && last.is_some() => run.points.push(at),
                // A new run starts where the tool was, so the lines join.
                _ => out.push(Run { cutting, points: last.into_iter().chain([at]).collect() }),
            }
            last = Some(at);
        }
    }
    out
}

/// The paths of the plan Check last made, over the part: cuts in the accent colour, the moves
/// between them faintly. Drawn while the dialog's values are those the plan was made for.
pub fn show(app: &SolveApp, painter: &egui::Painter, proj: &crate::viewport::Proj) {
    let Some(Kind::CutOut(k)) = app.dialog.as_ref().map(|d| &d.kind) else { return };
    if k.paths.is_empty() || k.refused || k.report_of != planned_for(app, k) {
        return;
    }
    draw(painter, proj, &k.paths);
}

/// Planned paths over the part: cuts in the accent colour, the moves between them faintly.
pub(crate) fn draw(painter: &egui::Painter, proj: &crate::viewport::Proj, paths: &[Run]) {
    let (theme, view) = (Tokens::get(), Tokens::view());
    let total: usize = paths.iter().map(|r| r.points.len()).sum();
    let every = total.div_ceil(MAX_DRAWN).max(1);
    for run in paths {
        let stroke = if run.cutting { egui::Stroke::new(1.5, theme.accent) } else { egui::Stroke::new(1.0, view.text.gamma_multiply(0.35)) };
        let last = run.points.len().saturating_sub(1);
        // What is behind the camera breaks the line; the rest is drawn.
        let mut line: Vec<egui::Pos2> = Vec::new();
        for (i, p) in run.points.iter().enumerate() {
            if i % every != 0 && i != last {
                continue;
            }
            match proj.to_screen(Vec3::new(p[0], p[1], p[2])) {
                Some(at) => line.push(at),
                None => {
                    if line.len() > 1 {
                        painter.add(egui::Shape::line(std::mem::take(&mut line), stroke));
                    }
                    line.clear();
                }
            }
        }
        if line.len() > 1 {
            painter.add(egui::Shape::line(line, stroke));
        }
    }
}

pub(crate) fn number(v: &Value) -> String {
    v.as_f64().map_or_else(|| "?".into(), |x| format!("{}", (x * 10.0).round() / 10.0))
}

/// The lines that tell a plan: its size and time, each cut with its speed and feed, each issue.
/// Short enough to stay a line each in the dialog's column, so a small window shows the plan.
fn lines(reply: &Value) -> Vec<String> {
    let tr = solidcraft_ui_text::tr_label;
    let (lo, hi) = (&reply["extents"]["min"], &reply["extents"]["max"]);
    let mut out = vec![
        format!("{}: {} min", tr("Cutting time"), number(&reply["readback"]["cut_minutes"])),
        format!("X {} … {}   Y {} … {} mm", number(&lo[0]), number(&hi[0]), number(&lo[1]), number(&hi[1])),
        format!("Z {} … {} mm", number(&lo[2]), number(&hi[2])),
    ];
    // A cut is named after its part (`Sketch1: Outside rough`): in the part's own dialog the
    // cut's name alone says which it is.
    let part = reply["operation"].as_str().map(|name| format!("{name}: ")).unwrap_or_default();
    for cut in reply["cuts"].as_array().into_iter().flatten() {
        let name = cut["name"].as_str().unwrap_or_default();
        out.push(format!("{} · {} rpm · {} mm/min", name.strip_prefix(part.as_str()).unwrap_or(name), number(&cut["rpm"]), number(&cut["feed"])));
    }
    for issue in reply["issues"].as_array().into_iter().flatten() {
        let at = match issue["at"].as_str() {
            Some(a) => match a.strip_prefix(part.as_str()) {
                Some(rest) => format!("{rest}: "),
                None if format!("{a}: ") == part => String::new(),
                None => format!("{a}: "),
            },
            None => String::new(),
        };
        out.push(format!("{}: {at}{}", tr("Warning"), issue["message"].as_str().unwrap_or_default()));
    }
    out
}

/// A plan's lines: the cuts' file, then the thread file's time, cuts and issues when it has one.
fn report(reply: &Value) -> Vec<String> {
    let mut out = lines(reply);
    let thread = &reply["thread"];
    if thread.is_object() {
        let mut told = thread.clone();
        told["operation"] = reply["operation"].clone();
        let mut threads = lines(&told);
        if let Some(first) = threads.first_mut() {
            *first = format!("{}: {} min", solidcraft_ui_text::tr_label("Thread milling time"), number(&thread["readback"]["cut_minutes"]));
        }
        // The thread file's own extents are inside the cuts': its time, cuts and issues are told.
        out.extend(threads.into_iter().enumerate().filter(|(i, _)| *i != 1 && *i != 2).map(|(_, line)| line));
    }
    out
}

/// Plan the cuts on a copy of the design: nothing is kept or written. The report's lines,
/// whether the plan was refused, and its paths.
fn check(app: &SolveApp, k: &CutOut) -> (Vec<String>, bool, Vec<Run>) {
    let planned = params(k).and_then(|mut p| {
        p["dry_run"] = json!(true);
        p["paths"] = json!(true);
        app.session.preview(&[("cam.cut_out".to_string(), p)]).map_err(|e| e.to_string())
    });
    match planned {
        Ok(preview) => {
            let reply = preview.replies.first();
            (reply.map(report).unwrap_or_default(), false, reply.map(runs).unwrap_or_default())
        }
        Err(why) => (vec![why], true, Vec::new()),
    }
}

/// What a plan depends on: the dialog's values and the design as it stands.
fn planned_for(app: &SolveApp, k: &CutOut) -> String {
    format!(
        "{:?}",
        (
            app.session.revision,
            (k.sketch, k.pocket_sketch, &k.pocket_depth),
            (k.material, &k.thickness, &k.diameter, k.flutes, &k.flute_length, k.tier, k.hold),
            (&k.threads, &k.tm_diameter, k.tm_flutes, &k.tm_reach, &k.tm_fit)
        )
    )
}

pub fn rows(app: &SolveApp, ui: &mut egui::Ui, k: &mut CutOut) -> bool {
    let tr = solidcraft_ui_text::tr_label;
    let mut enter = false;
    let list = sketches(app);
    form_row(ui, "Outline sketch", |ui| {
        if list.is_empty() {
            k.sketch = 0;
            ui.label(tr("Draw the part's outline in a sketch first."));
            return;
        }
        let labels: Vec<&str> = list.iter().map(|s| s.1.as_str()).collect();
        let mut at = list.iter().position(|s| s.0 == k.sketch).unwrap_or(list.len().saturating_sub(1));
        combo(ui, "cut_out_sketch", &labels, &mut at);
        k.sketch = list.get(at).map_or(0, |s| s.0);
    });
    // Pockets come from another sketch than the outline's: the first row is no pockets.
    let others: Vec<&(u64, String)> = list.iter().filter(|s| s.0 != k.sketch).collect();
    if !others.is_empty() {
        form_row(ui, "Pocket sketch", |ui| {
            let labels: Vec<&str> = std::iter::once(tr("No pockets")).chain(others.iter().map(|s| s.1.as_str())).collect();
            let mut at = others.iter().position(|s| s.0 == k.pocket_sketch).map_or(0, |i| i + 1);
            combo(ui, "cut_out_pocket_sketch", &labels, &mut at);
            k.pocket_sketch = at.checked_sub(1).and_then(|i| others.get(i)).map_or(0, |s| s.0);
        });
    } else {
        k.pocket_sketch = 0;
    }
    if k.pocket_sketch != 0 {
        form_row(ui, "Pocket depth", |ui| enter |= field(ui, &mut k.pocket_depth));
    }
    form_row(ui, "Material", |ui| {
        let labels: Vec<&str> = MATERIALS.iter().map(|m| tr(m.1)).collect();
        combo(ui, "cut_out_material", &labels, &mut k.material);
    });
    form_row(ui, "Plate thickness", |ui| enter |= field(ui, &mut k.thickness));
    // A cutter used before is chosen by name; the last row is a new one, typed below.
    if !k.cutters.is_empty() {
        form_row(ui, "Cutter", |ui| {
            let names: Vec<String> = k.cutters.iter().map(Cutter::label).collect();
            let labels: Vec<&str> = names.iter().map(String::as_str).chain([tr("New cutter…")]).collect();
            let mut at = k.cutter.min(k.cutters.len());
            combo(ui, "cut_out_cutter", &labels, &mut at);
            k.cutter = at;
        });
        if let Some(c) = k.cutters.get(k.cutter) {
            (k.diameter, k.flutes, k.flute_length) = (c.diameter.clone(), c.flutes.saturating_sub(1).min(FLUTES.len() - 1), c.flute_length.clone());
        }
    }
    if k.cutter >= k.cutters.len() {
        form_row(ui, "Cutter diameter", |ui| enter |= field(ui, &mut k.diameter));
        form_row(ui, "Flutes", |ui| combo(ui, "cut_out_flutes", &FLUTES, &mut k.flutes));
        form_row(ui, "Flute length", |ui| enter |= field(ui, &mut k.flute_length));
    }
    form_row(ui, "How hard to cut", |ui| {
        let labels: Vec<&str> = TIERS.iter().map(|t| tr(t.1)).collect();
        combo(ui, "cut_out_tier", &labels, &mut k.tier);
    });
    form_row(ui, "Hold-down", |ui| {
        let labels: Vec<&str> = HOLDS.iter().map(|h| tr(h.1)).collect();
        combo(ui, "cut_out_hold", &labels, &mut k.hold);
    });
    // The part's round holes, each with a thread to choose; read again when the outline's sketch
    // or the design changes. A thread whose hole is gone is kept and shown, never dropped.
    let holes_of = format!("{:?}", (app.session.revision, k.sketch));
    if k.holes_of != holes_of {
        (k.holes, k.thread_names) = holes(app, k.sketch);
        k.holes_of = holes_of;
    }
    if !k.holes.is_empty() && !k.thread_names.is_empty() {
        crate::ui_kit::text(ui, tr("Threads in round holes"), crate::ui_kit::Text::Note("cut_out_threads"));
        let listed: Vec<Hole> = k.holes.iter().take(MAX_HOLE_ROWS).cloned().collect();
        for (i, hole) in listed.iter().enumerate() {
            let label = format!("⌀{} · X{} Y{}", short(hole.diameter), short(hole.at[0]), short(hole.at[1]));
            form_row(ui, &label, |ui| {
                let labels: Vec<&str> = std::iter::once(tr("No thread")).chain(k.thread_names.iter().map(String::as_str)).collect();
                let was = k.thread_of(hole.at).and_then(|name| k.thread_names.iter().position(|n| n == name)).map_or(0, |n| n + 1);
                let mut at = was;
                combo(ui, &format!("cut_out_thread_{i}"), &labels, &mut at);
                if at != was {
                    let name = at.checked_sub(1).and_then(|n| k.thread_names.get(n)).cloned().unwrap_or_default();
                    k.set_thread(hole.at, &name);
                }
            });
        }
        if k.holes.len() > MAX_HOLE_ROWS {
            let more = format!("+{} {}", k.holes.len() - MAX_HOLE_ROWS, tr("holes not listed"));
            crate::ui_kit::text(ui, &more, crate::ui_kit::Text::Note("cut_out_more_holes"));
        }
    }
    // Threads chosen where no round hole is now (the hole was moved or deleted, or another sketch
    // is the outline): each is shown, to be taken off, and the plan is refused while one is left.
    let there: Vec<[i64; 2]> = k.holes.iter().map(|h| spot(h.at)).collect();
    let stale: Vec<([i64; 2], String)> = k.threads.iter().filter(|t| !there.contains(&t.0)).cloned().collect();
    if !stale.is_empty() {
        crate::ui_kit::text(ui, tr("cam.cut_out.thread_stale"), crate::ui_kit::Text::Error("cut_out_thread_stale"));
        for (i, (spot_of, name)) in stale.iter().take(MAX_HOLE_ROWS).enumerate() {
            let at = [spot_of[0] as f64 / 1000.0, spot_of[1] as f64 / 1000.0];
            form_row(ui, &format!("X{} Y{}", short(at[0]), short(at[1])), |ui| {
                let labels = [tr("No thread"), name.as_str()];
                let mut chosen = 1;
                combo(ui, &format!("cut_out_stale_thread_{i}"), &labels, &mut chosen);
                if chosen == 0 {
                    k.set_thread(at, "");
                }
            });
        }
    }
    if !k.threads.is_empty() {
        // A thread mill used before is chosen by name, with the fit it was used at; the last row is a new one.
        if !k.thread_mills.is_empty() {
            form_row(ui, "Thread mill", |ui| {
                let names: Vec<String> = k.thread_mills.iter().map(ThreadCutter::label).collect();
                let labels: Vec<&str> = names.iter().map(String::as_str).chain([tr("New thread mill…")]).collect();
                let mut at = k.thread_mill.min(k.thread_mills.len());
                combo(ui, "cut_out_thread_mill", &labels, &mut at);
                if at != k.thread_mill {
                    k.thread_mill = at;
                    if let Some(c) = k.thread_mills.get(at) {
                        k.tm_fit = c.fit.clone();
                    }
                }
            });
            if let Some(c) = k.thread_mills.get(k.thread_mill) {
                (k.tm_diameter, k.tm_flutes, k.tm_reach) = (c.diameter.clone(), c.flutes.saturating_sub(1).min(FLUTES.len() - 1), c.reach.clone());
            }
        }
        if k.thread_mill >= k.thread_mills.len() {
            form_row(ui, "Thread mill diameter", |ui| enter |= field(ui, &mut k.tm_diameter));
            form_row(ui, "Thread mill flutes", |ui| combo(ui, "cut_out_thread_mill_flutes", &FLUTES, &mut k.tm_flutes));
            form_row(ui, "Thread mill reach", |ui| enter |= field(ui, &mut k.tm_reach));
        }
        form_row(ui, "Thread fit (+ looser)", |ui| enter |= field(ui, &mut k.tm_fit));
        crate::ui_kit::text(ui, tr("cam.cut_out.thread_help"), crate::ui_kit::Text::Note("cut_out_thread_help"));
    }
    // A changed value, or a changed design, makes the last plan another part's.
    let now = planned_for(app, k);
    if k.report_of != now {
        k.report.clear();
        k.paths.clear();
        k.refused = false;
    }
    form_row(ui, "Plan", |ui| {
        let r = crate::ui_kit::button(ui, tr("Check"), crate::ui_kit::Button::Normal);
        crate::scenario::publish_handle("dialog:cut_out:check", r.rect.center());
        if r.clicked() {
            (k.report, k.refused, k.paths) = check(app, k);
            k.report_of = now.clone();
        }
    });
    for (i, line) in k.report.iter().enumerate() {
        let key = format!("cut_out_{i}");
        let style = if k.refused { crate::ui_kit::Text::Error(&key) } else { crate::ui_kit::Text::Note(&key) };
        crate::ui_kit::text(ui, line, style);
    }
    crate::ui_kit::text(ui, tr("cam.cut_out.zero_help"), crate::ui_kit::Text::Note("cut_out_zero"));
    enter
}

/// Save G-code: the plan is checked first, so a refusal is read in the dialog; then the file is
/// asked for, and the part is kept in the design as its program is written.
pub fn run(app: &mut SolveApp, k: &CutOut) -> Result<(), String> {
    let asked = params(k).map_err(|why| solidcraft_ui_text::tr_label(&why).to_owned())?;
    let mut dry = asked.clone();
    dry["dry_run"] = json!(true);
    app.session.preview(&[("cam.cut_out".to_string(), dry)]).map_err(|e| e.to_string())?;
    app.preferences.cut_out_memory = remembered(&app.preferences.cut_out_memory, k);
    let name = sketches(app).into_iter().find(|s| s.0 == k.sketch).map_or_else(|| app.session.doc.name.clone(), |s| s.1);
    app.pick_save(&format!("{name}.nc"), &["nc"], move |app, path| {
        let Some(path) = path else { return };
        let mut asked = asked;
        asked["path"] = json!(path);
        match app.run("cam.cut_out", asked) {
            Ok(reply) => {
                // With threads there are two files, the cuts' and the threads': both are told.
                let both = [&reply, &reply["thread"]];
                let warnings: usize = both.iter().map(|r| r["issues"].as_array().map_or(0, Vec::len)).sum();
                let minutes: f64 = both.iter().filter_map(|r| r["readback"]["cut_minutes"].as_f64()).sum();
                let files: Vec<&str> = both.iter().filter_map(|r| r["path"].as_str()).collect();
                let files = if files.is_empty() { path.clone() } else { files.join(", ") };
                let tr = solidcraft_ui_text::tr_label;
                app.set_status(
                    format!("{}: {files} ({} {}, {warnings} {})", tr("G-code saved"), number(&json!(minutes)), tr("min of cutting"), tr("warnings")),
                    false,
                );
            }
            Err(why) => app.set_status(why, true),
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plate(h: &mut crate::scenario::Harness) {
        h.app.run("sketch.create", json!({"plane": "XY"})).unwrap();
        h.app.run("sketch.rectangle.two_point", json!({"p0": [0, 0], "p1": [60, 40]})).unwrap();
        h.app.run("sketch.circle.center", json!({"center": [30, 20], "radius": 10})).unwrap();
        h.app.run("sketch.finish", json!({})).unwrap();
    }

    fn form(h: &crate::scenario::Harness) -> CutOut {
        match h.app.dialog.as_ref().map(|d| &d.kind) {
            Some(Kind::CutOut(k)) => k.clone(),
            other => panic!("no Cut Out Part dialog: {other:?}"),
        }
    }

    fn wait_for_picker(h: &mut crate::scenario::Harness) {
        let began = std::time::Instant::now();
        loop {
            h.frames(1);
            if h.app.file_load.idle() && !h.app.file_picker_busy() {
                break;
            }
            assert!(began.elapsed().as_secs() < 10, "the file dialog did not finish");
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    #[test]
    fn a_drawn_plate_is_checked_then_saved_as_gcode_and_kept_in_the_design() {
        let mut h = crate::scenario::Harness::new();
        plate(&mut h);
        h.app.start("cam.cut_out");
        h.frames(3);
        let k = form(&h);
        assert_eq!((k.material, k.thickness.as_str(), k.diameter.as_str(), k.flutes, k.tier, k.hold), (0, "6 mm", "6 mm", 0, 1, 0));
        assert!(k.cutters.is_empty() && k.cutter == 0, "no cutter is known yet: the fields are typed");
        assert!(k.sketch != 0 && k.report.is_empty() && k.paths.is_empty() && k.pocket_sketch == 0);
        // No field takes the keyboard as it opens (see `dialogs_dense::rows`).
        assert!(h.ctx.memory(|m| m.focused()).is_none(), "a field has the keyboard");

        // Check lists the plan and changes nothing.
        let design = h.app.session.doc.to_json();
        h.step(&json!({"click":{"handle":"dialog:cut_out:check"}})).unwrap();
        h.frames(2);
        let k = form(&h);
        assert!(!k.refused, "{:?}", k.report);
        assert!(k.report.iter().any(|l| l.contains("Outside finish") && l.contains("rpm")), "{:?}", k.report);
        // The plan's paths are kept to draw: cuts and the moves between them, down to 0.2 mm
        // under the plate.
        assert!(k.paths.iter().any(|r| r.cutting) && k.paths.iter().any(|r| !r.cutting));
        let low = k.paths.iter().flat_map(|r| &r.points).map(|p| p[2]).fold(f64::INFINITY, f64::min);
        assert!((low + 6.2).abs() < 1e-6, "{low}");
        assert!(k.paths.iter().map(|r| r.points.len()).sum::<usize>() > 100 && k.paths.iter().all(|r| !r.points.is_empty()));
        assert_eq!(h.app.session.doc.to_json(), design);
        // Without tabs the plan warns that the part comes loose; a changed value clears the last plan.
        if let Some(Kind::CutOut(k)) = h.app.dialog.as_mut().map(|d| &mut d.kind) {
            k.hold = 1;
        }
        h.frames(2);
        assert!(form(&h).report.is_empty() && form(&h).paths.is_empty());
        h.step(&json!({"click":{"handle":"dialog:cut_out:check"}})).unwrap();
        h.frames(2);
        assert!(form(&h).report.iter().any(|l| l.contains("nothing holds the part")), "{:?}", form(&h).report);
        if let Some(Kind::CutOut(k)) = h.app.dialog.as_mut().map(|d| &mut d.kind) {
            k.hold = 0;
        }
        h.frames(2);

        let dir = h.dir.clone();
        h.app.services.pick_save = Some(Box::new(move |name, extensions| {
            assert!(name.ends_with(".nc"), "{name}");
            assert_eq!(extensions, &["nc"]);
            Box::pin(std::future::ready(Some(dir.join("plate.nc").to_string_lossy().into_owned())))
        }));
        h.step(&json!({"click":{"handle":"dialog:Save G-code…"}})).unwrap();
        wait_for_picker(&mut h);
        let text = std::fs::read_to_string(h.dir.join("plate.nc")).unwrap();
        assert!(text.contains("G53 G0 Z-5") && text.contains("Z-6.2") && text.contains("Z-5.2") && text.trim_end().ends_with("M30"), "{text}");
        assert!(h.app.dialog.is_none());
        let cam = &h.app.session.doc.cam;
        assert_eq!((cam.tools.len(), cam.setups.len(), cam.setups[0].operations.len()), (1, 1, 1));

        // A second sketch gives pockets: its circle is cut 2 mm deep before everything else.
        h.app.run("sketch.create", json!({"plane": "XY"})).unwrap();
        h.app.run("sketch.circle.center", json!({"center": [10, 30], "radius": 5})).unwrap();
        h.app.run("sketch.finish", json!({})).unwrap();
        h.app.start("cam.cut_out");
        h.frames(3);
        let k = form(&h);
        let (outline, recess) = (k.sketch, sketches(&h.app).last().unwrap().0);
        assert!(outline != recess && k.pocket_sketch == 0, "the outline stays the part's sketch, not the newest: {k:?}");
        // So does it in a design that has cut nothing out yet: the outline is the sketch that covers the most.
        let mut fresh = crate::scenario::Harness::new();
        fresh.app.run("sketch.create", json!({"plane": "XY"})).unwrap();
        fresh.app.run("sketch.circle.center", json!({"center": [10, 30], "radius": 5})).unwrap();
        fresh.app.run("sketch.finish", json!({})).unwrap();
        plate(&mut fresh);
        fresh.app.run("sketch.create", json!({"plane": "XY"})).unwrap();
        fresh.app.run("sketch.circle.center", json!({"center": [50, 10], "radius": 4})).unwrap();
        fresh.app.run("sketch.finish", json!({})).unwrap();
        fresh.app.start("cam.cut_out");
        fresh.frames(3);
        assert_eq!(form(&fresh).sketch, sketches(&fresh.app)[1].0, "the plate's sketch, drawn second of three");
        if let Some(Kind::CutOut(k)) = h.app.dialog.as_mut().map(|d| &mut d.kind) {
            k.pocket_sketch = recess;
        }
        h.frames(2);
        h.step(&json!({"click":{"handle":"dialog:cut_out:check"}})).unwrap();
        h.frames(2);
        let k = form(&h);
        assert!(!k.refused && k.report.iter().any(|l| l.contains("Pocket rough")), "{:?}", k.report);
        let cut_out = k.report.iter().position(|l| l.contains("Pocket rough")).unwrap();
        assert!(cut_out < k.report.iter().position(|l| l.contains("Opening 1 rough")).unwrap());
        let dir = h.dir.clone();
        h.app.services.pick_save =
            Some(Box::new(move |_, _| Box::pin(std::future::ready(Some(dir.join("pocketed.nc").to_string_lossy().into_owned())))));
        h.step(&json!({"click":{"handle":"dialog:Save G-code…"}})).unwrap();
        wait_for_picker(&mut h);
        assert!(std::fs::read_to_string(h.dir.join("pocketed.nc")).unwrap().contains("Pocket rough"));
        h.app.start("cam.cut_out");
        h.frames(3);
        let k = form(&h);
        assert_eq!((k.sketch, k.pocket_sketch, k.pocket_depth.as_str()), (outline, recess, "2 mm"));
        h.step(&json!({"click":{"handle":"dialog:cancel"}})).unwrap();

        // The program saved is remembered for other designs: its cutter by name, its plate and choices.
        let memory = Memory::read(&h.app.preferences.cut_out_memory);
        assert_eq!(memory.cutters, [Cutter { diameter: "6 mm".into(), flutes: 1, flute_length: "18 mm".into() }]);
        assert_eq!((memory.material.as_str(), memory.thickness.as_str(), memory.hold.as_str()), ("aluminum", "6 mm", "tabs"));
        let mut other = crate::scenario::Harness::new();
        other.app.load_prefs(&h.app.prefs());
        other.app.preferences.cut_out_memory = remembered(
            &other.app.preferences.cut_out_memory,
            &CutOut { diameter: "3.175 mm".into(), flutes: 1, flute_length: "12 mm".into(), thickness: "3 mm".into(), material: 3, ..k.clone() },
        );
        plate(&mut other);
        other.app.start("cam.cut_out");
        other.frames(3);
        let k = form(&other);
        assert_eq!(k.cutters.iter().map(Cutter::label).collect::<Vec<_>>(), ["3.175 mm · 2F · 12 mm", "6 mm · 1F · 18 mm"]);
        assert_eq!((k.cutter, k.diameter.as_str(), k.flutes, k.flute_length.as_str()), (0, "3.175 mm", 1, "12 mm"));
        assert_eq!((k.material, k.thickness.as_str()), (3, "3 mm"));
        // The older cutter chosen by name fills the fields the program is asked with.
        if let Some(Kind::CutOut(k)) = other.app.dialog.as_mut().map(|d| &mut d.kind) {
            k.cutter = 1;
        }
        other.frames(2);
        let k = form(&other);
        assert_eq!((k.diameter.as_str(), k.flutes, k.flute_length.as_str()), ("6 mm", 0, "18 mm"));
        assert!(
            Memory::read("not a memory").cutters.is_empty()
                && Memory::read(r#"{"cutters":[{"diameter":"","flutes":9,"flute_length":""}]}"#).cutters.is_empty()
        );

        // Opened again, the dialog starts from that part's plate and cutter.
        h.app.run("cam.cut_out", json!({"sketch": outline, "material": "acrylic", "thickness": "3 mm", "tool_diameter": 3.175, "flute_length": 12, "flutes": 2, "tier": "careful", "hold": "none"})).unwrap();
        h.app.start("cam.cut_out");
        h.frames(3);
        let k = form(&h);
        assert_eq!(
            (k.material, k.thickness.as_str(), k.diameter.as_str(), k.flute_length.as_str(), k.flutes, k.tier, k.hold),
            (3, "3 mm", "3.175 mm", "12 mm", 1, 0, 1)
        );
    }

    #[test]
    fn a_plan_that_cannot_be_cut_is_refused_in_the_dialog_before_any_file_is_asked_for() {
        let mut h = crate::scenario::Harness::new();
        // No sketch yet: the dialog says what to do first.
        h.app.start("cam.cut_out");
        h.frames(3);
        assert_eq!(form(&h).sketch, 0);
        h.step(&json!({"click":{"handle":"dialog:Save G-code…"}})).unwrap();
        h.frames(2);
        assert!(h.app.dialog.as_ref().is_some_and(|d| d.error.as_deref().is_some_and(|e| e.contains("sketch first"))));
        h.step(&json!({"click":{"handle":"dialog:cancel"}})).unwrap();

        plate(&mut h);
        h.app.start("cam.cut_out");
        h.frames(3);
        if let Some(Kind::CutOut(k)) = h.app.dialog.as_mut().map(|d| &mut d.kind) {
            k.thickness = "40 mm".into();
        }
        let asked = std::rc::Rc::new(std::cell::Cell::new(false));
        let seen = asked.clone();
        h.app.services.pick_save = Some(Box::new(move |_, _| {
            seen.set(true);
            Box::pin(std::future::ready(None))
        }));
        h.step(&json!({"click":{"handle":"dialog:cut_out:check"}})).unwrap();
        h.frames(2);
        let k = form(&h);
        assert!(k.refused && k.report.iter().any(|l| l.contains("longer cutter")), "{:?}", k.report);
        h.step(&json!({"click":{"handle":"dialog:Save G-code…"}})).unwrap();
        h.frames(2);
        assert!(h.app.dialog.as_ref().is_some_and(|d| d.error.as_deref().is_some_and(|e| e.contains("longer cutter"))));
        assert!(!asked.get(), "no file is asked for while the plan is refused");
        assert!(h.app.session.doc.cam.is_empty());
    }

    fn drilled_plate(h: &mut crate::scenario::Harness) {
        h.app.run("sketch.create", json!({"plane": "XY"})).unwrap();
        h.app.run("sketch.rectangle.two_point", json!({"p0": [0, 0], "p1": [60, 40]})).unwrap();
        h.app.run("sketch.circle.center", json!({"center": [15, 20], "radius": 3.175})).unwrap();
        h.app.run("sketch.circle.center", json!({"center": [45, 20], "radius": 3.175})).unwrap();
        h.app.run("sketch.finish", json!({})).unwrap();
    }

    fn edit(h: &mut crate::scenario::Harness, change: impl FnOnce(&mut CutOut)) {
        match h.app.dialog.as_mut().map(|d| &mut d.kind) {
            Some(Kind::CutOut(k)) => change(k),
            other => panic!("no Cut Out Part dialog: {other:?}"),
        }
        h.frames(2);
    }

    #[test]
    fn the_dialog_lists_the_holes_and_saves_a_thread_file_beside_the_cuts() {
        let mut h = crate::scenario::Harness::new();
        drilled_plate(&mut h);
        h.app.start("cam.cut_out");
        h.frames(3);
        // The part's round holes are listed, none threaded, with the threads to choose from.
        let k = form(&h);
        let mut at: Vec<[f64; 2]> = k.holes.iter().map(|hole| hole.at).collect();
        at.sort_by(|a, b| a[0].total_cmp(&b[0]));
        assert_eq!(at, [[15.0, 20.0], [45.0, 20.0]]);
        assert!(k.holes.iter().all(|hole| (hole.diameter - 6.35).abs() < 1e-9));
        assert!(k.threads.is_empty() && k.thread_names.len() == 12 && k.thread_names.contains(&"1/4-20".to_string()), "{:?}", k.thread_names);
        assert!(k.thread_mills.is_empty() && k.thread_mill == 0);

        // One hole gets a 1/4-20 thread; Check lists the thread file after the cuts'.
        edit(&mut h, |k| {
            k.diameter = "3.175 mm".into();
            k.flute_length = "17 mm".into();
            k.set_thread([45.0, 20.0], "1/4-20");
            (k.tm_diameter, k.tm_flutes, k.tm_reach, k.tm_fit) = ("4.572 mm".into(), 2, "12 mm".into(), "0.1 mm".into());
        });
        let design = h.app.session.doc.to_json();
        h.step(&json!({"click":{"handle":"dialog:cut_out:check"}})).unwrap();
        h.frames(2);
        let k = form(&h);
        assert!(!k.refused, "{:?}", k.report);
        let line = |words: &[&str]| k.report.iter().position(|l| words.iter().all(|w| l.contains(w)));
        let (time, cut) = (line(&["Thread milling time", "min"]).expect("its time"), line(&["Thread 1/4-20, 1 hole", "12000 rpm"]).expect("its cut"));
        assert!(line(&["Outside finish"]).expect("the cuts") < time && time < cut, "{:?}", k.report);
        // What the cutter does for the thread is told with its cuts, what the thread mill does with its own.
        assert!(line(&["Warning", "⌀5.08 mm", "spoilboard"]).is_some_and(|i| i < time), "{:?}", k.report);
        assert!(line(&["Warning", "thread mill goes 1.77 mm below the plate"]).is_some_and(|i| i > cut), "{:?}", k.report);
        assert_eq!(k.report.iter().filter(|l| l.starts_with("X ")).count(), 1, "the thread file's extents are not told again: {:?}", k.report);
        // Its paths are drawn with the cuts': the thread mill goes 1.77 mm under the 6 mm plate, the bore 2.27 mm.
        let low = k.paths.iter().flat_map(|r| &r.points).map(|p| p[2]).fold(f64::INFINITY, f64::min);
        assert!((low + 8.27).abs() < 1e-6, "{low}");
        assert_eq!(h.app.session.doc.to_json(), design, "Check keeps nothing");

        // Save writes both files and keeps the part with its thread.
        let dir = h.dir.clone();
        h.app.services.pick_save =
            Some(Box::new(move |_, _| Box::pin(std::future::ready(Some(dir.join("plate.nc").to_string_lossy().into_owned())))));
        h.step(&json!({"click":{"handle":"dialog:Save G-code…"}})).unwrap();
        wait_for_picker(&mut h);
        assert!(h.app.dialog.is_none());
        let cuts = std::fs::read_to_string(h.dir.join("plate-1-cut.nc")).unwrap();
        let threads = std::fs::read_to_string(h.dir.join("plate-2-thread.nc")).unwrap();
        assert!(cuts.contains("Z-8.27") && threads.contains("Z-7.77") && threads.contains("G3 ") && !h.dir.join("plate.nc").exists());
        let cam = &h.app.session.doc.cam;
        assert_eq!((cam.tools.len(), cam.setups[0].operations.len()), (2, 1));
        // The thread mill is remembered with its fit.
        let memory = Memory::read(&h.app.preferences.cut_out_memory);
        assert_eq!(memory.thread_mills, [ThreadCutter { diameter: "4.572 mm".into(), flutes: 3, reach: "12 mm".into(), fit: "0.1 mm".into() }]);
        assert_eq!(memory.cutters.len(), 1);

        // Opened again, the dialog starts from the part's thread and thread mill, and does not
        // offer the thread mill as a cutter to cut the part out with.
        h.app.start("cam.cut_out");
        h.frames(3);
        let k = form(&h);
        assert_eq!((k.thread_of([45.0, 20.0]), k.thread_of([15.0, 20.0])), (Some("1/4-20"), None));
        assert_eq!((k.tm_diameter.as_str(), k.tm_flutes, k.tm_reach.as_str(), k.tm_fit.as_str()), ("4.572 mm", 2, "12 mm", "0.1 mm"));
        assert_eq!((k.thread_mills.len(), k.thread_mill), (1, 0));
        assert!(k.cutters.iter().all(|c| c.diameter != "4.572 mm"), "{:?}", k.cutters);
        // A thread mill that does not fit the hole is refused in the dialog, and no file is asked for.
        edit(&mut h, |k| {
            k.thread_mill = k.thread_mills.len();
            k.tm_diameter = "5 mm".into();
        });
        h.step(&json!({"click":{"handle":"dialog:cut_out:check"}})).unwrap();
        h.frames(2);
        let k = form(&h);
        assert!(k.refused && k.report.iter().any(|l| l.contains("does not fit")), "{:?}", k.report);
        // Its thread taken off, the part is one file's again and asks for no thread mill.
        edit(&mut h, |k| k.set_thread([45.0, 20.0], ""));
        h.step(&json!({"click":{"handle":"dialog:cut_out:check"}})).unwrap();
        h.frames(2);
        let k = form(&h);
        assert!(!k.refused && !k.report.iter().any(|l| l.contains("Thread")), "{:?}", k.report);
        h.step(&json!({"click":{"handle":"dialog:cancel"}})).unwrap();

        // In another design the thread mill is offered by name, with the fit it was used at.
        let mut other = crate::scenario::Harness::new();
        other.app.load_prefs(&h.app.prefs());
        drilled_plate(&mut other);
        other.app.start("cam.cut_out");
        other.frames(3);
        edit(&mut other, |k| {
            k.set_thread([15.0, 20.0], "M6");
            k.set_thread([99.0, 99.0], "M6");
        });
        let k = form(&other);
        assert_eq!(k.thread_mills.iter().map(ThreadCutter::label).collect::<Vec<_>>(), ["4.572 mm · 3F · 12 mm"]);
        assert_eq!(
            (k.thread_mill, k.tm_diameter.as_str(), k.tm_flutes, k.tm_reach.as_str(), k.tm_fit.as_str()),
            (0, "4.572 mm", 2, "12 mm", "0.1 mm")
        );
        assert!(
            Memory::read(r#"{"thread_mills":[{"diameter":"","flutes":9,"reach":""}]}"#).thread_mills.is_empty()
                && Memory::read("not a memory").thread_mills.is_empty()
        );
        // A thread whose hole is not there (the hole was moved or deleted in the sketch) is never
        // dropped silently: it stays in the dialog, and the plan is refused until it is taken off.
        other.step(&json!({"click":{"handle":"dialog:cut_out:check"}})).unwrap();
        other.frames(2);
        let k = form(&other);
        assert!(k.refused && k.report.iter().any(|l| l.contains("no round hole") && l.contains("X 99")), "{:?}", k.report);
        assert_eq!((k.thread_of([99.0, 99.0]), k.thread_of([15.0, 20.0])), (Some("M6"), Some("M6")));
        // Nor is a choice lost by looking at another sketch and coming back, or by a change to the design.
        let outline = k.sketch;
        edit(&mut other, |k| k.sketch = 0);
        edit(&mut other, |k| k.sketch = outline);
        other.app.run("sketch.create", json!({"plane": "XY"})).unwrap();
        other.app.run("sketch.circle.center", json!({"center": [200, 200], "radius": 2})).unwrap();
        other.app.run("sketch.finish", json!({})).unwrap();
        other.frames(3);
        edit(&mut other, |k| k.sketch = outline);
        let k = form(&other);
        assert_eq!((k.thread_of([99.0, 99.0]), k.thread_of([15.0, 20.0]), k.holes.len()), (Some("M6"), Some("M6"), 2));
        // Taken off, the hole that is there is threaded.
        edit(&mut other, |k| k.set_thread([99.0, 99.0], ""));
        other.step(&json!({"click":{"handle":"dialog:cut_out:check"}})).unwrap();
        other.frames(2);
        let k = form(&other);
        assert!(!k.refused && k.report.iter().any(|l| l.contains("Thread M6, 1 hole")), "{:?}", k.report);
    }
}
