//! The settings window.
//!
//! One ordinary window, reached from the tray menu and from Ctrl+comma.
//! Ordinary is the point: it has a title bar, it can be moved and resized,
//! and it stays where it was put. The panel is the thing that behaves
//! unusually, and it earns that by being summoned and dismissed dozens of
//! times an hour; a form somebody fills in once does not.
//!
//! # Immediate rather than deferred viewports
//!
//! egui offers both. A deferred viewport runs its own closure on its own
//! schedule, which means the closure must own everything it draws - so it
//! would need a snapshot of the state, taken each frame, whether or not the
//! window was open. An immediate viewport runs inline within the parent's
//! pass and can simply borrow. For a window that is shut almost all of the
//! time, that is the difference between paying for it always and paying for
//! it when it is open.
//!
//! ## It cannot outlive the panel, and deferring will not help
//!
//! Worth writing down, because it looks like a bug and the obvious fix does
//! not work. `Windows::show` is called from `eframe::App::ui`, so the window
//! is only re-shown while the panel is being drawn - and dismissing the panel
//! would therefore take it too.
//!
//! Making it deferred does **not** fix that. `Context::show_viewport_deferred`
//! must still be called "each pass when the child viewport should exist", and
//! `eframe::App::logic`'s own documentation says that while the window is
//! hidden "eframe runs no egui pass at all" and that you "may NOT show any ui"
//! from `logic`. There is nowhere left to call it from. The only way a child
//! could outlive the panel is to stop hiding the panel, which is the one thing
//! a summoned overlay must do.
//!
//! ## Which is why the panel does not park while it is open
//!
//! That consequence was tolerable while this was a document somebody read. It
//! is not tolerable for a form somebody fills in: dismissing the panel
//! mid-edit would take the window with it and discard whatever was half-typed.
//! `gui::Shell::park` therefore refuses to park while [`Windows::any_open`],
//! which is the one remaining way out named above - stop hiding the panel -
//! taken deliberately and only for as long as the window is up.
//!
//! # Why the diagnostics are in here
//!
//! `--doctor` prints the same report. But this program has no console, and a
//! tray application whose diagnostics need a command prompt is not integrated
//! with anything: the person who needs them is the one who cannot get it to
//! work, which is the worst moment to ask somebody to open a terminal.
//!
//! It used to be a second window of its own. It is a page now, which is what
//! Ueli does with its logs and is the arrangement that stops a program of
//! this size having two windows to learn. The tray keeps its Diagnostics
//! item and deep-links to the page, so nothing that was reachable stopped
//! being reachable.

use eframe::egui;

use crate::app::event::AppEvent;
use crate::app::state::{AppState, SettingChange};
use crate::config::Settings;
use crate::config::write::SettingKey;
use crate::gui::settings::{self, Form, lists, report};
use crate::gui::theme::Theme;
use crate::view::settings::{ActionId, PageId};

const TITLE: &str = "files - settings";
const ID: &str = "files-settings";

/// Wide enough for the nav plus a reading column, and short enough to open
/// whole on a 1366 by 768 laptop once the taskbar has taken its share. Ueli
/// uses 1000 by 800; the 800 is the part not to copy, because this
/// configuration follows people onto laptops by design.
const SIZE: [f32; 2] = [940.0, 700.0];

/// The smallest this window may be made.
///
/// Eight-twenty by five-sixty, up from seven-twenty by four-eighty. It went
/// *up* because the layout got better, which sounds backwards and is not:
/// the old floor was a guess at where things started breaking, and things
/// started breaking well above it, so the number was doing nothing. Now that
/// `measure` shrinks a control before it shreds a sentence, nothing breaks
/// at any width at all - so the floor stops being a damage limit and becomes
/// a readability one.
///
/// Eight-twenty leaves a 509-point content column after the 260-point nav
/// and the padding, which is above the ~420 where a setting row starts
/// taking width off its control. A window at this size is one where nothing
/// is compromised, rather than one where everything is merely still legible.
const MIN_SIZE: [f32; 2] = [820.0, 560.0];

