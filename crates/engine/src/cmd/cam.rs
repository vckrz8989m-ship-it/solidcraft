//! CAM commands: tools, setups, 2D contour/pocket operations, and G-code posting through
//! `solidcraft-cam`. Toolpaths are computed at post time and never stored on the design.
//! Geometry is a sketch profile or planar face (or explicit polylines); depths and feeds are
//! expressions evaluated when posting.

use serde_json::{Value, json};
use solidcore_record::{Loop2, Plane, Region2, Seg2, Vec2, Vec3};
use solidcraft_cam::router::auto::{PartCut, Planned, plan_part};
use solidcraft_cam::router::feeds::{CutKind, FeedRequest, Material, Tier, recipe};
use solidcraft_cam::router::{Issue, MachineProfile, Severity, check_program, check_text, check_travel};
use solidcraft_cam::{
    ContourParams, Cut, Feeds, Heights, Move, Operation, PocketParams, Post, Program, Side, Tool, ToolKind, contour, emit, pocket, readback,
};
use solidcraft_doc::cam::{
    CamContourParams, CamCut, CamFaceRef, CamGeometry, CamOperation, CamPocketParams, CamSettings, CamSetup, CamSide, MAX_OPS, MAX_SETUPS, MAX_TOOLS,
    PolygonRegion, StockBox, WcsOriginMode, default_contour_params, default_cut, default_pocket_params,
};
use solidcraft_doc::expr::Kind;
use solidcraft_doc::project::resolve_face_indices;
use solidcraft_doc::{Expr, FeatureKind, select_profiles};
use solidcraft_geom::prelude::*;
use solidcraft_sketch::{LinkGeom, LinkKind};

use super::CommandSpec;
use crate::params::{bad, bool_, expr, num, req_expr, str_, string_list, vec2, vec2_list, vec3};
use crate::{Result, Session};

pub static COMMANDS: &[CommandSpec] = &[
    CommandSpec::new("cam.tool.define", "Define Tool", tool_cmd).icon("hole").params(
        "number (1…9999); kind?: flat_endmill|ball_endmill|drill (default flat_endmill); diameter (mm); flute_length (mm); \
         description?; flutes?; spindle_rpm?; feed?; plunge_feed?; stepdown?; stepover?",
    ),
    CommandSpec::new("cam.tool.remove", "Remove Tool", tool_remove).icon("delete").params("number"),
    CommandSpec::new("cam.setup", "CAM Setup", setup_cmd).icon("settings").params(
        "name?: Setup 1, Setup 2, … when omitted; work_offset?: 1…6 (G54…G59, default 1); \
         tool_axis?: [x,y,z] (programmed +Z, default [0,0,1]); \
         x_axis?: [x,y,z] (programmed +X, square to the tool axis; omitted, derived); \
         work_origin?: [x,y,z] mm part zero in the model (default [0,0,0]); \
         stock?: {min:[x,y,z], max:[x,y,z]}; delete?: bool",
    ),
    CommandSpec::new("cam.op.contour", "2D Contour", contour_cmd).icon("cut").params(
        "setup; name; tool (number); sketch?: id or name, with profiles?: as solid.extrude; faces?: [{body, name, at?}]; \
         or outer: [[x,y],…] (or regions: [{outer, holes?}]); holes?: [[[x,y],…]]; \
         side?: outside|inside|on (default outside); climb?: bool (default true); stock_to_leave?; tolerance?; \
         ramp_angle?: deg (default 0); clearance?; retract?; top?; bottom (required unless cut given); stepdown?; \
         feed?; plunge_feed?; spindle_rpm?; coolant?",
    ),
    CommandSpec::new("cam.op.pocket", "2D Pocket", pocket_cmd).icon("hole").params(
        "setup; name; tool (number); sketch?: id or name, with profiles?; faces?; or outer/regions; holes?; \
         stepover? (default half the tool diameter); climb?; stock_to_leave?; tolerance?; ramp_angle?: deg; \
         clearance?; retract?; top?; bottom; stepdown?; feed?; plunge_feed?; spindle_rpm?; coolant?",
    ),
    CommandSpec::new("cam.op.part", "Cut Out Part", part_cmd).icon("cut").params(
        "setup; name; tool (number); sketch?: id or name, with profiles?; faces?; or outer/regions; holes?; \
         material: aluminum|aluminum_soft|brass|acrylic|soft_plastic|hardwood|softwood; thickness (of the plate); \
         tier?: careful|standard|bold (default standard); top? (the plate's top, default 0); finish_stock? (default 0.2 mm); \
         break_through? (below the plate, default 0.2 mm); coolant?: bool (default true); \
         hold?: tabs|none (default tabs: three to eight holding tabs are left round the outside); \
         machine?: (default flexihal-router; what the plan is tried on, cam.post names the one it is written for) → \
         {setup, operation, cuts: [names in cutting order], issues}. Every opening the tool fits is roughed and \
         finished, nearest first, then the outside is, at the machine's feeds; the cuts are planned again when posting.",
    ),
    CommandSpec::new("cam.cut_out", "Cut Out Part to G-code", cut_out_cmd).icon("cut").params(
        "sketch?: id or name (default the newest sketch), with profiles?; faces?; or outer/regions; holes?; \
         material: aluminum|aluminum_soft|brass|acrylic|soft_plastic|hardwood|softwood; thickness (of the plate); \
         tool?: number of a defined tool, or tool_diameter with flutes? (default 1) and flute_length? (default 3 diameters): \
         a tool of that size is used, or defined under the next free number; tier?: careful|standard|bold; hold?: tabs|none; \
         name?: the operation's (default the sketch's name); setup?: (default Router, made when missing: work zero at the \
         sketch origin, Z 0 on the plate's top); machine?: (default flexihal-router); path?: file the G-code is written to; \
         dry_run?: bool (plan and check only: nothing is kept in the design or written) → \
         {setup, operation, tool, cuts: [{name, rpm, feed}] in cutting order, issues, extents, gcode, readback, path?}. One step from a drawn outline to a \
         checked program of that part alone: the part is kept in the setup as cam.op.part would keep it.",
    ),
    CommandSpec::new("cam.op.remove", "Remove CAM Operation", delete_op).icon("delete").params("setup; name"),
    CommandSpec::new("cam.post", "Post Process", post_cmd).icon("export").noundo().params(
        "setup; post?: grblhal | rs274 | name of a saved post (default rs274, or the machine's own); \
         machine?: flexihal-router (the default when the setup cuts out a part; its post, and its checks: spindle range, feeds and arcs it can follow, codes and \
         machine-coordinate moves its controller refuses); machine_origin?: [x,y,z] the work zero in machine coordinates \
         (checks travel when the machine's is known); program?: name (default the setup's); \
         path?: file the G-code is written to, once it has passed every check → \
         {gcode, extension, post, operations, readback, extents: {min, max} of all motion in work coordinates, \
         machine?, issues?, path?}. G-code is always read back and checked \
         against the toolpaths; with a machine, an issue that would stop the program is an error.",
    ),
    CommandSpec::new("cam.feeds", "Feeds and Speeds", feeds_cmd).icon("settings").params(
        "material: aluminum|aluminum_soft|brass|acrylic|soft_plastic|hardwood|softwood; \
         kind: slot|pocket|adaptive|finish|bore; tool?: number of a defined tool, or diameter (mm) with flutes? (default 1); \
         tier?: careful|standard|bold (default standard); rpm?; width?; depth? (mm; for a bore the helix pitch); \
         machine?: (default flexihal-router); apply?: bool (write the speed, feeds, depth and width into the tool) → \
         {recipe: {rpm, feed, ramp_feed, width, depth, chip_thickness, cut_power, spindle_power, warnings, …}}. \
         A starting point for a test cut, not a proven recipe.",
    ),
    CommandSpec::new("cam.list", "List CAM", list_cmd).icon("list").noundo().params("→ tools, setups (with operations) and saved posts"),
];

fn tool_cmd(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "cam.tool.define";
    let number = num(p, "number")
        .filter(|n| *n >= 1.0 && *n <= 9999.0 && n.fract() == 0.0)
        .map(|n| n as u32)
        .ok_or_else(|| bad(cmd, "`number` must be an integer 1…9999"))?;
    let kind = match str_(p, "kind").unwrap_or("flat_endmill") {
        "flat_endmill" => ToolKind::FlatEndmill,
        "ball_endmill" => ToolKind::BallEndmill,
        "drill" => ToolKind::Drill,
        other => return Err(bad(cmd, format!("unknown tool kind `{other}` (flat_endmill, ball_endmill, drill)"))),
    };
    let diameter = num(p, "diameter").ok_or_else(|| bad(cmd, "`diameter` is required (mm)"))?;
    let flute_length = num(p, "flute_length").ok_or_else(|| bad(cmd, "`flute_length` is required (mm)"))?;
    let mut tool = Tool::new(number, kind, diameter, flute_length);
    if let Some(d) = str_(p, "description") {
        tool.description = d.chars().take(256).collect();
    }
    if let Some(f) = num(p, "flutes").filter(|n| *n >= 1.0 && *n <= 100.0 && n.fract() == 0.0) {
        tool.flutes = f as u32;
    }
    tool.spindle_rpm = num(p, "spindle_rpm").or(tool.spindle_rpm);
    tool.feed = num(p, "feed").or(tool.feed);
    tool.plunge_feed = num(p, "plunge_feed").or(tool.plunge_feed);
    tool.stepdown = num(p, "stepdown").or(tool.stepdown);
    tool.stepover = num(p, "stepover").or(tool.stepover);
    tool.validate().map_err(|e| bad(cmd, e.to_string()))?;
    let doc = s.doc_mut();
    if doc.cam.tools.len() >= MAX_TOOLS && !doc.cam.tools.iter().any(|t| t.number == number) {
        return Err(bad(cmd, format!("a design has at most {MAX_TOOLS} tools")));
    }
    match doc.cam.tools.iter_mut().find(|t| t.number == number) {
        Some(slot) => *slot = tool.clone(),
        None => doc.cam.tools.push(tool.clone()),
    }
    doc.cam.tools.sort_by_key(|t| t.number);
    Ok(json!({"tool": tool}))
}

fn tool_remove(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "cam.tool.remove";
    let number = num(p, "number")
        .filter(|n| *n >= 1.0 && *n <= 9999.0 && n.fract() == 0.0)
        .map(|n| n as u32)
        .ok_or_else(|| bad(cmd, "`number` must be an integer 1…9999"))?;
    let doc = s.doc_mut();
    if doc.cam.setups.iter().any(|setup| setup.operations.iter().any(|op| op.tool() == number)) {
        return Err(bad(cmd, format!("tool {number} is used by an operation")));
    }
    let before = doc.cam.tools.len();
    doc.cam.tools.retain(|t| t.number != number);
    if doc.cam.tools.len() == before {
        return Err(bad(cmd, format!("no tool {number}")));
    }
    Ok(json!({"deleted": number}))
}

