# Router CAM for Mark's Flexi-HAL router: design

Date: 2026-11-12. Branch: `router-cam` in `vckrz8989m-ship-it/solidcraft`.
Status: draft for review; see "Built so far" for what exists on the branch.

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

## Built so far

- Windows pipeline: pushing the `router-cam` branch builds an installer and a portable zip as
  one workflow artifact (builds 1 to 7 succeeded).
- Router layer in `crates/cam/src/router/`: the `grblhal` post, the `flexihal-router` machine
  profile, the feeds calculator, the program and text checks, and two planners:
  - `auto` plans a plate part: pockets to a depth, then every opening the cutter fits roughed
    and finished, nearest first, then the outside with holding tabs. A round opening too small
    for the cutter gets a centre mark to drill afterwards.
  - `solid` plans a solid part carved from a block: roughing level by level with a flat end
    mill, the walls level by level with the same cutter, then finishing passes side by side
    with a ball end mill (or the roughing cutter). The part stays on a skin of the block.
- Commands: upstream's `cam.*` commands ported onto the current library, plus `cam.feeds`,
  `cam.op.part` (a whole plate part as one saved operation, planned again at every post),
  `cam.cut_out` (sketch to checked G-code file in one step, with a dry run and the paths to
  draw) and `cam.carve` (solid body to one checked G-code file a cutter; nothing is kept in
  the design). `cam.post` takes a machine and writes a file.
- App: File > "Cut Out Part to G-code…" (outline sketch, pocket sketch and depth, material,
  thickness, cutter chosen from those used before, how hard to cut, hold-down) and File >
  "Carve Solid to G-code…" (body, material, roughing and finishing cutters, stock above and
  skin under the part). Both have a Check button that lists the plan and draws its paths over
  the part, and Save G-code.

Limits of the 3D carving as built: the body is cut from above as it lies in the model (no
choice of side, no second side); what the tool cannot see from above is not cut; roughing
plunges straight down at a slow feed, so the roughing cutter must cut at its centre; the plan
is made on the window thread, so the app waits while a large part is planned; the toolpaths
follow a fine mesh of the body, not its exact surfaces.

How far the 3D carving is tested: the planner, the command and the dialog pass their tests on
a domed part (the dialog on the headless harness). Build 8 was run on the laptop's screen:
the dialog opens, Check lists the plan and draws the paths, and the command writes both
files. Both files of the domed part went through grblHAL's own code (the grblHAL simulator
for Windows, build 20261112, default settings): in check mode it refused no line of the
roughing file (53 408 lines) or the finishing file (16 246 lines). ioSender 2.0.47 loaded
both with the planned program limits and ran the finishing file against the simulator to
its end (Idle, spindle and coolant off, Z at the safe retract). The Bolted Flange sample
(the hub: a 118 x 118 x 26.5 mm block, cutters with 30 mm of flute) was carved the same way
with build 9: grblHAL's check mode refused no line of its roughing file (257 769 lines) or
its finishing file (46 444 lines). The simulator has default
settings, not this machine's, so travel limits, speeds and the spindle are untested, and
nothing has been cut.

Not built yet: drilling and helical bores as operations of their own, a finishing pass on
pocket floors, a tool library with proven recipes, schemas for the CAM commands, carving a
second side.

## Thread milling in plate parts (2026-11-19)

Why: Mark wants screw threads in plate parts without hand tapping. Asked for in chat and
approved there as: a thread choice for each hole, the end mill bores the hole to size in the
normal job, threading is a file of its own run after changing to the thread mill and zeroing
its height, Check shows the thread passes and time, and a saved fit per thread mill.

Goals: a round hole of a plate part can be given a thread from a list; the part's G-code then
bores that hole to the thread's bore size, and a second file mills the thread with a
single-form 60° thread mill. Not goals: blind threads, threads in pockets or in carved solids,
external threads, multi-form thread mills, pipe threads, a thread drawn in the model. It
would be a failure if a thread were cut in the wrong hole, if the thread mill were sent into
material no cutter has cleared, or if a job with threads wrote one file that changes tools
without a new height zero.

