//! The regression suite: every failure mode this project has hit live is
//! encoded here as a deterministic test. If you change the state machine
//! and one of these fails, you are about to re-ship a bug that has
//! already burned a real user once.

use super::*;

const RES: f64 = 10.0; // units per mm -> 1 unit = 0.1mm = 1.2px at PX_PER_MM=12

fn timing(drag_end_delay_ms: u64) -> Timing {
    Timing {
        probe_delay: Duration::from_millis(15),
        entry_debounce: Duration::from_millis(50),
        drag_end_delay: Duration::from_millis(drag_end_delay_ms),
        press_grace: Duration::from_millis(75),
        px_per_mm: PX_PER_MM,
        // 0.1mm = 1 unit at RES: small enough that the existing motion
        // tests arm on their first move, big enough to prove the deadzone.
        drag_start_mm: 0.1,
    }
}

/// Deterministic-clock harness around the machine.
struct Sim {
    m: GestureMachine,
    now: Instant,
}

impl Sim {
    fn new() -> Self {
        Self::with_delay(0)
    }
    fn with_delay(drag_end_delay_ms: u64) -> Self {
        Sim {
            m: GestureMachine::new(timing(drag_end_delay_ms), RES, RES, 16),
            now: Instant::now(),
        }
    }
    /// A sim with a chosen device resolution and drag-start deadzone --
    /// used to exercise sub-threshold finger jitter at a fine enough
    /// resolution that integer positions land well inside the deadzone.
    fn with_res_threshold(res: f64, drag_start_mm: f64) -> Self {
        let mut t = timing(0);
        t.drag_start_mm = drag_start_mm;
        Sim {
            m: GestureMachine::new(t, res, res, 16),
            now: Instant::now(),
        }
    }
    /// Advance the clock and deliver the tick the I/O loop would.
    fn tick(&mut self, ms: u64) -> Vec<Output> {
        self.now += Duration::from_millis(ms);
        self.m.on_tick(self.now)
    }
    /// Deliver one frame (SYN_REPORT appended automatically).
    fn frame(&mut self, evs: &[Ev]) -> Vec<Output> {
        let mut f = evs.to_vec();
        f.push(Ev::syn());
        self.m.on_frame(&f, self.now)
    }
    /// Advance the clock, then deliver a frame.
    fn frame_at(&mut self, ms: u64, evs: &[Ev]) -> Vec<Output> {
        self.now += Duration::from_millis(ms);
        self.frame(evs)
    }
}

// -- tiny event builders -------------------------------------------------

fn down(slot: i32, id: i32, x: i32, y: i32) -> Vec<Ev> {
    vec![
        Ev::abs(ABS_MT_SLOT, slot),
        Ev::abs(ABS_MT_TRACKING_ID, id),
        Ev::abs(ABS_MT_POSITION_X, x),
        Ev::abs(ABS_MT_POSITION_Y, y),
    ]
}

fn up(slot: i32) -> Vec<Ev> {
    vec![Ev::abs(ABS_MT_SLOT, slot), Ev::abs(ABS_MT_TRACKING_ID, -1)]
}

fn mv(slot: i32, x: i32, y: i32) -> Vec<Ev> {
    vec![
        Ev::abs(ABS_MT_SLOT, slot),
        Ev::abs(ABS_MT_POSITION_X, x),
        Ev::abs(ABS_MT_POSITION_Y, y),
    ]
}

fn cat(parts: &[&[Ev]]) -> Vec<Ev> {
    parts.iter().flat_map(|p| p.iter().copied()).collect()
}

// -- output inspectors ---------------------------------------------------

fn mouse_downs(outs: &[Output]) -> usize {
    outs.iter()
        .filter(|o| matches!(o, Output::MouseDown))
        .count()
}

fn mouse_ups(outs: &[Output]) -> usize {
    outs.iter().filter(|o| matches!(o, Output::MouseUp)).count()
}

fn middle_clicks(outs: &[Output]) -> usize {
    outs.iter()
        .filter(|o| matches!(o, Output::MiddleClick))
        .count()
}

fn synth_events(outs: &[Output]) -> Vec<Ev> {
    outs.iter()
        .filter_map(|o| match o {
            Output::EmitSynth(evs) => Some(evs.clone()),
            _ => None,
        })
        .flatten()
        .collect()
}

fn total_move(outs: &[Output]) -> (i32, i32) {
    outs.iter().fold((0, 0), |acc, o| match o {
        Output::MouseMove { dx, dy } => (acc.0 + dx, acc.1 + dy),
        _ => acc,
    })
}

fn collect(mut acc: Vec<Output>, more: Vec<Output>) -> Vec<Output> {
    acc.extend(more);
    acc
}

