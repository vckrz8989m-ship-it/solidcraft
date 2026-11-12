# Router CAM for Mark's Flexi-HAL router: design

Date: 2026-11-12. Branch: `router-cam` in `vckrz8989m-ship-it/solidcraft`.
Status: draft for review. Nothing below is built yet except where "Proven" says so.

## Goal

Draw a plate part in SolidCraft, pick material, thickness and tool, and get a G-code file that
runs in ioSender on the Flexi-HAL with no hand-entered feeds. First version is 2.5D only.
ioSender does the sending, homing and probing.

Success for version 1: the 60 x 40 demo plate with a 20 mm bore, and one real part of Mark's,
go from sketch to a posted file inside the app, pass the G-code checks and grblHAL check mode
(`$C`), and cut correctly in aluminum after an air cut.

## The machine this is built for

- 3-axis moving gantry, about 20 x 30 in cut bed, 5 mm lead ball screws on all axes.
- StepperOnline 3-CLYS90-V20: 9.0 N*m closed-loop NEMA34 motors, CL86Y-V20 drivers, 60 V.
- G-Penny 2.2 kW water-cooled ER20 spindle, 4-pole, 800 Hz, 12,000 to 24,000 rpm.
- Fuling BD612 VFD. Flexi-HAL Rev A7 running grblHAL. ioSender on a Windows laptop.
- Not yet known: driver pulses per rev, tested max rate and acceleration, travel, firmware
  build, tool-length method. These are profile values, not code; defaults are conservative.

## Proven on 2026-11-12

- Upstream's newest CAM branch (pull request 15278, 18 commits on current main) compiles and
  its 308 tests pass. It is a library only: nothing in the app calls it.
- The demo plate, driven through that library with a grblHAL post written as a data file,
  produced a program with helix entry at 3 degrees, climb direction, rolled corners and
  finish passes. The external checker reported 0 errors.
- Compiling the CAM crate with the geometry kernel took 14 minutes on a 2-core machine.

## Scope

In version 1:
- Outside and inside profiles, pockets with islands, helical bores, drilling.
- Ramp and helix entries, lead-ins, finish passes, holding tabs.
- Automatic feeds and speeds from a tool library and the machine profile.
- A grblHAL/Flexi-HAL post, one file per tool by default.
- Checks before a file is written, and a Manufacture panel in the app.

Not in version 1: built-in sender, 3D surfacing, 4th axis, engraving text, adaptive clearing
as the default (the upstream code exists and can be switched on later), nesting, and VFD or
controller configuration.

## Design

Four layers, each testable without the ones above it.

1. **Toolpath library (upstream, `crates/cam`).** Used as it is. Changes go upstream-style:
   small, tested, no router-specific assumptions.

2. **Router layer (new, `crates/cam/src/router/` or its own crate if it grows).**
   - `MachineProfile`: travel, max feed, acceleration, spindle range and power curve,
     safe-retract height, coolant codes, tool-change mode. Stored as JSON; a built-in
     profile named `flexihal-router` carries the values above.
   - `ToolLibrary`: cutters with measured diameter, flute length, stick-out, and per-material
     recipes marked proven or unproven.
   - `feeds`: the calculator from the router skill, in Rust: target chip thickness by tool
     size, chip thinning, depth derating, rpm clamp to 12,000 to 24,000, spindle power check,
     and the arc-radius limit from acceleration. Unproven recipes use the conservative tier.
   - `auto`: turns closed regions into operations: holes to bores or drilling by size, inner
     loops to inside profiles or pockets, the outer loop to an outside profile cut last with
     tabs. Inside before outside; nearest neighbour within a group.
   - `grblhal-flexihal` post as a built-in data file beside `rs274`: header
     `G90 G94 G17 G21` then the work offset, no `G40`, `G43` or `G49`, retract
     `G53 G0 Z-5`, I/J arcs to 3 decimals, `S` then `M3` then a dwell, `M8`/`M9`, `M30`.
   - `checks`: the external checker's rules as library tests and as a pre-write check:
     arc radius agreement, feed set before cutting, rpm in range, no rapid below the stock
     top outside a cut pocket, no `G53 Z0`, soft limits when travel and offset are known.

3. **Commands and design file (port of upstream pull request 2881).** `cam.tool.define`,
   `cam.setup`, `cam.op.contour`, `cam.op.pocket`, `cam.op.bore`, `cam.op.drill`,
   `cam.op.remove`, `cam.post`, `cam.list`, plus `cam.auto` (run the auto rules on a
   sketch or face) and `cam.machine` (choose or edit the profile). CAM settings are saved
   in the design through one pending format file, as the project requires. Every command
   has input and output schemas and never opens a dialog when called by a program.

4. **Manufacture panel (new, `crates/ui-egui`).** Material, thickness and tool pickers; the
   operation list from `cam.auto` with a per-operation override; hold-down choice; toolpath
   preview in the viewport; warnings; a Post button that writes the file. Follows the
   project's UI rules (one outer container, theme tokens, strings in the locale catalog).

Data flow: sketch or face -> regions in the setup's work coordinates -> `auto` -> operations
with feeds -> toolpaths -> checks -> post -> `.nc` file.

Errors: every failure is a `Result` with a message that says what to change (for example
"tool 6 mm is wider than the 5 mm slot"). No panics; hostile parameters are refused.

## Build and install

- Work lives on the `router-cam` branch of the fork.
- `.github/workflows/router-windows.yml` builds the unsigned Windows x64 installer and a
  portable zip on every push to that branch, as a workflow artifact.
- The portable zip is unpacked on the laptop and a Desktop shortcut made. No admin rights.
- This is a personal build of a fork. It is not published as a release, because the brand
  licence does not allow handing a modified build to others under the SolidCraft name.

## Testing

- Rust tests for every layer; the demo plate is a fixed test with its expected checks.
- Every posted test program goes through the read-back check upstream already has and the
  new `checks` module.
- On the machine: grblHAL check mode, air cut, foam or MDF, aluminum scrap, then a part.
- Feeds start unproven. The test-cut ladder from the router skill turns them into proven
  recipes saved in the tool library.

## Order of work

1. Windows build pipeline: get the fork's current app building and installed on the laptop.
2. Router layer: post, machine profile, feeds, checks, with tests. No UI.
3. Commands and design-file settings, ported from pull request 2881 onto the newer library.
4. `cam.auto` and bore, drill and tab operations through commands.
5. Manufacture panel and toolpath preview.
6. Recipe proving workflow.

## Risks

- The upstream CAM branches are unmerged and will change; rebasing may be needed.
- Pull request 2881's wiring targets an older version of the library; the port is real work.
- A full Windows build has not been run from this fork yet; step 1 exists to find out.
- All feeds are estimates until cut on this machine.
