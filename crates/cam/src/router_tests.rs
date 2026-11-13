use std::f64::consts::PI;

use solidcore_record::{Loop2, Region2, Vec2, Vec3};
use solidcraft_geom::prelude::*;

use crate::router::auto::{Hold, PartCut, Pocket, plan_part, plan_part_with};
use crate::router::feeds::{CutKind, FeedRequest, Material, Recipe, Tier, chip_thinning, recipe};
use crate::router::{MachineProfile, Severity, check_program, check_text, check_travel};
use crate::*;

fn v(x: f64, y: f64) -> Vec2 {
    Vec2 { x, y }
}

fn close(a: f64, b: f64, tol: f64) {
    assert!((a - b).abs() <= tol, "{a} is not within {tol} of {b}");
}

fn machine() -> MachineProfile {
    MachineProfile::flexihal_router()
}

fn ask(d: f64, flutes: u32, kind: CutKind) -> FeedRequest {
    FeedRequest::new(Material::Aluminum, d, flutes, kind)
}

/// The 60 × 40 mm plate with a 20 mm bore, 6 mm thick, cut with a 6 mm single-flute cutter at
/// the recipes' own feeds: the bore roughed and finished, then the outside roughed and finished.
fn demo_plate() -> Program {
    let m = machine();
    let mut tool = Tool::new(1, ToolKind::FlatEndmill, 6.0, 17.0);
    tool.flutes = 1;
    let plate = [Region2 { outer: Loop2::polygon(&[v(0.0, 0.0), v(60.0, 0.0), v(60.0, 40.0), v(0.0, 40.0)]), holes: vec![] }];
    let bore = [Region2 { outer: Loop2::circle(v(30.0, 20.0), 10.0), holes: vec![] }];
    let heights = Heights { clearance: 5.0, retract: 0.5, top: 0.0, bottom: -6.2 };
    let ramp = 3.0f64.to_radians();
    let cut = |r: &Recipe, stepdown: f64| Cut { heights, feeds: Feeds { cut: r.feed, plunge: r.ramp_feed }, stepdown };
    let rough = recipe(&ask(6.0, 1, CutKind::Adaptive), &m).unwrap();
    let slot = recipe(&ask(6.0, 1, CutKind::Slot), &m).unwrap();
    let finish = recipe(&ask(6.0, 1, CutKind::Finish), &m).unwrap();
    let wall = |side: Side, stock: f64| ContourParams { side, climb: true, stock_to_leave: stock, tolerance: 0.005, ramp_angle: ramp };
    let paths = [
        (
            "Bore rough",
            pocket(
                &bore,
                &tool,
                &PocketParams { stepover: rough.width, climb: true, stock_to_leave: 0.2, tolerance: 0.005, ramp_angle: ramp },
                &cut(&rough, 3.1),
            )
            .unwrap(),
            rough.rpm,
        ),
        ("Bore finish", contour(&bore, &tool, &wall(Side::Inside, 0.0), &cut(&finish, 6.2)).unwrap(), finish.rpm),
        ("Profile rough", contour(&plate, &tool, &wall(Side::Outside, 0.2), &cut(&slot, 1.55)).unwrap(), slot.rpm),
        ("Profile finish", contour(&plate, &tool, &wall(Side::Outside, 0.0), &cut(&finish, 6.2)).unwrap(), finish.rpm),
    ];
    let operations = paths
        .into_iter()
        .map(|(name, path, rpm)| {
            verify(&path, &heights).unwrap();
            Operation { name: name.into(), tool: tool.clone(), spindle_rpm: rpm, coolant: true, path }
        })
        .collect();
    Program { name: "demo plate".into(), operations, work_offset: None }
}

#[test]
fn grblhal_post_writes_no_code_the_controller_refuses() {
    let post = Post::grblhal();
    post.validate().unwrap();
    assert_eq!(Post::builtin("GrblHAL"), Some(Post::grblhal()));
    let text = emit(&demo_plate(), &post).unwrap();
    let mut lines = text.lines();
    assert_eq!(lines.next(), Some("(demo plate)"));
    assert_eq!(lines.next(), Some("G90 G94 G17 G21"));
    assert_eq!(lines.next(), Some("G54"));
    assert!(text.lines().any(|l| l == "S18500 M3"), "{text}");
    assert_eq!(text.lines().last(), Some("M30"));
    for refused in ["G40", "G41", "G42", "G43", "G49", "G80", " R"] {
        assert!(!text.lines().filter(|l| !l.starts_with('(')).any(|l| l.contains(refused)), "`{refused}` in\n{text}");
    }
    // Arc centres and coordinates to a micron, feeds as whole numbers.
    assert!(text.lines().all(|l| l.split_whitespace().all(|w| w.split('.').nth(1).is_none_or(|frac| frac.len() <= 3))), "{text}");
    assert!(text.lines().any(|l| l.starts_with("G3 ") && l.contains(" I") && l.contains(" Z")), "no helix in\n{text}");
}

