use std::f64::consts::PI;

use solidcore_record::{Loop2, Region2, Vec2, Vec3};
use solidcraft_geom::prelude::*;

use crate::router::auto::{Hold, PartCut, Planned, Pocket, ThreadedHole, Threading, plan_part, plan_part_threaded, plan_part_with, round_holes};
use crate::router::feeds::{CutKind, FeedRequest, Material, Recipe, Tier, chip_thinning, recipe};
use crate::router::solid::{SolidCut, plan_solid};
use crate::router::threads::{THREADS, find, thread_feed};
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

/// A three-flute single-form thread mill with `reach` mm of reach.
fn thread_mill(diameter: f64, reach: f64) -> Tool {
    let mut tool = Tool::new(2, ToolKind::ThreadMill, diameter, reach);
    tool.flutes = 3;
    tool
}

#[test]
fn the_thread_table_bores_each_thread_at_its_tap_drill_size() {
    let bore = |name: &str| find(name).unwrap().bore();
    close(bore("1/4-20"), 5.08, 1e-9);
    close(bore("M6"), 5.0, 1e-9);
    close(bore("10-32"), 4.826 - 0.79375, 1e-9);
    close(bore("3/8-16"), 9.525 - 1.5875, 1e-9);
    // Names are found whatever their case and spacing, the metric ones with their pitch too.
    assert_eq!(find(" m8 ").map(|t| t.name), Some("M8"));
    assert_eq!(find("M6x1").map(|t| t.name), Some("M6"));
    assert_eq!(find("M10 x 1.5").map(|t| t.name), Some("M10"));
    assert!(find("M7").is_none() && find("").is_none() && find("1/4").is_none() && find("M6x0.75").is_none());
    assert_eq!(THREADS.len(), 12);
    // Every size by its numbers: major diameter and pitch (mm).
    let table: Vec<(&str, f64, f64)> = THREADS.iter().map(|t| (t.name, t.major, (t.pitch * 1.0e5).round() / 1.0e5)).collect();
    assert_eq!(
        table,
        [
            ("10-24", 4.826, 1.05833),
            ("10-32", 4.826, 0.79375),
            ("1/4-20", 6.35, 1.27),
            ("1/4-28", 6.35, 0.90714),
            ("5/16-18", 7.9375, 1.41111),
            ("5/16-24", 7.9375, 1.05833),
            ("3/8-16", 9.525, 1.5875),
            ("3/8-24", 9.525, 1.05833),
            ("M5", 5.0, 0.8),
            ("M6", 6.0, 1.0),
            ("M8", 8.0, 1.25),
            ("M10", 10.0, 1.5),
        ]
    );
    for (i, t) in THREADS.iter().enumerate() {
        assert!(t.pitch >= 0.79 && t.pitch <= 1.6 && t.major >= 4.8 && t.major <= 10.0, "{t:?}");
        assert!(t.bore() > 3.7 && t.bore() < t.major, "{t:?}");
        // The tip goes a pitch and 0.5 mm below the plate, in a hole bored 0.5 mm deeper.
        close(t.below(), t.pitch + 0.5, 1e-12);
        close(t.bored_below(), t.pitch + 1.0, 1e-12);
        assert!(THREADS.iter().skip(i + 1).all(|o| o.name != t.name), "{} is listed twice", t.name);
        assert_eq!(find(t.name), Some(*t));
    }
}