fn setup_cmd(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "cam.setup";
    let named = str_(p, "name").map(str::trim).filter(|n| !n.is_empty() && n.len() <= 128).map(str::to_string);
    if bool_(p, "delete").unwrap_or(false) {
        let name = named.ok_or_else(|| bad(cmd, "`name` is required to delete a setup"))?;
        let doc = s.doc_mut();
        let before = doc.cam.setups.len();
        doc.cam.setups.retain(|x| x.name != name);
        if doc.cam.setups.len() == before {
            return Err(bad(cmd, format!("no setup `{name}`")));
        }
        return Ok(json!({"deleted": name}));
    }
    let name = named.unwrap_or_else(|| s.doc.cam.unused_setup_name());
    let existing = s.doc.cam.setup(&name).cloned();
    let work_offset = num(p, "work_offset").map_or(existing.as_ref().map(|e| e.work_offset).unwrap_or(1), |n| n as u8);
    let tool_axis = match p.get("tool_axis") {
        Some(v) => {
            let a = vec3(v).ok_or_else(|| bad(cmd, "`tool_axis` must be [x, y, z]"))?;
            [a.x, a.y, a.z]
        }
        None => existing.as_ref().map(|e| e.tool_axis).unwrap_or([0.0, 0.0, 1.0]),
    };
    let work_origin = match p.get("work_origin") {
        Some(v) => {
            let a = vec3(v).ok_or_else(|| bad(cmd, "`work_origin` must be [x, y, z]"))?;
            [a.x, a.y, a.z]
        }
        None => existing.as_ref().map(|e| e.work_origin).unwrap_or([0.0, 0.0, 0.0]),
    };
    let x_axis = match p.get("x_axis") {
        Some(v) => {
            let a = vec3(v).ok_or_else(|| bad(cmd, "`x_axis` must be [x, y, z]"))?;
            Some([a.x, a.y, a.z])
        }
        None => existing.as_ref().and_then(|e| e.x_axis),
    };
    let stock = match p.get("stock") {
        None => existing.as_ref().and_then(|e| e.stock),
        Some(v) if v.get("min").is_some() => Some(stock_box(cmd, v)?),
        Some(_) => existing.as_ref().and_then(|e| e.stock),
    };
    let mut setup = CamSetup::new(&name);
    setup.work_offset = work_offset;
    setup.tool_axis = tool_axis;
    setup.x_axis = x_axis;
    setup.work_origin = work_origin;
    setup.stock = stock;
    setup.stock_spec = parse_value(cmd, p, "stock_spec", existing.as_ref().map(|e| e.stock_spec.clone()).unwrap_or_default())?;
    setup.wcs = parse_value(cmd, p, "wcs", existing.as_ref().map(|e| e.wcs.clone()).unwrap_or_default())?;
    setup.wcs_orientation = parse_value(cmd, p, "wcs_orientation", existing.as_ref().map(|e| e.wcs_orientation).unwrap_or_default())?;
    setup.wcs_origin_mode =
        parse_value(cmd, p, "wcs_origin_mode", existing.as_ref().map(|e| e.wcs_origin_mode).unwrap_or(WcsOriginMode::SelectedPoint))?;
    setup.box_point = parse_value(cmd, p, "box_point", existing.as_ref().map(|e| e.box_point).unwrap_or_default())?;
    setup.flip_z = bool_(p, "flip_z").unwrap_or(existing.as_ref().map(|e| e.flip_z).unwrap_or(false));
    setup.flip_x = bool_(p, "flip_x").unwrap_or(existing.as_ref().map(|e| e.flip_x).unwrap_or(false));
    setup.model_bodies = if p.get("model_bodies").is_some() {
        string_list(cmd, p, "model_bodies")?
    } else {
        existing.as_ref().map(|e| e.model_bodies.clone()).unwrap_or_default()
    };
    setup.operation_type = parse_value(cmd, p, "operation_type", existing.as_ref().map(|e| e.operation_type).unwrap_or_default())?;
    setup.post_opts = parse_value(cmd, p, "post_opts", existing.as_ref().map(|e| e.post_opts.clone()).unwrap_or_default())?;
    setup.machine = match p.get("machine") {
        Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
        Some(Value::Null) => None,
        Some(_) => return Err(bad(cmd, "`machine` must be a name or null")),
        None => existing.as_ref().and_then(|e| e.machine.clone()),
    };
    setup.post = match p.get("post") {
        Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
        Some(Value::Null) => None,
        Some(_) => return Err(bad(cmd, "`post` must be a name or null")),
        None => existing.as_ref().and_then(|e| e.post.clone()),
    };
    if p.get("wcs").is_some() {
        setup.work_origin = setup.wcs.origin;
        setup.tool_axis = setup.wcs.z_axis();
        setup.x_axis = Some(setup.wcs.x_axis);
    } else if existing.is_none() {
        setup.wcs.origin = setup.work_origin;
    }
    if (1..=6).contains(&setup.post_opts.wcs_offset) && p.get("post_opts").is_some() && p.get("work_offset").is_none() {
        setup.work_offset = setup.post_opts.wcs_offset as u8;
    }
    if let Some(existing) = &existing {
        setup.operations = existing.operations.clone();
    }
    setup.validate().map_err(|e| bad(cmd, e.to_string()))?;
    let doc = s.doc_mut();
    if doc.cam.setups.len() >= MAX_SETUPS && !doc.cam.setups.iter().any(|x| x.name == name) {
        return Err(bad(cmd, format!("a design has at most {MAX_SETUPS} setups")));
    }
    match doc.cam.setups.iter_mut().find(|x| x.name == name) {
        Some(slot) => {
            let operations = std::mem::take(&mut slot.operations);
            *slot = setup.clone();
            slot.operations = operations;
        }
        None => doc.cam.setups.push(setup.clone()),
    }
    Ok(json!({"setup": setup_json(s.doc.cam.setup(&name).unwrap_or(&setup))}))
}

fn parse_value<T: serde::de::DeserializeOwned>(cmd: &str, p: &Value, k: &str, fallback: T) -> Result<T> {
    match p.get(k) {
        None => Ok(fallback),
        Some(v) => serde_json::from_value(v.clone()).map_err(|e| bad(cmd, format!("`{k}`: {e}"))),
    }
}

fn stock_box(cmd: &str, v: &Value) -> Result<StockBox> {
    let min = v.get("min").and_then(Value::as_array).filter(|a| a.len() == 3);
    let max = v.get("max").and_then(Value::as_array).filter(|a| a.len() == 3);
    let (Some(min), Some(max)) = (min, max) else {
        return Err(bad(cmd, "`stock` must be {min:[x,y,z], max:[x,y,z]}"));
    };
    let parse = |a: &[Value]| -> Result<[f64; 3]> {
        Ok([
            a[0].as_f64().filter(|x| x.is_finite()).ok_or_else(|| bad(cmd, "stock coordinates must be finite"))?,
            a[1].as_f64().filter(|x| x.is_finite()).ok_or_else(|| bad(cmd, "stock coordinates must be finite"))?,
            a[2].as_f64().filter(|x| x.is_finite()).ok_or_else(|| bad(cmd, "stock coordinates must be finite"))?,
        ])
    };
    let box_ = StockBox { min: parse(min)?, max: parse(max)? };
    box_.validate().map_err(|e| bad(cmd, e.to_string()))?;
    Ok(box_)
}

fn contour_cmd(s: &mut Session, p: &Value) -> Result<Value> {
    op_cmd(s, p, "cam.op.contour", true)
}

fn pocket_cmd(s: &mut Session, p: &Value) -> Result<Value> {
    op_cmd(s, p, "cam.op.pocket", false)
}

fn op_cmd(s: &mut Session, p: &Value, cmd: &str, is_contour: bool) -> Result<Value> {
    let setup_name = str_(p, "setup").map(str::trim).filter(|n| !n.is_empty()).ok_or_else(|| bad(cmd, "`setup` is required"))?.to_string();
    let name = str_(p, "name").map(str::trim).filter(|n| !n.is_empty() && n.len() <= 128).ok_or_else(|| bad(cmd, "`name` is required"))?.to_string();
    let tool_n =
        num(p, "tool").filter(|n| *n >= 1.0 && n.fract() == 0.0).map(|n| n as u32).ok_or_else(|| bad(cmd, "`tool` must be a tool number"))?;
    let tool = s.doc.cam.tool(tool_n).cloned().ok_or_else(|| bad(cmd, format!("no tool {tool_n} (cam.tool.define first)")))?;
    let geometry = geometry_param(s, cmd, p)?;
    let cut = cut_param(cmd, p, &tool)?;
    let spindle_rpm = num(p, "spindle_rpm").or(tool.spindle_rpm);
    let coolant = bool_(p, "coolant").unwrap_or(false);
    let op = if is_contour {
        let mut params = default_contour_params();
        if let Some(side) = str_(p, "side") {
            params.side = parse_side(cmd, side)?;
        }
        if let Some(c) = bool_(p, "climb") {
            params.climb = c;
        }
        if let Some(v) = expr(p, "stock_to_leave") {
            params.stock_to_leave = v;
        }
        if let Some(v) = expr(p, "tolerance") {
            params.tolerance = v;
        }
        if let Some(v) = expr(p, "ramp_angle") {
            params.ramp_angle = v;
        }
        CamOperation::Contour { name: name.clone(), tool: tool_n, geometry, params, cut, spindle_rpm, coolant }
    } else {
        let stepover = match expr(p, "stepover") {
            Some(e) => e,
            None => match tool.stepover {
                Some(v) => Expr::new(format!("{v} mm")),
                None => Expr::new(format!("{} mm", tool.diameter * 0.5)),
            },
        };
        let mut params = default_pocket_params(stepover);
        if let Some(c) = bool_(p, "climb") {
            params.climb = c;
        }
        if let Some(v) = expr(p, "stock_to_leave") {
            params.stock_to_leave = v;
        }
        if let Some(v) = expr(p, "tolerance") {
            params.tolerance = v;
        }
        if let Some(v) = expr(p, "ramp_angle") {
            params.ramp_angle = v;
        }
        CamOperation::Pocket { name: name.clone(), tool: tool_n, geometry, params, cut, spindle_rpm, coolant }
    };
    op.validate().map_err(|e| bad(cmd, e.to_string()))?;
    let setup = s.doc.cam.setup(&setup_name).cloned().ok_or_else(|| bad(cmd, format!("no setup `{setup_name}` (cam.setup first)")))?;
    build_path(s, &setup, &op).map_err(|e| bad(cmd, e))?;
    store_op(s, cmd, &setup_name, op)?;
    Ok(json!({"setup": setup_name, "operation": name}))
}

/// Puts `op` in the setup, in place of the operation of its name if there is one.
fn store_op(s: &mut Session, cmd: &str, setup_name: &str, op: CamOperation) -> Result<()> {
    let doc = s.doc_mut();
    let setup = doc.cam.setup_mut(setup_name).ok_or_else(|| bad(cmd, format!("no setup `{setup_name}` (cam.setup first)")))?;
    if let Some(slot) = setup.operations.iter_mut().find(|o| o.name() == op.name()) {
        *slot = op;
    } else {
        if setup.operations.len() >= MAX_OPS {
            return Err(bad(cmd, format!("a setup has at most {MAX_OPS} operations")));
        }
        setup.operations.push(op);
    }
    Ok(())
}

