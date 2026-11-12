//! THROWAWAY build test (not product code): drive upstream's CAM library with the demo plate
//! from the router skill and write grblHAL G-code through a post that is plain data.
use std::f64::consts::PI;

use solidcore_record::{Loop2, Region2, Vec2};
use solidcraft_cam::{
    ContourParams, Cut, Feeds, Heights, Operation, PocketParams, Post, Program, Side, Tool, ToolKind, contour, emit,
    pocket, verify,
};

const GRBLHAL: &str = r#"{
  "name": "grblhal-flexihal",
  "extension": "nc",
  "units": "mm",
  "decimals": 3,
  "feed_decimals": 0,
  "line_numbers": null,
  "comments": "parentheses",
  "header": ["G90 G94 G17 {units}", "{work_offset}"],
  "tool_change": [],
  "tool_change_first": false,
  "spindle_on": ["S{rpm} M3", "G4 P8"],
  "spindle_off": ["M5"],
  "coolant_on": ["M8"],
  "coolant_off": ["M9"],
  "footer": ["M30"],
  "arcs": true,
  "arc_tolerance": 0.002,
  "helical_arcs": true,
  "safe_retract": {"lines": ["G53 G0 Z-5"], "at_start": true, "at_tool_change": true, "at_end": true}
}"#;

fn v(x: f64, y: f64) -> Vec2 {
    Vec2 { x, y }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let post: Post = serde_json::from_str(GRBLHAL)?;
    post.validate()?;

    let mut tool = Tool::new(1, ToolKind::FlatEndmill, 6.0, 17.0);
    tool.flutes = 1;
    tool.description = "6 mm 1F carbide".into();

    let plate = Region2 {
        outer: Loop2::polygon(&[v(0.0, 0.0), v(60.0, 0.0), v(60.0, 40.0), v(0.0, 40.0)]),
        holes: vec![],
    };
    let bore = Region2 { outer: Loop2::circle(v(30.0, 20.0), 10.0), holes: vec![] };
    let ramp = 3.0 * PI / 180.0;
    let heights = Heights { clearance: 5.0, retract: 0.5, top: 0.0, bottom: -6.2 };

    let rough = |stepdown: f64, feed: f64| Cut { heights, feeds: Feeds { cut: feed, plunge: feed / 2.0 }, stepdown };

    let bore_rough = pocket(
        std::slice::from_ref(&bore),
        &tool,
        &PocketParams { stepover: 1.2, climb: true, stock_to_leave: 0.2, tolerance: 0.005, ramp_angle: ramp },
        &rough(3.1, 1730.0),
    )?;
    let bore_finish = contour(
        std::slice::from_ref(&bore),
        &tool,
        &ContourParams { side: Side::Inside, climb: true, stock_to_leave: 0.0, tolerance: 0.005, ramp_angle: ramp },
        &rough(6.2, 1250.0),
    )?;
    let profile_rough = contour(
        std::slice::from_ref(&plate),
        &tool,
        &ContourParams { side: Side::Outside, climb: true, stock_to_leave: 0.2, tolerance: 0.005, ramp_angle: ramp },
        &rough(1.55, 1110.0),
    )?;
    let profile_finish = contour(
        std::slice::from_ref(&plate),
        &tool,
        &ContourParams { side: Side::Outside, climb: true, stock_to_leave: 0.0, tolerance: 0.005, ramp_angle: ramp },
        &rough(6.2, 1250.0),
    )?;

    let mut operations = Vec::new();
    for (name, path) in [
        ("Bore rough", bore_rough),
        ("Bore finish", bore_finish),
        ("Profile rough", profile_rough),
        ("Profile finish", profile_finish),
    ] {
        verify(&path, &heights)?;
        let s = path.stats();
        eprintln!("{name}: cut {:.0} mm, {:.2} min", s.cut_length, s.cut_minutes);
        operations.push(Operation { name: name.into(), tool: tool.clone(), spindle_rpm: 18500.0, coolant: true, path });
    }
    let program = Program { name: "demo plate 60x40 bore 20".into(), operations, work_offset: Some(1) };
    print!("{}", emit(&program, &post)?);
    Ok(())
}