#[test]
fn thread_feeds_keep_the_chip_at_the_cutting_edge_and_slow_the_tool_centre() {
    let m = machine();
    let quarter = find("1/4-20").unwrap();
    let mill = thread_mill(4.572, 12.0);
    let f = thread_feed(&quarter, 0.0, &mill, Material::Aluminum, Tier::Standard, &m).unwrap();
    // 120 m/min would be 8354 rpm: the spindle's slowest is used.
    close(f.rpm, 12000.0, 1e-9);
    // 0.004 of the diameter a tooth at the edge; the centre goes (6.35 - 4.572) / 6.35 as far.
    close(f.feed, 12000.0 * 3.0 * 0.004 * 4.572 * (6.35 - 4.572) / 6.35, 2.5);
    assert_eq!(f.feed % 5.0, 0.0);
    assert_eq!((f.radial_passes, f.spring_pass), (3, true));
    close(f.plunge, 300.0, 1e-9);
    // A looser fit moves the centre further out, so it goes a little faster.
    assert!(thread_feed(&quarter, 0.4, &mill, Material::Aluminum, Tier::Standard, &m).unwrap().feed > f.feed);
    let careful = thread_feed(&quarter, 0.0, &mill, Material::Aluminum, Tier::Careful, &m).unwrap();
    let bold = thread_feed(&quarter, 0.0, &mill, Material::Aluminum, Tier::Bold, &m).unwrap();
    close(careful.feed, 0.6 * f.feed, 5.0);
    close(bold.feed, 1.3 * f.feed, 5.0);
    assert_eq!((careful.radial_passes, bold.radial_passes), (4, 3));
    // A fine pitch takes a pass less; plastics take one less than metals.
    let fine = find("10-32").unwrap();
    let small = thread_mill(3.429, 12.0);
    assert_eq!(thread_feed(&fine, 0.0, &small, Material::Aluminum, Tier::Standard, &m).unwrap().radial_passes, 2);
    assert_eq!(thread_feed(&fine, 0.0, &small, Material::Acrylic, Tier::Standard, &m).unwrap().radial_passes, 1);
    assert_eq!(thread_feed(&quarter, 0.0, &mill, Material::Softwood, Tier::Standard, &m).unwrap().radial_passes, 2);
    // A small thread mill turns faster, never above the spindle's fastest.
    close(thread_feed(&fine, 0.0, &thread_mill(2.0, 8.0), Material::Aluminum, Tier::Standard, &m).unwrap().rpm, 120_000.0 / (PI * 2.0), 1.0);
    close(thread_feed(&fine, 0.0, &thread_mill(1.0, 8.0), Material::Aluminum, Tier::Standard, &m).unwrap().rpm, 24000.0, 1e-9);
    // What cannot be milled is an error: no thread mill, one as wide as the hole, a wild fit.
    assert!(thread_feed(&quarter, 0.0, &one_flute(3.0), Material::Aluminum, Tier::Standard, &m).is_err());
    assert!(thread_feed(&quarter, 0.0, &thread_mill(5.08, 12.0), Material::Aluminum, Tier::Standard, &m).is_err());
    for fit in [f64::NAN, f64::INFINITY, -0.21, 0.51, -1.0e9] {
        assert!(thread_feed(&quarter, fit, &mill, Material::Aluminum, Tier::Standard, &m).is_err(), "{fit}");
    }
    // A machine slower than any thread feed is held to, not a panic.
    let slow = MachineProfile { max_feed: 5.0, ..machine() };
    let held = thread_feed(&quarter, 0.0, &mill, Material::Aluminum, Tier::Standard, &slow).unwrap();
    assert_eq!((held.feed, held.plunge), (5.0, 5.0));
}

/// The cutting points of `op` as distance from `centre` and height.
fn round(op: &Operation, centre: Vec2) -> Vec<(f64, f64)> {
    op.path
        .points(0.01)
        .iter()
        .filter(|(_, cutting)| *cutting)
        .map(|(p, _)| (((p.x - centre.x).powi(2) + (p.y - centre.y).powi(2)).sqrt(), p.z))
        .collect()
}