fn part_cmd(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "cam.op.part";
    let setup_name = str_(p, "setup").map(str::trim).filter(|n| !n.is_empty()).ok_or_else(|| bad(cmd, "`setup` is required"))?.to_string();
    let name = str_(p, "name").map(str::trim).filter(|n| !n.is_empty() && n.len() <= 128).ok_or_else(|| bad(cmd, "`name` is required"))?.to_string();
    let tool = num(p, "tool").filter(|n| *n >= 1.0 && n.fract() == 0.0).map(|n| n as u32).ok_or_else(|| bad(cmd, "`tool` must be a tool number"))?;
    let cutter = s.doc.cam.tool(tool).cloned().ok_or_else(|| bad(cmd, format!("no tool {tool} (cam.tool.define first)")))?;
    let material: Material = word(cmd, p, "material", "aluminum, aluminum_soft, brass, acrylic, soft_plastic, hardwood or softwood")?
        .ok_or_else(|| bad(cmd, "`material` is required: aluminum, aluminum_soft, brass, acrylic, soft_plastic, hardwood or softwood"))?;
    let tier: Tier = word(cmd, p, "tier", "careful, standard or bold")?.unwrap_or_default();
    let machine = machine_named(cmd, str_(p, "machine").unwrap_or(DEFAULT_MACHINE))?;
    let op = CamOperation::Part {
        name: name.clone(),
        tool,
        geometry: geometry_param(s, cmd, p)?,
        material,
        thickness: req_expr(cmd, p, "thickness")?,
        tier,
        top: expr(p, "top").unwrap_or_else(|| Expr::new("0 mm")),
        finish_stock: expr(p, "finish_stock").unwrap_or_else(|| Expr::new("0.2 mm")),
        break_through: expr(p, "break_through").unwrap_or_else(|| Expr::new("0.2 mm")),
        coolant: bool_(p, "coolant").unwrap_or(true),
        hold: word(cmd, p, "hold", "tabs or none")?.unwrap_or_default(),
    };
    op.validate().map_err(|e| bad(cmd, e.to_string()))?;
    let setup = s.doc.cam.setup(&setup_name).cloned().ok_or_else(|| bad(cmd, format!("no setup `{setup_name}` (cam.setup first)")))?;
    let plan = plan_op(s, &setup, &op, &cutter, &machine).map_err(|e| bad(cmd, e))?;
    if let Some(stop) = plan.issues.iter().find(|i| i.severity == Severity::Error) {
        return Err(bad(cmd, stop.message.clone()));
    }
    store_op(s, cmd, &setup_name, op)?;
    Ok(json!({
        "setup": setup_name,
        "operation": name,
        "cuts": plan.operations.iter().map(|o| o.name.as_str()).collect::<Vec<_>>(),
        "issues": plan.issues,
    }))
}

/// The machine a part is planned for when none is named.
const DEFAULT_MACHINE: &str = "flexihal-router";

/// The cuts of a part operation on `machine`, named after it (`Plate: Outside rough`).
fn plan_op(s: &Session, setup: &CamSetup, op: &CamOperation, tool: &Tool, machine: &MachineProfile) -> std::result::Result<Planned, String> {
    let CamOperation::Part { name, geometry, material, thickness, tier, top, finish_stock, break_through, coolant, hold, .. } = op else {
        return Err(format!("`{}` is not a part operation", op.name()));
    };
    let regions = resolve_geometry(s, setup, geometry)?;
    let mut cut = PartCut::new(*material, eval_len(s, thickness, "thickness")?);
    cut.tier = *tier;
    cut.top = eval_len(s, top, "top")?;
    cut.finish_stock = eval_len(s, finish_stock, "finish_stock")?;
    cut.break_through = eval_len(s, break_through, "break_through")?;
    cut.coolant = *coolant;
    cut.hold = *hold;
    let mut plan = plan_part(&regions, tool, &cut, machine).map_err(|e| e.to_string())?;
    for o in &mut plan.operations {
        o.name = format!("{name}: {}", o.name);
    }
    for i in &mut plan.issues {
        i.at = Some(match i.at.take() {
            Some(at) => format!("{name}: {at}"),
            None => name.clone(),
        });
    }
    Ok(plan)
}

fn parse_side(cmd: &str, side: &str) -> Result<CamSide> {
    match side {
        "outside" => Ok(CamSide::Outside),
        "inside" => Ok(CamSide::Inside),
        "on" => Ok(CamSide::On),
        other => Err(bad(cmd, format!("`side` must be outside, inside or on (not `{other}`)"))),
    }
}

fn geometry_param(s: &Session, cmd: &str, p: &Value) -> Result<CamGeometry> {
    if p.get("sketch").is_some() {
        let key = super::sketch_key(p.get("sketch")).ok_or_else(|| bad(cmd, "`sketch` must be a sketch id or name"))?;
        let sketch = match s.doc.find_feature(&key) {
            Some(f) if matches!(f.kind, FeatureKind::Sketch { .. }) => f.id,
            _ => return Err(bad(cmd, format!("no sketch `{key}`"))),
        };
        let profiles = super::feature_profiles(p, cmd)?;
        return Ok(CamGeometry::Sketch { sketch, profiles });
    }
    if let Some(arr) = p.get("faces").and_then(Value::as_array) {
        if arr.is_empty() {
            return Err(bad(cmd, "`faces` is empty"));
        }
        let mut faces = Vec::new();
        for v in arr {
            let body = v.get("body").and_then(Value::as_str).filter(|n| !n.is_empty()).ok_or_else(|| bad(cmd, "each face needs `body`"))?;
            let name = v.get("name").and_then(Value::as_str).filter(|n| !n.is_empty()).ok_or_else(|| bad(cmd, "each face needs `name`"))?;
            let at = match v.get("at") {
                None => [0.0, 0.0, 0.0],
                Some(a) => {
                    let p = vec3(a).ok_or_else(|| bad(cmd, "`at` must be [x, y, z]"))?;
                    [p.x, p.y, p.z]
                }
            };
            faces.push(CamFaceRef { body: body.to_string(), name: name.to_string(), at });
        }
        return Ok(CamGeometry::Faces { faces });
    }
    Ok(CamGeometry::Polygons { regions: regions_param(cmd, p)? })
}

fn regions_param(cmd: &str, p: &Value) -> Result<Vec<PolygonRegion>> {
    if let Some(arr) = p.get("regions").and_then(Value::as_array) {
        return arr.iter().map(|v| polygon_region(cmd, v)).collect();
    }
    let outer = vec2_list(cmd, p, "outer", solidcraft_doc::cam::MAX_LOOP_POINTS)?;
    if outer.len() < 3 {
        return Err(bad(cmd, "`outer` needs at least 3 points (or pass `sketch`, `faces`, or `regions`)"));
    }
    let holes = match p.get("holes") {
        None => Vec::new(),
        Some(Value::Array(hs)) => hs
            .iter()
            .map(|h| {
                let pts = h.as_array().ok_or_else(|| bad(cmd, "`holes` must be a list of point lists"))?;
                if pts.len() < 3 || pts.len() > solidcraft_doc::cam::MAX_LOOP_POINTS {
                    return Err(bad(cmd, "each hole needs 3 to 10000 points"));
                }
                pts.iter().map(|v| vec2(v).map(|p| [p.x, p.y]).ok_or_else(|| bad(cmd, "hole points must be [x, y]"))).collect::<Result<Vec<_>>>()
            })
            .collect::<Result<Vec<_>>>()?,
        _ => return Err(bad(cmd, "`holes` must be a list of point lists")),
    };
    let region = PolygonRegion { outer: outer.into_iter().map(|p| [p.x, p.y]).collect(), holes };
    region.validate().map_err(|e| bad(cmd, e.to_string()))?;
    Ok(vec![region])
}

fn polygon_region(cmd: &str, v: &Value) -> Result<PolygonRegion> {
    let outer = v
        .get("outer")
        .and_then(Value::as_array)
        .ok_or_else(|| bad(cmd, "each region needs `outer`: [[x,y],…]"))?
        .iter()
        .map(|p| vec2(p).map(|p| [p.x, p.y]).ok_or_else(|| bad(cmd, "region points must be [x, y]")))
        .collect::<Result<Vec<_>>>()?;
    let holes = match v.get("holes") {
        None => Vec::new(),
        Some(Value::Array(hs)) => hs
            .iter()
            .map(|h| {
                h.as_array()
                    .ok_or_else(|| bad(cmd, "holes must be point lists"))?
                    .iter()
                    .map(|p| vec2(p).map(|p| [p.x, p.y]).ok_or_else(|| bad(cmd, "hole points must be [x, y]")))
                    .collect::<Result<Vec<_>>>()
            })
            .collect::<Result<Vec<_>>>()?,
        _ => return Err(bad(cmd, "`holes` must be a list of point lists")),
    };
    let region = PolygonRegion { outer, holes };
    region.validate().map_err(|e| bad(cmd, e.to_string()))?;
    Ok(region)
}

fn cut_param(cmd: &str, p: &Value, tool: &Tool) -> Result<CamCut> {
    let src = p.get("cut").unwrap_or(p);
    let bottom = req_expr(cmd, src, "bottom")?;
    let top = expr(src, "top").unwrap_or_else(|| Expr::new("0 mm"));
    let retract = expr(src, "retract").unwrap_or_else(|| Expr::new("2 mm"));
    let clearance = expr(src, "clearance").unwrap_or_else(|| Expr::new("10 mm"));
    let stepdown = match expr(src, "stepdown") {
        Some(e) => e,
        None => match tool.stepdown {
            Some(v) => Expr::new(format!("{v} mm")),
            None => Expr::new(format!("{} mm", tool.diameter * 0.5)),
        },
    };
    let feed = match expr(src, "feed") {
        Some(e) => e,
        None => match tool.feed {
            Some(v) => Expr::new(format!("{v} mm")),
            None => Expr::new("600 mm"),
        },
    };
    let plunge_feed = match expr(src, "plunge_feed") {
        Some(e) => e,
        None => match tool.plunge_feed {
            Some(v) => Expr::new(format!("{v} mm")),
            None => Expr::new("200 mm"),
        },
    };
    let mut cut = default_cut(bottom, stepdown);
    cut.top = top;
    cut.retract = retract;
    cut.clearance = clearance;
    cut.feed = feed;
    cut.plunge_feed = plunge_feed;
    Ok(cut)
}

fn delete_op(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "cam.op.remove";
    let setup_name = str_(p, "setup").map(str::trim).filter(|n| !n.is_empty()).ok_or_else(|| bad(cmd, "`setup` is required"))?.to_string();
    let name = str_(p, "name").map(str::trim).filter(|n| !n.is_empty()).ok_or_else(|| bad(cmd, "`name` is required"))?.to_string();
    let doc = s.doc_mut();
    let setup = doc.cam.setup_mut(&setup_name).ok_or_else(|| bad(cmd, format!("no setup `{setup_name}`")))?;
    let before = setup.operations.len();
    setup.operations.retain(|o| o.name() != name);
    if setup.operations.len() == before {
        return Err(bad(cmd, format!("no operation `{name}` in setup `{setup_name}`")));
    }
    Ok(json!({"deleted": name, "setup": setup_name}))
}

fn post_cmd(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "cam.post";
    let setup_name = str_(p, "setup").map(str::trim).filter(|n| !n.is_empty()).ok_or_else(|| bad(cmd, "`setup` is required"))?.to_string();
    let setup = s.doc.cam.setup(&setup_name).cloned().ok_or_else(|| bad(cmd, format!("no setup `{setup_name}`")))?;
    if setup.operations.is_empty() {
        return Err(bad(cmd, format!("setup `{setup_name}` has no operations")));
    }
    let has_part = setup.operations.iter().any(|op| matches!(op, CamOperation::Part { .. }));
    let machine = match str_(p, "machine") {
        Some(name) => Some(machine_named(cmd, name)?),
        // A part's feeds are a machine's, so its program is written for one.
        None if has_part => Some(machine_named(cmd, DEFAULT_MACHINE)?),
        None => None,
    };
    let post = match (&machine, str_(p, "post")) {
        (Some(m), None) => m.post().map_err(|e| bad(cmd, e.to_string()))?,
        (_, name) => resolve_post(&s.doc.cam, name.unwrap_or("rs274")).map_err(|e| bad(cmd, e))?,
    };
    let program_name = str_(p, "program").unwrap_or(setup.name.as_str()).to_string();
    let mut program = Program { name: program_name, operations: Vec::new(), work_offset: Some(setup.work_offset) };
    let mut planning: Vec<Issue> = Vec::new();
    for op in &setup.operations {
        let tool = s.doc.cam.tool(op.tool()).cloned().ok_or_else(|| bad(cmd, format!("operation `{}` needs tool {}", op.name(), op.tool())))?;
        let (spindle_rpm, coolant) = match (op, &machine) {
            (CamOperation::Contour { spindle_rpm, coolant, .. } | CamOperation::Pocket { spindle_rpm, coolant, .. }, _) => {
                (spindle_rpm.or(tool.spindle_rpm).unwrap_or(6000.0), *coolant)
            }
            (CamOperation::Part { .. }, Some(m)) => {
                let plan = plan_op(s, &setup, op, &tool, m).map_err(|e| bad(cmd, format!("operation `{}`: {e}", op.name())))?;
                program.operations.extend(plan.operations);
                planning.extend(plan.issues);
                continue;
            }
            (CamOperation::Part { .. }, None) => return Err(bad(cmd, format!("operation `{}` needs a machine", op.name()))),
        };
        let path = build_path(s, &setup, op).map_err(|e| bad(cmd, e))?;
        program.operations.push(Operation { name: op.name().into(), tool, spindle_rpm, coolant, path });
    }
    let path = path_param(cmd, p)?;
    write_program(cmd, &program, &post, machine.as_ref(), planning, p.get("machine_origin"), path)
}