#[test]
fn a_machines_post_retracts_in_machine_coordinates_and_waits_for_the_spindle() {
    let m = machine();
    let post = m.post().unwrap();
    let program = demo_plate();
    let text = emit(&program, &post).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.get(3), Some(&"G53 G0 Z-5"), "{text}");
    let on = lines.iter().position(|l| *l == "S18500 M3").unwrap();
    assert_eq!(lines.get(on + 1), Some(&"G4 P8"));
    let n = lines.len();
    assert_eq!(&lines[n - 2..], ["G53 G0 Z-5", "M30"]);
    // What was written is what was planned.
    let back = readback(&text, &program, &post).unwrap();
    assert!(back.max_deviation <= back.allowed, "{} > {}", back.max_deviation, back.allowed);
    assert_eq!(check_text(&text, &m).unwrap(), vec![]);
    // Nothing stops the program. One thing will not go as planned: the entry helix and the
    // first rings of the bore turn on a 0.8 mm radius, tighter than 400 mm/s² follows at feed.
    let issues = check_program(&program, &m).unwrap();
    assert_eq!(issues.len(), 1, "{issues:#?}");
    assert_eq!((issues[0].severity, issues[0].at.as_deref()), (Severity::Warning, Some("Bore rough")));
    assert!(issues[0].message.contains("radius 0.8 mm at 1730 mm/min"), "{}", issues[0].message);

    // A spindle the controller waits for itself gets no dwell.
    let mut waits = machine();
    waits.spindle.spinup_seconds = 0.0;
    assert!(!emit(&program, &waits.post().unwrap()).unwrap().contains("G4 P"));
}

#[test]
fn demo_plate_cuts_climb_with_ramps_and_stays_off_the_part_until_the_finish() {
    let program = demo_plate();
    let below = |op: &Operation, z: f64| -> Vec<Vec3> {
        op.path.points(0.01).into_iter().filter(|(p, cutting)| *cutting && p.z < z).map(|(p, _)| p).collect()
    };
    // Roughing round the plate stays 0.2 mm off it: the tool centre is 3.2 mm from the walls.
    let rough = below(&program.operations[2], -0.01);
    let nearest = rough.iter().map(|p| (p.x.min(60.0 - p.x)).min(p.y.min(40.0 - p.y))).fold(f64::INFINITY, f64::min);
    close(nearest, -3.2, 1e-6);
    // Climb round the outside of a part is clockwise.
    let turn: f64 = rough.iter().zip(rough.iter().skip(1)).map(|(a, b)| (a.x - 30.0) * (b.y - 20.0) - (a.y - 20.0) * (b.x - 30.0)).sum();
    assert!(turn < 0.0);
    // The finish pass round the bore is on the wall: 10 − 3 mm from its centre.
    let finish = below(&program.operations[1], -6.19);
    for p in &finish {
        close(((p.x - 30.0).powi(2) + (p.y - 20.0).powi(2)).sqrt(), 7.0, 0.01);
    }
    assert!(!finish.is_empty());
    // No move at feed goes into new depth steeper than the 3° ramp asked for (3.2° here: the
    // points are the ends of chords, a little shorter than the arcs the ramp is measured
    // along). Going straight down to a depth already cut is going down through air.
    for op in &program.operations {
        let mut from: Option<Vec3> = None;
        let mut deepest = 0.0f64;
        for (p, cutting) in op.path.points(0.01) {
            if let Some(a) = from
                && cutting
                && p.z < deepest - 1e-9
            {
                let run = ((p.x - a.x).powi(2) + (p.y - a.y).powi(2)).sqrt();
                let drop = a.z.min(deepest) - p.z;
                assert!(drop <= run * (3.2f64.to_radians().tan()) + 1e-6, "{}: steep descent from {a:?} to {p:?}", op.name);
            }
            deepest = deepest.min(p.z);
            from = Some(p);
        }
    }
}