/// Drive a full staggered 3-finger touchdown into a committed drag, then
/// actually press the button. The button is now pressed only by real drag
/// motion, so we nudge the reference finger out and back: net-zero motion
/// (baseline and carry return to where they started) but a genuine
/// MouseDown along the way -- exactly the "active, pressed drag" state the
/// callers depend on. Returns everything emitted along the way.
fn start_drag(sim: &mut Sim) -> Vec<Output> {
    let mut outs = commit_drag_only(sim);
    // baseline is at x=500 (the touchdown position); move +2 units (0.2mm,
    // past the 0.1mm test deadzone) to arm and press, then back to 500 so
    // the net motion is zero and the baseline/carry are exactly where the
    // callers expect them.
    outs = collect(outs, sim.frame_at(5, &mv(0, 502, 500)));
    outs = collect(outs, sim.frame_at(5, &mv(0, 500, 500)));
    outs
}

/// Like start_drag, but stops right after the drag commits -- inside the
/// press-grace window, before any button press.
fn commit_drag_only(sim: &mut Sim) -> Vec<Output> {
    let mut outs = sim.frame(&down(0, 100, 500, 500));
    outs = collect(outs, sim.frame_at(5, &down(1, 101, 600, 500)));
    outs = collect(outs, sim.frame_at(5, &down(2, 102, 700, 500)));
    outs = collect(outs, sim.tick(45)); // debounce window closes: committed
    outs
}

// =========================================================================
// touchdown classification
// =========================================================================

/// Fingers land asynchronously (the normal case). The compositor must
/// see NOTHING -- the historical bug was the transient 1-2 finger states
/// leaking out as phantom taps / right-clicks.
#[test]
fn staggered_3finger_touchdown_becomes_drag_without_leaking() {
    let mut sim = Sim::new();
    let outs = start_drag(&mut sim);
    assert_eq!(mouse_downs(&outs), 1, "drag must press the button once");
    assert!(
        synth_events(&outs).is_empty(),
        "no trace of the 3-finger touch may reach the compositor, got {:?}",
        synth_events(&outs)
    );
}

/// A quick 3-finger tap (ends before the debounce window) is NOT a drag
/// and NOT a left click: it is the macOS/libinput "3-finger tap = middle
/// click". We own that click rather than leaking the withheld touchdowns
/// to the compositor, so it fires regardless of libinput's tap settings.
#[test]
fn quick_3finger_tap_middle_clicks() {
    let mut sim = Sim::new();
    let f1 = cat(&[
        &down(0, 1, 100, 100),
        &down(1, 2, 200, 100),
        &down(2, 3, 300, 100),
    ]);
    let f2 = cat(&[&up(0), &up(1), &up(2)]);

    let mut outs = sim.frame(&f1);
    outs = collect(outs, sim.frame_at(25, &f2)); // all up at 25ms < 50ms

    assert_eq!(mouse_downs(&outs), 0, "a tap must not start a left drag");
    assert_eq!(middle_clicks(&outs), 1, "a 3-finger tap is a middle click");
    assert!(
        synth_events(&outs).is_empty(),
        "the withheld touchdowns must never reach the compositor, got {:?}",
        synth_events(&outs)
    );
}

/// A 4-finger swipe whose 4th finger lands a beat late passes through a
/// transient "exactly 3" state. It must NOT be classified as a drag.
#[test]
fn four_finger_touchdown_with_late_4th_is_not_a_drag() {
    let mut sim = Sim::new();
    let mut outs = sim.frame(&down(0, 1, 100, 100));
    outs = collect(outs, sim.frame_at(5, &down(1, 2, 200, 100)));
    outs = collect(outs, sim.frame_at(5, &down(2, 3, 300, 100)));
    outs = collect(outs, sim.frame_at(20, &down(3, 4, 400, 100)));

    assert_eq!(mouse_downs(&outs), 0);
    // reaching 4 settles instantly: the whole touchdown must have flushed
    assert!(
        !synth_events(&outs).is_empty(),
        "4-finger touch must be released to the compositor"
    );

    // and staying longer changes nothing
    let outs = sim.tick(100);
    assert_eq!(mouse_downs(&outs), 0);
}

/// THE 4->3 LIFTOFF BUG: fingers never lift in unison, so the tail of
/// every 4-finger swipe passes through exactly 3 active fingers. That
/// moment must NOT hijack the gesture into a drag (which ended as a
/// phantom click at the end of every 4-finger desktop switch).
#[test]
fn four_finger_liftoff_through_three_is_not_hijacked() {
    let mut sim = Sim::new();
    // full 4-finger touchdown, settled
    let mut outs = sim.frame(&cat(&[
        &down(0, 1, 100, 100),
        &down(1, 2, 200, 100),
        &down(2, 3, 300, 100),
        &down(3, 4, 400, 100),
    ]));
    // swipe...
    outs = collect(
        outs,
        sim.frame_at(30, &cat(&[&mv(0, 150, 100), &mv(1, 250, 100)])),
    );
    // ...and a staggered liftoff: 4 -> 3 -> 2 -> 1 -> 0
    outs = collect(outs, sim.frame_at(10, &up(3)));
    outs = collect(outs, sim.frame_at(8, &up(2)));
    outs = collect(outs, sim.frame_at(8, &up(1)));
    outs = collect(outs, sim.frame_at(8, &up(0)));
    outs = collect(outs, sim.tick(100));

    assert_eq!(
        mouse_downs(&outs),
        0,
        "4-finger liftoff passing through count==3 must never become a drag"
    );
    assert_eq!(mouse_ups(&outs), 0);
    // every liftoff frame must have been relayed (the compositor needs
    // to see the gesture end)
    let release_count = synth_events(&outs)
        .iter()
        .filter(|e| e.code == ABS_MT_TRACKING_ID && e.value == -1)
        .count();
    assert_eq!(
        release_count, 4,
        "all four liftoffs must reach the compositor"
    );
}

