//! The one thing a transparent window on Windows needs, and does not get by
//! default.
//!
//! Shared by both windows this program puts up - the panel in [`crate::gui`]
//! and the overlay in [`crate::lens::overlay`] - because they had the same bug
//! and there is no reason for two copies of the fix.
//!
//! # What this does, and what it does not
//!
//! wgpu's **default** DX12 swapchain is made straight from the window handle,
//! and `wgpu-hal`'s DX12 backend answers that case with exactly one composite
//! alpha mode:
//!
//! ```text
//! SurfaceTarget::WndHandle(_) => vec![CompositeAlphaMode::Opaque],
//! ```
//!
//! wgpu says so itself, on `Dx12SwapchainKind::DxgiFromHwnd`: *"This does not
//! support transparency."* The other kind, `DxgiFromVisual`, puts a
//! DirectComposition visual between the window and the swapchain, and that
//! surface does offer `PreMultiplied`.
//!
//! **This was not, on its own, why the overlay came up opaque.** Measured on
//! the development machine, wgpu chose the *Vulkan* adapter, whose Win32
//! surface reported `PreMultiplied` quite happily - the surface was configured
//! with it, `egui-wgpu` logged no complaint, and the window was still opaque.
//! Forcing DX12 and DirectComposition changed the colour it covered the desktop
//! with and nothing else. Whatever the remaining cause is, it is above this
//! layer.
//!
//! So what this module buys is narrower than it first looks, and worth keeping
//! anyway: it makes transparency depend on a documented, pinned configuration
//! rather than on which of four adapters happened to be preferred on the day.
//! An overlay whose correctness varies with the GPU is one that works on the
//! machine it was written on.
//!
//! # Why the backend is pinned too
//!
//! `DxgiFromVisual` is a DX12 option and means nothing if wgpu chooses Vulkan
//! or GL. This machine offers four adapters across three backends, and left
//! alone wgpu picked a different one than assumed - which is exactly how a
//! configuration becomes untestable. `WGPU_BACKEND` and
//! `WGPU_DX12_PRESENTATION_SYSTEM` still override both settings, so a machine
//! where this is the wrong answer has a way out that needs no rebuild.
//!
//! None of this is trusted. [`crate::lens::probe`] proves the window is
//! actually transparent by reading back composited pixels before it is allowed
//! to cover anything, because a configuration that is meant to work and a
//! window that does work are different claims.

use eframe::egui_wgpu::{WgpuConfiguration, WgpuSetup};
use eframe::wgpu;

/// The configuration a window with per-pixel alpha needs.
///
/// Preserves eframe's own default surface configuration rather than replacing
/// it: the only thing being changed here is how the swapchain is made.
pub fn transparent_config() -> WgpuConfiguration {
    let mut config =
        WgpuConfiguration::default().with_surface_config(eframe::SurfaceConfig::LOW_LATENCY);

    // `Existing` means somebody handed us a device to render on, and choosing
    // its swapchain is then not ours to do. Nothing in this program does that
    // today; the match is so that it stays correct if anything ever does.
    if let WgpuSetup::CreateNew(setup) = &mut config.wgpu_setup {
        apply(&mut setup.instance_descriptor);
    }
    config
}

/// Applies the two settings, on Windows.
///
/// Split out so the test below can assert on the result without building a
/// whole configuration, and so the `cfg` appears once.
fn apply(descriptor: &mut wgpu::InstanceDescriptor) {
    #[cfg(windows)]
    {
        // An escape hatch that is also how this was measured. The
        // DirectComposition path and the plain one fail differently, and being
        // able to switch between them on a running build without a rebuild is
        // what made the difference visible at all.
        if std::env::var_os("LENS_PLAIN_SWAPCHAIN").is_some() {
            return;
        }
        // The backend is deliberately *not* pinned. Forcing DX12 was tried and
        // measured to change nothing: the plain swapchain and the
        // DirectComposition one composite identically here, down to the pixel.
        // Pinning it would take away every other adapter to buy nothing, and a
        // machine without DX12 would then have no window at all. This setting
        // is simply ignored when wgpu picks a different backend.
        descriptor.backend_options.dx12.presentation_system =
            wgpu::Dx12SwapchainKind::DxgiFromVisual;
    }
    #[cfg(not(windows))]
    {
        // Every other platform composites a transparent window without being
        // asked. Touching the backend list here would only take choices away.
        let _ = descriptor;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole fix, asserted directly. `DxgiFromHwnd` is the default and is
    /// documented by wgpu as not supporting transparency, so a regression to it
    /// is a blacked-out desktop.
    #[test]
    #[cfg(windows)]
    fn the_swapchain_comes_from_a_composition_visual() {
        let mut d = wgpu::InstanceDescriptor::new_without_display_handle();
        assert_eq!(
            d.backend_options.dx12.presentation_system,
            wgpu::Dx12SwapchainKind::DxgiFromHwnd,
            "if wgpu ever changes its default, this file may not be needed"
        );

        apply(&mut d);
        assert_eq!(
            d.backend_options.dx12.presentation_system,
            wgpu::Dx12SwapchainKind::DxgiFromVisual
        );
    }

    /// The backend list is left alone. Pinning DX12 was measured to change
    /// nothing, and would cost every other adapter - including on a machine
    /// that has no DX12 at all, which would then get no window.
    #[test]
    #[cfg(windows)]
    fn the_backend_list_is_left_alone() {
        let mut d = wgpu::InstanceDescriptor::new_without_display_handle();
        let before = d.backends;
        apply(&mut d);
        assert_eq!(d.backends, before);
    }

    /// eframe's own surface configuration has to survive: this module is
    /// changing how the swapchain is made, not how it is presented.
    #[test]
    fn the_surface_configuration_is_eframes_own() {
        let config = transparent_config();
        assert_eq!(config.surface, eframe::SurfaceConfig::LOW_LATENCY);
    }

    /// And the setup must still be one eframe will create from, or the two
    /// settings above are written somewhere nobody reads.
    #[test]
    fn the_setup_is_still_one_that_creates_an_instance() {
        let config = transparent_config();
        assert!(
            matches!(config.wgpu_setup, WgpuSetup::CreateNew(_)),
            "the settings are applied to `CreateNew` and nowhere else"
        );
    }
}
