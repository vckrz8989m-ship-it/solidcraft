# Thread Milling in Plate Parts Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A round hole of a plate part can be given a screw thread: the part's G-code bores it to size and a second file thread-mills it.

**Architecture:** A thread table and the thread planning live in the router layer (`router/threads.rs`, `router/auto.rs`) on top of upstream's `helical::thread_mill`. `cam.cut_out` carries the choice, splits the plan into one program a cutter and writes two files; the Cut Out Part dialog lists the holes.

**Tech Stack:** Rust (crates `solidcraft-cam`, `solidcraft-doc`, `solidcraft-engine`, `solidcraft-ui-egui`), egui.

**Spec:** `docs/superpowers/specs/2026-11-12-router-cam-design.md`, section "Thread milling in plate parts (2026-11-19)".

## Global Constraints

- Repo `AGENTS.md`: no unwrap/expect/panic outside tests, hostile input is a `Result`, everything is a command, `mod` lines and locale keys sorted, clippy `-D warnings`, a design-format change is one new file in `crates/doc/src/format/pending/`, new UI strings in `assets/locales/pending/router-cam.json` for en, ar, he, ja, vi, zh.
- Build with `cargo ... -j 1`. Planner tests: `cargo test -j 1 -p solidcraft-cam --lib router`. Engine and dialog tests run from one scratch integration test `crates/ui-egui/tests/zz_thread_scratch.rs` (git-ignored); the same tests are also added to the crates' own test modules.
- Bore = major - pitch. Tip lowest = plate bottom - (pitch + 0.5). Bore floor = plate bottom - (pitch + 1.0). Thread top = plate top + pitch / 2. Fit -0.2 to 0.5 mm.
- Files with threads: `-1-cut` and `-2-thread` before the extension.

## Review Focus

- A thread whose hole moved or went from the sketch: the plan must stop, never thread elsewhere or skip silently.
- The thread mill's end and core below the plate: every position of the thread program lies inside what the end mill bored (bore radius, bore floor).
- Hostile numbers (NaN, huge, negative fit, zero-diameter thread mill, 10,000 threads): errors, no panic.
- A job with threads never writes one file while failing the other: both are checked before either is written.
- An end mill too large for the bore, or a thread mill too large for it or too short: an error that says what to change.

---

### Task 1: Thread table, feeds and planner

**Files:** Create `crates/cam/src/router/threads.rs`; modify `crates/cam/src/router.rs` (`pub mod threads;`, sorted), `crates/cam/src/router/auto.rs`; test `crates/cam/src/router_tests.rs`.

**Interfaces (produces):**
- `threads::Thread { name: &'static str, major: f64, pitch: f64 }` with `bore()`, `below()` (= pitch + 0.5), `bored_below()` (= pitch + 1.0); `threads::THREADS: [Thread; 12]`; `threads::find(name: &str) -> Option<Thread>` (trimmed, case-insensitive, `M6x1`-style names accepted for the metric rows).
- `threads::ThreadFeed { rpm, feed, plunge, radial_passes: u32, spring_pass: bool }`; `threads::thread_feed(thread: &Thread, fit: f64, tool: &Tool, material: Material, tier: Tier, machine: &MachineProfile) -> Result<ThreadFeed>`.
- `auto::ThreadedHole { center: Vec2, thread: Thread }`; `auto::Threading { holes: Vec<ThreadedHole>, tool: Tool, fit: f64 }`; `auto::RoundHole { name: String, center: Vec2, diameter: f64 }`; `auto::round_holes(regions: &[Region2]) -> Vec<RoundHole>` (names as the plan's openings); `auto::plan_part_threaded(regions, pockets, threading: Option<&Threading>, tool, cut, machine) -> Result<Planned>`; `plan_part_with` calls it with `None`.
- Thread operations come last in `Planned::operations`, carry the thread mill as their tool, and are named `Thread <name> × <n>`.

- [ ] Tests first (each watched failing): the table's bores (1/4-20 → 5.08, M6 → 5.0, 10-32 → 4.032) and `find`; a threaded hole is bored at the bore size and to a pitch + 1 mm below the plate whatever was drawn, the thread operation is last, uses the thread mill, rises one pitch a turn counter-clockwise from below the plate to above its top, ends at the major radius less the tool's (plus half the fit), stays inside the bore's floor, and posts with no error from `check_text`/`check_program`; feeds follow the formula; a missing hole, an end mill too wide for the bore, a thread mill too wide or too short, a fit outside its range and hostile numbers are issues or errors, never panics; `round_holes` names match the plan's.
- [ ] Implement, run `cargo test -j 1 -p solidcraft-cam --lib router`, commit.

### Task 2: Saved part and commands

**Files:** Modify `crates/doc/src/cam.rs`; create `crates/doc/src/format/pending/cam-part-threads.rs`; modify `crates/engine/src/cmd/cam.rs`.

**Interfaces:**
- Consumes Task 1's `Thread`, `find`, `Threading`, `ThreadedHole`, `plan_part_threaded`, `round_holes`.
- Produces: `CamOperation::Part { threads: Vec<CamPartThread>, thread_tool: Option<u32>, thread_fit: Expr, .. }`, `CamPartThread { at: [f64; 2], thread: String }`, `MAX_PART_THREADS = 500`.
- `cam.cut_out` / `cam.op.part` params: `threads: [{at: [x, y], thread}]`, `thread_mill` (tool number) or `thread_mill_diameter` with `thread_mill_flutes?` (default 3) and `thread_mill_reach?` (default 3 diameters), `thread_fit?`. `cam.cut_out` reply gains `thread: {name, tool, cuts, gcode, readback, extents, issues, path?}`; with threads the files are `-1-cut` and `-2-thread`. New command `cam.holes` (same geometry params, `setup?`) → `{holes: [{name, at: [x, y], diameter}]}`.

- [ ] Tests first: a design with threads round-trips and an older one without the fields still reads; `cam.holes` lists the plate's holes; `cam.cut_out` with a thread writes two checked files (the first bores ⌀5.08 below the plate, the second has helical `G3` arcs with rising Z and only the thread mill), keeps both tools and the choice in the design, and a dry run writes and keeps nothing; a thread at a point with no hole, an unknown thread name, and threads without a thread mill are refused before any file is written.
- [ ] Implement, run the scratch test, commit.

### Task 3: Dialog

**Files:** Modify `crates/ui-egui/src/dialogs_cut_out.rs`, `assets/locales/pending/router-cam.json`.

**Interfaces:** consumes `cam.holes` and the `thread` reply. `CutOut` gains `holes`, `holes_of`, `threads: Vec<([i64; 2], usize)>` (centre in micrometres, index into the thread names), `thread_mills`, `thread_mill`, `tm_diameter`, `tm_flutes`, `tm_reach`, `tm_fit`; `Memory` gains `thread_mills: Vec<ThreadCutter>`.

- [ ] Tests first: the dialog lists the plate's hole with "No thread"; choosing a thread shows the thread mill fields, Check lists the thread file's time and its cut, Save writes both files and remembers the thread mill with its fit; opened again it starts from the part's threads.
- [ ] Implement, add every new string to all six locales, run the scratch test, clippy and fmt, commit.

### Task 4: Proof and record

- [ ] Write a threaded demo plate's two files from a test, run the router skill's `gcode_check.py` on both, and note the result in the spec's "Built so far" and limits.
- [ ] Final review, one fix pass, push `router-layer`.