/// The window, which page it is on, and anything it had to compute.
pub struct Windows {
    open: bool,
    /// Which page is showing.
    ///
    /// Survives a close, so Ctrl+comma brings back the page you were last
    /// on. A named menu item deliberately does not: see `Shell::serve_requests`.
    page: PageId,
    /// The diagnostic report, and whatever is being done about it.
    ///
    /// A state machine on a worker thread rather than a `String` taken
    /// inline. `doctor` reads volume flags, times round trips to every
    /// enabled share, decodes the whole index and walks a cache directory;
    /// it was being run on the frame thread *before the viewport was
    /// created*, so opening the window on this page showed nothing at all -
    /// no title bar, no nav - until it came back. See
    /// [`crate::gui::settings::report`].
    report: report::Reporter,
    /// The text box being typed in, and what is in it.
    ///
    /// Not a second copy of the settings: it exists between the moment a box
    /// takes the keyboard and the moment it gives it up, which is state any
    /// text box has to have. One, because only one can have the keyboard.
    editing: Option<(SettingKey, String)>,
    /// The alias being typed into the "add" row, and why it cannot be added.
    draft: lists::AliasDraft,
    /// The drive being typed into the "add" row. Same category as `draft`.
    drive: lists::DriveDraft,
    /// Whether each configured drive is really there. See
    /// [`crate::gui::settings::exists`].
    exists: settings::exists::DirCache,
}

impl Default for Windows {
    fn default() -> Self {
        Self {
            open: false,
            page: PageId::General,
            report: report::Reporter::default(),
            editing: None,
            draft: lists::AliasDraft::default(),
            drive: lists::DriveDraft::default(),
            exists: settings::exists::DirCache::default(),
        }
    }
}

impl Windows {
    /// Opens the window on a named page.
    ///
    /// What the tray menu does. A menu item named "Diagnostics" has to land
    /// on the diagnostics, not on wherever the window was last left.
    pub fn open_at(&mut self, page: PageId) {
        self.go_to(page);
        self.open = true;
    }

    /// Opens the window, or shuts it if it is already up.
    ///
    /// What Ctrl+comma does, as against what the tray menu does. A menu item
    /// named "Settings" that closed the window when it was open would be a
    /// menu that lies, so that route still opens; a key advertised as "show
    /// or hide" has to do both. It names no page, because a toggle should
    /// come back to where you were.
    pub fn toggle(&mut self) {
        self.open = !self.open;
    }

    /// Moves to a page.
    ///
    /// This used to throw the diagnostics report away on every arrival, so
    /// that somebody who had fixed a drive and come back saw the new answer
    /// rather than the old one. That is the right instinct and the wrong
    /// lever: it also meant clicking Diagnostics, stepping to About to check
    /// a version, and clicking back re-ran a multi-second network probe. The
    /// freshness question belongs to the thing that knows when the answer
    /// was taken, which is [`report::Reporter`] and its one-minute life - and
    /// the case the old rule was really about now has a button.
    fn go_to(&mut self, page: PageId) {
        self.page = page;
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Kept as a name rather than folded into [`Self::is_open`] because
    /// `Shell::park` and `Shell::logic` both read as questions about the
    /// program rather than about this struct.
    pub fn any_open(&self) -> bool {
        self.open
    }

    /// Draws the window if it is open, and reports anything that was pressed.
    ///
    /// A return value rather than a callback because the window is drawn
    /// inline inside the parent's pass: `Shell` is already borrowed for the
    /// frame, and a closure that could reach back into it would not compile.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        theme: &Theme,
        state: &AppState,
        settings: &Settings,
        placement: Option<(i32, i32)>,
        wake: impl Fn() + Clone + Send + 'static,
    ) -> Clicked {
        let mut clicked = Clicked::default();
        if !self.open {
            return clicked;
        }

        // Once per frame, for both readers. This walks the configuration
        // and allocates eight `Page`s; it used to be done twice on the frame
        // the window opened, because `wants_report` built its own copy to
        // answer one question about one of them.
        let pages = crate::view::settings::pages(state, settings, placement);

        // Asked for on the first frame the page is showing rather than when
        // the menu item was clicked, so the window appears immediately and
        // the waiting happens with something on screen. Asked of the model
        // rather than of the page id, so a report added to a second page
        // does not silently show a stale one.
        let now = std::time::Instant::now();
        self.report.poll(now);
        self.report.wanted(
            settings::wants_report(&pages, self.page),
            settings,
            now,
            wake.clone(),
        );

        self.exists.forget_stale(now);
        let view = self.report.view();
        let mut chosen = self.page;
        let mut aliases = None;
        let mut mappings = None;
        let was_editing = self.editing.is_some();
        {
            let editing = &mut self.editing;
            let draft = &mut self.draft;
            let drive = &mut self.drive;
            let exists = &mut self.exists;
            let changed = &mut clicked.changed;
            let actions = &mut clicked.actions;
            let page = self.page;
            let open = show_one(ctx, was_editing, |ui| {
                let mut form = Form {
                    editing,
                    draft,
                    drive,
                    changed,
                    actions,
                    aliases: None,
                    mappings: None,
                    exists,
                    now,
                };
                chosen = settings::show(ui, theme, &pages, settings, page, view, &mut form);
                aliases = form.aliases;
                mappings = form.mappings;
            });
            self.open = open;
        }
        // Before the caller sees it, because the reporter is here and
        // nowhere else. The shell has the same arm and does nothing in it.
        if clicked.actions.contains(&ActionId::RefreshReport) {
            self.report.refresh(settings, wake);
        }
        self.go_to(chosen);
        clicked.aliases = aliases;
        clicked.mappings = mappings;
        clicked
    }
}

