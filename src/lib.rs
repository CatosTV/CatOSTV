//! CatosTV GUI support library (eframe/egui).
//!
//! All decisions about focus, navigation, install safety and system state
//! live in the dependency-free `catostv-core` crate. This crate only draws and
//! performs effects.

pub mod canvas;
pub mod catfyn;
pub mod config;
pub mod hidden_pin;
pub mod icons;
pub mod images;
pub mod jobs;
pub mod keys;
pub mod motion;
pub mod pointer;
pub mod saver;
pub mod theme;
pub mod ui;
pub mod weather_icons;
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
        // The shell draws Quick settings and the volume panel over apps.
        .with_transparent(true)
        .with_inner_size([1280.0, 720.0]);
    eframe::NativeOptions {
        viewport,
        renderer,
        ..Default::default()
    }
}

/// Log recoverable wgpu errors instead of panicking. When the TV went
/// away the window was once resized to 3× the screen (11520×6480, over the
/// GPU's 8192 limit) and configuring the surface took the whole interface
/// down; the next resize sets it up again. The first few are logged.
fn log_gpu_errors(app_id: &str, cc: &eframe::CreationContext<'_>) {
    let Some(rs) = &cc.wgpu_render_state else {
        return;
    };
    if app_id == "catostv-shell" {
        std::thread::spawn(|| {
            loop {
                std::thread::sleep(std::time::Duration::from_secs(5));
                if catostv_core::gpu_memory::current()
                    .is_ok_and(|bytes| bytes > catostv_core::gpu_memory::SHELL_LIMIT)
                {
                    restart_graphics("catostv-shell", "graphics memory limit reached");
                }
            }
        });
    }
    let app_id = app_id.to_string();
    let logged = std::sync::atomic::AtomicU32::new(0);
    rs.device
        .on_uncaptured_error(std::sync::Arc::new(move |error| {
            handle_gpu_error(&app_id, error, &logged);
        }));
}

fn restart_with_opengl() -> std::io::Error {
    use std::os::unix::process::CommandExt;
    match std::env::current_exe() {
        Ok(exe) => std::process::Command::new(exe)
            .args(std::env::args_os().skip(1))
            .env("CATOSTV_RENDERER", "glow")
            .exec(),
        Err(error) => error,
    }
}

fn handle_gpu_error(
    app_id: &str,
    error: eframe::wgpu::Error,
    logged: &std::sync::atomic::AtomicU32,
) {
    if app_id == "catostv-shell" && matches!(error, eframe::wgpu::Error::OutOfMemory { .. }) {
        // Invalid textures cannot recover by continuing to draw with this device.
        restart_graphics(app_id, "graphics memory exhausted");
    }
    if logged.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 20 {
        eprintln!("{app_id}: GPU error: {error}");
    }
}

fn restart_graphics(app_id: &str, reason: &str) -> ! {
    eprintln!("{app_id}: {reason}; restarting with OpenGL");
    eprintln!("{app_id}: OpenGL restart failed: {}", restart_with_opengl());
    std::process::exit(1);
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
        Box::new(move |cc| {
            log_gpu_errors(app_id, cc);
            Ok(Box::new(make(cc)))
        }),
    );
    if let Err(e) = &result
        && !explicit
    {
        eprintln!("{app_id}: wgpu renderer failed ({e}); retrying with OpenGL");
        eprintln!("{app_id}: re-exec failed: {}", restart_with_opengl());
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    #[test]
    fn graphics_memory_failure_restarts_from_a_worker_with_opengl() {
        const CHILD: &str = "CATOSTV_TEST_GPU_RECOVERY";
        if std::env::var_os(CHILD).is_some() {
            if std::env::var("CATOSTV_RENDERER").as_deref() == Ok("glow") {
                return;
            }
            std::thread::spawn(|| {
                handle_gpu_error(
                    "catostv-shell",
                    eframe::wgpu::Error::OutOfMemory {
                        source: Box::new(std::io::Error::other("test allocation failure")),
                    },
                    &AtomicU32::new(20),
                );
            })
            .join()
            .unwrap();
            panic!("Out-of-memory returned without recovery");
        }
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "tests::graphics_memory_failure_restarts_from_a_worker_with_opengl",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env("CATOSTV_RENDERER", "wgpu")
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(String::from_utf8_lossy(&result.stderr).contains("graphics memory exhausted"));
    }
    #[test]
    fn validation_errors_do_not_restart_the_process() {
        let logged = AtomicU32::new(20);
        handle_gpu_error(
            "catostv-check",
            eframe::wgpu::Error::Validation {
                source: Box::new(std::io::Error::other("test validation")),
                description: "test validation".into(),
            },
            &logged,
        );
        assert_eq!(logged.load(Ordering::Relaxed), 21);
    }
    #[test]
    fn installer_memory_error_does_not_interrupt_disk_work() {
        let logged = AtomicU32::new(20);
        handle_gpu_error(
            "catostv-installer",
            eframe::wgpu::Error::OutOfMemory {
                source: Box::new(std::io::Error::other("test allocation failure")),
            },
            &logged,
        );
        assert_eq!(logged.load(Ordering::Relaxed), 21);
    }
}