Approaches considered:
1. Thread per hole, chosen in Cut Out Part, bored by the end mill, a second file (chosen):
   one dialog, no drilling by hand, and each cutter gets its own zero as in Carve.
2. A thread operation of its own (`cam.op.thread`) on holes drilled beforehand: more steps,
   and the hole size is left to the user, who then has to know tap drill sizes.
3. One file with a tool-change stop: a stop in the program gives no way to zero the new
   tool on this machine, so the thread mill would run at the end mill's height.

How it works:
- `router/threads.rs` holds the thread table: 10-24, 10-32, 1/4-20, 1/4-28, 5/16-18, 5/16-24,
  3/8-16, 3/8-24, M5, M6, M8 and M10 (coarse pitches). The bore is the major diameter less one
  pitch (the tap drill rule), so the mill cuts half a pitch deep on the radius.
- A threaded hole is named by its centre in the setup's XY (`threads: [{at: [x, y], thread}]`).
  It must be a round opening of the part: one that is not there stops the plan, since the
  sketch has changed under the choice. The drawn diameter does not matter: the opening is cut
  at the bore size. A cutter that does not fit the bore stops the plan (no centre mark).
- Through threads only. The thread mill's tip goes a pitch and 0.5 mm below the plate so the
  thread runs out of the bottom, and starts half a pitch above the top. The end mill
  therefore bores a threaded hole a pitch and 1 mm below the plate (into the spoilboard), so
  the thread mill's end never meets uncut material; the plan says how deep.
- The thread is cut by `helical::thread_mill` (upstream): internal, right-hand, climb, from
  the bottom up, one pitch a turn, in two radial passes in metals when the pitch is 1 mm or
  less and three above it (one and two in plastics and wood, one more on the careful tier),
  then a spring pass. One operation a thread size, nearest hole first.
- Feed: chip per tooth 0.004 of the thread mill's diameter (0.6 of that careful, 1.3 bold),
  at the spindle's lowest speed that gives 120 m/min or more; the feed written is the tool
  centre's, the edge feed times (major - cutter) / major. A starting point, not a proven recipe.
- Fit: millimetres added to the thread's major diameter (-0.2 to 0.5, default 0): above 0 cuts
  deeper and loosens the thread. Kept with each remembered thread mill in the dialog.
- The thread mill is a `ThreadMill` tool: diameter, flutes and reach (kept as its flute
  length). Its reach must cover the plate and what it goes below it; it must fit the bore.
- `cam.cut_out` with threads writes two files, `-1-cut` and `-2-thread` before the extension,
  both checked before either is written; its reply gains `thread` (the second program as
  the first is told) and the saved part keeps `threads`, `thread_tool` and `thread_fit`.
  `cam.holes` lists a part's round openings (name, centre, diameter) for a front end.
  `cam.post` of a setup whose part has threads puts them after the cuts in one program, with
  the post's tool-change stop, as for any setup with two tools.
- Cut Out Part lists the round holes with a thread choice each; once one is chosen it asks for
  the thread mill (remembered ones by name) and the fit. Check lists the thread file's time
  and cuts after the first file's.

Left to the builder: wording of messages, the row layout in the dialog.

Limits to state: the part is on its holding tabs while it is threaded; the tooth is taken to
be at the thread mill's tip; threads smaller than 10-24 are not offered; feeds and the bore
size are untested on the machine, so the first thread is a test hole in scrap.

## Scope

In version 1:
- Outside and inside profiles, pockets with islands, helical bores, drilling.
- Ramp and helix entries, lead-ins, finish passes, holding tabs.
- Automatic feeds and speeds from a tool library and the machine profile.
- A grblHAL/Flexi-HAL post, one file per tool by default.
- Checks before a file is written, and a Manufacture panel in the app.

Not in version 1: built-in sender, 4th axis, engraving text, adaptive clearing
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
   `cam.op.remove`, `cam.post`, `cam.list`, plus `cam.op.part` and `cam.cut_out` (the auto
   rules on a sketch or face; first called `cam.auto` here) and `cam.machine` (choose or edit
   the profile). CAM settings are saved
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