/// The file a command is to write, when it names one.
fn path_param<'a>(cmd: &str, p: &'a Value) -> Result<Option<&'a str>> {
    let path = match p.get("path") {
        None | Some(Value::Null) => return Ok(None),
        Some(_) => str_(p, "path").filter(|v| !v.trim().is_empty() && v.len() < 4096).ok_or_else(|| bad(cmd, "`path` must be a file name"))?,
    };
    solidcraft_io::vfs::check(path).map_err(|e| crate::EngineError::Other(e.to_string()))?;
    Ok(Some(path))
}

/// `program` as G-code of `post`, read back and checked (for `machine` too, with what
/// `planning` it came with), and written to `path` once nothing stops it: the reply of `cam.post`.
fn write_program(
    cmd: &str,
    program: &Program,
    post: &Post,
    machine: Option<&MachineProfile>,
    planning: Vec<Issue>,
    machine_origin: Option<&Value>,
    path: Option<&str>,
) -> Result<Value> {
    post.validate().map_err(|e| bad(cmd, e.to_string()))?;
    let stopped = |i: &Issue| {
        let at = i.at.as_deref().map(|a| format!("{a}: ")).unwrap_or_default();
        format!("{at}{}", i.message)
    };
    if let Some(stop) = planning.iter().find(|i| i.severity == Severity::Error) {
        return Err(bad(cmd, stopped(stop)));
    }
    if program.operations.is_empty() {
        let why = planning.first().map(|i| format!(": {}", stopped(i))).unwrap_or_default();
        return Err(bad(cmd, format!("there is nothing to cut{why}")));
    }
    let gcode = emit(program, post).map_err(|e| bad(cmd, e.to_string()))?;
    let mut out = json!({
        "gcode": gcode,
        "extension": post.extension,
        "post": post.name,
        "operations": program.operations.len(),
        "work_offset": program.work_offset.unwrap_or(1),
    });
    let rb = readback(&gcode, program, post).map_err(|e| bad(cmd, e.to_string()))?;
    if !rb.within() {
        return Err(bad(cmd, format!("G-code read-back deviation {:.4} mm exceeds allowed {:.4} mm", rb.max_deviation, rb.allowed)));
    }
    out["readback"] = json!({
        "max_deviation": rb.max_deviation,
        "allowed": rb.allowed,
        "cut_length": rb.cut_length,
        "cut_minutes": rb.cut_minutes,
        "within": rb.within(),
    });
    if let Some(m) = machine {
        let mut issues = planning;
        issues.extend(check_program(program, m).map_err(|e| bad(cmd, e.to_string()))?);
        issues.extend(check_text(&gcode, m).map_err(|e| bad(cmd, e.to_string()))?);
        if let Some(origin) = machine_origin {
            let o = vec3(origin).ok_or_else(|| bad(cmd, "`machine_origin` must be [x, y, z] in machine coordinates (mm)"))?;
            issues.extend(check_travel(program, m, [o.x, o.y, o.z]).map_err(|e| bad(cmd, e.to_string()))?);
        }
        if let Some(stop) = issues.iter().find(|i| i.severity == Severity::Error) {
            return Err(bad(cmd, format!("not written for {}: {}", m.name, stopped(stop))));
        }
        out["machine"] = json!(m.name);
        out["issues"] = json!(issues);
    }
    out["extents"] = extents(program);
    if let Some(path) = path {
        solidcraft_io::write_atomic(std::path::Path::new(path), gcode.as_bytes(), false).map_err(|e| crate::EngineError::Other(e.to_string()))?;
        out["path"] = json!(path);
    }
    Ok(out)
}

/// The box all of a program's motion stays in, in work coordinates (mm): where the work zero
/// has to be for the cut to fit the plate and the table.
fn extents(program: &Program) -> Value {
    let (mut lo, mut hi) = ([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]);
    for op in &program.operations {
        for (pt, _) in op.path.points(0.05) {
            for (k, v) in [pt.x, pt.y, pt.z].into_iter().enumerate() {
                if let (Some(a), Some(b)) = (lo.get_mut(k), hi.get_mut(k)) {
                    *a = a.min(v);
                    *b = b.max(v);
                }
            }
        }
    }
    if lo.iter().chain(&hi).all(|v| v.is_finite()) { json!({"min": lo, "max": hi}) } else { Value::Null }
}

/// The newest sketch of the design, for a command that was not told which.
fn newest_sketch(s: &Session) -> Option<u64> {
    s.doc.features.iter().rev().find(|f| matches!(f.kind, FeatureKind::Sketch { .. }) && !f.suppressed).map(|f| f.id)
}

fn cut_out_cmd(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "cam.cut_out";
    let setup_name = str_(p, "setup").map(str::trim).filter(|n| !n.is_empty() && n.len() <= 128).unwrap_or("Router").to_string();
    let machine = machine_named(cmd, str_(p, "machine").unwrap_or(DEFAULT_MACHINE))?;
    let dry_run = bool_(p, "dry_run").unwrap_or(false);
    // What is cut: what the caller named, else the newest sketch.
    let mut source = if p.is_object() { p.clone() } else { json!({}) };
    if !["sketch", "faces", "outer", "regions"].iter().any(|k| p.get(*k).is_some()) {
        let id = newest_sketch(s).ok_or_else(|| bad(cmd, "draw the part's outline in a sketch first (or name `sketch`, `faces` or `outer`)"))?;
        source["sketch"] = json!(id);
    }
    let geometry = geometry_param(s, cmd, &source)?;
    let name = match str_(p, "name").map(str::trim).filter(|n| !n.is_empty() && n.len() <= 128) {
        Some(n) => n.to_string(),
        None => match &geometry {
            CamGeometry::Sketch { sketch, .. } => s.doc.feature(*sketch).map(|f| f.name.clone()).filter(|n| !n.is_empty() && n.len() <= 128),
            _ => None,
        }
        .unwrap_or_else(|| "Part".to_string()),
    };
    // The tool: a defined one by number, or one of the size given, defined when the design has none.
    let (tool, new_tool) = match (p.get("tool").filter(|v| !v.is_null()), length_param(s, cmd, p, "tool_diameter")?) {
        (Some(_), _) => {
            let n =
                num(p, "tool").filter(|n| *n >= 1.0 && n.fract() == 0.0).map(|n| n as u32).ok_or_else(|| bad(cmd, "`tool` must be a tool number"))?;
            (s.doc.cam.tool(n).cloned().ok_or_else(|| bad(cmd, format!("no tool {n} (give `tool_diameter` to define one)")))?, false)
        }
        (None, Some(d)) => {
            let flutes = match p.get("flutes") {
                None | Some(Value::Null) => 1,
                Some(_) => num(p, "flutes")
                    .filter(|n| *n >= 1.0 && *n <= 100.0 && n.fract() == 0.0)
                    .map(|n| n as u32)
                    .ok_or_else(|| bad(cmd, "`flutes` must be a whole number 1…100"))?,
            };
            let flute_length = length_param(s, cmd, p, "flute_length")?.unwrap_or(3.0 * d);
            let same = |a: f64, b: f64| (a - b).abs() <= 1e-9;
            let known = s
                .doc
                .cam
                .tools
                .iter()
                .find(|t| t.kind == ToolKind::FlatEndmill && same(t.diameter, d) && same(t.flute_length, flute_length) && t.flutes == flutes);
            match known {
                Some(t) => (t.clone(), false),
                None => {
                    let number = (1..=9999u32).find(|n| s.doc.cam.tool(*n).is_none()).ok_or_else(|| bad(cmd, "no tool number is free"))?;
                    if s.doc.cam.tools.len() >= MAX_TOOLS {
                        return Err(bad(cmd, format!("a design has at most {MAX_TOOLS} tools")));
                    }
                    let mut tool = Tool::new(number, ToolKind::FlatEndmill, d, flute_length);
                    tool.flutes = flutes;
                    tool.description = format!("{d} mm flat end mill, {flutes} flute{}", if flutes == 1 { "" } else { "s" });
                    tool.validate().map_err(|e| bad(cmd, e.to_string()))?;
                    (tool, true)
                }
            }
        }
        (None, None) => return Err(bad(cmd, "give `tool` (a defined tool's number) or `tool_diameter`")),
    };
    let (setup, new_setup) = match s.doc.cam.setup(&setup_name) {
        Some(setup) => (setup.clone(), false),
        None => {
            if s.doc.cam.setups.len() >= MAX_SETUPS {
                return Err(bad(cmd, format!("a design has at most {MAX_SETUPS} setups")));
            }
            let setup = CamSetup::new(setup_name.clone());
            setup.validate().map_err(|e| bad(cmd, e.to_string()))?;
            (setup, true)
        }
    };
    if setup.operations.len() >= MAX_OPS && !setup.operations.iter().any(|o| o.name() == name) {
        return Err(bad(cmd, format!("a setup has at most {MAX_OPS} operations")));
    }
    let material: Material = word(cmd, p, "material", "aluminum, aluminum_soft, brass, acrylic, soft_plastic, hardwood or softwood")?
        .ok_or_else(|| bad(cmd, "`material` is required: aluminum, aluminum_soft, brass, acrylic, soft_plastic, hardwood or softwood"))?;
    let op = CamOperation::Part {
        name: name.clone(),
        tool: tool.number,
        geometry,
        material,
        thickness: req_expr(cmd, p, "thickness")?,
        tier: word(cmd, p, "tier", "careful, standard or bold")?.unwrap_or_default(),
        top: expr(p, "top").unwrap_or_else(|| Expr::new("0 mm")),
        finish_stock: expr(p, "finish_stock").unwrap_or_else(|| Expr::new("0.2 mm")),
        break_through: expr(p, "break_through").unwrap_or_else(|| Expr::new("0.2 mm")),
        coolant: bool_(p, "coolant").unwrap_or(true),
        hold: word(cmd, p, "hold", "tabs or none")?.unwrap_or_default(),
    };
    op.validate().map_err(|e| bad(cmd, e.to_string()))?;
    // Everything is planned, written and checked before the design is touched.
    let plan = plan_op(s, &setup, &op, &tool, &machine).map_err(|e| bad(cmd, e))?;
    let cuts: Vec<Value> = plan.operations.iter().map(|o| json!({"name": o.name, "rpm": o.spindle_rpm, "feed": top_feed(&o.path)})).collect();
    let program = Program { name: name.clone(), operations: plan.operations, work_offset: Some(setup.work_offset) };
    let post = machine.post().map_err(|e| bad(cmd, e.to_string()))?;
    let path = if dry_run { None } else { path_param(cmd, p)? };
    let mut out = write_program(cmd, &program, &post, Some(&machine), plan.issues, None, path)?;
    out["setup"] = json!(setup_name);
    out["operation"] = json!(name);
    out["tool"] = json!(tool);
    out["cuts"] = json!(cuts);
    if dry_run {
        out["dry_run"] = json!(true);
        return Ok(out);
    }
    let doc = s.doc_mut();
    if new_tool {
        doc.cam.tools.push(tool);
        doc.cam.tools.sort_by_key(|t| t.number);
    }
    if new_setup {
        doc.cam.setups.push(setup);
    }
    store_op(s, cmd, &setup_name, op)?;
    Ok(out)
}

