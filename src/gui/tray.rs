//! The notification-area icon and its menu.
//!
//! # Events use handlers, not polling
//!
//! `tray-icon` and `muda` publish events on global channels. A hidden panel
//! requests no frames, so a frame-loop poll would leave tray clicks undelivered.
//! Both crates are therefore given handlers that post the action and wake the
//! event loop, as other producers do via [`crate::app::event::Events`].

#![cfg(windows)]

use std::sync::Arc;

use muda::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder, TrayIconEvent};

use super::{icon, taskbar};

/// A tray action chosen by the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayAction {
    /// Shows the panel. Also triggered by a left-click on the icon.
    Show,
    Settings,
    Diagnostics,
    /// Installs the offered version and restarts. On the menu only while an
    /// update is offered; see [`Tray::offer_update`].
    InstallUpdate,
    Quit,
}

/// The icon, its menu, and the item ids that map to actions.
///
/// Must be held for the life of the process: dropping a `TrayIcon` removes it
/// from the notification area.
pub struct Tray {
    icon: TrayIcon,
    menu: Menu,
    /// The install item, created up front so its id is in the handler's table
    /// (the handler owns a copy and cannot learn about later items). Inserted
    /// into the menu only while an update is offered.
    install: MenuItem,
    /// The version currently offered, if any. Also prevents repeat
    /// notifications for the same version.
    offered: Option<crate::update::Version>,
    /// The icon's shell GUID, used for notifications. `None` if the shell
    /// rejected it; see [`Tray::new`].
    guid: Option<u128>,
    /// Whether the icon is tinted for a dark taskbar.
    on_dark: bool,
    /// Read only by tests; the handler owns its own copy.
    #[cfg_attr(not(test), allow(dead_code))]
    ids: Vec<(muda::MenuId, TrayAction)>,
}

impl Tray {
    /// Builds the icon and installs the event handlers.
    ///
    /// `wake` is called after each event is recorded; see the module docs.
    pub fn new(
        wake: impl Fn(TrayAction) + Send + Sync + 'static,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let menu = Menu::new();
        let show = MenuItem::new("Show files", true, None);
        let settings = MenuItem::new("Settings", true, None);
        let diagnostics = MenuItem::new("Diagnostics", true, None);
        let quit = MenuItem::new("Quit files", true, None);
        let install = MenuItem::new("Install update and restart", true, None);
        menu.append_items(&[
            &show,
            &PredefinedMenuItem::separator(),
            &settings,
            &diagnostics,
            &PredefinedMenuItem::separator(),
            &quit,
        ])?;

        let ids = vec![
            (show.id().clone(), TrayAction::Show),
            (settings.id().clone(), TrayAction::Settings),
            (diagnostics.id().clone(), TrayAction::Diagnostics),
            (install.id().clone(), TrayAction::InstallUpdate),
            (quit.id().clone(), TrayAction::Quit),
        ];

        // The notification needs a GUID. It is derived from the executable path
        // because Windows binds an icon GUID to the path that first registered
        // it and rejects it from any other, so a fixed GUID would conflict
        // between development and installed builds.
        let guid = std::env::current_exe().ok().map(|exe| guid_for(&exe));
        let on_dark = taskbar::is_dark();
        let builder = |guid: Option<u128>| {
            let builder = TrayIconBuilder::new()
                .with_menu(Box::new(menu.clone()))
                .with_tooltip(env!("CARGO_PKG_DESCRIPTION"))
                .with_icon(tinted(on_dark)?)
                // Menu on right-click only; left-click shows the panel.
                .with_menu_on_left_click(false);
            let builder = match guid {
                Some(guid) => builder.with_guid(guid),
                None => builder,
            };
            Ok::<_, Box<dyn std::error::Error>>(builder.build()?)
        };
        // Retry without the GUID if it is rejected; an icon without
        // notifications is better than no icon.
        let (icon, guid) = match builder(guid) {
            Ok(icon) => (icon, guid),
            Err(_) => (builder(None)?, None),
        };

        let wake = Arc::new(wake);

        // A left-click or double-click shows the panel.
        {
            let wake = Arc::clone(&wake);
            TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
                let clicked = matches!(
                    event,
                    TrayIconEvent::Click {
                        button: tray_icon::MouseButton::Left,
                        button_state: tray_icon::MouseButtonState::Up,
                        ..
                    } | TrayIconEvent::DoubleClick {
                        button: tray_icon::MouseButton::Left,
                        ..
                    }
                );
                if clicked {
                    wake(TrayAction::Show);
                }
            }));
        }

        {
            let wake = Arc::clone(&wake);
            let ids = ids.clone();
            MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
                if let Some((_, action)) = ids.iter().find(|(id, _)| *id == event.id) {
                    wake(*action);
                }
            }));
        }

        Ok(Self {
            icon,
            menu,
            install,
            offered: None,
            guid,
            on_dark,
            ids,
        })
    }

    /// Re-tints the icon if the taskbar has switched between light and dark.
    pub fn follow_taskbar(&mut self) {
        let on_dark = taskbar::is_dark();
        if on_dark == self.on_dark {
            return;
        }
        match tinted(on_dark).map(|i| self.icon.set_icon(Some(i))) {
            Ok(Ok(())) => self.on_dark = on_dark,
            Ok(Err(err)) => log::warn!("could not re-tint the tray icon: {err}"),
            Err(err) => log::warn!("could not re-tint the tray icon: {err}"),
        }
    }

    /// Adds the install item for `version` and shows a notification once.
    ///
    /// Called every frame while an update is available; does nothing for a
    /// version already offered.
    pub fn offer_update(&mut self, version: crate::update::Version) {
        if self.offered == Some(version) {
            return;
        }
        let first = self.offered.is_none();
        self.offered = Some(version);
        self.install
            .set_text(format!("Install version {version} and restart"));
        // Second position, directly under "Show files".
        if first {
            let _ = self.menu.insert(&self.install, 1);
        }
        self.notify(
            &format!("files {version} is available"),
            "Right-click the tray icon to install it.",
        );
    }

    /// Removes the install item when no update is available.
    pub fn withdraw_update(&mut self) {
        if self.offered.take().is_some() {
            let _ = self.menu.remove(&self.install);
        }
    }

    /// Shows a balloon notification (a toast on current Windows) from this icon.
    fn notify(&self, title: &str, body: &str) {
        use windows_sys::Win32::UI::Shell::{
            NIF_GUID, NIF_INFO, NIIF_INFO, NIIF_RESPECT_QUIET_TIME, NIM_MODIFY, NOTIFYICONDATAW,
            Shell_NotifyIconW,
        };

        let Some(guid) = self.guid else {
            return;
        };
        let mut data = NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.icon.window_handle(),
            uFlags: NIF_INFO | NIF_GUID,
            guidItem: windows_sys::core::GUID::from_u128(guid),
            // Suppressed during quiet time (the first hour after a new account
            // signs in, and presentation mode).
            dwInfoFlags: NIIF_INFO | NIIF_RESPECT_QUIET_TIME,
            ..Default::default()
        };
        copy_truncated(&mut data.szInfoTitle, title);
        copy_truncated(&mut data.szInfo, body);
        // SAFETY: `data` is a fully initialised local of the size its first
        // field claims, and is not retained by the call.
        unsafe { Shell_NotifyIconW(NIM_MODIFY, &data) };
    }

    /// Returns the number of menu actions. Test-only.
    #[cfg(test)]
    fn len(&self) -> usize {
        self.ids.len()
    }
}