/// What the user pressed, for the caller to act on.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Clicked {
    /// Buttons pressed this frame, in the order they were pressed.
    pub actions: Vec<ActionId>,
    /// The alias list as it should now be, when this frame changed it.
    pub aliases: Option<Vec<crate::alias::Alias>>,
    /// And the drive list, likewise.
    pub mappings: Option<Vec<crate::paths::Mapping>>,
    /// Controls the user moved, in the order they moved them.
    pub changed: Vec<SettingChange>,
}

impl Clicked {
    /// Everything this frame asks the state machine to do.
    ///
    /// A function rather than three lines in `Shell::ui`, and the reason is
    /// the bug it was written to close. Those three lines were *missing*:
    /// `show` has always returned `changed`, `aliases` and `mappings`
    /// alongside `actions`, and the caller read `actions` and dropped the
    /// rest - so every control in this window was decorative, and
    /// `AppState::on_setting`, `on_aliases`, `on_drives` and
    /// `Cmd::SaveSetting` were reachable only from `tests/state_machine.rs`.
    ///
    /// It was invisible for exactly as long as it lived in `Shell::ui`,
    /// which owns three threads, a tray icon and a window, and which no
    /// test drives. Out here it is a pure function over a plain struct, and
    /// the test below is the thing that was impossible to write.
    ///
    /// `actions` is deliberately not in here. Two of them reach outside the
    /// program and one restarts it, so they are the shell's to run, not the
    /// state machine's.
    pub fn events(&mut self) -> Vec<AppEvent> {
        let mut out: Vec<AppEvent> = self.changed.drain(..).map(AppEvent::Setting).collect();
        if let Some(aliases) = self.aliases.take() {
            out.push(AppEvent::Aliases(aliases));
        }
        if let Some(mappings) = self.mappings.take() {
            out.push(AppEvent::Drives(mappings));
        }
        out
    }
}