/// A lone finger with intentional pointer motion settles after the short
/// probe and is then live -- ordinary pointer movement must never feel
/// debounced.
#[test]
fn one_finger_settles_after_probe_then_relays_live() {
    let mut sim = Sim::new();
    let outs = sim.frame(&down(0, 7, 100, 100));
    assert!(
        synth_events(&outs).is_empty(),
        "still inside the probe window"
    );

    let outs = sim.frame_at(5, &mv(0, 106, 100)); // 0.6mm: intentional motion
    assert!(
        synth_events(&outs).is_empty(),
        "still inside the 15ms probe"
    );

    let outs = sim.tick(10);
    assert!(
        !synth_events(&outs).is_empty(),
        "probe over: buffered touch flushes"
    );

    // subsequent motion relays immediately, same frame
    let outs = sim.frame_at(5, &mv(0, 110, 100));
    assert_eq!(
        synth_events(&outs).len(),
        4,
        "live motion must relay instantly"
    );
}

/// THE PHANTOM RIGHT-CLICK RACE: the first finger of a 3-finger tap can
/// rest longer than the short pointer probe before the other two land. If
/// that stationary finger is exposed at 15ms, the second finger is exposed
/// too, and withdrawing both when the third arrives makes libinput emit a
/// 2-finger tap (right-click). A stationary first finger must remain buffered
/// for the full entry window so the complete tap is owned as a middle click.
#[test]
fn late_landing_stationary_3finger_tap_never_leaks_as_right_click() {
    let mut sim = Sim::new();
    let mut outs = sim.frame(&down(0, 1, 100, 100));
    outs = collect(outs, sim.tick(15));
    assert!(
        synth_events(&outs).is_empty(),
        "stationary first finger must remain protected after probe"
    );

    outs = collect(outs, sim.frame_at(7, &down(1, 2, 200, 100)));
    outs = collect(outs, sim.frame_at(7, &down(2, 3, 300, 100)));
    outs = collect(outs, sim.frame_at(10, &cat(&[&up(0), &up(1), &up(2)])));

    assert_eq!(middle_clicks(&outs), 1, "3-finger tap must middle-click");
    assert!(
        synth_events(&outs).is_empty(),
        "no 1- or 2-finger prefix may reach libinput: {:?}",
        synth_events(&outs)
    );
}

/// A stationary one-finger hold still becomes live; it merely uses the full
/// classification window because there was no pointer motion proving that it
/// could not be the start of a staggered multi-finger tap.
#[test]
fn stationary_one_finger_settles_at_entry_debounce() {
    let mut sim = Sim::new();
    sim.frame(&down(0, 7, 100, 100));
    assert!(synth_events(&sim.tick(49)).is_empty());
    assert!(!synth_events(&sim.tick(1)).is_empty());
}

/// Two fingers landing staggered (a scroll) must flush after the
/// debounce and never become a drag.
#[test]
fn two_finger_scroll_relays_after_debounce() {
    let mut sim = Sim::new();
    let mut outs = sim.frame(&down(0, 1, 100, 100));
    outs = collect(outs, sim.frame_at(8, &down(1, 2, 200, 100)));
    outs = collect(outs, sim.tick(50));

    assert_eq!(mouse_downs(&outs), 0);
    assert!(
        !synth_events(&outs).is_empty(),
        "scroll touch must flush to compositor"
    );

    // live scrolling from here on
    let outs = sim.frame_at(5, &cat(&[&mv(0, 100, 120), &mv(1, 200, 120)]));
    assert!(!synth_events(&outs).is_empty());
}

