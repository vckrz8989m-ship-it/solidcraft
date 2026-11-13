//! The Cut Out Part dialog (`cam.cut_out`): the sketch that is the part's outline, the plate
//! (material and thickness) and the cutter, then a file dialog for the G-code. A second sketch
//! may give pockets: its closed shapes are cut to a depth before the openings. Check plans the
//! cuts on a copy of the design, lists them with what the plan warns of and draws their paths
//! over the part ([`show`]): cuts in the accent colour, moves between them faintly. Nothing is
//! written or kept until Save G-code. The values start from the part the design last cut out.

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
const MATERIALS: [(&str, &str); 7] = [
    ("aluminum", "Aluminium 6061"),
    ("aluminum_soft", "Aluminium, soft or cast"),
    ("brass", "Brass"),
    ("acrylic", "Acrylic"),
    ("soft_plastic", "HDPE and soft plastics"),
    ("hardwood", "Hardwood"),
    ("softwood", "Softwood, plywood and MDF"),
];
/// How hard to cut, as `cam.cut_out` names it, and its label.
const TIERS: [(&str, &str); 3] = [("careful", "Careful (first cut)"), ("standard", "Standard"), ("bold", "Bold (proven machine)")];
const FLUTES: [&str; 4] = ["1", "2", "3", "4"];
/// What holds the part while its outside is cut through, as `cam.cut_out` names it, and its label.
const HOLDS: [(&str, &str); 2] = [("tabs", "Holding tabs"), ("none", "None (screws or tape)")];
/// The setup the dialog's parts are kept in.
const SETUP: &str = "Router";
/// Most points of a plan's paths drawn in a frame: past it, every few are.
const MAX_DRAWN: usize = 60_000;

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
    /// The sketch whose closed shapes are pockets (a feature id; 0: none).
    pub pocket_sketch: u64,
    pub pocket_depth: String,
    /// The paths of the plan Check last made, drawn over the part.
    pub paths: Vec<Run>,
    /// What Check last found, a line each; the flag says the plan was refused.
    pub report: Vec<String>,
    pub refused: bool,
    /// The values and the design the report was made for: any other and it is not shown.
    pub report_of: String,
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

fn mm(v: f64) -> String {
    format!("{} mm", (v * 1000.0).round() / 1000.0)
}

pub fn start(app: &SolveApp, id: &str) -> Option<(Kind, Vec<SelInput>)> {
    if id != "cam.cut_out" {
        return None;
    }
    let newest = sketches(app).last().map_or(0, |s| s.0);
    let mut k = CutOut {
        sketch: newest,
        material: 0,
        thickness: "6 mm".into(),
        diameter: "6 mm".into(),
        flutes: 0,
        flute_length: "18 mm".into(),
        tier: 1,
        hold: 0,
        pocket_sketch: 0,
        pocket_depth: "2 mm".into(),
        paths: Vec::new(),
        report: Vec::new(),
        refused: false,
        report_of: String::new(),
    };
    // The part cut out last gives the plate, the cutter and, while they are still there, its sketches.
    let cam = &app.session.doc.cam;
    let last = cam.setup(SETUP).and_then(|s| s.operations.iter().rev().find(|op| matches!(op, CamOperation::Part { .. })));
    if let Some(CamOperation::Part { tool, material, thickness, tier, hold, geometry, pockets, .. }) = last {
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
    Some((Kind::CutOut(k), Vec::new()))
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
    Ok(asked)
}

/// A reply's paths as runs: each cut, and each move between cuts, a line of its own.
fn runs(reply: &Value) -> Vec<Run> {
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
    let (theme, view) = (Tokens::get(), Tokens::view());
    let total: usize = k.paths.iter().map(|r| r.points.len()).sum();
    let every = total.div_ceil(MAX_DRAWN).max(1);
    for run in &k.paths {
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

fn number(v: &Value) -> String {
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
            (reply.map(lines).unwrap_or_default(), false, reply.map(runs).unwrap_or_default())
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
            (k.material, &k.thickness, &k.diameter, k.flutes, &k.flute_length, k.tier, k.hold)
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
    form_row(ui, "Cutter diameter", |ui| enter |= field(ui, &mut k.diameter));
    form_row(ui, "Flutes", |ui| combo(ui, "cut_out_flutes", &FLUTES, &mut k.flutes));
    form_row(ui, "Flute length", |ui| enter |= field(ui, &mut k.flute_length));
    form_row(ui, "How hard to cut", |ui| {
        let labels: Vec<&str> = TIERS.iter().map(|t| tr(t.1)).collect();
        combo(ui, "cut_out_tier", &labels, &mut k.tier);
    });
    form_row(ui, "Hold-down", |ui| {
        let labels: Vec<&str> = HOLDS.iter().map(|h| tr(h.1)).collect();
        combo(ui, "cut_out_hold", &labels, &mut k.hold);
    });
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
    let name = sketches(app).into_iter().find(|s| s.0 == k.sketch).map_or_else(|| app.session.doc.name.clone(), |s| s.1);
    app.pick_save(&format!("{name}.nc"), &["nc"], move |app, path| {
        let Some(path) = path else { return };
        let mut asked = asked;
        asked["path"] = json!(path);
        match app.run("cam.cut_out", asked) {
            Ok(reply) => {
                let warnings = reply["issues"].as_array().map_or(0, Vec::len);
                let tr = solidcraft_ui_text::tr_label;
                app.set_status(
                    format!(
                        "{}: {path} ({} {}, {warnings} {})",
                        tr("G-code saved"),
                        number(&reply["readback"]["cut_minutes"]),
                        tr("min of cutting"),
                        tr("warnings")
                    ),
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
        assert!(k.sketch != 0 && k.report.is_empty() && k.paths.is_empty() && k.pocket_sketch == 0);

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
}