#[test]
fn a_threaded_hole_is_bored_to_size_then_thread_milled_last_with_the_thread_mill() {
    let m = machine();
    let tool = one_flute(3.175);
    let quarter = find("1/4-20").unwrap();
    // Both holes are drawn 6.35 mm across; the second is to be threaded.
    let regions = plate_with(&[(15.0, 20.0, 3.175), (45.0, 20.0, 3.175)]);
    let threading = Threading { holes: vec![ThreadedHole { center: v(45.0, 20.0), thread: quarter }], tool: thread_mill(4.572, 12.0), fit: 0.0 };
    let cut = PartCut::new(Material::Aluminum, 6.0);
    let plan = plan_part_threaded(&regions, &[], Some(&threading), &tool, &cut, &m).unwrap();
    let names: Vec<&str> = plan.operations.iter().map(|o| o.name.as_str()).collect();
    assert_eq!(
        names,
        ["Opening 1 rough", "Opening 1 finish", "Opening 2 rough", "Opening 2 finish", "Outside rough", "Outside finish", "Thread 1/4-20, 1 hole"]
    );
    let far = |pts: &[(f64, f64)]| pts.iter().map(|p| p.0).fold(0.0, f64::max);
    let low = |pts: &[(f64, f64)]| pts.iter().map(|p| p.1).fold(f64::INFINITY, f64::min);
    // The plain hole is cut as drawn and 0.2 mm through; the threaded one at Ø5.08 mm whatever
    // was drawn, and a pitch and 1 mm below the plate.
    let plain = round(&plan.operations[1], v(15.0, 20.0));
    close(far(&plain), 3.175 - 1.5875, 0.01);
    close(low(&plain), -6.2, 1e-6);
    let bored = round(&plan.operations[3], v(45.0, 20.0));
    close(far(&bored), 2.54 - 1.5875, 0.01);
    close(low(&bored), -(6.0 + 1.27 + 1.0), 1e-6);
    close(low(&round(&plan.operations[2], v(45.0, 20.0))), -(6.0 + 1.27 + 1.0), 1e-6);
    assert!(far(&round(&plan.operations[2], v(45.0, 20.0))) <= 2.54 - 1.5875 - 0.2 + 0.01);
    // The outside is cut as ever.
    close(low(&round(&plan.operations[5], v(0.0, 0.0))), -6.2, 1e-6);
    // The plan says, of the part as a whole (the cutter's program), that the hole is cut at the
    // thread's size, not as drawn, and how deep under the plate; and of the thread operation how
    // far the thread mill goes.
    assert_eq!(plan.issues.len(), 2, "{:?}", plan.issues);
    assert_eq!((plan.issues[0].severity, plan.issues[0].at.as_deref()), (Severity::Warning, None));
    assert!(plan.issues[0].message.contains("2.27 mm below the plate") && plan.issues[0].message.contains("spoilboard"), "{:?}", plan.issues);
    assert!(plan.issues[0].message.contains("Ø5.08 mm whatever was drawn") && plan.issues[0].message.contains("1/4-20"), "{:?}", plan.issues);
    assert_eq!((plan.issues[1].severity, plan.issues[1].at.as_deref()), (Severity::Warning, Some("Thread 1/4-20, 1 hole")));
    assert!(
        plan.issues[1].message.contains("thread mill goes 1.77 mm below the plate") && !plan.issues[1].message.contains("spoilboard"),
        "{:?}",
        plan.issues
    );

    // The thread is milled with the thread mill at the thread's own feed.
    let thread = &plan.operations[6];
    let feed = thread_feed(&quarter, 0.0, &threading.tool, Material::Aluminum, Tier::Standard, &m).unwrap();
    assert_eq!((thread.tool.kind, thread.tool.number, thread.coolant), (ToolKind::ThreadMill, 2, true));
    close(thread.spindle_rpm, feed.rpm, 1e-9);
    assert!(plan.operations.iter().take(6).all(|o| o.tool.kind == ToolKind::FlatEndmill && o.tool.number == 1));
    let fastest = thread.path.moves.iter().filter_map(|mv| if let Move::Arc { feed, .. } = mv { Some(*feed) } else { None }).fold(0.0, f64::max);
    close(fastest, feed.feed, 1e-9);
    // Its tip goes a pitch and 0.5 mm below the plate, 0.5 mm above the bored hole's floor,
    // and half a pitch above the plate's top; its centre reaches the major radius less its own.
    let cutting = round(thread, v(45.0, 20.0));
    close(low(&cutting), -(6.0 + 1.27 + 0.5), 1e-6);
    let arcs_end =
        thread.path.moves.iter().filter_map(|mv| if let Move::Arc { to, .. } = mv { Some(to.z) } else { None }).fold(f64::NEG_INFINITY, f64::max);
    close(arcs_end, 0.5 * 1.27, 1e-6);
    close(far(&cutting), 3.175 - 2.286, 1e-6);
    // Nothing of it leaves the hole: every position, rapids too, is over the bore.
    assert!(thread.path.points(0.01).iter().all(|(p, _)| ((p.x - 45.0).powi(2) + (p.y - 20.0).powi(2)).sqrt() <= 3.175 - 2.286 + 1e-6));
    // Three passes and a spring pass go round counter-clockwise, rising one pitch a turn.
    let mut at: Option<Vec3> = None;
    let (mut turned, mut risen, mut radii) = (0.0, 0.0, Vec::<f64>::new());
    for mv in &thread.path.moves {
        if let (Move::Arc { to, center, ccw, .. }, Some(from)) = (mv, at) {
            let rho = ((to.x - center.x).powi(2) + (to.y - center.y).powi(2)).sqrt();
            // A helix turns about the hole's own centre; a lead about a point beside it.
            if (center.x - 45.0).abs() < 1e-9 && (center.y - 20.0).abs() < 1e-9 {
                assert!(*ccw, "an internal right-hand thread is climb milled counter-clockwise");
                assert!(to.z >= from.z - 1e-9, "and from the bottom up");
                if (rho - (3.175 - 2.286)).abs() < 1e-6 {
                    turned += arc_sweep(from, *to, *center, *ccw);
                    risen += to.z - from.z;
                }
                if !radii.iter().any(|r| (r - rho).abs() < 1e-6) {
                    radii.push(rho);
                }
            }
        }
        if let Some(to) = mv.target() {
            at = Some(to);
        }
    }
    close(risen / (turned / (2.0 * PI)), 1.27, 1e-6);
    // The hole's wall is 0.254 mm from the tool at the centre: the passes share the 0.635 mm.
    assert_eq!(radii.len(), 3, "{radii:?}");
    close(radii[0], 0.254 + 0.635 / 3.0, 1e-6);
    close(radii[2], 0.889, 1e-6);
    // Two full-depth passes: the last of three and the spring pass.
    close(turned / (2.0 * PI), 2.0 * (6.0 + 1.27 + 0.5 + 0.635 - 0.5 * 1.27) / 1.27, 1e-6);

    // A looser fit cuts deeper by half of it on the radius.
    let loose = Threading { fit: 0.1, ..threading.clone() };
    let plan_loose = plan_part_threaded(&regions, &[], Some(&loose), &tool, &cut, &m).unwrap();
    close(far(&round(&plan_loose.operations[6], v(45.0, 20.0))), 3.175 - 2.286 + 0.05, 1e-6);

    // Each cutter's program posts for the machine with nothing it refuses or warns of.
    let (cuts, threads): (Vec<Operation>, Vec<Operation>) = plan.operations.into_iter().partition(|o| o.tool.number == 1);
    for operations in [cuts, threads] {
        let program = Program { name: "plate".into(), operations, ..Program::default() };
        let text = emit(&program, &m.post().unwrap()).unwrap();
        assert_eq!(check_text(&text, &m).unwrap(), []);
        let found = check_program(&program, &m).unwrap();
        assert!(found.iter().all(|i| i.severity != Severity::Error), "{found:?}");
        // The thread mill's small circles are slow enough for the machine to follow.
        assert!(program.operations[0].tool.number == 1 || found.is_empty(), "{found:?}");
        assert!(readback(&text, &program, &m.post().unwrap()).unwrap().within());
    }
}