/// Growing an already-settled touch to exactly 3 (1 -> settle -> +2)
/// still becomes a drag, and the already-relayed slots are explicitly
/// released on the clone first (never left half-open). The relayed tool
/// state (BTN_TOUCH etc.) must be released too: with tap-to-click on, a
/// slot release without its tool release reads as a tap (phantom
/// click), and stuck BTN_TOOL_* bits desync libinput's finger counting.
#[test]
fn growth_to_three_after_settle_becomes_drag_with_clean_release() {
    const BTN_TOUCH: u16 = 0x14a;
    let mut sim = Sim::new();
    let f = cat(&[&down(0, 1, 100, 100), &[Ev::new(EV_KEY, BTN_TOUCH, 1)][..]]);
    sim.frame(&f);
    sim.frame_at(5, &mv(0, 106, 100)); // intentional pointer motion
    sim.tick(10); // settles as 1-finger, flushed live (incl. BTN_TOUCH=1)

    let mut outs = sim.frame_at(10, &cat(&[&down(1, 2, 200, 100), &down(2, 3, 300, 100)]));
    let evs = synth_events(&outs);
    assert!(
        evs.contains(&Ev::abs(ABS_MT_TRACKING_ID, -1)),
        "the relayed slot must be released on the clone before suppressing"
    );
    assert!(
        evs.contains(&Ev::new(EV_KEY, BTN_TOUCH, 0)),
        "relayed tool state must be released along with the slots: {evs:?}"
    );

    // the press is deferred, but motion commits it
    outs = sim.frame_at(10, &mv(0, 150, 100));
    assert_eq!(
        mouse_downs(&outs),
        1,
        "late growth to 3 is a deliberate drag"
    );
}

// =========================================================================
// deferred press & the late-4th-finger bailout
// =========================================================================

/// A fast 4-finger swipe staggers its fingers hard: the 4th can land
/// AFTER the entry window closed on a stable-looking 3-finger touch.
/// The committed drag must abort -- with no click, since nothing had
/// pressed the button yet -- and the touch must be handed to the
/// compositor mid-gesture so the rest of the swipe still registers.
#[test]
fn late_4th_finger_aborts_drag_without_click() {
    const BTN_TOUCH: u16 = 0x14a;
    let mut sim = Sim::new();
    let mut outs = sim.frame(&cat(&[
        &down(0, 1, 100, 100),
        &[Ev::new(EV_KEY, BTN_TOUCH, 1)][..],
    ]));
    outs = collect(outs, sim.frame_at(5, &down(1, 2, 200, 100)));
    outs = collect(outs, sim.frame_at(5, &down(2, 3, 300, 100)));
    outs = collect(outs, sim.tick(45)); // committed as drag at ~55ms
    assert_eq!(mouse_downs(&outs), 0, "press must be deferred at commit");

    // the 4th finger lands 15ms after commit
    let outs = sim.frame_at(15, &down(3, 4, 400, 100));
    assert_eq!(mouse_downs(&outs), 0, "no press may ever have happened");
    assert_eq!(mouse_ups(&outs), 0);
    let evs = synth_events(&outs);
    let ids: Vec<i32> = evs
        .iter()
        .filter(|e| e.code == ABS_MT_TRACKING_ID)
        .map(|e| e.value)
        .collect();
    assert_eq!(
        ids,
        vec![1, 2, 3, 4],
        "all four fingers must be introduced to the clone: {evs:?}"
    );
    assert!(
        evs.contains(&Ev::new(EV_KEY, BTN_TOUCH, 1)),
        "real tool state must accompany the handoff: {evs:?}"
    );

    // and the rest of the swipe relays live
    let outs = sim.frame_at(10, &cat(&[&mv(0, 100, 200), &mv(1, 200, 200)]));
    assert!(
        !synth_events(&outs).is_empty(),
        "post-handoff motion must relay"
    );
    assert_eq!(mouse_downs(&outs), 0);
}

/// If the drag already pressed (motion happened before the 4th finger),
/// the abort must still recover: release the button and hand off.
#[test]
fn late_4th_after_motion_releases_and_hands_off() {
    let mut sim = Sim::new();
    commit_drag_only(&mut sim);
    let outs = sim.frame_at(10, &mv(0, 520, 500)); // motion presses
    assert_eq!(mouse_downs(&outs), 1);

    let outs = sim.frame_at(10, &down(3, 9, 400, 100));
    assert_eq!(mouse_ups(&outs), 1, "held button must be released on abort");
    let up_idx = outs
        .iter()
        .position(|o| matches!(o, Output::MouseUp))
        .unwrap();
    let synth_idx = outs
        .iter()
        .position(|o| matches!(o, Output::EmitSynth(_)))
        .unwrap();
    assert!(
        up_idx < synth_idx,
        "release before the compositor sees the touch"
    );
}

/// The deferred press must not change what a drag feels like: the press
/// lands in the same output batch as (and before) the first motion.
#[test]
fn deferred_press_lands_before_first_motion() {
    let mut sim = Sim::new();
    commit_drag_only(&mut sim);
    let outs = sim.frame_at(10, &mv(0, 510, 500));
    let down_idx = outs.iter().position(|o| matches!(o, Output::MouseDown));
    let move_idx = outs
        .iter()
        .position(|o| matches!(o, Output::MouseMove { .. }));
    assert!(down_idx.is_some() && move_idx.is_some());
    assert!(
        down_idx < move_idx,
        "MouseDown must precede the motion it enables"
    );
}

