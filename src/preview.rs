use anyhow::Result;
use egui::ColorImage;

pub fn svg_to_image(svg_data: &str, width: u32, height: u32) -> Result<ColorImage> {
    let opt = usvg::Options::default();
    let tree = usvg::Tree::from_str(svg_data, &opt)?;

    let mut pixmap = tiny_skia::Pixmap::new(width, height)
        .ok_or_else(|| anyhow::anyhow!("Failed to create pixmap"))?;

    // Fill white background
    pixmap.fill(tiny_skia::Color::WHITE);

    let svg_size = tree.size();
    let scale_x = width as f32 / svg_size.width();
    let scale_y = height as f32 / svg_size.height();
    let scale = scale_x.min(scale_y);

    let offset_x = (width as f32 - svg_size.width() * scale) / 2.0;
    let offset_y = (height as f32 - svg_size.height() * scale) / 2.0;

    let transform = tiny_skia::Transform::from_scale(scale, scale)
        .post_translate(offset_x, offset_y);

    resvg::render(&tree, transform, &mut pixmap.as_mut());

    let pixels: Vec<egui::Color32> = pixmap
        .pixels()
        .iter()
        .map(|p| egui::Color32::from_rgba_premultiplied(p.red(), p.green(), p.blue(), p.alpha()))
        .collect();

    Ok(ColorImage {
        size: [width as usize, height as usize],
        pixels,
    })
}