#[test]
fn threads_of_a_size_share_an_operation_and_round_holes_are_named_as_the_plan_names_them() {
    let m = machine();
    let tool = one_flute(3.175);
    let (quarter, m5) = (find("1/4-20").unwrap(), find("M5").unwrap());
    let regions = plate_with(&[(50.0, 30.0, 3.0), (10.0, 10.0, 3.0), (30.0, 20.0, 2.5), (50.0, 10.0, 3.0)]);
    let holes = round_holes(&regions).unwrap();
    assert_eq!(holes.iter().map(|h| h.name.as_str()).collect::<Vec<_>>(), ["Opening 1", "Opening 2", "Opening 3", "Opening 4"]);
    assert_eq!((holes[2].center, holes[2].diameter), (v(30.0, 20.0), 5.0));
    // An opening that is not round is not a hole to thread, and keeps its number.
    let mut slotted = plate_with(&[(10.0, 10.0, 3.0)]);
    slotted[0].holes.insert(0, Loop2::polygon(&[v(20.0, 5.0), v(40.0, 5.0), v(40.0, 12.0), v(20.0, 12.0)]).reversed());
    assert_eq!(round_holes(&slotted).unwrap().iter().map(|h| h.name.as_str()).collect::<Vec<_>>(), ["Opening 2"]);
    // Two plates: the holes are named with their part.
    let two = [
        plate_with(&[(10.0, 10.0, 3.0)])[0].clone(),
        Region2 {
            outer: Loop2::polygon(&[v(100.0, 0.0), v(140.0, 0.0), v(140.0, 40.0), v(100.0, 40.0)]),
            holes: vec![Loop2::circle(v(120.0, 20.0), 4.0).reversed()],
        },
    ];
    assert_eq!(round_holes(&two).unwrap().iter().map(|h| h.name.as_str()).collect::<Vec<_>>(), ["Part 1 opening 1", "Part 2 opening 1"]);
    assert!(round_holes(&[]).unwrap().is_empty());

    let threading = Threading {
        holes: vec![
            ThreadedHole { center: v(50.0, 30.0), thread: quarter },
            ThreadedHole { center: v(30.0, 20.0), thread: m5 },
            ThreadedHole { center: v(10.0, 10.0), thread: quarter },
            // Within a hundredth of a millimetre is the same hole.
            ThreadedHole { center: v(50.004, 10.0), thread: quarter },
        ],
        tool: thread_mill(3.429, 12.0),
        fit: 0.0,
    };
    let plan = plan_part_threaded(&regions, &[], Some(&threading), &tool, &PartCut::new(Material::Aluminum, 6.0), &m).unwrap();
    let names: Vec<&str> = plan.operations.iter().map(|o| o.name.as_str()).collect();
    assert_eq!(&names[names.len() - 2..], ["Thread 1/4-20, 3 holes", "Thread M5, 1 hole"], "{names:?}");
    // The three are milled from the nearest to the work zero on, each from the one before.
    let quarter_op = &plan.operations[plan.operations.len() - 2];
    let mut visited: Vec<(f64, f64)> = Vec::new();
    for (p, cutting) in quarter_op.path.points(0.01) {
        let near = [(10.0, 10.0), (50.0, 10.0), (50.0, 30.0)].into_iter().find(|c| ((p.x - c.0).powi(2) + (p.y - c.1).powi(2)).sqrt() < 2.0);
        if let (true, Some(c)) = (cutting, near) {
            if visited.last() != Some(&c) {
                visited.push(c);
            }
        }
    }
    assert_eq!(visited, [(10.0, 10.0), (50.0, 10.0), (50.0, 30.0)]);
    // From hole to hole it crosses the plate at the clearance height, as every other cut does:
    // over a clamp or a screw head, never 1 mm above the plate.
    let mut from: Option<Vec3> = None;
    for mv in &quarter_op.path.moves {
        if let (Move::Rapid { to }, Some(at)) = (mv, from) {
            let sideways = (to.x - at.x).abs() > 1e-9 || (to.y - at.y).abs() > 1e-9;
            assert!(!sideways || (to.z >= 5.0 - 1e-9 && at.z >= 5.0 - 1e-9), "a rapid sideways from Z{} to Z{}", at.z, to.z);
        }
        if let Some(to) = mv.target() {
            from = Some(to);
        }
    }
    // The hole 0.004 mm from where it was named is threaded on its own centre.
    assert!(round(quarter_op, v(50.0, 10.0)).iter().filter(|p| p.0 < 2.0).map(|p| p.0).fold(0.0, f64::max) <= 3.175 - 1.7145 + 1e-6);
    assert_eq!(plan.issues.iter().filter(|i| i.severity == Severity::Warning).count(), 4, "{:?}", plan.issues);
    // No threads asked for is the plan without them.
    let none = Threading { holes: vec![], ..threading.clone() };
    let cut = PartCut::new(Material::Aluminum, 6.0);
    assert_eq!(plan_part_threaded(&regions, &[], Some(&none), &tool, &cut, &m).unwrap(), plan_part_with(&regions, &[], &tool, &cut, &m).unwrap());
}