/// A stationary 3-finger hold that never moves must NOT press the left
/// button -- no timer does that anymore. Holding still is not a drag.
#[test]
fn stationary_hold_never_left_presses() {
    let mut sim = Sim::new();
    let outs = commit_drag_only(&mut sim);
    assert_eq!(mouse_downs(&outs), 0, "commit alone must not press");
    // however long it rests, an unmoved hold stays unpressed
    let outs = sim.tick(200);
    assert_eq!(
        mouse_downs(&outs),
        0,
        "a motionless hold is not a left drag"
    );
    assert_eq!(middle_clicks(&outs), 0, "nothing fires until liftoff");
}

/// THE BUG THIS FIX ADDRESSES: a 3-finger tap that lingers past the
/// debounce window (an ordinary human tap is easily >50ms) used to commit
/// as a drag and emit a stray LEFT click on liftoff, stealing the normal
/// middle click. It must middle-click instead -- regardless of how long
/// the fingers rested -- as long as they never moved.
#[test]
fn slow_stationary_3finger_tap_middle_clicks() {
    let mut sim = Sim::new();
    commit_drag_only(&mut sim); // committed after the 50ms debounce
    let outs = sim.tick(60); // rest well past the old 75ms grace...
    assert_eq!(mouse_downs(&outs), 0, "still no left press while resting");

    let outs = sim.frame_at(20, &cat(&[&up(0), &up(1), &up(2)])); // ...then lift
    assert_eq!(
        middle_clicks(&outs),
        1,
        "an unmoved 3-finger tap is a middle click"
    );
    assert_eq!(mouse_downs(&outs), 0, "and never a left click");
    assert_eq!(mouse_ups(&outs), 0);
    assert!(
        synth_events(&outs).is_empty(),
        "nothing leaks to the compositor"
    );
}

/// THE UNRELIABILITY BUG: a resting finger always jitters by a fraction
/// of a millimetre. Without a deadzone, whichever frame's noise happened
/// to compute >=1px tripped the button press -- so the *same* tap
/// middle-clicked sometimes and left-click-dragged other times, at random.
/// Sub-threshold jitter (net or peak) must never arm the drag, so a
/// stationary tap middle-clicks every single time.
#[test]
fn jittering_stationary_tap_still_middle_clicks() {
    // 100 units/mm with a 0.5mm deadzone: the jitter below peaks at 0.03mm.
    let mut sim = Sim::with_res_threshold(100.0, 0.5);
    commit_drag_only(&mut sim); // committed at x=500, unpressed

    // resting-finger noise around x=500: small back-and-forth, every
    // excursion far under 0.5mm, netting ~zero
    let mut outs = Vec::new();
    for &x in &[503, 498, 501, 499, 502, 500, 497, 500] {
        outs = collect(outs, sim.frame_at(5, &mv(0, x, 500)));
    }
    assert_eq!(
        mouse_downs(&outs),
        0,
        "sub-threshold jitter must never arm a drag"
    );

    let outs = sim.frame_at(5, &cat(&[&up(0), &up(1), &up(2)]));
    assert_eq!(
        middle_clicks(&outs),
        1,
        "a jittery but stationary tap still middle-clicks"
    );
    assert_eq!(mouse_downs(&outs), 0, "and never a stray left click");
}

/// The flip side: a 3-finger touch that *moves* is a real drag -- left
/// button held, cursor driven, released on liftoff -- and never a middle
/// click. This is the discriminator: motion, not time.
#[test]
fn moving_3finger_touch_is_a_left_drag_not_a_middle_click() {
    let mut sim = Sim::new();
    commit_drag_only(&mut sim);
    let outs = sim.frame_at(10, &mv(0, 520, 500)); // motion commits the press
    assert_eq!(mouse_downs(&outs), 1, "motion presses the left button");
    let (dx, _) = total_move(&outs);
    assert!(dx > 0, "and drives the cursor");

    let outs = sim.frame_at(10, &cat(&[&up(0), &up(1), &up(2)]));
    assert_eq!(mouse_ups(&outs), 1, "liftoff releases the drag");
    assert_eq!(
        middle_clicks(&outs),
        0,
        "a moving drag is never a middle click"
    );
}

// =========================================================================
// drag behavior
// =========================================================================

/// Motion of the reference finger drives the virtual mouse, scaled by
/// resolution and px_per_mm, with sub-pixel remainders carried (not
/// discarded) so slow drags don't lose motion.
#[test]
fn drag_motion_scales_and_carries_subpixels() {
    let mut sim = Sim::new();
    start_drag(&mut sim);

    // 10 units = 1mm = 12px exactly
    let outs = sim.frame_at(10, &mv(0, 510, 500));
    assert_eq!(total_move(&outs), (12, 0));

    // five frames of 0.3px each (0.25 units): trunc-only would emit 0
    // forever; the carry must accumulate to exactly 1px total (0.3*5)
    let mut acc = Vec::new();
    for i in 1..=5 {
        // 0.25 units per frame is below integer resolution; simulate by
        // moving 1 unit every 4th frame is NOT the same thing -- so use
        // y axis: 1 unit = 0.1mm = 1.2px... instead go smaller: 0.4px =
        // not representable. Use repeated 1-unit moves: 1.2px -> 1px + 0.2 carry
        acc = collect(acc, sim.frame_at(8, &mv(0, 510 + i, 500)));
    }
    // 5 units = 0.5mm = 6.0px total; trunc-per-frame would give 5px
    let (dx, _) = total_move(&acc);
    assert_eq!(dx, 6, "sub-pixel carry must not lose motion (got {dx})");
}