#[test]
fn recipes_for_aluminium_match_the_worked_table() {
    let m = machine();
    // (diameter, flutes, kind, rpm, feed, width, depth)
    let table = [
        (3.175, 1, CutKind::Slot, 24000.0, 770.0, 3.175, 0.794),
        (3.175, 1, CutKind::Adaptive, 24000.0, 1200.0, 0.635, 3.175),
        (4.0, 1, CutKind::Slot, 24000.0, 960.0, 4.0, 1.0),
        (4.0, 1, CutKind::Adaptive, 24000.0, 1500.0, 0.8, 4.0),
        (6.0, 1, CutKind::Slot, 18500.0, 1110.0, 6.0, 1.5),
        (6.0, 1, CutKind::Adaptive, 18500.0, 1730.0, 1.2, 6.0),
        (6.0, 1, CutKind::Finish, 18500.0, 1250.0, 0.21, 12.0),
        (6.0, 1, CutKind::Bore, 18500.0, 970.0, 6.0, 0.6),
        (8.0, 1, CutKind::Slot, 14000.0, 1120.0, 8.0, 2.0),
        (8.0, 1, CutKind::Adaptive, 14000.0, 1750.0, 1.6, 8.0),
        (10.0, 3, CutKind::Finish, 12000.0, 1940.0, 0.3, 20.0),
    ];
    for (d, flutes, kind, rpm, feed, width, depth) in table {
        let r = recipe(&ask(d, flutes, kind), &m).unwrap();
        assert_eq!((r.rpm, r.feed), (rpm, feed), "{d} mm {kind:?}");
        close(r.width, width, 1e-3);
        close(r.depth, depth, 1e-3);
        assert!(r.sourced);
    }
    let r = recipe(&ask(6.0, 1, CutKind::Adaptive), &m).unwrap();
    close(r.chip_thickness, 0.075, 1e-9);
    close(r.chip_thinning, 1.25, 1e-9);
    close(r.feed_per_tooth, 0.09375, 1e-9);
    close(r.ramp_feed, 870.0, 1e-9);
    close(r.removal_rate, 12.4875, 1e-6);
    close(r.cut_power, 0.1665, 1e-4);
    close(r.spindle_power, 2.2 * 18500.0 / 24000.0 * 0.8, 1e-9);
    close(r.min_arc_radius, 2.09, 0.01);
    assert!(r.warnings.is_empty(), "{:?}", r.warnings);

    let careful = recipe(&FeedRequest { tier: Tier::Careful, ..ask(6.0, 1, CutKind::Adaptive) }, &m).unwrap();
    assert_eq!((careful.feed, careful.width, careful.depth), (1540.0, 0.6000000000000001, 3.0));
    assert!(!recipe(&FeedRequest::new(Material::Acrylic, 6.0, 1, CutKind::Slot), &m).unwrap().sourced);
}

#[test]
fn recipes_respect_the_spindle_the_cutter_and_the_machine() {
    let m = machine();
    for (ratio, factor) in [(0.05, 2.294), (0.10, 1.667), (0.20, 1.25), (0.30, 1.091), (0.5, 1.0), (1.0, 1.0)] {
        close(chip_thinning(10.0, 10.0 * ratio), factor, 1e-3);
    }
    // The spindle's range: a small cutter is held at the top, a large one at the bottom.
    assert_eq!(recipe(&ask(2.0, 1, CutKind::Slot), &m).unwrap().rpm, 24000.0);
    let large = recipe(&ask(12.0, 2, CutKind::Finish), &m).unwrap();
    assert_eq!(large.rpm, 12000.0);
    assert!(large.warnings.iter().any(|w| w.contains("below the spindle's slowest")));
    // A speed asked for is held to the range and said to be.
    let slow = recipe(&FeedRequest { rpm: Some(8000.0), ..ask(6.0, 1, CutKind::Slot) }, &m).unwrap();
    assert_eq!(slow.rpm, 12000.0);
    assert!(slow.warnings.iter().any(|w| w.contains("outside the spindle's range")));
    // Deeper than the cutter is wide: a quarter less chip at two diameters, half at three.
    let chip = |depth: f64| recipe(&FeedRequest { depth: Some(depth), ..ask(6.0, 1, CutKind::Adaptive) }, &m).unwrap().chip_thickness;
    close(chip(12.0), 0.075 * 0.75, 1e-9);
    close(chip(18.0), 0.075 * 0.5, 1e-9);
    // The depth never exceeds the flutes.
    let short = recipe(&FeedRequest { flute_length: Some(4.0), ..ask(6.0, 1, CutKind::Adaptive) }, &m).unwrap();
    assert_eq!(short.depth, 4.0);
    // A feed the machine cannot hold is cut back, and the thinner chip is said.
    let mut sluggish = machine();
    sluggish.max_feed = 600.0;
    let held = recipe(&ask(6.0, 1, CutKind::Adaptive), &sluggish).unwrap();
    assert_eq!(held.feed, 600.0);
    assert!(held.chip_thickness < 0.075 && held.warnings.iter().any(|w| w.contains("thinner chip")));
    // A cut beyond the spindle's power at its speed is said.
    let heavy = recipe(&FeedRequest { width: Some(12.0), depth: Some(30.0), ..ask(12.0, 3, CutKind::Pocket) }, &m).unwrap();
    assert!(heavy.cut_power > heavy.spindle_power && heavy.warnings.iter().any(|w| w.contains("cut narrower or shallower")));
    // The spindle's power falls with its speed, its torque does not.
    close(m.spindle.power_at(12000.0), 0.88, 1e-9);
    close(m.spindle.power_at(30000.0), 1.76, 1e-9);
    close(m.spindle.torque(), 0.875 * 0.8, 1e-3);
    // A recipe fills in a tool.
    let mut tool = Tool::new(1, ToolKind::FlatEndmill, 6.0, 17.0);
    recipe(&ask(6.0, 1, CutKind::Adaptive), &m).unwrap().apply(&mut tool);
    assert_eq!((tool.spindle_rpm, tool.feed, tool.plunge_feed, tool.stepdown), (Some(18500.0), Some(1730.0), Some(870.0), Some(6.0)));
}

