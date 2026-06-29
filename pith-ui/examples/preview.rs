//! Proves the "no-compile UI" loop on the desktop:
//!   build a UiDoc  ->  serialize to a postcard blob  ->  load it back at runtime
//!   ->  render via embedded-graphics  ->  write preview.png
//!
//! Run: `cargo run --example preview --features std`

use pith_ui::{demo_doc, demo_telem, render_screen, Framebuffer, RenderCache, UiDoc};

fn main() {
    // serialize (this is what gets stored in flash / pushed over USB)
    let blob = demo_doc().to_postcard();
    println!("UiDoc -> {} bytes of postcard", blob.len());

    // load it back AT RUNTIME from the opaque blob (no recompile)
    let loaded = UiDoc::from_postcard(&blob).expect("decode UiDoc");

    // interpret + render the loaded doc against a demo telemetry frame (the device
    // passes its real Telemetry). A second dirty-rect pass with unchanged telemetry
    // repaints nothing.
    let s = &loaded.screens[0];
    let t = demo_telem();
    let mut fb = Framebuffer::new(s.w, s.h);
    render_screen(s, &t, 0, 0, &pith_ui::CarData::default(), &pith_ui::Relatives::default(), &mut fb);
    let mut cache = RenderCache::new();
    pith_ui::render_screen_diff(s, &t, 0, 0, &pith_ui::CarData::default(), &pith_ui::Relatives::default(), &mut cache, &mut fb); // prime the cache

    image::save_buffer("preview.png", &fb.to_rgba8(), fb.w, fb.h, image::ExtendedColorType::Rgba8)
        .expect("save preview.png");
    println!("rendered loaded doc -> preview.png ({}x{})", fb.w, fb.h);
}