/// Once dragging, a partial liftoff (3 -> 2 -> 1) continues the drag
/// with the remaining finger, and NOTHING leaks to the compositor --
/// the historical phantom-right-click bug.
#[test]
fn drag_partial_liftoff_continues_and_never_leaks() {
    let mut sim = Sim::new();
    start_drag(&mut sim);

    let mut outs = sim.frame_at(10, &up(1)); // 3 -> 2
    outs = collect(outs, sim.frame_at(10, &mv(0, 520, 500))); // still drags
    let (dx, _) = total_move(&outs);
    assert!(dx > 0, "drag must continue on remaining fingers");
    assert!(
        synth_events(&outs).is_empty(),
        "leaked events during drag liftoff"
    );

    outs = sim.frame_at(10, &up(0)); // 2 -> 1: re-baselines onto slot 2 (700,500)
    outs = collect(outs, sim.frame_at(10, &mv(2, 710, 500))); // +10 units = 12px
    outs = collect(outs, sim.frame_at(10, &mv(2, 720, 500))); // +10 units = 12px
    let (dx, _) = total_move(&outs);
    // 24px total proves a clean handoff: a stale baseline from slot 0's
    // position (500) would have produced a 210-unit = 252px jump instead
    assert_eq!(
        dx, 24,
        "after reference handoff, motion resumes from new baseline"
    );

    outs = sim.frame_at(10, &up(2)); // all up
    assert_eq!(mouse_ups(&outs), 1, "drag ends when the last finger lifts");
    assert!(synth_events(&outs).is_empty());
}

/// After a drag fully ends, the next touch must get fresh debounce
/// treatment (the bookkeeping-reset bug: inheriting settled/touch_max
/// from the drag would skip protection entirely).
#[test]
fn post_drag_touch_state_is_fully_reset() {
    let mut sim = Sim::new();
    start_drag(&mut sim);
    sim.frame_at(50, &cat(&[&up(0), &up(1), &up(2)]));

    // a new 1-finger touch: must be buffered (not instantly relayed)
    let outs = sim.frame_at(20, &down(4, 50, 400, 400));
    assert!(
        synth_events(&outs).is_empty(),
        "next touch after a drag must be re-classified from scratch"
    );
    // and a new 3-finger touch must become a fresh drag
    let mut sim2 = Sim::new();
    start_drag(&mut sim2);
    sim2.frame_at(50, &cat(&[&up(0), &up(1), &up(2)]));
    let outs = start_drag(&mut sim2);
    assert_eq!(
        mouse_downs(&outs),
        1,
        "a second drag must work after the first"
    );
}

/// One finger moving immediately after a drag (delay = 0) must not drag:
/// the button is already up before the motion is relayed.
#[test]
fn no_drag_smearing_after_drag_with_zero_delay() {
    let mut sim = Sim::new();
    start_drag(&mut sim);
    let outs = sim.frame_at(30, &cat(&[&up(0), &up(1), &up(2)]));
    assert_eq!(
        mouse_ups(&outs),
        1,
        "delay=0 releases immediately on liftoff"
    );

    let mut outs = sim.frame_at(10, &down(0, 60, 100, 100));
    outs = collect(outs, sim.tick(15));
    outs = collect(outs, sim.frame_at(5, &mv(0, 200, 100)));
    assert_eq!(mouse_downs(&outs), 0);
    assert_eq!(mouse_ups(&outs), 0, "button state must not change again");
}

// =========================================================================
// drag-lock (drag_end_delay > 0)
// =========================================================================

/// Lift + re-3-touch inside the window: the SAME hold continues -- no
/// MouseUp, and no duplicate MouseDown.
#[test]
fn drag_lock_resumes_held_drag() {
    let mut sim = Sim::with_delay(300);
    start_drag(&mut sim);
    let outs = sim.frame_at(30, &cat(&[&up(0), &up(1), &up(2)]));
    assert_eq!(
        mouse_ups(&outs),
        0,
        "inside the lock window the button stays held"
    );

    let outs = start_drag(&mut sim); // ~55ms later, still inside 300ms
    assert_eq!(
        mouse_downs(&outs),
        0,
        "resuming a held drag must not re-press"
    );
    assert_eq!(mouse_ups(&outs), 0);

    // and the resumed drag still moves
    let outs = sim.frame_at(10, &mv(0, 520, 500));
    let (dx, _) = total_move(&outs);
    assert!(dx > 0);
}