#[test]
fn checks_find_what_stops_the_controller_or_the_machine() {
    let m = machine();
    let bad = "\
G90 G21 G17 G49
G53 G0 Z0
S8000 M3
g41 d1
G1 X20 F800 (G43 in a comment is not a word)
G2 X30 Y10 R5 ; G42 here neither
G43 H1
M5
";
    let issues = check_text(bad, &m).unwrap();
    let found: Vec<(Severity, &str)> = issues.iter().map(|i| (i.severity, i.at.as_deref().unwrap_or(""))).collect();
    assert_eq!(
        found,
        [
            (Severity::Error, "line 1"),
            (Severity::Error, "line 2"),
            (Severity::Error, "line 3"),
            (Severity::Error, "line 4"),
            (Severity::Warning, "line 6"),
            (Severity::Error, "line 7"),
        ],
        "{issues:#?}"
    );
    assert!(issues[1].message.contains("homing switch"));
    // G43.1 (a dynamic offset) and a retract below the pull-off are what grblHAL takes.
    assert_eq!(check_text("G43.1 Z1.5\nG53 G0 Z-5\nS0 M5\n", &m).unwrap(), vec![]);

    // A program planned for a faster spindle and machine than this one.
    let mut program = demo_plate();
    program.operations[0].spindle_rpm = 30000.0;
    for m in &mut program.operations[2].path.moves {
        if let Move::Feed { feed, .. } | Move::Arc { feed, .. } = m {
            *feed = 6000.0;
        }
    }
    let issues = check_program(&program, &m).unwrap();
    assert!(issues.iter().any(|i| i.severity == Severity::Error && i.at.as_deref() == Some("Bore rough") && i.message.contains("30000 rpm")));
    assert!(issues.iter().any(|i| i.severity == Severity::Warning && i.at.as_deref() == Some("Profile rough") && i.message.contains("6000 mm/min")));
    // Its corner arcs (3.2 mm radius) cannot be followed at 6000 mm/min: 400 mm/s² allows 2147.
    close(m.arc_feed_limit(3.2), (400.0f64 * 3.2).sqrt() * 60.0, 1e-9);
    assert!(issues.iter().any(|i| i.message.contains("tighter than the machine follows")), "{issues:#?}");

    // Travel: nothing is said until the machine's is known.
    let program = demo_plate();
    assert_eq!(check_travel(&program, &m, [-700.0, -450.0, -60.0]).unwrap(), vec![]);
    let mut measured = machine();
    measured.travel = Some([760.0, 500.0, 200.0]);
    assert_eq!(check_travel(&program, &measured, [-700.0, -450.0, -60.0]).unwrap(), vec![]);
    let out = check_travel(&program, &measured, [-30.0, -450.0, -2.0]).unwrap();
    assert_eq!(out.len(), 2, "{out:#?}");
    assert!(out[0].message.starts_with("X runs from -33.2 to 33.2") && out[1].message.starts_with("Z runs from"));
}

