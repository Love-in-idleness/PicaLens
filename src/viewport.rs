use gtk::{gdk, glib, graphene, gsk, prelude::*, subclass::prelude::*};

use crate::{
    preview::rgba_texture,
    session::Result,
    settings::{Filter, Layout},
    upscale::{self, ScaledFrame, Size},
};

struct CachedTexture {
    source: Size,
    texture: gdk::Texture,
}

mod imp {
    use super::*;
    use std::cell::{Cell, RefCell};

    #[derive(Default)]
    pub struct Viewport {
        pub frame: RefCell<Option<gdk::Texture>>,
        pub(super) scaled: RefCell<Option<CachedTexture>>,
        pub layout: Cell<Layout>,
        pub filter: Cell<Filter>,
        pub require_scaled: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Viewport {
        const NAME: &'static str = "PicaLensViewport";
        type Type = super::Viewport;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for Viewport {}
    impl WidgetImpl for Viewport {
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            self.obj().draw(snapshot);
        }
    }
}

glib::wrapper! {
    pub struct Viewport(ObjectSubclass<imp::Viewport>)
        @extends gtk::Widget, @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Viewport {
    pub fn new(layout: Layout, filter: Filter) -> Self {
        let viewport: Self = glib::Object::new();
        viewport.imp().layout.set(layout);
        viewport.imp().filter.set(filter);
        viewport
    }

    pub fn set_frame(&self, frame: gdk::Texture) {
        self.imp().frame.replace(Some(frame));
        self.queue_draw();
    }

    pub fn require_scaled(&self) {
        self.imp().require_scaled.set(true);
    }

    pub fn set_scaled_frame(&self, frame: ScaledFrame) -> Result<()> {
        self.set_scaled_texture(
            frame.source,
            rgba_texture(frame.image.size, &frame.image.bytes)?.upcast(),
        );
        Ok(())
    }

    pub fn set_scaled_texture(&self, source: Size, texture: gdk::Texture) {
        if !self
            .imp()
            .frame
            .borrow()
            .as_ref()
            .is_some_and(|frame| size(frame) == source)
        {
            return;
        }
        self.imp()
            .scaled
            .replace(Some(CachedTexture { source, texture }));
        self.queue_draw();
    }

    pub fn clear_scaled_texture(&self) {
        self.imp().scaled.take();
        self.queue_draw();
    }

    pub fn processing_size(&self) -> Option<Size> {
        let frame = self.imp().frame.borrow();
        let frame = frame.as_ref()?;
        let scale = self.scale();
        self.scaled_size(size(frame), self.frame_layout(frame, scale), scale)
    }

    pub fn screenshot(&self) -> Option<gdk::Texture> {
        if self.imp().frame.borrow().is_none() || self.width() <= 0 || self.height() <= 0 {
            return None;
        }
        let scale = self.scale();
        let snapshot = gtk::Snapshot::new();
        snapshot.scale(scale as f32, scale as f32);
        self.draw(&snapshot);
        let node = snapshot.to_node()?;
        let bounds = graphene::Rect::new(
            0.0,
            0.0,
            (f64::from(self.width()) * scale).round() as f32,
            (f64::from(self.height()) * scale).round() as f32,
        );
        self.native()?
            .renderer()
            .map(|renderer| renderer.render_texture(&node, Some(&bounds)))
    }

    fn scale(&self) -> f64 {
        self.native()
            .and_then(|native| native.surface())
            .map_or(1.0, |surface| surface.scale())
    }

    fn frame_layout(&self, frame: &gdk::Texture, scale: f64) -> [f64; 4] {
        destination(
            self.imp().layout.get(),
            (f64::from(frame.width()), f64::from(frame.height())),
            (f64::from(self.width()), f64::from(self.height())),
            scale,
        )
    }

    fn scaled_size(&self, source: Size, [_, _, w, h]: [f64; 4], scale: f64) -> Option<Size> {
        let size = upscale::processing_size(
            self.imp().filter.get(),
            source,
            ((w * scale).round() as u32, (h * scale).round() as u32),
        );
        size.or_else(|| (self.imp().require_scaled.get() && w > 0.0 && h > 0.0).then_some(source))
    }

    fn draw(&self, snapshot: &gtk::Snapshot) {
        let width = f64::from(self.width());
        let height = f64::from(self.height());
        if width <= 0.0 || height <= 0.0 {
            return;
        }
        let clip = graphene::Rect::new(0.0, 0.0, width as f32, height as f32);
        snapshot.append_color(&gdk::RGBA::BLACK, &clip);
        let frame = self.imp().frame.borrow();
        let Some(frame) = frame.as_ref() else { return };
        let scale = self.scale();
        let layout = self.frame_layout(frame, scale);
        let [x, y, w, h] = layout;
        let area = graphene::Rect::new(x as f32, y as f32, w as f32, h as f32);
        let filter = match self.imp().filter.get() {
            Filter::Nearest | Filter::Scale2x => gsk::ScalingFilter::Nearest,
            _ => gsk::ScalingFilter::Linear,
        };
        let source = size(frame);
        let target = self.scaled_size(source, layout, scale);
        let scaled = self.imp().scaled.borrow();
        let scaled = scaled
            .as_ref()
            .filter(|cached| cached.source == source && target == Some(size(&cached.texture)))
            .map(|cached| &cached.texture);
        if self.imp().require_scaled.get() && scaled.is_none() {
            return;
        }
        let frame = scaled.unwrap_or(frame);
        snapshot.push_clip(&clip);
        snapshot.append_scaled_texture(frame, filter, &area);
        snapshot.pop();
    }
}

fn size(frame: &gdk::Texture) -> Size {
    (frame.width() as u32, frame.height() as u32)
}

fn destination(
    layout: Layout,
    (source_w, source_h): (f64, f64),
    (w, h): (f64, f64),
    device_scale: f64,
) -> [f64; 4] {
    let fit = (w / source_w).min(h / source_h);
    if layout == Layout::Stretch {
        return [0.0, 0.0, w, h];
    }
    let factor = match layout {
        Layout::Fill => (w / source_w).max(h / source_h),
        Layout::Integer if (fit * device_scale).floor() >= 1.0 => {
            let factor = (fit * device_scale).floor() / device_scale;
            let width = source_w * factor;
            let height = source_h * factor;
            // Align to device pixels so a block never straddles a physical pixel.
            let align = |distance: f64| (distance * device_scale / 2.0).floor() / device_scale;
            return [align(w - width), align(h - height), width, height];
        }
        _ => fit,
    };
    let width = source_w * factor;
    let height = source_h * factor;
    [(w - width) / 2.0, (h - height) / 2.0, width, height]
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn distinguishes_fill_fit_and_device_integer_at_fractional_scale() {
        assert_eq!(
            destination(Layout::Fit, (800.0, 600.0), (1280.0, 800.0), 1.5),
            [106.66666666666674, 0.0, 1066.6666666666665, 800.0]
        );
        assert_eq!(
            destination(Layout::Fill, (800.0, 600.0), (1280.0, 800.0), 1.5),
            [0.0, -80.0, 1280.0, 960.0]
        );
        assert_eq!(
            destination(Layout::Integer, (640.0, 360.0), (1280.0, 800.0), 1.5),
            [0.0, 40.0, 1280.0, 720.0]
        );
        assert_eq!(
            destination(Layout::Integer, (1600.0, 1200.0), (800.0, 600.0), 1.5),
            [0.0, 0.0, 800.0, 600.0]
        );
    }
}