/// The fastest a path cuts (mm/min).
fn top_feed(path: &solidcraft_cam::Toolpath) -> f64 {
    path.moves
        .iter()
        .filter_map(|m| match m {
            Move::Feed { feed, .. } | Move::Arc { feed, .. } | Move::PlaneArc { feed, .. } => Some(*feed),
            Move::Rapid { .. } | Move::Dwell { .. } => None,
        })
        .fold(0.0, f64::max)
}

/// A length parameter: a number in mm, or an expression over the design's parameters and units.
fn length_param(s: &Session, cmd: &str, p: &Value, k: &str) -> Result<Option<f64>> {
    match p.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(_)) => num(p, k).map(Some).ok_or_else(|| bad(cmd, format!("`{k}` must be a length"))),
        Some(_) => {
            let e = req_expr(cmd, p, k)?;
            eval_len(s, &e, k).map(Some).map_err(|e| bad(cmd, e))
        }
    }
}

fn machine_named(cmd: &str, name: &str) -> Result<MachineProfile> {
    MachineProfile::builtin(name).ok_or_else(|| bad(cmd, format!("unknown machine `{name}` (built-in: {})", MachineProfile::BUILTIN.join(", "))))
}

/// A word of a closed list, read the way the design file writes it (`snake_case`).
fn word<T: serde::de::DeserializeOwned>(cmd: &str, p: &Value, k: &str, choices: &str) -> Result<Option<T>> {
    match p.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => serde_json::from_value(v.clone()).map(Some).map_err(|_| bad(cmd, format!("`{k}` must be one of {choices}"))),
    }
}

fn feeds_cmd(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "cam.feeds";
    let machine = machine_named(cmd, str_(p, "machine").unwrap_or("flexihal-router"))?;
    let materials = "aluminum, aluminum_soft, brass, acrylic, soft_plastic, hardwood, softwood";
    let material: Material = word(cmd, p, "material", materials)?.ok_or_else(|| bad(cmd, format!("`material` is required ({materials})")))?;
    let kinds = "slot, pocket, adaptive, finish, bore";
    let kind: CutKind = word(cmd, p, "kind", kinds)?.ok_or_else(|| bad(cmd, format!("`kind` is required ({kinds})")))?;
    let tier: Tier = word(cmd, p, "tier", "careful, standard, bold")?.unwrap_or_default();
    let apply = bool_(p, "apply").unwrap_or(false);
    let number = match p.get("tool") {
        None | Some(Value::Null) => None,
        Some(_) => Some(
            num(p, "tool")
                .filter(|n| *n >= 1.0 && *n <= 9999.0 && n.fract() == 0.0)
                .map(|n| n as u32)
                .ok_or_else(|| bad(cmd, "`tool` must be a tool number 1…9999"))?,
        ),
    };
    let mut request = match number {
        Some(n) => {
            let tool = s.doc.cam.tool(n).ok_or_else(|| bad(cmd, format!("no tool {n}")))?;
            let mut r = FeedRequest::new(material, tool.diameter, tool.flutes, kind);
            r.flute_length = Some(tool.flute_length);
            r
        }
        None => {
            let diameter = num(p, "diameter").ok_or_else(|| bad(cmd, "give `tool` (a defined tool's number) or `diameter` (mm)"))?;
            let flutes = match p.get("flutes") {
                None | Some(Value::Null) => 1,
                Some(_) => num(p, "flutes")
                    .filter(|n| *n >= 1.0 && *n <= 12.0 && n.fract() == 0.0)
                    .map(|n| n as u32)
                    .ok_or_else(|| bad(cmd, "`flutes` must be 1…12"))?,
            };
            FeedRequest::new(material, diameter, flutes, kind)
        }
    };
    request.tier = tier;
    request.rpm = num(p, "rpm");
    request.width = num(p, "width");
    request.depth = num(p, "depth");
    let found = recipe(&request, &machine).map_err(|e| bad(cmd, e.to_string()))?;
    let mut out = json!({"recipe": found, "machine": machine.name});
    if apply {
        let n = number.ok_or_else(|| bad(cmd, "`apply` needs `tool`: the recipe is written into a defined tool"))?;
        let doc = s.doc_mut();
        let tool = doc.cam.tools.iter_mut().find(|t| t.number == n).ok_or_else(|| bad(cmd, format!("no tool {n}")))?;
        found.apply(tool);
        tool.validate().map_err(|e| bad(cmd, e.to_string()))?;
        out["tool"] = json!(tool);
    }
    Ok(out)
}

fn resolve_post(cam: &CamSettings, name: &str) -> std::result::Result<Post, String> {
    if let Some(p) = cam.post(name) {
        return Ok(p.clone());
    }
    Post::builtin(name).ok_or_else(|| format!("unknown post `{name}` (built-in: {}; or save one on the design)", Post::BUILTIN.join(", ")))
}

struct WorkFrame {
    o: Vec3,
    x: Vec3,
    y: Vec3,
    z: Vec3,
}

impl WorkFrame {
    fn local(&self, p: Vec3) -> Vec3 {
        let d = p - self.o;
        Vec3::new(d.dot(self.x), d.dot(self.y), d.dot(self.z))
    }
}

/// Work X and Y for a tool axis. Along a world axis they are world axes too.
fn axes(axis: Vec3) -> Option<(Vec3, Vec3, Vec3)> {
    let z = axis.normalized().filter(|z| z.is_finite())?;
    let x = if z.z.abs() > 1.0 - 1e-9 || z.y.abs() > 1.0 - 1e-9 {
        Vec3::new(1.0, 0.0, 0.0)
    } else if z.x.abs() > 1.0 - 1e-9 {
        Vec3::new(0.0, z.x.signum(), 0.0)
    } else {
        Vec3::new(0.0, 0.0, 1.0).cross(z).normalized()?
    };
    Some((x, z.cross(x), z))
}

fn work_frame(setup: &CamSetup) -> std::result::Result<WorkFrame, String> {
    let z = setup.tool_axis_vec().normalized().filter(|z| z.is_finite()).ok_or("the setup's tool axis is not a direction")?;
    let x = match setup.x_axis {
        Some(a) => {
            let x = Vec3::new(a[0], a[1], a[2]).normalized().filter(|x| x.is_finite()).ok_or("the setup's X axis is not a direction")?;
            if x.dot(z).abs() > 1e-6 {
                return Err("the setup's X axis must be square to the tool axis".into());
            }
            x
        }
        None => axes(z).ok_or("the setup's tool axis is not a direction")?.0,
    };
    let y = z.cross(x);
    let y = y.normalized().filter(|y| y.is_finite()).ok_or("the setup's axes do not form a frame")?;
    Ok(WorkFrame { o: setup.work_origin_vec(), x, y, z })
}

/// Sketch-plane regions in a setup's XY. The sketch plane must face along the tool axis.
fn to_setup(plane: &Plane, frame: &WorkFrame, regions: &[Region2]) -> std::result::Result<Vec<Region2>, String> {
    let facing = plane.normal().dot(frame.z);
    if !(facing.abs() > 1.0 - 1e-9) {
        return Err("not supported yet: operations on a sketch or face that is not square to the setup's tool axis".into());
    }
    let mirrored = facing < 0.0;
    let x_axis = plane.dir_to_world(Vec2::new(1.0, 0.0));
    let turn = x_axis.dot(frame.y).atan2(x_axis.dot(frame.x));
    let point = |p: Vec2| {
        let w = frame.local(plane.to_world(p));
        Vec2::new(w.x, w.y)
    };
    let seg = |s: &Seg2| match *s {
        Seg2::Line { a, b } => Seg2::Line { a: point(a), b: point(b) },
        Seg2::Arc { center, radius, start, sweep } => Seg2::Arc {
            center: point(center),
            radius,
            start: if mirrored { turn - start } else { turn + start },
            sweep: if mirrored { -sweep } else { sweep },
        },
        Seg2::Cubic { p0, p1, p2, p3 } => Seg2::Cubic { p0: point(p0), p1: point(p1), p2: point(p2), p3: point(p3) },
        Seg2::Conic { a, apex, b, w } => Seg2::Conic { a: point(a), apex: point(apex), b: point(b), w },
        Seg2::Bezier { n, p } => Seg2::Bezier { n, p: p.map(point) },
    };
    let lp = |l: &Loop2| Loop2 { segs: l.segs.iter().map(seg).collect() };
    Ok(regions.iter().map(|r| Region2 { outer: lp(&r.outer), holes: r.holes.iter().map(lp).collect() }).collect())
}

fn outline(geom: &[LinkGeom]) -> std::result::Result<Region2, String> {
    let mut loops: Vec<Loop2> = Vec::new();
    let mut open: Vec<Seg2> = Vec::new();
    for g in geom {
        match *g {
            LinkGeom::Point(_) => {}
            LinkGeom::Line(a, b) => open.push(Seg2::Line { a, b }),
            LinkGeom::Circle(c, r) => loops.push(Loop2::circle(c, r)),
            LinkGeom::Arc { c, a, b } => {
                let (start, end) = ((a - c).angle(), (b - c).angle());
                let sweep = (end - start).rem_euclid(std::f64::consts::TAU);
                open.push(Seg2::Arc { center: c, radius: a.dist(c), start, sweep: if sweep > 1e-12 { sweep } else { std::f64::consts::TAU } });
            }
            LinkGeom::Conic { a, apex, b, rho } if rho > 1e-6 && rho < 1.0 - 1e-6 => open.push(Seg2::Conic { a, apex, b, w: rho / (1.0 - rho) }),
            _ => return Err("not supported yet: faces with elliptical or spline edges".into()),
        }
    }
    if open.len() > 20_000 {
        return Err("the face has too many edges".into());
    }
    const JOIN: f64 = 1e-3;
    let mut used = vec![false; open.len()];
    for first in 0..open.len() {
        if used.get(first).copied().unwrap_or(true) {
            continue;
        }
        let Some(seed) = open.get(first).copied() else { continue };
        if let Some(flag) = used.get_mut(first) {
            *flag = true;
        }
        let mut chain = vec![seed];
        let origin = seed.start();
        let mut end = seed.end();
        while end.dist(origin) > JOIN {
            let next = open.iter().zip(&used).enumerate().filter(|(_, (_, taken))| !**taken).find_map(|(i, (seg, _))| {
                if seg.start().dist(end) <= JOIN {
                    Some((i, *seg))
                } else if seg.end().dist(end) <= JOIN {
                    Some((i, seg.reversed()))
                } else {
                    None
                }
            });
            let Some((i, seg)) = next else { return Err("the face's edges do not form closed loops".into()) };
            if let Some(flag) = used.get_mut(i) {
                *flag = true;
            }
            end = seg.end();
            chain.push(seg);
        }
        loops.push(Loop2 { segs: chain });
    }
    let outer = loops
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.signed_area().abs().total_cmp(&b.1.signed_area().abs()))
        .map(|(i, _)| i)
        .ok_or("the face has no outline")?;
    let outer = loops.remove(outer);
    Ok(Region2 { outer, holes: loops })
}