#[test]
fn hostile_machines_and_requests_are_errors_never_panics() {
    let good = machine();
    good.validate().unwrap();
    let bad = [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -1.0, 0.0, 1.0e300];
    for b in bad {
        for change in 0..9 {
            let mut m = machine();
            match change {
                0 => m.max_feed = b,
                1 => m.acceleration = b,
                2 => m.spindle.power_kw = b,
                3 => m.spindle.rpm_min = b,
                4 => m.spindle.rpm_max = b,
                5 => m.spindle.base_rpm = b,
                6 => m.spindle.efficiency = b,
                7 => m.travel = Some([b, 100.0, 100.0]),
                _ => m.safe_z = b,
            }
            let refused = m.validate().is_err();
            // A minimum of 0 rpm is the only one of these that is a machine.
            assert!(refused, "change {change} to {b} was accepted");
            assert!(m.post().is_err());
            assert!(recipe(&ask(6.0, 1, CutKind::Slot), &m).is_err());
            assert!(check_text("G0 X0", &m).is_err() && check_program(&Program::default(), &m).is_err());
        }
        for change in 0..5 {
            let mut r = ask(6.0, 1, CutKind::Adaptive);
            match change {
                0 => r.diameter = b,
                1 => r.rpm = Some(b),
                2 => r.width = Some(b),
                3 => r.depth = Some(b),
                _ => r.flute_length = Some(b),
            }
            assert!(recipe(&r, &good).is_err(), "request change {change} to {b} was accepted");
        }
        let _ = check_travel(&demo_plate(), &good, [b, 0.0, 0.0]);
        let _ = chip_thinning(b, b);
        let _ = good.arc_feed_limit(b);
        let _ = good.min_arc_radius(b);
    }
    assert!(recipe(&ask(6.0, 0, CutKind::Slot), &good).is_err());
    assert!(recipe(&ask(6.0, 99, CutKind::Slot), &good).is_err());
    // A safe Z inside the homing pull-off would run into the switch.
    let mut m = machine();
    m.safe_z = -1.0;
    assert!(m.validate().is_err());
    // Any text can be checked.
    for text in ["", "(((", "G", "G53 Z", "S-1e400", "G2 R\u{0} X", &"G1 X1 ".repeat(10_000), "%\n\u{feff}G53G0Z0"] {
        let _ = check_text(text, &good).unwrap();
    }
    assert_eq!(check_text("G53G0Z0", &good).unwrap().len(), 1);
    // Profiles are data, and the built-in ones are sound.
    assert_eq!(MachineProfile::builtin(" FlexiHAL-Router "), Some(machine()));
    assert!(MachineProfile::BUILTIN.iter().all(|n| MachineProfile::builtin(n).is_some_and(|m| m.validate().is_ok() && m.name == *n)));
    assert_eq!(MachineProfile::builtin("lathe"), None);
    let json = serde_json::to_string(&good).unwrap();
    assert_eq!(serde_json::from_str::<MachineProfile>(&json).unwrap(), good);
    let _ = PI;
}

fn one_flute(diameter: f64) -> Tool {
    let mut tool = Tool::new(1, ToolKind::FlatEndmill, diameter, 17.0);
    tool.flutes = 1;
    tool
}

/// A 60 × 40 mm plate with round openings, each given as centre x, centre y and radius.
fn plate_with(holes: &[(f64, f64, f64)]) -> [Region2; 1] {
    [Region2 {
        outer: Loop2::polygon(&[v(0.0, 0.0), v(60.0, 0.0), v(60.0, 40.0), v(0.0, 40.0)]),
        holes: holes.iter().map(|&(x, y, r)| Loop2::circle(v(x, y), r).reversed()).collect(),
    }]
}