/// Returns the tray icon tinted for a dark or light taskbar.
fn tinted(on_dark: bool) -> Result<Icon, tray_icon::BadIcon> {
    Icon::from_rgba(icon::pixels(on_dark), icon::SIZE, icon::SIZE)
}

/// Returns a GUID that is stable for one executable path (case-insensitive) and
/// distinct for others.
///
/// Uses FNV-1a with two offsets rather than `DefaultHasher`, whose output may
/// change between Rust releases and would reset the user's tray visibility
/// choice on rebuild.
fn guid_for(exe: &std::path::Path) -> u128 {
    let text = exe.to_string_lossy().to_lowercase();
    let fnv = |offset: u64| {
        text.bytes().fold(offset, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
        })
    };
    (u128::from(fnv(0xcbf2_9ce4_8422_2325)) << 64) | u128::from(fnv(0x8422_2325_cbf2_9ce4))
}

/// Copies `text` into a fixed NUL-terminated UTF-16 field, truncating to fit.
fn copy_truncated(field: &mut [u16], text: &str) {
    let room = field.len().saturating_sub(1);
    for (slot, unit) in field.iter_mut().zip(text.encode_utf16().take(room)) {
        *slot = unit;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_executable_keeps_its_icon_identity_and_another_gets_its_own() {
        let installed = std::path::Path::new(r"C:\Program Files\files\files.exe");
        let same = std::path::Path::new(r"C:\PROGRAM FILES\files\files.exe");
        let dev = std::path::Path::new(r"C:\code\files\target\debug\files.exe");
        assert_eq!(guid_for(installed), guid_for(same));
        assert_ne!(guid_for(installed), guid_for(dev));
    }

    #[test]
    fn a_long_notification_is_cut_to_fit_and_terminated() {
        let mut field = [0xffffu16; 8];
        copy_truncated(&mut field, "far longer than eight");
        assert_eq!(
            &field[..7],
            "far lon".encode_utf16().collect::<Vec<_>>().as_slice()
        );
        // The last slot is never written; real fields start zeroed, which
        // provides the terminator.
        let mut zeroed = [0u16; 8];
        copy_truncated(&mut zeroed, "far longer than eight");
        assert_eq!(zeroed[7], 0);
    }

    #[test]
    fn the_menu_offers_every_action() {
        // If the notification area rejects the icon (no message loop in tests),
        // there is nothing to check.
        let Ok(tray) = Tray::new(|_| {}) else {
            return;
        };
        assert_eq!(tray.len(), 5);

        let mut actions: Vec<_> = tray.ids.iter().map(|(_, a)| *a).collect();
        actions.sort_by_key(|a| format!("{a:?}"));
        actions.dedup();
        assert_eq!(actions.len(), 5, "two menu items run the same action");
    }
}