fn resolve_geometry(s: &Session, setup: &CamSetup, source: &CamGeometry) -> std::result::Result<Vec<Region2>, String> {
    let frame = work_frame(setup)?;
    let state = s.model.state();
    match source {
        CamGeometry::Polygons { regions } => regions.iter().map(|r| r.to_region().map_err(|e| e.to_string())).collect(),
        CamGeometry::Sketch { sketch, profiles } => {
            let sketch = state.sketch(*sketch).ok_or("its sketch is not built (deleted, rolled back, suppressed or failed)")?;
            let regions = select_profiles(sketch, profiles).map_err(|e| e.to_string())?;
            to_setup(&sketch.plane, &frame, &regions)
        }
        CamGeometry::Faces { faces } => {
            let mut regions = Vec::new();
            for face in faces {
                let body = state.body(&face.body).ok_or_else(|| format!("body `{}` is gone", face.body))?;
                let (index, ..) = solidcraft_doc::naming::face_on_body(body, &face.name, face.at_vec())
                    .ok_or_else(|| format!("its face on `{}` is gone", face.body))?;
                let plane = body.body.planar_face(index).ok_or("a picked face is not planar")?;
                let resolved = resolve_face_indices(&state, &plane, LinkKind::Project, &face.body, &[index]).map_err(|e| e.to_string())?;
                let region = outline(&resolved.geom)?;
                regions.extend(to_setup(&plane, &frame, &[region])?);
            }
            Ok(regions)
        }
    }
}

fn eval_len(s: &Session, e: &Expr, key: &str) -> std::result::Result<f64, String> {
    s.doc.eval(e.as_str(), Kind::Length).map_err(|err| format!("{key}: {err}"))
}

fn eval_ang(s: &Session, e: &Expr, key: &str) -> std::result::Result<f64, String> {
    s.doc.eval(e.as_str(), Kind::Angle).map_err(|err| format!("{key}: {err}"))
}

fn engine_cut(s: &Session, cut: &CamCut) -> std::result::Result<Cut, String> {
    let heights = Heights {
        clearance: eval_len(s, &cut.clearance, "clearance")?,
        retract: eval_len(s, &cut.retract, "retract")?,
        top: eval_len(s, &cut.top, "top")?,
        bottom: eval_len(s, &cut.bottom, "bottom")?,
    };
    let feeds = Feeds { cut: eval_len(s, &cut.feed, "feed")?, plunge: eval_len(s, &cut.plunge_feed, "plunge_feed")? };
    let stepdown = eval_len(s, &cut.stepdown, "stepdown")?;
    heights.validate().map_err(|e| e.to_string())?;
    feeds.validate().map_err(|e| e.to_string())?;
    if !(stepdown > 0.0) || !stepdown.is_finite() {
        return Err("stepdown must be above 0".into());
    }
    Ok(Cut { heights, feeds, stepdown })
}

fn engine_contour(s: &Session, p: &CamContourParams) -> std::result::Result<ContourParams, String> {
    Ok(ContourParams {
        side: match p.side {
            CamSide::Outside => Side::Outside,
            CamSide::Inside => Side::Inside,
            CamSide::On => Side::On,
        },
        climb: p.climb,
        stock_to_leave: eval_len(s, &p.stock_to_leave, "stock_to_leave")?,
        tolerance: eval_len(s, &p.tolerance, "tolerance")?,
        ramp_angle: eval_ang(s, &p.ramp_angle, "ramp_angle")?,
    })
}

fn engine_pocket(s: &Session, p: &CamPocketParams) -> std::result::Result<PocketParams, String> {
    Ok(PocketParams {
        stepover: eval_len(s, &p.stepover, "stepover")?,
        climb: p.climb,
        stock_to_leave: eval_len(s, &p.stock_to_leave, "stock_to_leave")?,
        tolerance: eval_len(s, &p.tolerance, "tolerance")?,
        ramp_angle: eval_ang(s, &p.ramp_angle, "ramp_angle")?,
    })
}

fn build_path(s: &Session, setup: &CamSetup, op: &CamOperation) -> std::result::Result<solidcraft_cam::Toolpath, String> {
    let regions = resolve_geometry(s, setup, op.geometry())?;
    let cut = engine_cut(s, op.cut().ok_or("a part operation is planned for a machine, not cut as one path")?)?;
    let tool = s.doc.cam.tool(op.tool()).ok_or_else(|| format!("there is no tool {}", op.tool()))?;
    match op {
        CamOperation::Part { .. } => Err("a part operation is planned for a machine, not cut as one path".into()),
        CamOperation::Contour { params, .. } => {
            let params = engine_contour(s, params)?;
            contour(&regions, tool, &params, &cut).map_err(|e| e.to_string())
        }
        CamOperation::Pocket { params, .. } => {
            let params = engine_pocket(s, params)?;
            pocket(&regions, tool, &params, &cut).map_err(|e| e.to_string())
        }
    }
}

fn list_cmd(s: &mut Session, _p: &Value) -> Result<Value> {
    Ok(json!({
        "tools": s.doc.cam.tools,
        "setups": s.doc.cam.setups.iter().map(setup_json).collect::<Vec<_>>(),
        "posts": s.doc.cam.posts.iter().map(|p| p.name.clone()).collect::<Vec<_>>(),
        "builtin_posts": Post::BUILTIN,
        "builtin_machines": MachineProfile::BUILTIN,
    }))
}