#[test]
fn a_part_is_planned_openings_first_and_its_outside_last() {
    let m = machine();
    let tool = one_flute(6.0);
    let plan = plan_part(&plate_with(&[(30.0, 20.0, 10.0)]), &tool, &PartCut::new(Material::Aluminum, 6.0), &m).unwrap();
    let names: Vec<&str> = plan.operations.iter().map(|o| o.name.as_str()).collect();
    assert_eq!(names, ["Opening 1 rough", "Opening 1 finish", "Outside rough", "Outside finish"]);
    // Tabs hold the part, so there is nothing to warn of: three round this outline, 1 mm high,
    // left by the last roughing pass and by the finishing pass.
    assert_eq!(plan.issues, []);
    let tabs = |op: &Operation| {
        let on_tab: Vec<bool> = op.path.points(0.01).iter().filter(|(_, cutting)| *cutting).map(|(p, _)| (p.z + 5.2).abs() < 1e-6).collect();
        on_tab.windows(2).filter(|w| !w[0] && w[1]).count()
    };
    assert_eq!((tabs(&plan.operations[2]), tabs(&plan.operations[3])), (3, 3));
    assert_eq!((tabs(&plan.operations[0]), tabs(&plan.operations[1])), (0, 0));
    // Without them the plan says the part comes loose, and cuts all the way round at the bottom.
    let loose =
        plan_part(&plate_with(&[(30.0, 20.0, 10.0)]), &tool, &PartCut { hold: Hold::None, ..PartCut::new(Material::Aluminum, 6.0) }, &m).unwrap();
    assert_eq!(loose.issues.len(), 1, "{:?}", loose.issues);
    assert_eq!((loose.issues[0].severity, loose.issues[0].at.as_deref()), (Severity::Warning, Some("Outside")));
    assert!(loose.issues[0].message.contains("nothing holds the part"), "{:?}", loose.issues);
    assert_eq!(tabs(&loose.operations[3]), 0);

    // Feeds and speeds are the recipes', and every cut goes through the plate by 0.2 mm.
    let slot = recipe(&ask(6.0, 1, CutKind::Slot), &m).unwrap();
    let finish = recipe(&ask(6.0, 1, CutKind::Finish), &m).unwrap();
    let clear = recipe(&ask(6.0, 1, CutKind::Pocket), &m).unwrap();
    close(plan.operations[0].spindle_rpm, clear.rpm, 1e-9);
    close(plan.operations[2].spindle_rpm, slot.rpm, 1e-9);
    close(plan.operations[3].spindle_rpm, finish.rpm, 1e-9);
    for op in &plan.operations {
        let low = op.path.points(0.01).iter().map(|(p, _)| p.z).fold(f64::INFINITY, f64::min);
        close(low, -6.2, 1e-6);
        let fastest = op.path.moves.iter().filter_map(|mv| if let Move::Feed { feed, .. } = mv { Some(*feed) } else { None }).fold(0.0, f64::max);
        assert!(fastest <= slot.feed.max(finish.feed).max(clear.feed) + 1e-9, "{}: {fastest}", op.name);
    }
    // Roughing stays 0.2 mm off the wall the finishing pass then cuts: 3.2 and 3.0 mm from it.
    let reach =
        |op: &Operation| op.path.points(0.01).iter().filter(|(p, cutting)| *cutting && p.z < -1.0).map(|(p, _)| p.x).fold(f64::INFINITY, f64::min);
    close(reach(&plan.operations[2]), -3.2, 0.01);
    close(reach(&plan.operations[3]), -3.0, 0.01);

    // The whole plan posts for the machine with nothing the machine refuses.
    let program = Program { name: "plate".into(), operations: plan.operations, ..Program::default() };
    let text = emit(&program, &m.post().unwrap()).unwrap();
    assert!(check_text(&text, &m).unwrap().iter().all(|i| i.severity != Severity::Error));
    assert!(check_program(&program, &m).unwrap().iter().all(|i| i.severity != Severity::Error));
}

#[test]
fn openings_are_cut_nearest_first_and_only_where_the_cutter_fits() {
    let m = machine();
    let tool = one_flute(6.0);
    let cut = PartCut::new(Material::Aluminum, 6.0);
    // Opening 1 is far, 2 is near, 3 is too small for the cutter, 4 has room for one pass only.
    let part = plate_with(&[(50.0, 30.0, 5.0), (10.0, 10.0, 5.0), (30.0, 30.0, 2.5), (30.0, 10.0, 3.1)]);
    let plan = plan_part(&part, &tool, &cut, &m).unwrap();
    let names: Vec<&str> = plan.operations.iter().map(|o| o.name.as_str()).collect();
    // The round opening the cutter does not fit is marked at its centre in its turn, for a drill.
    assert_eq!(
        names,
        [
            "Opening 2 rough",
            "Opening 2 finish",
            "Opening 4",
            "Opening 3 mark",
            "Opening 1 rough",
            "Opening 1 finish",
            "Outside rough",
            "Outside finish"
        ]
    );
    let unfit: Vec<_> = plan.issues.iter().filter(|i| i.message.contains("does not fit")).collect();
    assert_eq!(unfit.len(), 1, "{:?}", plan.issues);
    assert_eq!(unfit[0].at.as_deref(), Some("Opening 3"));
    assert_eq!(unfit[0].severity, Severity::Warning);
    assert!(unfit[0].message.contains("5.000 mm hole") && unfit[0].message.contains("marked 0.5 mm deep"), "{}", unfit[0].message);
    let mark = &plan.operations[3];
    let cut_points: Vec<Vec3> = mark.path.points(0.01).into_iter().filter(|(_, c)| *c).map(|(p, _)| p).collect();
    assert_eq!(cut_points.len(), 1, "one plunge: {cut_points:?}");
    close(cut_points[0].x, 30.0, 1e-9);
    close(cut_points[0].y, 30.0, 1e-9);
    close(cut_points[0].z, -0.5, 1e-9);
    // An opening that is not round, and too narrow, is left alone and said so.
    let mut slotted = plate_with(&[]);
    slotted[0].holes.push(Loop2::polygon(&[v(20.0, 10.0), v(40.0, 10.0), v(40.0, 14.0), v(20.0, 14.0)]).reversed());
    let narrow = plan_part(&slotted, &tool, &cut, &m).unwrap();
    let cut_names: Vec<&str> = narrow.operations.iter().map(|o| o.name.as_str()).collect();
    assert_eq!(cut_names, ["Outside rough", "Outside finish"]);
    assert!(narrow.issues.iter().any(|i| i.message.contains("left uncut")), "{:?}", narrow.issues);

    // A plate thicker than the flutes is an error to read, and nothing is cut.
    let thick = plan_part(&plate_with(&[]), &tool, &PartCut::new(Material::Aluminum, 20.0), &m).unwrap();
    assert!(thick.operations.is_empty());
    assert!(thick.issues.iter().any(|i| i.severity == Severity::Error && i.message.contains("longer cutter")), "{:?}", thick.issues);

    // A sketch of a plate with a hole gives the hole's disc as a region too: it is the opening's
    // waste, not a second part whose outside would be cut into the plate.
    let mut drawn = plate_with(&[(30.0, 20.0, 10.0)]).to_vec();
    drawn.push(Region2 { outer: Loop2::circle(v(30.0, 20.0), 10.0), holes: vec![] });
    // A disc of 8 mm has no room for three tabs: it is cut without, and the plan says so.
    let coin = [Region2 { outer: Loop2::circle(v(0.0, 0.0), 4.0), holes: vec![] }];
    let small = plan_part(&coin, &tool, &cut, &m).unwrap();
    assert_eq!(small.operations.len(), 2);
    assert!(small.issues.iter().any(|i| i.message.contains("too small for holding tabs")), "{:?}", small.issues);

    let from_sketch = plan_part(&drawn, &tool, &cut, &m).unwrap();
    let alone = plan_part(&plate_with(&[(30.0, 20.0, 10.0)]), &tool, &cut, &m).unwrap();
    assert_eq!(from_sketch, alone);

    // Two parts are told apart by name, and both outsides come after every opening.
    let mut two = plate_with(&[(30.0, 20.0, 8.0)]).to_vec();
    two.push(Region2 { outer: Loop2::circle(v(100.0, 20.0), 15.0), holes: vec![Loop2::circle(v(100.0, 20.0), 6.0).reversed()] });
    let plan = plan_part(&two, &tool, &cut, &m).unwrap();
    let names: Vec<&str> = plan.operations.iter().map(|o| o.name.as_str()).collect();
    assert_eq!(names[..4], ["Part 1 opening 1 rough", "Part 1 opening 1 finish", "Part 2 opening 1 rough", "Part 2 opening 1 finish"]);
    assert_eq!(names[4..], ["Part 1 outside rough", "Part 1 outside finish", "Part 2 outside rough", "Part 2 outside finish"]);
}