/// THE DRAG-LOCK REGRESSION: one finger moving during the lock window
/// dragged whatever was under the cursor. The button must be released
/// BEFORE the finger's buffered motion reaches the compositor.
#[test]
fn drag_lock_releases_before_relaying_other_touches() {
    let mut sim = Sim::with_delay(300);
    start_drag(&mut sim);
    sim.frame_at(30, &cat(&[&up(0), &up(1), &up(2)]));

    // one finger lands, moves, and settles inside the lock window
    let mut outs = sim.frame_at(50, &down(0, 70, 100, 100));
    outs = collect(outs, sim.frame_at(5, &mv(0, 106, 100)));
    outs = collect(outs, sim.tick(10));

    assert_eq!(mouse_ups(&outs), 1, "lock must break for a non-drag touch");
    // ordering: MouseUp strictly before the flushed touch events
    let up_idx = outs
        .iter()
        .position(|o| matches!(o, Output::MouseUp))
        .unwrap();
    let synth_idx = outs
        .iter()
        .position(|o| matches!(o, Output::EmitSynth(_)))
        .unwrap();
    assert!(
        up_idx < synth_idx,
        "button must be released BEFORE the compositor sees the new touch"
    );
}

/// No new touch: the lock expires on its own and releases exactly once.
#[test]
fn drag_lock_times_out() {
    let mut sim = Sim::with_delay(300);
    start_drag(&mut sim);
    sim.frame_at(30, &cat(&[&up(0), &up(1), &up(2)]));

    let outs = sim.tick(299);
    assert_eq!(mouse_ups(&outs), 0, "not yet");
    let outs = sim.tick(1);
    assert_eq!(mouse_ups(&outs), 1, "lock expires");
    let outs = sim.tick(500);
    assert_eq!(mouse_ups(&outs), 0, "and only once");
}

// =========================================================================
// SYN_DROPPED recovery
// =========================================================================

/// Resync during a live (settled) touch: the clone gets an explicit
/// correction frame asserting the authoritative positions.
#[test]
fn resync_live_touch_emits_correction() {
    let mut sim = Sim::new();
    sim.frame(&down(0, 1, 100, 100));
    sim.tick(15); // settled live

    let mut snapshot = vec![(-1, 0, 0); 16];
    snapshot[0] = (1, 150, 150); // moved during the drop
    let outs = sim.m.on_resync(&snapshot, sim.now);
    let evs = synth_events(&outs);
    assert!(evs.contains(&Ev::abs(ABS_MT_POSITION_X, 150)));
}

/// Resync that reveals a finger lifted during the drop: the clone must
/// get a release for it, or it holds a phantom touch forever.
#[test]
fn resync_releases_slots_that_lifted_during_the_drop() {
    let mut sim = Sim::new();
    sim.frame(&cat(&[&down(0, 1, 100, 100), &down(1, 2, 200, 100)]));
    sim.tick(50); // settled live 2-finger touch, relayed

    let mut snapshot = vec![(-1, 0, 0); 16];
    snapshot[0] = (1, 100, 100); // slot 1 lifted during the drop
    let outs = sim.m.on_resync(&snapshot, sim.now);
    let evs = synth_events(&outs);
    let has_release = evs
        .windows(2)
        .any(|w| w[0] == Ev::abs(ABS_MT_SLOT, 1) && w[1] == Ev::abs(ABS_MT_TRACKING_ID, -1));
    assert!(
        has_release,
        "slot 1 must be explicitly released on the clone: {evs:?}"
    );
}

/// Resync during a drag must re-baseline instead of applying the gap as
/// one giant cursor jump.
#[test]
fn resync_during_drag_rebaselines_without_jump() {
    let mut sim = Sim::new();
    start_drag(&mut sim);
    sim.frame_at(10, &mv(0, 510, 500)); // establish motion

    let mut snapshot = vec![(-1, 0, 0); 16];
    snapshot[0] = (100, 900, 900); // reference finger "teleported" in the drop
    snapshot[1] = (101, 600, 500);
    snapshot[2] = (102, 700, 500);
    let outs = sim.m.on_resync(&snapshot, sim.now);
    assert_eq!(
        total_move(&outs),
        (0, 0),
        "the drop gap must not become a jump"
    );

    // next real motion moves normally from the new (900,900) baseline
    let outs = sim.frame_at(10, &mv(0, 910, 910));
    assert_eq!(total_move(&outs), (12, 12));
}

/// Resync revealing the whole touch ended during the drop while
/// dragging: the drag must end (button released), not hang forever.
#[test]
fn resync_with_all_lifted_ends_drag() {
    let mut sim = Sim::new();
    start_drag(&mut sim);

    let snapshot = vec![(-1, 0, 0); 16];
    let outs = sim.m.on_resync(&snapshot, sim.now);
    assert_eq!(
        mouse_ups(&outs),
        1,
        "drag must end when resync shows no fingers"
    );
}

