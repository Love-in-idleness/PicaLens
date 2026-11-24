use gtk::{gdk, glib, graphene, gsk, prelude::*, subclass::prelude::*};

use crate::settings::{Filter, Layout};

mod imp {
    use super::*;
    use std::cell::{Cell, RefCell};

    #[derive(Default)]
    pub struct Viewport {
        pub frame: RefCell<Option<gdk::Texture>>,
        pub layout: Cell<Layout>,
        pub filter: Cell<Filter>,
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
        let [x, y, w, h] = destination(
            self.imp().layout.get(),
            (f64::from(frame.width()), f64::from(frame.height())),
            (width, height),
            self.scale(),
        );
        let area = graphene::Rect::new(x as f32, y as f32, w as f32, h as f32);
        let filter = match self.imp().filter.get() {
            Filter::Linear => gsk::ScalingFilter::Linear,
            Filter::Nearest => gsk::ScalingFilter::Nearest,
        };
        snapshot.push_clip(&clip);
        snapshot.append_scaled_texture(frame, filter, &area);
        snapshot.pop();
    }
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
