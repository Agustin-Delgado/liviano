//! How Nea keeps its memory low: the stages of a tab out of sight, the
//! window while minimized, and what changes when the system is low on
//! memory.
//!
//! A tab out of sight goes through three stages, each cheaper than the one
//! before and each slower to come back from:
//!
//! 1. Lowered, after `LOWER_AFTER`: the engine may drop what it can make
//!    again (caches, decoded images, garbage). Scripts and timers go on.
//!    Coming back costs a few milliseconds of decoding.
//! 2. Suspended, after the "suspend" setting: scripts and timers stop, and
//!    the engine trims the page's memory. Coming back is a resume. Just
//!    before, the view is parked (`park_step`), so that the GPU process
//!    lets go of the surfaces that drew it.
//! 3. Discarded: the view and its renderer are closed and only the
//!    address, the title and the scroll position (`ScrollMemo`) stay.
//!    Coming back loads the page again, and the page loses its history,
//!    its form fields and the state of its scripts.
//!
//! Stage 2 gives back the working set (the memory of the computer that the
//! page uses) but not the commit (space that the system keeps for it, in
//! the page file if necessary). Thus a suspended tab is almost free while
//! the system has commit to give, and stage 3 is not a stage in time by
//! default. The "discard after" and "live tabs limit" settings bring back
//! a discard by time and by number. Both are off by default. Tabs that
//! are never discarded (pinned, private, playing sound, kept awake) are
//! not counted.
//!
//! The tabs on screen stay as they are. While the window is minimized, they
//! are hidden after `MINIMIZED_HIDE_AFTER` and then follow stages 1 and 2
//! from the time the window was minimized, but are never discarded: they
//! are the pages the person sees first when the window comes back.
//!
//! When the system is low on memory (`Pressure::Low`: the load setting, or
//! a commit near its limit), every tab out of sight is suspended at once
//! and only `PRESSURE_LIVE_TABS` views out of sight stay live. A discard
//! of a suspended tab gives back commit, not memory in use, so the other
//! tabs are discarded only when the system has almost nothing left
//! (`Pressure::High`: the low memory signal of Windows, or the commit at
//! its limit). In both cases the app gives back its own working set, and
//! no spare view is made ahead of time.

use std::{
    collections::{HashMap, HashSet},
    time::{Duration, Instant},
};

use windows::Win32::{
    Foundation::{HWND, LPARAM, WPARAM},
    System::{
        Memory::{
            CreateMemoryResourceNotification, HeapOptimizeResources, HeapSetInformation, LowMemoryResourceNotification,
        },
        ProcessStatus::EmptyWorkingSet,
        Threading::{
            GetCurrentProcess, INFINITE, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SET_QUOTA,
            WaitForSingleObject,
        },
    },
    UI::WindowsAndMessaging::PostMessageW,
};

/// A hidden tab is lowered this long after it left the screen. Shorter, and
/// a quick look back at the last tab decodes its images again.
pub const LOWER_AFTER: Duration = Duration::from_secs(20);
/// A window minimized for this long hides its pages. A minimize that comes
/// right back (a look at the desktop) costs nothing.
pub const MINIMIZED_HIDE_AFTER: Duration = Duration::from_secs(3);
/// After a low memory signal, the next one is waited for this long later.
/// The signal stays on while memory is low, and the sleep timer already
/// looks at the load every few seconds.
pub const PRESSURE_RECHECK: Duration = Duration::from_secs(30);
/// A window on screen that got no input for this long gives back the app's
/// memory, one time for each such pause. The person is away: the first
/// paint after it comes back takes the pages back from memory.
pub const IDLE_TRIM_AFTER: Duration = Duration::from_secs(120);

/// Whether the app gives back its memory now: nothing came (no input and no
/// script request) for `quiet`, at least `IDLE_TRIM_AFTER`, and the last trim
/// (`since_trim` ago) came before this pause started. One trim for each pause.
pub fn idle_trim_due(quiet: Duration, since_trim: Option<Duration>) -> bool {
    quiet >= IDLE_TRIM_AFTER && since_trim.is_none_or(|t| t > quiet)
}

/// How low the system is on memory.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum Pressure {
    #[default]
    None,
    /// Tabs out of sight are suspended at once, and only
    /// `PRESSURE_LIVE_TABS` of them stay live.
    Low,
    /// Each tab out of sight that may be discarded is discarded at once.
    High,
}