/// Resync revealing a still-buffered touch fully ended during the drop:
/// the truncated tap is swallowed (never half-relayed), leaving the
/// clone consistent.
#[test]
fn resync_with_all_lifted_during_pending_swallows_cleanly() {
    let mut sim = Sim::new();
    sim.frame(&cat(&[&down(0, 1, 100, 100), &down(1, 2, 200, 100)]));

    let snapshot = vec![(-1, 0, 0); 16];
    let outs = sim.m.on_resync(&snapshot, sim.now);
    assert!(
        synth_events(&outs).is_empty(),
        "a touchdown whose release was dropped must not be flushed"
    );

    // and the machine is fully usable afterwards
    let outs = sim.frame_at(10, &down(0, 9, 100, 100));
    assert!(synth_events(&outs).is_empty()); // buffered, fresh touch
    let outs = sim.tick(50);
    assert!(!synth_events(&outs).is_empty()); // settles normally
}

// =========================================================================
// robustness details
// =========================================================================

/// Out-of-range slot indices must clamp, not panic or corrupt memory.
#[test]
fn out_of_range_slot_clamps() {
    let mut sim = Sim::new();
    sim.frame(&[
        Ev::abs(ABS_MT_SLOT, 99),
        Ev::abs(ABS_MT_TRACKING_ID, 5),
        Ev::abs(ABS_MT_SLOT, -3),
        Ev::abs(ABS_MT_TRACKING_ID, -1),
    ]);
    // no panic = pass; also the machine must still classify sanely
    let outs = sim.frame_at(5, &down(0, 1, 100, 100));
    assert!(synth_events(&outs).is_empty());
}

/// A device with fewer slots than MAX_SLOTS must not grow phantom
/// touches from snapshot entries beyond its real slot range.
#[test]
fn small_slot_count_has_no_phantom_slots() {
    let mut m = GestureMachine::new(timing(0), RES, RES, 5);
    let now = Instant::now();
    // kernel snapshot buffers are MAX_SLOTS long; entries past the
    // device's 5 real slots arrive zeroed (tracking_id 0 looks "active")
    let snapshot = [(0, 0, 0); 16];
    // slots 0-4 zeroed = 5 "active" (a genuinely full pad); the machine
    // must at least never count slots 5-15
    m.on_resync(&snapshot[..5.min(snapshot.len())], now);
    assert_eq!(m.active_count(), 5);
    let m2 = GestureMachine::new(timing(0), RES, RES, 5);
    assert_eq!(m2.slot_count, 5);
}

/// next_deadline steers the event loop: debounce deadline for a stationary
/// lone finger, short probe deadline after pointer motion, none while dragging
/// or idle, and the lock deadline while a lock is pending.
#[test]
fn next_deadline_tracks_state() {
    let mut sim = Sim::new();
    assert_eq!(sim.m.next_deadline(), None, "idle: nothing scheduled");

    sim.frame(&down(0, 1, 100, 100));
    let touch_start = sim.now;
    let d = sim.m.next_deadline().expect("stationary debounce deadline");
    assert_eq!(d, touch_start + Duration::from_millis(50));

    sim.frame_at(5, &mv(0, 106, 100));
    let d = sim.m.next_deadline().expect("pointer probe deadline");
    assert_eq!(d, touch_start + Duration::from_millis(15));

    sim.frame_at(5, &down(1, 2, 200, 100));
    let d = sim.m.next_deadline().expect("debounce deadline");
    assert_eq!(d, touch_start + Duration::from_millis(50));

    let mut sim = Sim::new();
    commit_drag_only(&mut sim);
    assert_eq!(
        sim.m.next_deadline(),
        None,
        "a committed drag is purely event-driven -- no timed press"
    );

    let mut sim = Sim::with_delay(300);
    start_drag(&mut sim);
    sim.frame_at(30, &cat(&[&up(0), &up(1), &up(2)]));
    let d = sim.m.next_deadline().expect("lock deadline");
    assert_eq!(d, sim.now + Duration::from_millis(300));
}

/// Non-touch events inside frames (BTN_TOUCH, BTN_TOOL_*, legacy ABS_X)
/// ride along verbatim: buffered while pending, relayed when live,
/// withheld while dragging.
#[test]
fn non_mt_events_ride_along_with_their_frames() {
    const EV_KEY: u16 = 0x01;
    const BTN_TOUCH: u16 = 0x14a;
    let mut sim = Sim::new();
    let f = cat(&[&down(0, 1, 100, 100), &[Ev::new(EV_KEY, BTN_TOUCH, 1)][..]]);
    sim.frame(&f);
    let outs = sim.tick(50);
    assert!(
        synth_events(&outs).contains(&Ev::new(EV_KEY, BTN_TOUCH, 1)),
        "key events must flush with their touch"
    );
}

/// button_held reflects reality for the shutdown safety-release.
#[test]
fn button_held_tracks_drag_state() {
    let mut sim = Sim::new();
    assert!(!sim.m.button_held());
    start_drag(&mut sim);
    assert!(sim.m.button_held());
    sim.frame_at(30, &cat(&[&up(0), &up(1), &up(2)]));
    assert!(!sim.m.button_held());
}
