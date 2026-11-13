//! The Carve Solid dialog (`cam.carve`): the solid body, the material, the cutter that roughs
//! and the cutter that finishes, then a file dialog for the G-code: one file a cutter. Check
//! plans the cuts, lists them with the block the part is carved from and what the plan warns
//! of, and draws their paths over the body (as the Cut Out Part dialog does). Nothing is
//! written until Save G-code, and nothing is kept in the design.

use serde_json::{Value, json};

use crate::SolveApp;
use crate::dialogs::{Kind, combo, field, form_row};
use crate::dialogs_cut_out::{FLUTES, MATERIALS, Memory, Run, TIERS, draw, number, runs};
use crate::selection::SelInput;

/// What makes the finishing passes, as `cam.carve` names it (`None`: the roughing cutter), and
/// its label.
const FINISHERS: [(Option<&str>, &str); 3] = [(Some("ball"), "Ball end mill"), (Some("flat"), "Flat end mill"), (None, "The roughing cutter")];

/// The dialog's values.
#[derive(Clone, Debug, PartialEq)]
pub struct Carve {
    /// The body carved, by name (empty while the design has none).
    pub body: String,
    /// Index into [`MATERIALS`].
    pub material: usize,
    pub diameter: String,
    /// Index into [`FLUTES`].
    pub flutes: usize,
    pub flute_length: String,
    /// Index into [`FINISHERS`].
    pub finisher: usize,
    pub finish_diameter: String,
    /// Index into [`FLUTES`].
    pub finish_flutes: usize,
    pub finish_flute_length: String,
    /// Index into [`TIERS`].
    pub tier: usize,
    pub stock_top: String,
    pub skin: String,
    /// The paths of the plan Check last made, drawn over the body.
    pub paths: Vec<Run>,
    /// What Check last found, a line each; the flag says the plan was refused.
    pub report: Vec<String>,
    pub refused: bool,
    /// The values and the design the report was made for: any other and it is not shown.
    pub report_of: String,
}

/// The design's solid bodies, by name.
fn bodies(app: &SolveApp) -> Vec<String> {
    app.session.model.state().bodies.iter().filter(|b| !b.body.is_surface()).map(|b| b.name.clone()).collect()
}

pub fn start(app: &SolveApp, id: &str) -> Option<(Kind, Vec<SelInput>)> {
    if id != "cam.carve" {
        return None;
    }
    let mut k = Carve {
        body: bodies(app).pop().unwrap_or_default(),
        material: 0,
        diameter: "6 mm".into(),
        flutes: 0,
        flute_length: "22 mm".into(),
        finisher: 0,
        finish_diameter: "6 mm".into(),
        finish_flutes: 1,
        finish_flute_length: "22 mm".into(),
        tier: 1,
        stock_top: "0.5 mm".into(),
        skin: "0.5 mm".into(),
        paths: Vec::new(),
        report: Vec::new(),
        refused: false,
        report_of: String::new(),
    };
    // The plate programs' memory gives the material, how hard to cut and the cutter last used.
    let memory = Memory::read(&app.preferences.cut_out_memory);
    k.material = MATERIALS.iter().position(|m| m.0 == memory.material).unwrap_or(k.material);
    k.tier = TIERS.iter().position(|t| t.0 == memory.tier).unwrap_or(k.tier);
    if let Some(first) = memory.cutters.first() {
        (k.diameter, k.flutes, k.flute_length) =
            (first.diameter.clone(), first.flutes.saturating_sub(1).min(FLUTES.len() - 1), first.flute_length.clone());
    }
    Some((Kind::Carve(k), Vec::new()))
}

/// What `cam.carve` is asked.
fn params(k: &Carve) -> Result<Value, String> {
    if k.body.is_empty() {
        return Err("Model the part as a solid body first.".into());
    }
    let text = |value: &str, what: &str| {
        let value = value.trim();
        if value.is_empty() { Err(format!("{what} is empty")) } else { Ok(value.to_owned()) }
    };
    let mut asked = json!({
        "body": k.body,
        "material": MATERIALS.get(k.material).map_or("aluminum", |m| m.0),
        "tool_diameter": text(&k.diameter, "Cutter diameter")?,
        "flutes": k.flutes + 1,
        "flute_length": text(&k.flute_length, "Flute length")?,
        "tier": TIERS.get(k.tier).map_or("standard", |t| t.0),
        "stock_top": text(&k.stock_top, "Stock above the part")?,
        "skin": text(&k.skin, "Skin under the part")?,
    });
    if let Some(kind) = FINISHERS.get(k.finisher).and_then(|f| f.0) {
        asked["finish_kind"] = json!(kind);
        asked["finish_diameter"] = json!(text(&k.finish_diameter, "Finishing cutter diameter")?);
        asked["finish_flutes"] = json!(k.finish_flutes + 1);
        asked["finish_flute_length"] = json!(text(&k.finish_flute_length, "Finishing flute length")?);
    }
    Ok(asked)
}