#[test]
fn hostile_parts_and_cuts_are_errors_never_panics() {
    let m = machine();
    let tool = one_flute(6.0);
    let good = PartCut::new(Material::Aluminum, 6.0);
    assert!(plan_part(&[], &tool, &good, &m).is_err());
    for bad in [f64::NAN, f64::INFINITY, -1.0, 0.0, 1.0e9] {
        assert!(plan_part(&plate_with(&[]), &tool, &PartCut { thickness: bad, ..good }, &m).is_err(), "thickness {bad}");
        assert!(plan_part(&plate_with(&[]), &tool, &PartCut { clearance: bad, ..good }, &m).is_err(), "clearance {bad}");
    }
    for bad in [f64::NAN, f64::INFINITY, -1.0, 1.0e9] {
        assert!(plan_part(&plate_with(&[]), &tool, &PartCut { finish_stock: bad, ..good }, &m).is_err(), "stock {bad}");
        assert!(plan_part(&plate_with(&[]), &tool, &PartCut { break_through: bad, ..good }, &m).is_err(), "through {bad}");
        assert!(plan_part(&plate_with(&[]), &tool, &PartCut { ramp_angle: bad, ..good }, &m).is_err(), "ramp {bad}");
        assert!(plan_part(&plate_with(&[]), &tool, &PartCut { top: bad, ..good }, &m).is_ok() == (bad == -1.0), "top {bad}");
    }
    // A loop that is no outline at all is refused by the cuts it would need, not a crash.
    let nothing = [Region2 { outer: Loop2 { segs: vec![] }, holes: vec![] }];
    assert!(plan_part(&nothing, &tool, &good, &m).is_err());
    let many = [Region2 { outer: plate_with(&[])[0].outer.clone(), holes: vec![Loop2::circle(v(5.0, 5.0), 1.0); 2001] }];
    assert!(plan_part(&many, &tool, &good, &m).is_err());
}

