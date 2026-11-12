use std::f64::consts::PI;

use solidcore_record::{Loop2, Region2, Vec2, Vec3};

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
