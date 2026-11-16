//! Lucide weather icons. Original vectors and licence are in assets/weather.
use crate::canvas::Canvas;
use eframe::egui::{self, Color32, Id, Rect, TextureHandle, TextureOptions, vec2};

fn bytes(name: &str) -> &'static [u8] {
    match name {
        "sun" => include_bytes!("../assets/weather/sun.png"),
        "moon" => include_bytes!("../assets/weather/moon.png"),
        "cloud-sun" => include_bytes!("../assets/weather/cloud-sun.png"),
        "cloud-moon" => include_bytes!("../assets/weather/cloud-moon.png"),
        "rain" => include_bytes!("../assets/weather/cloud-rain.png"),
        "snow" => include_bytes!("../assets/weather/cloud-snow.png"),
        "storm" => include_bytes!("../assets/weather/cloud-lightning.png"),
        "fog" => include_bytes!("../assets/weather/cloud-fog.png"),
        _ => include_bytes!("../assets/weather/cloud.png"),
    }
}
pub fn draw<H: Copy>(c: &Canvas<H>, name: &str, cx: f32, cy: f32, size: f32, color: Color32) {
    let key = Id::new(("lucide-weather", name));
    let texture = c
        .ctx
        .data_mut(|data| data.get_temp::<TextureHandle>(key))
        .or_else(|| {
            let image = image::load_from_memory(bytes(name)).ok()?.to_rgba8();
            let (width, height) = image.dimensions();
            let texture = c.ctx.load_texture(
                format!("weather-{name}"),
                egui::ColorImage::from_rgba_unmultiplied(
                    [width as usize, height as usize],
                    image.as_raw(),
                ),
                TextureOptions::LINEAR,
            );
            c.ctx
                .data_mut(|data| data.insert_temp(key, texture.clone()));
            Some(texture)
        });
    if let Some(texture) = texture {
        c.image(
            Rect::from_center_size(c.p(cx, cy), vec2(c.len(size), c.len(size))),
            texture.id(),
            color,
        );
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bundled_icons_decode_with_transparency() {
        for name in [
            "sun",
            "moon",
            "cloud",
            "cloud-sun",
            "cloud-moon",
            "rain",
            "snow",
            "storm",
            "fog",
        ] {
            let image = image::load_from_memory(bytes(name)).unwrap().to_rgba8();
            assert_eq!(image.dimensions(), (96, 96));
            assert!(
                image
                    .as_raw()
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .any(|pixel| pixel[3] == 0)
            );
            assert!(
                image
                    .as_raw()
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .any(|pixel| pixel[3] > 0)
            );
        }
    }
}
