//! Live proof that the dashboard GUI shows the EXACT device render: the same
//! `pith-ui` engine renders a UiDoc into a framebuffer, which we blit into a
//! `slint::Image`. Pixel-identical to what the ESP32 panel draws from the same blob.
//!
//! Run: `cargo run --example ui_preview`

slint::slint! {
    export component PreviewWindow inherits Window {
        in property <image> screen;
        title: "pith-ui — exact device render";
        preferred-width: 520px;
        preferred-height: 392px;
        background: #0b0c0e;
        VerticalLayout {
            padding: 16px;
            spacing: 10px;
            Text {
                text: "Same pith-ui engine as the device — pixel-identical (480×320):";
                color: #c9ccd1;
                font-size: 13px;
            }
            Image {
                source: root.screen;
                width: 480px;
                height: 320px;
                image-rendering: pixelated;
            }
        }
    }
}

fn render_screen_image(doc: &pith_ui::UiDoc) -> slint::Image {
    let s = &doc.screens[0];
    let mut fb = pith_ui::Framebuffer::new(s.w, s.h);
    // resolve live-bound fields via demo telemetry (same engine the device runs)
    pith_ui::render_screen_with(s, &pith_ui::demo_telem, &mut fb).expect("render");
    let mut buf = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(s.w, s.h);
    buf.make_mut_bytes().copy_from_slice(&fb.to_rgba8());
    slint::Image::from_rgba8(buf)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // serialize -> load at runtime -> render, exactly like the device will.
    let blob = pith_ui::demo_doc().to_postcard();
    let doc = pith_ui::UiDoc::from_postcard(&blob)?;

    let w = PreviewWindow::new()?;
    w.set_screen(render_screen_image(&doc));
    w.run()?;
    Ok(())
}