#[test]
fn pockets_are_cut_to_their_depth_before_the_openings_and_round_their_islands() {
    let m = machine();
    let tool = one_flute(6.0);
    let cut = PartCut::new(Material::Aluminum, 6.0);
    let part = plate_with(&[(48.0, 20.0, 5.0)]);
    // A 24 × 24 mm pocket 2 mm deep with a 6 mm boss left standing in its middle, drawn as a
    // sketch gives it: the ring, and the boss's disc as a region of its own.
    let square = Loop2::polygon(&[v(6.0, 8.0), v(30.0, 8.0), v(30.0, 32.0), v(6.0, 32.0)]);
    let boss = Loop2::circle(v(18.0, 20.0), 3.0);
    let drawn = vec![Region2 { outer: square.clone(), holes: vec![boss.reversed()] }, Region2 { outer: boss.clone(), holes: vec![] }];
    let plan = plan_part_with(&part, &[Pocket { regions: drawn, depth: 2.0 }], &tool, &cut, &m).unwrap();
    let names: Vec<&str> = plan.operations.iter().map(|o| o.name.as_str()).collect();
    assert_eq!(names, ["Pocket rough", "Pocket finish", "Opening 1 rough", "Opening 1 finish", "Outside rough", "Outside finish"]);
    assert_eq!(plan.issues, []);
    for op in &plan.operations[..2] {
        let cutting: Vec<Vec3> = op.path.points(0.01).into_iter().filter(|(_, c)| *c).map(|(p, _)| p).collect();
        let low = cutting.iter().map(|p| p.z).fold(f64::INFINITY, f64::min);
        close(low, -2.0, 1e-6);
        for p in cutting.iter().filter(|p| p.z < 0.0) {
            // The cutter's centre stays 3 mm inside the walls and 3 mm off the boss.
            assert!(p.x > 9.0 - 0.01 && p.x < 27.0 + 0.01 && p.y > 11.0 - 0.01 && p.y < 29.0 + 0.01, "{}: {p:?} is in the wall", op.name);
            assert!((p.x - 18.0).hypot(p.y - 20.0) > 6.0 - 0.01, "{}: {p:?} is in the boss", op.name);
        }
    }
    // The finishing pass goes round the walls and round the boss.
    let finish: Vec<Vec3> =
        plan.operations[1].path.points(0.01).into_iter().filter(|(p, c)| *c && (p.z + 2.0).abs() < 1e-6).map(|(p, _)| p).collect();
    assert!(finish.iter().any(|p| (p.x - 9.0).abs() < 0.01) && finish.iter().any(|p| ((p.x - 18.0).hypot(p.y - 20.0) - 6.0).abs() < 0.01));
    let program = Program { name: "plate".into(), operations: plan.operations, ..Program::default() };
    assert!(check_program(&program, &m).unwrap().iter().all(|i| i.severity != Severity::Error));

    // Two pockets are numbered and cut nearest first; one the cutter does not fit is left and said.
    let disc = |x: f64, y: f64, r: f64| Region2 { outer: Loop2::circle(v(x, y), r), holes: vec![] };
    let pockets = [
        Pocket { regions: vec![disc(50.0, 30.0, 6.0), disc(12.0, 10.0, 6.0)], depth: 3.0 },
        Pocket { regions: vec![disc(30.0, 30.0, 2.0)], depth: 1.0 },
    ];
    let plan = plan_part_with(&plate_with(&[]), &pockets, &tool, &cut, &m).unwrap();
    let names: Vec<&str> = plan.operations.iter().map(|o| o.name.as_str()).collect();
    assert_eq!(names[..4], ["Pocket 2 rough", "Pocket 2 finish", "Pocket 1 rough", "Pocket 1 finish"]);
    assert!(plan.issues.iter().any(|i| i.at.as_deref() == Some("Pocket 3") && i.message.contains("does not fit this pocket")), "{:?}", plan.issues);
    assert!(names.iter().all(|n| !n.contains("mark")), "a pocket too small for the cutter is not marked: {names:?}");

    // A pocket as deep as the plate is an opening drawn in the wrong sketch: an error, nothing cut.
    let through = plan_part_with(&plate_with(&[]), &[Pocket { regions: vec![disc(30.0, 20.0, 8.0)], depth: 6.0 }], &tool, &cut, &m).unwrap();
    assert!(through.operations.is_empty());
    assert!(through.issues.iter().any(|i| i.severity == Severity::Error && i.message.contains("goes through")), "{:?}", through.issues);
    // One beside the part is cut as asked, with a warning.
    let beside = plan_part_with(&plate_with(&[]), &[Pocket { regions: vec![disc(100.0, 20.0, 8.0)], depth: 2.0 }], &tool, &cut, &m).unwrap();
    assert!(beside.issues.iter().any(|i| i.message.contains("not inside the part's outline")), "{:?}", beside.issues);
    for bad in [f64::NAN, f64::INFINITY, -1.0, 0.0] {
        assert!(
            plan_part_with(&plate_with(&[]), &[Pocket { regions: vec![disc(30.0, 20.0, 8.0)], depth: bad }], &tool, &cut, &m).is_err(),
            "depth {bad}"
        );
    }
}