/// The commit of the system (percent of its limit) from which it is low.
/// Suspended tabs keep their commit, so many of them can fill a small page
/// file.
pub const COMMIT_LOW_PERCENT: u32 = 90;
/// The commit from which the system has almost nothing left: at the limit,
/// an allocation fails in any program.
pub const COMMIT_HIGH_PERCENT: u32 = 97;
/// The live views out of sight that stay while the system is low on
/// memory (half of them after `LONG_OUT_OF_SIGHT`): the tabs that the
/// person goes back and forth between.
pub const PRESSURE_LIVE_TABS: usize = 8;

/// How low the system is, from its memory load and its commit (percent)
/// and the "memory pressure" setting.
pub fn pressure(memory_load: u32, commit_load: u32, memory_pressure_percent: u32) -> Pressure {
    if commit_load >= COMMIT_HIGH_PERCENT {
        Pressure::High
    } else if memory_load >= memory_pressure_percent || commit_load >= COMMIT_LOW_PERCENT {
        Pressure::Low
    } else {
        Pressure::None
    }
}

/// The live views out of sight that stay at most: the "live tabs limit"
/// setting (0: no limit), and `PRESSURE_LIVE_TABS` while the system is low
/// on memory. None: all of them stay.
pub fn live_limit(setting: usize, pressure: Pressure) -> Option<usize> {
    let set = (setting > 0).then_some(setting);
    let low = (pressure != Pressure::None).then_some(PRESSURE_LIVE_TABS);
    match (set, low) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

/// The commit in use as a percent of the commit limit.
pub fn commit_percent(limit: u64, available: u64) -> u32 {
    if limit == 0 {
        return 0;
    }
    (limit.saturating_sub(available).saturating_mul(100) / limit) as u32
}

/// When a tab out of sight goes to its next stage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Policy {
    pub lower_after: Duration,
    pub suspend_after: Duration,
    /// None: a tab is never discarded for its time out of sight.
    pub discard_after: Option<Duration>,
}

impl Policy {
    /// `discard_after_mins` 0: no discard by time.
    pub fn new(suspend_after_secs: u64, discard_after_mins: u64) -> Self {
        let suspend_after = Duration::from_secs(suspend_after_secs);
        Policy {
            // A suspend set sooner than the lower stage takes its place.
            lower_after: LOWER_AFTER.min(suspend_after),
            suspend_after,
            discard_after: (discard_after_mins > 0).then(|| Duration::from_secs(discard_after_mins * 60)),
        }
    }
}

/// What the policy needs to know about one tab with a live view that is out
/// of sight, or on screen in a minimized window.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Facts {
    /// How long since the tab was last on screen.
    pub idle: Duration,
    /// On screen in a minimized window: it may sleep but not be discarded.
    pub minimized_on_screen: bool,
    /// A site the person keeps awake.
    pub keep_awake: bool,
    pub pinned: bool,
    pub private: bool,
    pub playing_audio: bool,
    pub lowered: bool,
    pub suspended: bool,
}

/// The next stage of one tab.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Keep,
    Lower,
    Suspend,
    Discard,
}

/// The next stage of a tab, now.
pub fn step(facts: &Facts, policy: &Policy, pressure: Pressure) -> Step {
    if facts.keep_awake {
        return Step::Keep;
    }
    // Pinned and private tabs may sleep but are never destroyed; a page that
    // plays sound is never cut off.
    let may_discard = !facts.pinned && !facts.private && !facts.playing_audio && !facts.minimized_on_screen;
    if may_discard && (pressure == Pressure::High || policy.discard_after.is_some_and(|after| facts.idle >= after)) {
        return Step::Discard;
    }
    if !facts.suspended && (pressure != Pressure::None || facts.idle >= policy.suspend_after) {
        return Step::Suspend;
    }
    if !facts.suspended && !facts.lowered && facts.idle >= policy.lower_after {
        return Step::Lower;
    }
    Step::Keep
}

/// A view is parked this long before it is suspended: time for the engine
/// to draw one frame at the small size. Each sleep tick that has work left
/// comes 250 ms or more after the last one.
pub const PARK_FOR: Duration = Duration::from_millis(200);

/// The next part of a suspend for one view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Park {
    /// Park the view now.
    Start,
    /// The view is parked; the frame at the small size may not be drawn yet.
    Wait,
    /// Suspend the view now.
    Done,
}

/// The engine keeps the surfaces of a hidden view at the size that it last
/// drew (approximately 11 MB of GPU process commit for each view at
/// 1400 x 900). It lets them go only after it draws the view at a new
/// size, and it draws only a visible view. Thus a view that is due for a
/// suspend is first parked (`WebTab::park`): visible again, but out of the
/// window and 1 pixel high. After `PARK_FOR`, it is hidden and suspended.
/// `may_park` is false under memory pressure (each tab goes to its last
/// stage at once) and for a page on screen in a minimized window (it comes
/// back first, at its size).
pub fn park_step(may_park: bool, parked_for: Option<Duration>) -> Park {
    match parked_for {
        _ if !may_park => Park::Done,
        None => Park::Start,
        Some(time) if time < PARK_FOR => Park::Wait,
        Some(_) => Park::Done,
    }
}

