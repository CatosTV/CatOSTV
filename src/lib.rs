//! CatosTV GUI support library (eframe/egui).
//!
//! All decisions about focus, navigation, install safety and system state
//! live in the dependency-free `catostv-core` crate. This crate only draws and
//! performs effects.

pub mod canvas;
pub mod catfyn;
pub mod config;
pub mod icons;
pub mod images;
pub mod jobs;
pub mod keys;
pub mod pointer;
pub mod theme;
pub mod ui;
pub mod widgets;

pub use catostv_core as core;

/// Renderer selected by `CATOSTV_RENDERER` (`glow` or `wgpu`, default wgpu).
/// Returns `(renderer, explicitly_chosen)`.
fn renderer_from_env() -> (eframe::Renderer, bool) {
    match std::env::var("CATOSTV_RENDERER").ok().as_deref() {
        Some("glow") => (eframe::Renderer::Glow, true),
        Some("wgpu") => (eframe::Renderer::Wgpu, true),
        _ => (eframe::Renderer::Wgpu, false),
    }
}

/// Window options shared by shell and installer.
///
/// * `CATOSTV_WINDOWED=1`, decorated 1280×720 development window.
/// * `CATOSTV_RENDERER=glow|wgpu`, force a renderer (default: wgpu, with an
///   automatic OpenGL retry, see [`run_app`]).
pub fn native_options(
    app_id: &str,
    title: &str,
    renderer: eframe::Renderer,
) -> eframe::NativeOptions {
    let windowed = catostv_core::env::is_windowed();
    let viewport = eframe::egui::ViewportBuilder::default()
        .with_title(title)
        .with_app_id(app_id)
        .with_decorations(windowed)
        .with_fullscreen(!windowed)
        .with_inner_size([1280.0, 720.0]);
    eframe::NativeOptions {
        viewport,
        renderer,
        ..Default::default()
    }
}

/// Run a CatosTV app. If the default wgpu renderer fails to start (VMs
/// without Vulkan, very old GPUs), the process re-executes itself with
/// `CATOSTV_RENDERER=glow`. Re-exec is used because winit cannot create a
/// second event loop in the same process.
pub fn run_app<A: eframe::App + 'static>(
    app_id: &str,
    title: &str,
    make: fn(&eframe::CreationContext<'_>) -> A,
) -> eframe::Result {
    let (renderer, explicit) = renderer_from_env();
    let result = eframe::run_native(
        title,
        native_options(app_id, title, renderer),
        Box::new(move |cc| Ok(Box::new(make(cc)))),
    );
    if let Err(e) = &result
        && !explicit
    {
        eprintln!("{app_id}: wgpu renderer failed ({e}); retrying with OpenGL");
        if let Ok(exe) = std::env::current_exe() {
            use std::os::unix::process::CommandExt;
            let err = std::process::Command::new(exe)
                .args(std::env::args_os().skip(1))
                .env("CATOSTV_RENDERER", "glow")
                .exec();
            eprintln!("{app_id}: re-exec failed: {err}");
        }
    }
    result
}