#[test]
fn threads_that_cannot_be_milled_as_asked_stop_the_plan_and_say_what_to_change() {
    let m = machine();
    let tool = one_flute(3.175);
    let quarter = find("1/4-20").unwrap();
    let regions = plate_with(&[(15.0, 20.0, 3.175), (45.0, 20.0, 3.175)]);
    let cut = PartCut::new(Material::Aluminum, 6.0);
    let at = |x: f64, y: f64| vec![ThreadedHole { center: v(x, y), thread: quarter }];
    let asked = |holes: Vec<ThreadedHole>, mill: Tool, tool: &Tool, cut: &PartCut| {
        plan_part_threaded(&regions, &[], Some(&Threading { holes, tool: mill, fit: 0.0 }), tool, cut, &m)
    };
    let stopped = |plan: Planned, words: &[&str]| {
        assert!(plan.operations.is_empty(), "nothing is cut of a plan that is stopped");
        let errors: Vec<&str> = plan.issues.iter().filter(|i| i.severity == Severity::Error).map(|i| i.message.as_str()).collect();
        assert_eq!(errors.len(), 1, "{:?}", plan.issues);
        assert!(words.iter().all(|w| errors[0].contains(w)), "{errors:?} lacks {words:?}");
    };
    // The hole is not where the thread was chosen: the sketch has changed under it.
    stopped(asked(at(40.0, 20.0), thread_mill(4.572, 12.0), &tool, &cut).unwrap(), &["no round hole", "X 40", "Y 20", "1/4-20"]);
    // An opening that is not round is not threaded.
    let mut slotted = plate_with(&[]);
    slotted[0].holes.push(Loop2::polygon(&[v(20.0, 5.0), v(40.0, 5.0), v(40.0, 12.0), v(20.0, 12.0)]).reversed());
    let slot = Threading { holes: at(30.0, 8.5), tool: thread_mill(4.572, 12.0), fit: 0.0 };
    stopped(plan_part_threaded(&slotted, &[], Some(&slot), &tool, &cut, &m).unwrap(), &["no round hole"]);
    // The 6 mm cutter does not fit the Ø5.08 mm hole of the thread: no centre mark, the plan stops.
    stopped(asked(at(45.0, 20.0), thread_mill(4.572, 12.0), &one_flute(6.0), &cut).unwrap(), &["6 mm cutter", "5.08 mm", "1/4-20"]);
    // A cutter nearly as wide as the hole would plunge with no room for its chips: half a
    // millimetre of room is asked, and little room is said.
    stopped(asked(at(45.0, 20.0), thread_mill(4.572, 12.0), &one_flute(4.7), &cut).unwrap(), &["4.7 mm cutter", "5.08 mm", "at most 4.58 mm"]);
    let tight = asked(at(45.0, 20.0), thread_mill(4.572, 12.0), &one_flute(4.5), &cut).unwrap();
    assert!(!tight.operations.is_empty() && tight.issues.iter().all(|i| i.severity == Severity::Warning));
    assert!(tight.issues.iter().any(|i| i.message.contains("little room for chips") && i.message.contains("4.5 mm cutter")), "{:?}", tight.issues);
    // A thread whose hole is bored larger than it was drawn must stay inside the part and clear
    // of its other openings: a wrong hole picked for a large thread is stopped, not cut through.
    let m10 = find("M10").unwrap();
    let near_edge = plate_with(&[(3.0, 20.0, 1.5), (30.0, 20.0, 1.5), (35.0, 20.0, 1.5)]);
    let big = |x: f64| Threading { holes: vec![ThreadedHole { center: v(x, 20.0), thread: m10 }], tool: thread_mill(4.572, 12.0), fit: 0.0 };
    stopped(plan_part_threaded(&near_edge, &[], Some(&big(3.0)), &tool, &cut, &m).unwrap(), &["M10", "Ø3 mm", "edge or into another opening"]);
    stopped(plan_part_threaded(&near_edge, &[], Some(&big(30.0)), &tool, &cut, &m).unwrap(), &["M10", "edge or into another opening"]);
    let roomy = plate_with(&[(30.0, 20.0, 1.5)]);
    assert!(plan_part_threaded(&roomy, &[], Some(&big(30.0)), &tool, &cut, &m).unwrap().issues.iter().all(|i| i.severity == Severity::Warning));
    // The thread mill must fit the hole with room, and reach through the plate and below it.
    stopped(asked(at(45.0, 20.0), thread_mill(5.0, 12.0), &tool, &cut).unwrap(), &["thread mill", "5.08 mm", "4.98 mm"]);
    stopped(asked(at(45.0, 20.0), thread_mill(4.572, 7.5), &tool, &cut).unwrap(), &["reach", "7.77 mm"]);
    // The end mill's flutes must cover the deeper hole: 12 + 2.27 mm here.
    let thick = PartCut::new(Material::Aluminum, 15.0);
    stopped(asked(at(45.0, 20.0), thread_mill(4.572, 25.0), &tool, &thick).unwrap(), &["17.27 mm", "flute"]);

    // Hostile values are errors, never panics.
    let mill = thread_mill(4.572, 12.0);
    assert!(asked(at(45.0, 20.0), one_flute(4.0), &tool, &cut).is_err(), "an end mill is not a thread mill");
    assert!(asked(at(f64::NAN, 20.0), mill.clone(), &tool, &cut).is_err());
    assert!(asked(at(1.0e12, 20.0), mill.clone(), &tool, &cut).is_err());
    let twice = [at(45.0, 20.0), at(45.0, 20.005)].concat();
    assert!(asked(twice, mill.clone(), &tool, &cut).unwrap_err().to_string().contains("two threads"));
    assert!(asked(vec![ThreadedHole { center: v(45.0, 20.0), thread: quarter }; 2001], mill.clone(), &tool, &cut).is_err());
    for fit in [f64::NAN, f64::NEG_INFINITY, 0.6, -0.3] {
        let wild = Threading { holes: at(45.0, 20.0), tool: mill.clone(), fit };
        assert!(plan_part_threaded(&regions, &[], Some(&wild), &tool, &cut, &m).is_err(), "{fit}");
    }
    let mut broken = mill.clone();
    broken.diameter = f64::NAN;
    assert!(asked(at(45.0, 20.0), broken, &tool, &cut).is_err());
    let mut zero = mill;
    zero.flutes = 0;
    assert!(asked(at(45.0, 20.0), zero, &tool, &cut).is_err());
}