fn setup_json(setup: &CamSetup) -> Value {
    json!({
        "name": setup.name,
        "work_offset": setup.work_offset,
        "tool_axis": setup.tool_axis,
        "x_axis": setup.x_axis,
        "work_origin": setup.work_origin,
        "stock": setup.stock,
        "stock_spec": setup.stock_spec,
        "wcs": setup.wcs,
        "model_bodies": setup.model_bodies,
        "operations": setup.operations.iter().map(|op| match op {
            CamOperation::Contour { name, tool, geometry, params, cut, spindle_rpm, coolant } => json!({
                "type": "contour", "name": name, "tool": tool, "geometry": geometry,
                "params": params, "cut": cut, "spindle_rpm": spindle_rpm, "coolant": coolant,
            }),
            CamOperation::Pocket { name, tool, geometry, params, cut, spindle_rpm, coolant } => json!({
                "type": "pocket", "name": name, "tool": tool, "geometry": geometry,
                "params": params, "cut": cut, "spindle_rpm": spindle_rpm, "coolant": coolant,
            }),
            CamOperation::Part { name, tool, geometry, material, thickness, tier, top, finish_stock, break_through, coolant, hold } => json!({
                "type": "part", "name": name, "tool": tool, "geometry": geometry, "material": material, "thickness": thickness,
                "tier": tier, "top": top, "finish_stock": finish_stock, "break_through": break_through, "coolant": coolant, "hold": hold,
            }),
        }).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn run(s: &mut Session, id: &str, p: Value) -> Value {
        match s.execute(id, &p) {
            Ok(v) => v,
            Err(e) => panic!("{id} {p}: {e}"),
        }
    }

    #[test]
    fn contour_and_pocket_post_to_verified_gcode() {
        let mut s = Session::default();
        run(
            &mut s,
            "cam.tool.define",
            json!({"number": 1, "diameter": 6, "flute_length": 20, "spindle_rpm": 8000, "feed": 600, "plunge_feed": 200, "stepdown": 3}),
        );
        run(&mut s, "cam.setup", json!({"name": "Top", "work_offset": 1, "stock": {"min": [0, 0, -10], "max": [40, 30, 0]}}));
        run(
            &mut s,
            "cam.op.contour",
            json!({
                "setup": "Top", "name": "Outside", "tool": 1,
                "outer": [[0,0],[40,0],[40,30],[0,30]],
                "side": "outside", "bottom": -6, "stepdown": 3,
            }),
        );
        run(
            &mut s,
            "cam.op.pocket",
            json!({
                "setup": "Top", "name": "Pocket", "tool": 1,
                "outer": [[5,5],[35,5],[35,25],[5,25]],
                "stepover": 2.5, "bottom": -3, "stepdown": 3,
            }),
        );
        let posted = run(&mut s, "cam.post", json!({"setup": "Top"}));
        let gcode = posted["gcode"].as_str().unwrap();
        assert!(gcode.contains("G21"));
        assert!(gcode.contains("G54"));
        assert!(gcode.contains("M30"));
        assert!(posted["readback"]["within"].as_bool().unwrap());
        assert_eq!(posted["operations"], 2);

        let cold = Session::new(solidcraft_doc::Document::from_json(&s.doc.to_json()).unwrap());
        assert_eq!(cold.doc.cam.tools.len(), 1);
        assert_eq!(cold.doc.cam.setups[0].operations.len(), 2);
        assert!(cold.doc.format_pending.iter().any(|n| n == "cam-milling"));
        let again = run(&mut Session::new(cold.doc.as_ref().clone()), "cam.post", json!({"setup": "Top"}));
        assert!(again["readback"]["within"].as_bool().unwrap());

        let forced = run(&mut Session::new(s.doc.as_ref().clone()), "cam.post", json!({"setup": "Top", "verify": false}));
        assert!(forced["readback"]["within"].as_bool().unwrap());
    }

    #[test]
    fn a_machine_supplies_feeds_its_post_and_its_checks() {
        let mut s = Session::default();
        run(&mut s, "cam.tool.define", json!({"number": 1, "diameter": 6, "flute_length": 17, "flutes": 1}));
        // A recipe without touching the design, then one written into the tool.
        let asked = run(&mut s, "cam.feeds", json!({"material": "aluminum", "kind": "adaptive", "diameter": 6}));
        assert_eq!((asked["recipe"]["rpm"].as_f64(), asked["recipe"]["feed"].as_f64()), (Some(18500.0), Some(1730.0)));
        assert_eq!(asked["machine"], "flexihal-router");
        assert_eq!(s.doc.cam.tools[0].feed, None);
        let slot = run(&mut s, "cam.feeds", json!({"material": "aluminum", "kind": "slot", "tool": 1, "apply": true}));
        assert_eq!(slot["tool"]["feed"].as_f64(), Some(1110.0));
        let tool = &s.doc.cam.tools[0];
        assert_eq!((tool.spindle_rpm, tool.feed, tool.plunge_feed, tool.stepdown), (Some(18500.0), Some(1110.0), Some(560.0), Some(1.5)));

        run(&mut s, "cam.setup", json!({"name": "Top"}));
        run(
            &mut s,
            "cam.op.contour",
            json!({
                "setup": "Top", "name": "Outside", "tool": 1, "outer": [[0,0],[60,0],[60,40],[0,40]],
                "side": "outside", "bottom": -6.2, "ramp_angle": 3,
            }),
        );
        let posted = run(&mut s, "cam.post", json!({"setup": "Top", "machine": "flexihal-router", "machine_origin": [-700, -450, -60]}));
        let gcode = posted["gcode"].as_str().unwrap();
        let lines: Vec<&str> = gcode.lines().collect();
        assert_eq!(&lines[1..4], ["G90 G94 G17 G21", "G54", "G53 G0 Z-5"], "{gcode}");
        let on = lines.iter().position(|l| *l == "S18500 M3").unwrap();
        assert_eq!(lines[on + 1], "G4 P8");
        assert_eq!(&lines[lines.len() - 2..], ["G53 G0 Z-5", "M30"]);
        assert!(!gcode.contains("G43") && !gcode.contains("G49"));
        assert_eq!(
            (posted["post"].as_str(), posted["extension"].as_str(), posted["machine"].as_str()),
            (Some("grblhal"), Some("nc"), Some("flexihal-router"))
        );
        assert!(posted["readback"]["within"].as_bool().unwrap());
        assert_eq!(posted["issues"], json!([]));
        // The machine with another post still gets the machine's checks.
        let other = run(&mut s, "cam.post", json!({"setup": "Top", "machine": "flexihal-router", "post": "grblhal"}));
        assert!(!other["gcode"].as_str().unwrap().contains("G53"));

        // What would stop the program is an error, with the operation it is in.
        run(&mut s, "cam.tool.define", json!({"number": 2, "diameter": 6, "flute_length": 17, "spindle_rpm": 8000, "feed": 600}));
        run(&mut s, "cam.op.contour", json!({"setup": "Top", "name": "Slow", "tool": 2, "outer": [[0,0],[60,0],[60,40],[0,40]], "bottom": -1}));
        let refused = s.execute("cam.post", &json!({"setup": "Top", "machine": "flexihal-router"})).unwrap_err().to_string();
        assert!(refused.contains("Slow") && refused.contains("8000 rpm is outside the spindle"), "{refused}");
        // Without a machine the same program is written as before.
        assert!(run(&mut s, "cam.post", json!({"setup": "Top"}))["issues"].is_null());

        for (params, why) in [
            (json!({"material": "aluminum", "kind": "slot", "diameter": 6, "apply": true}), "`apply` needs `tool`"),
            (json!({"material": "unobtainium", "kind": "slot", "diameter": 6}), "`material` must be one of"),
            (json!({"material": "aluminum", "kind": "slot"}), "give `tool`"),
            (json!({"material": "aluminum", "kind": "slot", "tool": 9}), "no tool 9"),
            (json!({"material": "aluminum", "kind": "slot", "diameter": 6, "machine": "lathe"}), "unknown machine `lathe`"),
            (json!({"material": "aluminum", "kind": "slot", "diameter": -6}), "diameter"),
        ] {
            let e = s.execute("cam.feeds", &params).unwrap_err().to_string();
            assert!(e.contains(why), "{params}: {e}");
        }
        assert!(s.execute("cam.post", &json!({"setup": "Top", "machine": "lathe"})).is_err());
        assert!(run(&mut s, "cam.list", json!({}))["builtin_machines"].as_array().is_some_and(|m| m.contains(&json!("flexihal-router"))));
    }

    #[test]
    fn a_drawn_plate_is_cut_out_in_one_operation_that_follows_the_design() {
        let mut s = Session::default();
        run(&mut s, "parameters.add", json!({"name": "plate", "expression": "6 mm"}));
        run(&mut s, "sketch.create", json!({"plane": "XY"}));
        run(&mut s, "sketch.rectangle.two_point", json!({"p0": [0, 0], "p1": [60, 40]}));
        run(&mut s, "sketch.circle.center", json!({"center": [30, 20], "radius": 10}));
        let sketch = s.active_sketch.expect("sketch");
        run(&mut s, "sketch.finish", json!({}));
        run(&mut s, "cam.tool.define", json!({"number": 1, "diameter": 6, "flute_length": 17, "flutes": 1}));
        run(&mut s, "cam.setup", json!({"name": "Top"}));
        let planned = run(
            &mut s,
            "cam.op.part",
            json!({"setup": "Top", "name": "Plate", "tool": 1, "sketch": sketch, "material": "aluminum", "thickness": "plate"}),
        );
        // The drawn circle is the plate's opening, not a second part.
        assert_eq!(planned["cuts"], json!(["Plate: Opening 1 rough", "Plate: Opening 1 finish", "Plate: Outside rough", "Plate: Outside finish"]));
        // Tabs hold it, so the plan has nothing to warn of.
        assert_eq!(planned["issues"], json!([]));
        let listed = run(&mut s, "cam.list", json!({}));
        assert_eq!((&listed["setups"][0]["operations"][0]["type"], &listed["setups"][0]["operations"][0]["hold"]), (&json!("part"), &json!("tabs")));

        // Posting needs no machine named: a part is written for the router, with its checks.
        let posted = run(&mut s, "cam.post", json!({"setup": "Top"}));
        let gcode = posted["gcode"].as_str().unwrap();
        assert_eq!(
            (posted["machine"].as_str(), posted["post"].as_str(), posted["operations"].as_u64()),
            (Some("flexihal-router"), Some("grblhal"), Some(4))
        );
        assert!(posted["readback"]["within"].as_bool().unwrap());
        assert!(gcode.contains("Z-6.2") && gcode.contains("G53 G0 Z-5") && (gcode.contains("G2 ") || gcode.contains("G3 ")), "{gcode}");
        assert!(posted["issues"].as_array().unwrap().iter().all(|i| i["severity"] == "warning"), "{}", posted["issues"]);
        // The tabs are 1 mm of the plate left at the bottom of the outside.
        assert!(gcode.contains("Z-5.2"), "{gcode}");

        // Without tabs the same operation says the part comes loose.
        let loose = run(
            &mut s,
            "cam.op.part",
            json!({"setup": "Top", "name": "Plate", "tool": 1, "sketch": sketch, "material": "aluminum", "thickness": "plate", "hold": "none"}),
        );
        let issues = loose["issues"].as_array().unwrap();
        assert_eq!(issues.len(), 1, "{issues:?}");
        assert_eq!((issues[0]["severity"].as_str(), issues[0]["at"].as_str()), (Some("warning"), Some("Plate: Outside")));
        assert_eq!(s.doc.cam.setups[0].operations.len(), 1);

        // A thicker plate is the same operation, cut deeper.
        run(&mut s, "parameters.change", json!({"name": "plate", "expression": "10 mm"}));
        let deeper = run(&mut s, "cam.post", json!({"setup": "Top"}));
        let text = deeper["gcode"].as_str().unwrap();
        assert!(text.contains("Z-10.2") && !text.contains("Z-6.2"), "{text}");

        // A plate the flutes cannot get through is refused, with why and where.
        run(&mut s, "parameters.change", json!({"name": "plate", "expression": "25 mm"}));
        let refused = s.execute("cam.post", &json!({"setup": "Top"})).unwrap_err().to_string();
        assert!(refused.contains("Plate") && refused.contains("longer cutter"), "{refused}");

        for (params, why) in [
            (json!({"setup": "Top", "name": "P", "tool": 1, "sketch": sketch, "thickness": 6}), "`material` is required"),
            (json!({"setup": "Top", "name": "P", "tool": 1, "sketch": sketch, "material": "cheese", "thickness": 6}), "`material` must be one of"),
            (json!({"setup": "Top", "name": "P", "tool": 1, "sketch": sketch, "material": "aluminum"}), "`thickness`"),
            (json!({"setup": "Top", "name": "P", "tool": 1, "sketch": sketch, "material": "aluminum", "thickness": -6}), "thickness"),
            (json!({"setup": "Top", "name": "P", "tool": 7, "sketch": sketch, "material": "aluminum", "thickness": 6}), "no tool 7"),
            (json!({"setup": "Nope", "name": "P", "tool": 1, "sketch": sketch, "material": "aluminum", "thickness": 6}), "no setup `Nope`"),
            (
                json!({"setup": "Top", "name": "P", "tool": 1, "sketch": sketch, "material": "aluminum", "thickness": 6, "machine": "lathe"}),
                "unknown machine",
            ),
            (
                json!({"setup": "Top", "name": "P", "tool": 1, "sketch": sketch, "material": "aluminum", "thickness": 6, "hold": "glue"}),
                "`hold` must be one of tabs or none",
            ),
        ] {
            let e = s.execute("cam.op.part", &params).unwrap_err().to_string();
            assert!(e.contains(why), "{params}: {e}");
        }
        assert_eq!(s.doc.cam.setups[0].operations.len(), 1);
    }

    #[test]
    fn a_sketch_goes_to_a_checked_gcode_file_in_one_step() {
        let mut s = Session::default();
        let ask = json!({"material": "aluminum", "thickness": 6, "tool_diameter": 6});
        let nothing = s.execute("cam.cut_out", &ask).unwrap_err().to_string();
        assert!(nothing.contains("draw the part's outline in a sketch first"), "{nothing}");
        run(&mut s, "sketch.create", json!({"plane": "XY"}));
        run(&mut s, "sketch.rectangle.two_point", json!({"p0": [0, 0], "p1": [60, 40]}));
        run(&mut s, "sketch.circle.center", json!({"center": [30, 20], "radius": 10}));
        let sketch = s.active_sketch.expect("sketch");
        run(&mut s, "sketch.finish", json!({}));
        let sketch_name = s.doc.feature(sketch).unwrap().name.clone();

        let dir = std::env::temp_dir().join(format!("solidcraft-cam-cut-out-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("plate.nc");
        let mut with_path = ask.clone();
        with_path["path"] = json!(path.to_string_lossy());
        let out = run(&mut s, "cam.cut_out", with_path);
        assert_eq!(
            (out["setup"].as_str(), out["operation"].as_str(), out["machine"].as_str()),
            (Some("Router"), Some(sketch_name.as_str()), Some("flexihal-router"))
        );
        assert_eq!(out["cuts"].as_array().map(Vec::len), Some(4), "{}", out["cuts"]);
        assert_eq!(
            (out["cuts"][2]["name"].as_str(), out["cuts"][2]["rpm"].as_f64(), out["cuts"][2]["feed"].as_f64()),
            (Some(format!("{sketch_name}: Outside rough").as_str()), Some(18500.0), Some(1110.0))
        );
        assert_eq!(
            (out["tool"]["number"].as_u64(), out["tool"]["flutes"].as_u64(), out["tool"]["flute_length"].as_f64()),
            (Some(1), Some(1), Some(18.0))
        );
        assert!(out["readback"]["within"].as_bool().unwrap());
        // The roughing pass round the outside is the widest motion: 3.2 mm off the plate, 0.2 mm through it.
        let (lo, hi) = (&out["extents"]["min"], &out["extents"]["max"]);
        for (got, want) in [(&lo[0], -3.2), (&lo[1], -3.2), (&lo[2], -6.2), (&hi[0], 63.2), (&hi[1], 43.2), (&hi[2], 5.0)] {
            assert!((got.as_f64().unwrap() - want).abs() < 0.01, "{got} is not {want}: {}", out["extents"]);
        }
        let written = std::fs::read_to_string(&path).unwrap();
        assert_eq!(Some(written.as_str()), out["gcode"].as_str());
        assert!(written.contains("G53 G0 Z-5") && written.trim_end().ends_with("M30"), "{written}");

        // A dry run plans and checks the same program and keeps nothing.
        let kept = s.doc.clone();
        let mut dry = ask.clone();
        dry["dry_run"] = json!(true);
        dry["thickness"] = json!(10);
        dry["tool_diameter"] = json!("8 mm");
        dry["path"] = json!(dir.join("never.nc").to_string_lossy());
        let tried = run(&mut s, "cam.cut_out", dry);
        assert_eq!(
            (tried["dry_run"].as_bool(), tried["tool"]["number"].as_u64(), tried["tool"]["diameter"].as_f64()),
            (Some(true), Some(2), Some(8.0))
        );
        assert!(tried["path"].is_null() && !dir.join("never.nc").exists());
        assert!(std::sync::Arc::ptr_eq(&s.doc, &kept), "a dry run leaves the design alone, with no undo step");

        // Asked again, the same tool, setup and operation are used, not added to.
        let mut thinner = ask.clone();
        thinner["thickness"] = json!(3);
        let again = run(&mut s, "cam.cut_out", thinner);
        assert!(again["path"].is_null());
        assert!((again["extents"]["min"][2].as_f64().unwrap() + 3.2).abs() < 0.01);
        assert_eq!((s.doc.cam.tools.len(), s.doc.cam.setups.len(), s.doc.cam.setups[0].operations.len()), (1, 1, 1));
        // Another cutter is another tool; a defined one is used by its number.
        let small =
            run(&mut s, "cam.cut_out", json!({"material": "acrylic", "thickness": 3, "tool_diameter": 3.175, "flute_length": 12, "name": "Small"}));
        assert_eq!((small["tool"]["number"].as_u64(), small["operation"].as_str()), (Some(2), Some("Small")));
        let by_number = run(&mut s, "cam.cut_out", json!({"material": "aluminum", "thickness": 6, "tool": 1, "sketch": sketch}));
        assert_eq!(by_number["tool"]["number"].as_u64(), Some(1));
        assert_eq!((s.doc.cam.tools.len(), s.doc.cam.setups[0].operations.len()), (2, 2));

        // One step to undo, and a refusal leaves the design as it was.
        let before = s.doc.cam.clone();
        for (params, why) in [
            (json!({"material": "aluminum", "thickness": 6}), "give `tool`"),
            (json!({"material": "aluminum", "thickness": 6, "tool": 9}), "no tool 9"),
            (json!({"material": "aluminum", "thickness": 6, "tool_diameter": 6, "flutes": 0}), "`flutes`"),
            (json!({"material": "aluminum", "thickness": 6, "tool_diameter": -6}), "diameter"),
            (json!({"material": "aluminum", "thickness": 60, "tool_diameter": 8, "setup": "Other"}), "longer cutter"),
            (json!({"material": "aluminum", "thickness": 6, "tool_diameter": 6, "machine": "lathe"}), "unknown machine"),
            (json!({"material": "aluminum", "thickness": 6, "tool_diameter": 6, "path": ""}), "`path`"),
            (json!({"thickness": 6, "tool_diameter": 6}), "`material` is required"),
        ] {
            let e = s.execute("cam.cut_out", &params).unwrap_err();
            assert!(matches!(&e, crate::EngineError::BadParams { cmd, msg } if cmd == "cam.cut_out" && msg.contains(why)), "{params}: {e}");
            assert_eq!(s.doc.cam, before, "{params}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn setup_stores_model_bodies_and_wcs() {
        let mut s = Session::default();
        run(
            &mut s,
            "cam.setup",
            json!({
                "name": "Bodies",
                "model_bodies": ["Part"],
                "wcs": {"origin": [1.0, 2.0, 3.0], "x_axis": [1.0, 0.0, 0.0], "y_axis": [0.0, 1.0, 0.0]}
            }),
        );
        assert_eq!(s.doc.cam.setups[0].model_bodies, ["Part"]);
        assert_eq!(s.doc.cam.setups[0].work_origin, [1.0, 2.0, 3.0]);
        assert_eq!(s.doc.cam.setups[0].wcs.origin, [1.0, 2.0, 3.0]);
    }

    #[test]
    fn setup_keeps_room_for_tool_axis_and_work_origin() {
        let mut s = Session::default();
        run(&mut s, "cam.setup", json!({"name": "Side", "tool_axis": [1, 0, 0], "x_axis": [0, 1, 0], "work_origin": [10, 20, 30]}));
        assert_eq!(s.doc.cam.setups[0].tool_axis, [1.0, 0.0, 0.0]);
        assert_eq!(s.doc.cam.setups[0].x_axis, Some([0.0, 1.0, 0.0]));
        assert_eq!(s.doc.cam.setups[0].work_origin, [10.0, 20.0, 30.0]);
        let cold = Session::new(solidcraft_doc::Document::from_json(&s.doc.to_json()).unwrap());
        assert_eq!(cold.doc.cam.setups[0].tool_axis, [1.0, 0.0, 0.0]);
        assert_eq!(cold.doc.cam.setups[0].x_axis, Some([0.0, 1.0, 0.0]));
        assert_eq!(cold.doc.cam.setups[0].work_origin, [10.0, 20.0, 30.0]);
        assert!(s.execute("cam.setup", &json!({"name": "Bad", "tool_axis": [0, 0, 0]})).is_err());
        assert!(s.execute("cam.setup", &json!({"name": "Skew", "x_axis": [0, 0, 1]})).is_err());
    }

    fn gcode_x_words(gcode: &str) -> Vec<i64> {
        gcode
            .split_whitespace()
            .filter_map(|w| w.strip_prefix('X'))
            .filter_map(|n| n.parse::<f64>().ok())
            .map(|x| (x * 100.0).round() as i64)
            .collect()
    }

    fn sketch_outside_gcode(s: &mut Session, setup: Value) -> String {
        run(s, "sketch.create", json!({"plane": "XY"}));
        run(s, "sketch.rectangle.two_point", json!({"p0": [0, 0], "p1": [40, 30]}));
        let sketch = s.active_sketch.expect("sketch");
        run(s, "sketch.finish", json!({}));
        run(s, "cam.tool.define", json!({"number": 1, "diameter": 6, "flute_length": 20, "feed": 600, "plunge_feed": 200, "stepdown": 3}));
        run(s, "cam.setup", setup);
        run(
            s,
            "cam.op.contour",
            json!({
                "setup": "Top", "name": "Outside", "tool": 1,
                "sketch": sketch, "side": "outside", "bottom": -3, "stepdown": 3,
            }),
        );
        run(s, "cam.post", json!({"setup": "Top"}))["gcode"].as_str().unwrap().to_string()
    }

    #[test]
    fn work_origin_shifts_sketch_xy_and_offset_writes_g55() {
        let g0 = sketch_outside_gcode(&mut Session::default(), json!({"name": "Top"}));
        let g10 = sketch_outside_gcode(&mut Session::default(), json!({"name": "Top", "work_origin": [10, 0, 0]}));
        let g55 = sketch_outside_gcode(&mut Session::default(), json!({"name": "Top", "work_offset": 2}));
        assert!(g0.contains("G54"), "{g0}");
        assert!(g55.contains("G55"), "{g55}");
        assert!(!g55.contains("G54"), "{g55}");
        let x0 = gcode_x_words(&g0);
        let x10 = gcode_x_words(&g10);
        assert!(!x0.is_empty(), "{g0}");
        assert_ne!(x0, x10, "origin must move posted X\n{g0}\n{g10}");
        let shifted: Vec<i64> = x0.iter().map(|x| x - 1000).collect();
        assert_eq!(shifted, x10, "X(origin 10) should be X(origin 0) minus 10 mm\n{g0}\n{g10}");
    }

    #[test]
    fn x_axis_rotates_work_xy() {
        let g = sketch_outside_gcode(&mut Session::default(), json!({"name": "Top"}));
        let r = sketch_outside_gcode(&mut Session::default(), json!({"name": "Top", "x_axis": [0, 1, 0]}));
        assert_ne!(gcode_x_words(&g), gcode_x_words(&r), "a work X along model Y must change posted X\n{g}\n{r}");
        assert!(r.contains("G54"), "{r}");
    }

    #[test]
    fn refuses_unknown_tool_and_empty_post() {
        let mut s = Session::default();
        assert!(s.execute("cam.op.contour", &json!({"setup": "A", "name": "C", "tool": 1, "outer": [[0,0],[10,0],[10,10]], "bottom": -1})).is_err());
        run(&mut s, "cam.tool.define", json!({"number": 1, "diameter": 6, "flute_length": 20}));
        run(&mut s, "cam.setup", json!({"name": "A"}));
        assert!(s.execute("cam.post", &json!({"setup": "A"})).is_err());
        assert!(s.execute("cam.op.remove", &json!({"setup": "A", "name": "missing"})).is_err());
    }

    #[test]
    fn setup_without_name_uses_setup_n() {
        let mut s = Session::default();
        assert_eq!(run(&mut s, "cam.setup", json!({}))["setup"]["name"], "Setup 1");
        assert_eq!(run(&mut s, "cam.setup", json!({}))["setup"]["name"], "Setup 2");
        assert_eq!(s.doc.cam.setups.len(), 2);
        assert!(s.execute("cam.setup", &json!({"delete": true})).is_err());
    }

    #[test]
    fn list_and_delete() {
        let mut s = Session::default();
        run(&mut s, "cam.tool.define", json!({"number": 2, "diameter": 3, "flute_length": 12, "kind": "ball_endmill"}));
        run(&mut s, "cam.setup", json!({"name": "S"}));
        run(&mut s, "cam.op.contour", json!({"setup": "S", "name": "C", "tool": 2, "outer": [[0,0],[20,0],[20,10],[0,10]], "bottom": -2}));
        let list = run(&mut s, "cam.list", json!({}));
        assert_eq!(list["tools"].as_array().unwrap().len(), 1);
        assert_eq!(list["setups"][0]["operations"].as_array().unwrap().len(), 1);
        run(&mut s, "cam.op.remove", json!({"setup": "S", "name": "C"}));
        run(&mut s, "cam.setup", json!({"name": "S", "delete": true}));
        run(&mut s, "cam.tool.remove", json!({"number": 2}));
        assert!(s.doc.cam.is_empty());
    }

    #[test]
    fn sketch_profile_follows_the_design_and_exprs_eval() {
        let mut s = Session::default();
        run(&mut s, "parameters.add", json!({"name": "depth", "expression": "6 mm"}));
        run(&mut s, "sketch.create", json!({"plane": "XY"}));
        run(&mut s, "sketch.rectangle.two_point", json!({"p0": [0, 0], "p1": [40, 30]}));
        let sketch = s.active_sketch.expect("sketch");
        run(&mut s, "sketch.finish", json!({}));
        run(&mut s, "cam.tool.define", json!({"number": 1, "diameter": 6, "flute_length": 20, "feed": 600, "plunge_feed": 200, "stepdown": 3}));
        run(&mut s, "cam.setup", json!({"name": "Top"}));
        run(
            &mut s,
            "cam.op.contour",
            json!({
                "setup": "Top", "name": "Outside", "tool": 1,
                "sketch": sketch, "side": "outside", "bottom": "-depth", "stepdown": 3,
            }),
        );
        match &s.doc.cam.setups[0].operations[0] {
            CamOperation::Contour { geometry: CamGeometry::Sketch { .. }, cut, .. } => {
                assert_eq!(cut.bottom.as_str(), "-depth");
            }
            other => panic!("{other:?}"),
        }
        let posted = run(&mut s, "cam.post", json!({"setup": "Top"}));
        let gcode = posted["gcode"].as_str().unwrap();
        assert!(gcode.contains("Z-6"), "{gcode}");
        assert!(posted["readback"]["within"].as_bool().unwrap());

        run(&mut s, "parameters.change", json!({"name": "depth", "expression": "8 mm"}));
        let again = run(&mut s, "cam.post", json!({"setup": "Top"}));
        let g2 = again["gcode"].as_str().unwrap();
        assert!(g2.contains("Z-8"), "{g2}");
        assert!(!g2.contains("Z-6"), "{g2}");
    }

    #[test]
    fn circle_sketch_posts_g2_or_g3() {
        let mut s = Session::default();
        run(&mut s, "sketch.create", json!({"plane": "XY"}));
        run(&mut s, "sketch.circle.center", json!({"center": [0, 0], "radius": 10}));
        let sketch = s.active_sketch.expect("sketch");
        run(&mut s, "sketch.finish", json!({}));
        run(&mut s, "cam.tool.define", json!({"number": 1, "diameter": 4, "flute_length": 16, "feed": 400, "plunge_feed": 100, "stepdown": 2}));
        run(&mut s, "cam.setup", json!({"name": "Top"}));
        run(
            &mut s,
            "cam.op.contour",
            json!({
                "setup": "Top", "name": "Boss", "tool": 1,
                "sketch": sketch, "side": "outside", "bottom": -2, "stepdown": 2,
            }),
        );
        let posted = run(&mut s, "cam.post", json!({"setup": "Top"}));
        let gcode = posted["gcode"].as_str().unwrap();
        assert!(gcode.contains("G2 ") || gcode.contains("G3 "), "{gcode}");
        assert!(posted["readback"]["within"].as_bool().unwrap());
    }
}