/// The paths of the plan Check last made, over the body. Drawn while the dialog's values are
/// those the plan was made for.
pub fn show(app: &SolveApp, painter: &egui::Painter, proj: &crate::viewport::Proj) {
    let Some(Kind::Carve(k)) = app.dialog.as_ref().map(|d| &d.kind) else { return };
    if k.paths.is_empty() || k.refused || k.report_of != planned_for(app, k) {
        return;
    }
    draw(painter, proj, &k.paths);
}

/// The minutes of cutting in all of a reply's programs.
fn minutes(reply: &Value) -> f64 {
    reply["programs"].as_array().into_iter().flatten().filter_map(|p| p["readback"]["cut_minutes"].as_f64()).sum()
}

/// The lines that tell a plan: the block, then each program with its time and its cuts with
/// their speed and feed, then each issue. Short enough to stay a line each in the dialog.
fn lines(reply: &Value) -> Vec<String> {
    let tr = solidcraft_ui_text::tr_label;
    let side = |k: usize| match (reply["block"]["min"][k].as_f64(), reply["block"]["max"][k].as_f64()) {
        (Some(lo), Some(hi)) => number(&json!(hi - lo)),
        _ => "?".into(),
    };
    let mut out = vec![format!("{}: {} × {} × {} mm", tr("Block, at least"), side(0), side(1), side(2))];
    for program in reply["programs"].as_array().into_iter().flatten() {
        out.push(format!("{}: {} min", program["name"].as_str().unwrap_or_default(), number(&program["readback"]["cut_minutes"])));
        for cut in program["cuts"].as_array().into_iter().flatten() {
            out.push(format!("{} · {} rpm · {} mm/min", cut["name"].as_str().unwrap_or_default(), number(&cut["rpm"]), number(&cut["feed"])));
        }
    }
    let mut said: Vec<&str> = Vec::new();
    let issues = reply["issues"].as_array().into_iter().flatten();
    for issue in issues.chain(reply["programs"].as_array().into_iter().flatten().flat_map(|p| p["issues"].as_array().into_iter().flatten())) {
        let message = issue["message"].as_str().unwrap_or_default();
        if !said.contains(&message) {
            said.push(message);
            out.push(format!("{}: {message}", tr("Warning")));
        }
    }
    out
}