/// A 60 × 40 mm base from Z −16 to Z −7 with a round boss on it at (30, 20): 10 mm radius,
/// its wall up to Z −4.51, its top a dome of 16 mm radius reaching Z −1. A closed body.
fn bossed_base() -> Surface {
    let mut tris = crate::foundation_tests::block(Vec3::new(0.0, 0.0, -16.0), Vec3::new(60.0, 40.0, -7.0)).triangles().to_vec();
    // The dome is about (30, 20, −17): at 10 mm from the axis it is √(16² − 10²) = 12.49 above.
    let widest = (10.0f64 / 16.0).asin();
    let dome = (0..=24).map(|k| widest * (1.0 - k as f64 / 24.0)).map(|a| (16.0 * a.sin(), -17.0 + 16.0 * a.cos()));
    let profile: Vec<(f64, f64)> = std::iter::once((10.0, -7.0)).chain(dome).collect();
    let n = 96;
    let at = |r: f64, z: f64, k: usize| {
        let th = 2.0 * PI * k as f64 / n as f64;
        Vec3::new(30.0 + r * th.cos(), 20.0 + r * th.sin(), z)
    };
    for w in profile.windows(2) {
        for k in 0..n {
            tris.push([at(w[0].0, w[0].1, k), at(w[1].0, w[1].1, k), at(w[1].0, w[1].1, k + 1)]);
            tris.push([at(w[0].0, w[0].1, k), at(w[1].0, w[1].1, k + 1), at(w[0].0, w[0].1, k + 1)]);
        }
    }
    Surface::new(tris).unwrap()
}