/// Where a view is parked, from its bounds in the window: above the
/// window's client area, where the system clips it away, with the same
/// width and 1 pixel high.
pub fn parked_bounds(bounds: windows::Win32::Foundation::RECT) -> windows::Win32::Foundation::RECT {
    let width = (bounds.right - bounds.left).max(1);
    windows::Win32::Foundation::RECT { left: 0, top: -PARK_OFFSET, right: width, bottom: -PARK_OFFSET + 1 }
}

/// How far above the client area a parked view is.
const PARK_OFFSET: i32 = 1000;

/// A view out of sight for this long counts against half of the cap: the
/// person went through the recent tabs a while ago, not now.
pub const LONG_OUT_OF_SIGHT: Duration = Duration::from_secs(5 * 60);

/// Of the tabs out of sight that may be discarded (when each was last on
/// screen, and its id), the ones beyond the `cap` seen most recently: the
/// least recently seen first. A suspended view still holds its commit, so
/// only a cap on their number bounds that total. The app applies it only
/// when there is a limit (`live_limit`).
///
/// The views out of sight for `LONG_OUT_OF_SIGHT` or more keep only half of
/// the cap (rounded up): eight tabs looked at one after the other stay
/// live while the person goes back and forth, but not for the rest of the
/// day. Each one costs approximately 60 MB of commit in the engine.
pub fn over_cap(candidates: &[(Instant, u64)], cap: usize, now: Instant) -> Vec<(Instant, u64)> {
    let mut newest_first = candidates.to_vec();
    newest_first.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    let long_cap = cap.div_ceil(2);
    let mut over: Vec<(Instant, u64)> = newest_first
        .iter()
        .enumerate()
        .filter(|&(i, &(since, _))| {
            i >= cap || (i >= long_cap && now.saturating_duration_since(since) >= LONG_OUT_OF_SIGHT)
        })
        .map(|(_, c)| *c)
        .collect();
    over.reverse();
    over
}

/// After the trims of `ENGINE_TRIM_AGAIN`, one more each this long while
/// there are suspended pages. The renderer of a heavy page (frames of
/// other sites, workers) continues to bring memory back for minutes after
/// its suspend: 20 real pages went from 238 MB to 583 MB in 3 min.
pub const ENGINE_TRIM_EVERY: Duration = Duration::from_secs(30);

/// The engine can say no to a suspend (a page that plays media, for
/// example). Nea asks again this long after.
pub const SUSPEND_AGAIN_AFTER: Duration = Duration::from_secs(30);

/// The views out of sight that stay awake (not suspended) at most. A view
/// that is awake costs 30 to 40 MB of memory in its renderer; a suspended
/// one comes back in tens of milliseconds, as it was. Thus the tabs that
/// the person goes back and forth between stay awake, and the others are
/// suspended at once, before the "suspend" time.
pub const AWAKE_TABS: usize = 1;

/// Of the views out of sight that are awake (when each was last on screen,
/// and its id), the ones beyond the `cap` seen most recently: the least
/// recently seen first.
pub fn over_awake(awake: &[(Instant, u64)], cap: usize) -> Vec<u64> {
    let mut newest_first = awake.to_vec();
    newest_first.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    newest_first.iter().skip(cap).rev().map(|(_, id)| *id).collect()
}

/// The engine trims a suspended page, but its renderer keeps some
/// megabytes in memory (4 MB for each tab in the test, 30 tabs). The
/// renderer does not run, so the system can take all of it: this long
/// after a suspend, Nea empties the working set of the renderers that
/// serve only suspended pages. The suspend is not complete at once.
pub const ENGINE_TRIM_AFTER: Duration = Duration::from_secs(2);
/// The engine works again in the renderer of a page approximately 20 s
/// after its suspend, and that brings back 12 to 15 MB for each tab, all
/// that a trim gave to the system (30 tabs: 374 MB at 16 s, 717 MB at
/// 26 s). Thus more trims come, close together in that time: each one this
/// long after the one before.
pub const ENGINE_TRIM_AGAIN: [Duration; 6] = [
    Duration::from_secs(18),
    Duration::from_secs(5),
    Duration::from_secs(5),
    Duration::from_secs(5),
    Duration::from_secs(10),
    Duration::from_secs(35),
];