/// The window, returning whether it is still open.
/// `was_editing` is whether a text box had the keyboard when this frame
/// began. Taken before the pass, because by the time the pass has run the box
/// has already let go of it - see the Escape note below.
fn show_one(ctx: &egui::Context, was_editing: bool, mut body: impl FnMut(&mut egui::Ui)) -> bool {
    let mut open = true;
    ctx.show_viewport_immediate(
        egui::ViewportId::from_hash_of(ID),
        egui::ViewportBuilder::default()
            .with_title(TITLE)
            .with_inner_size(SIZE)
            .with_min_inner_size(MIN_SIZE),
        |ctx, _class| {
            egui::CentralPanel::default()
                .frame(egui::Frame::NONE)
                .show(ctx, &mut body);

            // Escape, innermost thing first.
            //
            // This used to be one unconditional read, and it was wrong in a
            // way nothing reported: egui drops widget focus on an
            // *unconsumed* Escape inside `Focus::begin_pass`, so a press
            // while a text box had the keyboard both ended the edit and shut
            // the window in the same frame. With a read-only document that
            // was invisible. With a form it threw the window away from under
            // somebody who was backing out of one field.
            //
            // So a press that a box has just answered is a press this does
            // not see. `was_editing` is taken before the pass, because by
            // the time the pass has run the box has already let go.
            let escaped =
                ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
            let closing = ctx.input(|i| i.viewport().close_requested());
            if closing || (escaped && !was_editing) {
                open = false;
            }
        },
    );
    open
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::write::Typed;

    fn moved(key: SettingKey, label: &'static str) -> SettingChange {
        SettingChange {
            key,
            typed: Typed::Flag(true),
            label,
        }
    }

    /// Everything the window changed reaches the state machine.
    ///
    /// The test that was impossible to write while this mapping lived in
    /// `Shell::ui`, and the one that would have caught the whole window
    /// being decorative: `changed`, `aliases` and `mappings` were returned
    /// every frame and read by nobody.
    #[test]
    fn everything_the_window_changed_becomes_an_event() {
        let mut clicked = Clicked {
            actions: vec![ActionId::CopyReport],
            aliases: Some(Vec::new()),
            mappings: Some(Vec::new()),
            changed: vec![
                moved(SettingKey::History, "Recent codes"),
                moved(SettingKey::DevMode, "Developer mode"),
            ],
        };

        let events = clicked.events();

        assert_eq!(events.len(), 4, "something was dropped: {events:?}");
        // The controls first, in the order they were moved.
        assert!(matches!(
            &events[0],
            AppEvent::Setting(SettingChange {
                key: SettingKey::History,
                ..
            })
        ));
        assert!(matches!(
            &events[1],
            AppEvent::Setting(SettingChange {
                key: SettingKey::DevMode,
                ..
            })
        ));
        assert!(matches!(&events[2], AppEvent::Aliases(_)));
        assert!(matches!(&events[3], AppEvent::Drives(_)));
    }

    /// The buttons are the shell's, not the state machine's: two of them
    /// reach outside the program and one restarts it.
    #[test]
    fn a_button_is_not_an_event() {
        let mut clicked = Clicked {
            actions: vec![ActionId::InstallUpdate, ActionId::ForgetPlacement],
            ..Clicked::default()
        };
        assert!(clicked.events().is_empty());
        assert_eq!(clicked.actions.len(), 2, "the buttons were consumed");
    }

    /// A frame in which nothing was touched asks for nothing. Every frame
    /// the window is up is one of these, so a stray event here would be a
    /// write to the configuration file sixty times a second.
    #[test]
    fn an_untouched_frame_asks_for_nothing() {
        assert!(Clicked::default().events().is_empty());
    }

    /// What Ctrl+comma does. A toggle used to only ever open, so a second
    /// press was a no-op and the window could be shut by nothing but Escape
    /// or its title bar - while the key was advertised as "show or hide".
    #[test]
    fn toggling_the_window_opens_it_and_then_shuts_it() {
        let mut windows = Windows::default();

        windows.toggle();
        assert!(windows.is_open(), "the first press did not open");

        windows.toggle();
        assert!(!windows.is_open(), "the second press did not shut");
        assert!(!windows.any_open());
    }

    /// The tray menu opens; only the key toggles. A menu item that closed the
    /// window it names would be a menu that lies.
    #[test]
    fn opening_an_already_open_window_leaves_it_open() {
        let mut windows = Windows::default();
        windows.open_at(PageId::General);
        windows.open_at(PageId::General);
        assert!(windows.is_open());
    }

    /// A named menu item is a destination.
    #[test]
    fn opening_at_a_page_lands_on_that_page() {
        let mut windows = Windows::default();
        windows.open_at(PageId::Diagnostics);
        assert_eq!(windows.page, PageId::Diagnostics);
    }

    /// And the key is a resumption: it comes back where you left it.
    #[test]
    fn the_window_comes_back_on_the_page_it_was_left_on() {
        let mut windows = Windows::default();
        windows.open_at(PageId::Diagnostics);
        windows.toggle();
        windows.toggle();
        assert_eq!(windows.page, PageId::Diagnostics);
    }

    /// A window that has not asked for a report has nothing to show, and
    /// moving between pages does not change that on its own.
    ///
    /// Four tests used to live here, all about when the cached report was
    /// thrown away on arrival at a page. That rule is gone: throwing it away
    /// on arrival meant that stepping to About to check a version and
    /// stepping back re-ran a multi-second network probe. Freshness is now
    /// `report::Reporter`'s, which knows when the answer was taken, and the
    /// tests that matter are beside it.
    #[test]
    fn moving_between_pages_does_not_take_a_reading_by_itself() {
        let mut windows = Windows::default();
        windows.open_at(PageId::Diagnostics);
        windows.go_to(PageId::General);
        windows.go_to(PageId::Diagnostics);
        assert_eq!(windows.report.text(), "");
    }
}