fn ball(diameter: f64) -> Tool {
    Tool::new(2, ToolKind::BallEndmill, diameter, 20.0)
}

/// How close the cutting points of `op` come to where its cutter touches `surface` from above
/// (mm; negative when the cutter goes into the part), and the lowest Z it cuts at.
fn closest_to(op: &Operation, surface: &Surface) -> (f64, f64) {
    let cutter = op.tool.cutter().unwrap();
    let (mut gap, mut low) = (f64::INFINITY, f64::INFINITY);
    for (p, cutting) in op.path.points(0.01) {
        if cutting {
            low = low.min(p.z);
            if let Some(z) = drop_z(surface, &cutter, p.x, p.y) {
                gap = gap.min(p.z - z);
            }
        }
    }
    (gap, low)
}

#[test]
fn a_solid_is_roughed_walled_and_finished_without_cutting_into_it() {
    let (m, part) = (machine(), bossed_base());
    let mut rough = one_flute(6.0);
    rough.flute_length = 22.0;
    let cut = SolidCut::new(Material::Aluminum);
    let plan = plan_solid(&part, &rough, Some(&ball(6.0)), &cut, &m).unwrap();
    let names: Vec<&str> = plan.operations.iter().map(|o| o.name.as_str()).collect();
    assert_eq!(names, ["Rough", "Walls", "Finish"]);
    assert_eq!(plan.operations.iter().map(|o| o.tool.number).collect::<Vec<_>>(), [1, 1, 2]);
    // The part stays on its skin, and the plan says so.
    assert_eq!(plan.issues.len(), 1, "{:?}", plan.issues);
    assert!(plan.issues[0].severity == Severity::Warning && plan.issues[0].message.contains("0.50 mm skin"), "{:?}", plan.issues);

    // Speeds and feeds are the recipes' for each cutter.
    let slot = recipe(&ask(6.0, 1, CutKind::Slot), &m).unwrap();
    let wall = recipe(&ask(6.0, 1, CutKind::Finish), &m).unwrap();
    let fine = recipe(&ask(6.0, 2, CutKind::Finish), &m).unwrap();
    for (op, r) in plan.operations.iter().zip([&slot, &wall, &fine]) {
        close(op.spindle_rpm, r.rpm, 1e-9);
        let fastest = op.path.moves.iter().filter_map(|mv| if let Move::Feed { feed, .. } = mv { Some(*feed) } else { None }).fold(0.0, f64::max);
        close(fastest, r.feed, 1e-9);
    }

    // Roughing stays the finishing stock above the part; the walls and the finishing passes come
    // to it and never into it. Nothing cuts below the skin: the part is 15 mm high from Z −16,
    // so the floor is Z −15.5, and roughing and the walls reach it.
    let (gap, low): (Vec<f64>, Vec<f64>) = plan.operations.iter().map(|op| closest_to(op, &part)).unzip();
    assert!(gap[0] >= 0.3 - 0.02, "roughing comes within {} mm", gap[0]);
    assert!(gap[1] >= -0.02 && gap[1] < 0.05, "the walls come within {} mm", gap[1]);
    assert!(gap[2] >= -0.02 && gap[2] < 0.02, "finishing comes within {} mm", gap[2]);
    close(low[0], -15.5, 1e-6);
    close(low[1], -15.5, 1e-6);
    assert!(low[2] >= -15.5 - 1e-6);
    // The finishing passes are a tenth of the ball's diameter apart and go over the dome's top.
    let over: Vec<Vec3> = plan.operations[2].path.points(0.01).into_iter().filter(|(_, cutting)| *cutting).map(|(p, _)| p).collect();
    let mut rows: Vec<f64> = over.iter().map(|p| (p.y / 0.6).round()).collect();
    rows.sort_by(f64::total_cmp);
    rows.dedup();
    assert!(rows.len() >= 67 && rows.windows(2).all(|w| w[1] - w[0] <= 1.0), "{} rows", rows.len());
    assert!(over.iter().any(|p| (p.x - 30.0).hypot(p.y - 20.0) < 0.7 && (p.z + 1.0).abs() < 0.05));

    // Every operation leaves from and returns to above the block, and the plan posts with
    // nothing the machine refuses.
    let heights = Heights { clearance: 4.5, retract: 1.0, top: -0.5, bottom: -15.5 };
    for op in &plan.operations {
        verify(&op.path, &heights).unwrap();
    }
    let program = Program { name: "boss".into(), operations: plan.operations, ..Program::default() };
    let text = emit(&program, &m.post().unwrap()).unwrap();
    assert!(check_text(&text, &m).unwrap().iter().all(|i| i.severity != Severity::Error));
    let refused: Vec<_> = check_program(&program, &m).unwrap().into_iter().filter(|i| i.severity == Severity::Error).collect();
    assert_eq!(refused, []);
}