/// Of the renderer processes (the id of each one, and the main frame of
/// each page that has a frame in it), the ones that serve only suspended
/// pages. `suspended`: the main frames of the suspended pages. A renderer
/// with no frame (a spare one, a worker) and a renderer that a live page
/// also uses stay as they are: a trim of a process that runs makes it
/// slow.
pub fn renderers_to_trim(renderers: &[(u32, Vec<u32>)], suspended: &HashSet<u32>) -> Vec<u32> {
    renderers
        .iter()
        .filter(|(_, pages)| !pages.is_empty() && pages.iter().all(|page| suspended.contains(page)))
        .map(|(pid, _)| *pid)
        .collect()
}

/// Gives the working set of another process back to the system. The pages
/// come back from the standby list or the page file when the process uses
/// them again.
pub fn trim_process(pid: u32) {
    unsafe {
        let Ok(process) = OpenProcess(PROCESS_SET_QUOTA | PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return;
        };
        let _ = EmptyWorkingSet(process);
        let _ = windows::Win32::Foundation::CloseHandle(process);
    }
}

/// Where each page was scrolled when it last left the screen. A tab that
/// was discarded since loads its page again when it comes back, and the
/// page then goes to the same place: the return is a reload, but the
/// person does not have to look for where they were. Only the position of
/// the page itself is kept (not of a box that scrolls inside it), and only
/// while the address is the same.
#[derive(Default)]
pub struct ScrollMemo {
    saved: HashMap<u64, SavedScroll>,
}

struct SavedScroll {
    url: String,
    x: i64,
    y: i64,
    /// The view was made again after a discard: its next load restores.
    woken: bool,
}

impl ScrollMemo {
    /// The page of tab `id` (at `url`) was at (`x`, `y`) when it left the
    /// screen. The top left corner needs nothing.
    pub fn left(&mut self, id: u64, url: &str, x: i64, y: i64) {
        if x <= 0 && y <= 0 {
            self.saved.remove(&id);
            return;
        }
        self.saved.insert(id, SavedScroll { url: url.to_string(), x, y, woken: false });
    }

    /// The tab gets a new view after a discard: its page loads again.
    pub fn woke(&mut self, id: u64) {
        if let Some(saved) = self.saved.get_mut(&id) {
            saved.woken = true;
        }
    }

    /// A page of tab `id` finished loading at `url`. After a discard, the
    /// place to go to, once: a later load (a link, a reload) keeps its own.
    pub fn arrived(&mut self, id: u64, url: &str, ok: bool) -> Option<(i64, i64)> {
        if !self.saved.get(&id)?.woken {
            return None;
        }
        let saved = self.saved.remove(&id)?;
        (ok && saved.url == url).then_some((saved.x, saved.y))
    }

    pub fn forget(&mut self, id: u64) {
        self.saved.remove(&id);
    }
}

/// Posts `message` to `hwnd` each time Windows signals that the system is
/// low on memory. The thread waits on the signal and uses no CPU.
pub fn watch_low_memory(hwnd: HWND, message: u32) {
    let window = hwnd.0 as isize;
    let _ = std::thread::Builder::new().name("low memory".into()).spawn(move || unsafe {
        let Ok(signal) = CreateMemoryResourceNotification(LowMemoryResourceNotification) else { return };
        loop {
            if WaitForSingleObject(signal, INFINITE).0 != 0 {
                return;
            }
            crate::diag::trace(|| "low memory signal".to_string());
            if PostMessageW(Some(HWND(window as *mut _)), message, WPARAM(0), LPARAM(0)).is_err() {
                return;
            }
            std::thread::sleep(PRESSURE_RECHECK);
        }
    });
}

/// `HEAP_OPTIMIZE_RESOURCES_INFORMATION`.
#[repr(C)]
struct HeapOptimize {
    version: u32,
    flags: u32,
}

/// Gives the app's memory back to the system. First the heaps release the
/// free pages that they keep for later allocations (this decreases the
/// commit), then the working set goes: the pages that stay come back from
/// the page file when they are used again. For a window that nobody looks
/// at (minimized, or no input for a long time) or a system that is low on
/// memory.
pub fn trim_working_set() {
    let optimize = HeapOptimize { version: 1, flags: 0 };
    unsafe {
        // No heap handle: all the heaps of the process.
        let _ = HeapSetInformation(
            None,
            HeapOptimizeResources,
            Some(&optimize as *const HeapOptimize as *const _),
            std::mem::size_of::<HeapOptimize>(),
        );
        let _ = EmptyWorkingSet(GetCurrentProcess());
    }
}

#[cfg(all(test, nea_tests))]
#[path = "../tests/unit/memory.rs"]
#[rustfmt::skip]
mod tests;