/// Plan the cuts: nothing is kept or written. The report's lines, whether the plan was
/// refused, and its paths.
fn check(app: &SolveApp, k: &Carve) -> (Vec<String>, bool, Vec<Run>) {
    let planned = params(k).map_err(|why| solidcraft_ui_text::tr_label(&why).to_owned()).and_then(|mut p| {
        p["dry_run"] = json!(true);
        p["paths"] = json!(true);
        app.session.preview(&[("cam.carve".to_string(), p)]).map_err(|e| e.to_string())
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
fn planned_for(app: &SolveApp, k: &Carve) -> String {
    format!(
        "{:?}",
        (
            app.session.revision,
            (&k.body, k.material, k.tier, &k.stock_top, &k.skin),
            (&k.diameter, k.flutes, &k.flute_length),
            (k.finisher, &k.finish_diameter, k.finish_flutes, &k.finish_flute_length)
        )
    )
}

pub fn rows(app: &SolveApp, ui: &mut egui::Ui, k: &mut Carve) -> bool {
    let tr = solidcraft_ui_text::tr_label;
    let mut enter = false;
    let list = bodies(app);
    form_row(ui, "Body", |ui| {
        if list.is_empty() {
            k.body.clear();
            ui.label(tr("Model the part as a solid body first."));
            return;
        }
        let labels: Vec<&str> = list.iter().map(String::as_str).collect();
        let mut at = list.iter().position(|b| *b == k.body).unwrap_or(list.len().saturating_sub(1));
        combo(ui, "carve_body", &labels, &mut at);
        k.body = list.get(at).cloned().unwrap_or_default();
    });
    form_row(ui, "Material", |ui| {
        let labels: Vec<&str> = MATERIALS.iter().map(|m| tr(m.1)).collect();
        combo(ui, "carve_material", &labels, &mut k.material);
    });
    form_row(ui, "Cutter diameter", |ui| enter |= field(ui, &mut k.diameter));
    form_row(ui, "Flutes", |ui| combo(ui, "carve_flutes", &FLUTES, &mut k.flutes));
    form_row(ui, "Flute length", |ui| enter |= field(ui, &mut k.flute_length));
    form_row(ui, "Finishing cutter", |ui| {
        let labels: Vec<&str> = FINISHERS.iter().map(|f| tr(f.1)).collect();
        combo(ui, "carve_finisher", &labels, &mut k.finisher);
    });
    if FINISHERS.get(k.finisher).is_some_and(|f| f.0.is_some()) {
        form_row(ui, "Finishing diameter", |ui| enter |= field(ui, &mut k.finish_diameter));
        form_row(ui, "Finishing flutes", |ui| combo(ui, "carve_finish_flutes", &FLUTES, &mut k.finish_flutes));
        form_row(ui, "Finishing flute length", |ui| enter |= field(ui, &mut k.finish_flute_length));
    }
    form_row(ui, "How hard to cut", |ui| {
        let labels: Vec<&str> = TIERS.iter().map(|t| tr(t.1)).collect();
        combo(ui, "carve_tier", &labels, &mut k.tier);
    });
    form_row(ui, "Stock above the part", |ui| enter |= field(ui, &mut k.stock_top));
    form_row(ui, "Skin under the part", |ui| enter |= field(ui, &mut k.skin));
    // A changed value, or a changed design, makes the last plan another part's.
    let now = planned_for(app, k);
    if k.report_of != now {
        k.report.clear();
        k.paths.clear();
        k.refused = false;
    }
    form_row(ui, "Plan", |ui| {
        let r = crate::ui_kit::button(ui, tr("Check"), crate::ui_kit::Button::Normal);
        crate::scenario::publish_handle("dialog:carve:check", r.rect.center());
        if r.clicked() {
            (k.report, k.refused, k.paths) = check(app, k);
            k.report_of = now.clone();
        }
    });
    for (i, line) in k.report.iter().enumerate() {
        let key = format!("carve_{i}");
        let style = if k.refused { crate::ui_kit::Text::Error(&key) } else { crate::ui_kit::Text::Note(&key) };
        crate::ui_kit::text(ui, line, style);
    }
    crate::ui_kit::text(ui, tr("cam.carve.zero_help"), crate::ui_kit::Text::Note("carve_zero"));
    enter
}

/// Save G-code: unless Check has just passed these values, the plan is checked first, so a
/// refusal is read in the dialog; then the file is asked for and the programs are written.
pub fn run(app: &mut SolveApp, k: &Carve) -> Result<(), String> {
    let asked = params(k).map_err(|why| solidcraft_ui_text::tr_label(&why).to_owned())?;
    if k.report.is_empty() || k.refused || k.report_of != planned_for(app, k) {
        let mut dry = asked.clone();
        dry["dry_run"] = json!(true);
        app.session.preview(&[("cam.carve".to_string(), dry)]).map_err(|e| e.to_string())?;
    }
    app.pick_save(&format!("{}.nc", k.body), &["nc"], move |app, path| {
        let Some(path) = path else { return };
        let mut asked = asked;
        asked["path"] = json!(path);
        match app.run("cam.carve", asked) {
            Ok(reply) => {
                let tr = solidcraft_ui_text::tr_label;
                let files: Vec<&str> = reply["programs"].as_array().into_iter().flatten().filter_map(|p| p["path"].as_str()).collect();
                let warnings = lines(&reply).iter().filter(|l| l.starts_with(tr("Warning"))).count();
                app.set_status(
                    format!(
                        "{}: {} ({} {}, {warnings} {})",
                        tr("G-code saved"),
                        files.join(", "),
                        number(&json!(minutes(&reply))),
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

    /// A 60 × 40 × 9 mm base with a dome of 8 mm radius on it.
    fn domed(h: &mut crate::scenario::Harness) {
        h.app.run("solid.box", json!({"corner": [0, 0, 0], "length": 60, "width": 40, "height": 9})).unwrap();
        h.app.run("solid.sphere", json!({"center": [30, 20, 9], "radius": 8, "operation": "join"})).unwrap();
    }

    fn form(h: &crate::scenario::Harness) -> Carve {
        match h.app.dialog.as_ref().map(|d| &d.kind) {
            Some(Kind::Carve(k)) => k.clone(),
            other => panic!("no Carve Solid dialog: {other:?}"),
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
    fn a_solid_is_checked_then_saved_as_one_gcode_file_a_cutter() {
        let mut h = crate::scenario::Harness::new();
        domed(&mut h);
        h.app.start("cam.carve");
        h.frames(3);
        let k = form(&h);
        assert_eq!((k.material, k.diameter.as_str(), k.flutes, k.finisher, k.finish_flutes, k.tier), (0, "6 mm", 0, 0, 1, 1));
        assert!(!k.body.is_empty() && k.report.is_empty() && k.paths.is_empty());
        assert!(h.ctx.memory(|m| m.focused()).is_none(), "a field has the keyboard");

        // Check lists the block, both programs and their cuts, and keeps the paths to draw.
        let design = h.app.session.doc.to_json();
        h.step(&json!({"click":{"handle":"dialog:carve:check"}})).unwrap();
        h.frames(2);
        let k = form(&h);
        assert!(!k.refused, "{:?}", k.report);
        assert!(k.report[0].contains("78 × 58 × 17.5 mm"), "{:?}", k.report);
        for said in ["rough:", "finish:", "Rough · 18500 rpm · 1110 mm/min", "Walls ·", "Finish ·", "0.50 mm skin"] {
            assert!(k.report.iter().any(|l| l.contains(said)), "{said}: {:?}", k.report);
        }
        assert!(k.paths.iter().any(|r| r.cutting) && k.paths.iter().any(|r| !r.cutting));
        let low = k.paths.iter().filter(|r| r.cutting).flat_map(|r| &r.points).map(|p| p[2]).fold(f64::INFINITY, f64::min);
        assert!((low - 0.5).abs() < 1e-6, "{low}");
        assert_eq!(h.app.session.doc.to_json(), design);
        // A changed value clears the last plan; a cutter too short is refused in the dialog.
        if let Some(Kind::Carve(k)) = h.app.dialog.as_mut().map(|d| &mut d.kind) {
            k.flute_length = "10 mm".into();
        }
        h.frames(2);
        assert!(form(&h).report.is_empty() && form(&h).paths.is_empty());
        h.step(&json!({"click":{"handle":"dialog:carve:check"}})).unwrap();
        h.frames(2);
        assert!(form(&h).refused && form(&h).report[0].contains("mm of flute"), "{:?}", form(&h).report);
        h.app.services.pick_save = Some(Box::new(|_, _| panic!("a refused plan asks for no file")));
        h.step(&json!({"click":{"handle":"dialog:Save G-code…"}})).unwrap();
        h.frames(2);
        assert!(h.app.dialog.is_some(), "the dialog stays open on a refusal");
        if let Some(Kind::Carve(k)) = h.app.dialog.as_mut().map(|d| &mut d.kind) {
            k.flute_length = "22 mm".into();
        }
        h.frames(2);

        let dir = h.dir.clone();
        h.app.services.pick_save = Some(Box::new(move |name, extensions| {
            assert!(name.ends_with(".nc"), "{name}");
            assert_eq!(extensions, &["nc"]);
            Box::pin(std::future::ready(Some(dir.join("dome.nc").to_string_lossy().into_owned())))
        }));
        h.step(&json!({"click":{"handle":"dialog:Save G-code…"}})).unwrap();
        wait_for_picker(&mut h);
        h.frames(2);
        for file in ["dome-1-rough.nc", "dome-2-finish.nc"] {
            let written = std::fs::read_to_string(h.dir.join(file)).unwrap_or_else(|e| panic!("{file}: {e}"));
            assert!(written.contains("G21") && written.trim_end().ends_with("M30"), "{file}");
        }
        assert!(!h.dir.join("dome.nc").exists());
        assert_eq!(h.app.session.doc.to_json(), design, "carving keeps nothing in the design");

        // The roughing cutter alone makes one file, the one named.
        h.app.start("cam.carve");
        h.frames(3);
        if let Some(Kind::Carve(k)) = h.app.dialog.as_mut().map(|d| &mut d.kind) {
            k.finisher = 2;
        }
        // The finishing cutter's rows go, so the buttons move: a frame lays them out again.
        h.frames(2);
        let dir = h.dir.clone();
        h.app.services.pick_save = Some(Box::new(move |_, _| Box::pin(std::future::ready(Some(dir.join("one.nc").to_string_lossy().into_owned())))));
        h.step(&json!({"click":{"handle":"dialog:Save G-code…"}})).unwrap();
        wait_for_picker(&mut h);
        h.frames(2);
        assert!(h.dir.join("one.nc").exists());
    }

    #[test]
    fn a_design_without_a_solid_says_so_and_asks_for_no_file() {
        let mut h = crate::scenario::Harness::new();
        h.app.start("cam.carve");
        h.frames(3);
        assert!(form(&h).body.is_empty());
        h.app.services.pick_save = Some(Box::new(|_, _| panic!("nothing to carve asks for no file")));
        h.step(&json!({"click":{"handle":"dialog:carve:check"}})).unwrap();
        h.frames(2);
        assert!(form(&h).refused && form(&h).report[0].contains("solid body first"), "{:?}", form(&h).report);
        h.step(&json!({"click":{"handle":"dialog:Save G-code…"}})).unwrap();
        h.frames(2);
        assert!(h.app.dialog.is_some());
    }
}