#[test]
fn one_cutter_can_do_a_whole_solid_and_what_cannot_be_cut_is_said() {
    let (m, part) = (machine(), bossed_base());
    let mut rough = one_flute(6.0);
    rough.flute_length = 22.0;
    let cut = SolidCut::new(Material::Aluminum);
    // One flat cutter: the same three operations, and a note that a flat end mill leaves steps.
    let plan = plan_solid(&part, &rough, None, &cut, &m).unwrap();
    assert_eq!(plan.operations.iter().map(|o| (o.name.as_str(), o.tool.number)).collect::<Vec<_>>(), [("Rough", 1), ("Walls", 1), ("Finish", 1)]);
    assert!(plan.issues.iter().any(|i| i.message.contains("a ball end mill follows them")), "{:?}", plan.issues);
    assert!(closest_to(&plan.operations[2], &part).0 >= -0.02);
    // With no skin the part is cut free, and the plan warns of it.
    let free = plan_solid(&part, &rough, None, &SolidCut { skin: 0.0, ..cut }, &m).unwrap();
    assert!(free.issues.iter().any(|i| i.message.contains("nothing holds it")), "{:?}", free.issues);
    close(closest_to(&free.operations[0], &part).1, -16.0, 1e-6);

    // A cutter too short for the cut, or a skin as thick as the part, is an error and no cut.
    let stub = Tool { flute_length: 12.0, ..one_flute(6.0) };
    let short = plan_solid(&part, &stub, None, &cut, &m).unwrap();
    assert!(
        short.operations.is_empty() && short.issues[0].severity == Severity::Error && short.issues[0].message.contains("12.00 mm of flute"),
        "{:?}",
        short.issues
    );
    let thick = plan_solid(&part, &rough, None, &SolidCut { skin: 15.0, ..cut }, &m).unwrap();
    assert!(thick.operations.is_empty() && thick.issues[0].severity == Severity::Error, "{:?}", thick.issues);

    // Hostile input is an error, never a panic.
    assert!(plan_solid(&part, &ball(6.0), None, &cut, &m).is_err());
    let mut drill = one_flute(6.0);
    drill.kind = ToolKind::Drill;
    assert!(plan_solid(&part, &rough, Some(&drill), &cut, &m).is_err());
    for bad in [f64::NAN, f64::INFINITY, -1.0, 1.0e9] {
        for f in 0..7 {
            let mut c = cut;
            match f {
                0 => c.margin = bad,
                1 => c.stock_top = bad,
                2 => c.finish_stock = bad,
                3 => c.skin = bad,
                4 => c.finish_stepover = Some(bad),
                5 => c.clearance = bad,
                _ => c.retract = bad,
            }
            assert!(plan_solid(&part, &rough, None, &c, &m).is_err(), "field {f} = {bad}");
        }
    }
    let far = Surface::new(vec![[Vec3::new(0.0, 0.0, 0.0), Vec3::new(5.0e5, 0.0, 0.0), Vec3::new(0.0, 5.0e5, -5.0)]]).unwrap();
    assert!(plan_solid(&far, &rough, None, &cut, &m).is_err());
    // A flat sheet has no height to cut.
    let flat = Surface::new(vec![[Vec3::new(0.0, 0.0, 0.0), Vec3::new(10.0, 0.0, 0.0), Vec3::new(0.0, 10.0, 0.0)]]).unwrap();
    let nothing = plan_solid(&flat, &rough, None, &cut, &m).unwrap();
    assert!(nothing.operations.is_empty() && nothing.issues[0].severity == Severity::Error, "{:?}", nothing.issues);
}
