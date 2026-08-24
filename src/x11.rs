use raylib::prelude::{RaylibHandle, Vector2};
use std::ffi::c_void;

#[repr(C)]
struct XRectangle {
    x: i16,
    y: i16,
    width: u16,
    height: u16,
}

#[repr(C)]
struct XSetWindowAttributes {
    background_pixmap: usize,
    background_pixel: usize,
    border_pixmap: usize,
    border_pixel: usize,
    bit_gravity: i32,
    win_gravity: i32,
    backing_store: i32,
    backing_planes: usize,
    backing_pixel: usize,
    save_under: i32,
    event_mask: isize,
    do_not_propagate_mask: isize,
    override_redirect: i32,
    colormap: usize,
    cursor: usize,
}

#[repr(C)]
struct XRROutputInfo {
    timestamp: usize,
    crtc: usize,
}

#[repr(C)]
struct XRRCrtcInfo {
    timestamp: usize,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
}

#[repr(C)]
struct XRRScreenResources {
    _opaque: [u8; 0],
}

#[link(name = "Xext")]
unsafe extern "C" {
    fn XShapeCombineRectangles(
        display: *mut c_void,
        window: usize,
        kind: i32,
        x: i32,
        y: i32,
        rectangles: *const XRectangle,
        count: i32,
        operation: i32,
        ordering: i32,
    );
}
#[link(name = "X11")]
unsafe extern "C" {
    fn XOpenDisplay(name: *const i8) -> *mut c_void;
    fn XDefaultRootWindow(display: *mut c_void) -> usize;
    fn XQueryPointer(
        display: *mut c_void,
        window: usize,
        root: *mut usize,
        child: *mut usize,
        root_x: *mut i32,
        root_y: *mut i32,
        window_x: *mut i32,
        window_y: *mut i32,
        mask: *mut u32,
    ) -> i32;
    fn XChangeWindowAttributes(
        display: *mut c_void,
        window: usize,
        mask: usize,
        attributes: *mut XSetWindowAttributes,
    ) -> i32;
    fn XMapWindow(display: *mut c_void, window: usize) -> i32;
    fn XFlush(display: *mut c_void) -> i32;
    fn XCloseDisplay(display: *mut c_void) -> i32;
}

#[link(name = "Xrandr")]
unsafe extern "C" {
    fn XRRGetScreenResourcesCurrent(display: *mut c_void, window: usize)
    -> *mut XRRScreenResources;
    fn XRRFreeScreenResources(resources: *mut XRRScreenResources);
    fn XRRGetOutputPrimary(display: *mut c_void, window: usize) -> usize;
    fn XRRGetOutputInfo(
        display: *mut c_void,
        resources: *mut XRRScreenResources,
        output: usize,
    ) -> *mut XRROutputInfo;
    fn XRRFreeOutputInfo(info: *mut XRROutputInfo);
    fn XRRGetCrtcInfo(
        display: *mut c_void,
        resources: *mut XRRScreenResources,
        crtc: usize,
    ) -> *mut XRRCrtcInfo;
    fn XRRFreeCrtcInfo(info: *mut XRRCrtcInfo);
}

pub fn primary_monitor() -> Option<(i32, i32, i32)> {
    // SAFETY: all pointers come from Xlib/XRandR and are freed by their matching APIs.
    unsafe {
        let display = XOpenDisplay(std::ptr::null());
        if display.is_null() {
            return None;
        }
        let root = XDefaultRootWindow(display);
        let resources = XRRGetScreenResourcesCurrent(display, root);
        let output = XRRGetOutputPrimary(display, root);
        if resources.is_null() || output == 0 {
            if !resources.is_null() {
                XRRFreeScreenResources(resources);
            }
            XCloseDisplay(display);
            return None;
        }
        let output_info = XRRGetOutputInfo(display, resources, output);
        let crtc = if output_info.is_null() {
            0
        } else {
            (*output_info).crtc
        };
        let crtc_info = if crtc == 0 {
            std::ptr::null_mut()
        } else {
            XRRGetCrtcInfo(display, resources, crtc)
        };
        let result = (!crtc_info.is_null() && (*crtc_info).width > 0)
            .then(|| ((*crtc_info).x, (*crtc_info).y, (*crtc_info).width as i32));
        if !crtc_info.is_null() {
            XRRFreeCrtcInfo(crtc_info);
        }
        if !output_info.is_null() {
            XRRFreeOutputInfo(output_info);
        }
        XRRFreeScreenResources(resources);
        XCloseDisplay(display);
        result
    }
}

pub fn map_window(window: &mut RaylibHandle) {
    // SAFETY: raylib exposes a live X11 window id; Xlib does not retain these pointers.
    unsafe {
        let native = window.get_window_handle();
        if native.is_null() {
            return;
        }
        let display = XOpenDisplay(std::ptr::null());
        if display.is_null() {
            return;
        }
        let xwindow = *(native.cast::<usize>());
        let mut attributes: XSetWindowAttributes = std::mem::zeroed();
        attributes.override_redirect = 1;
        XChangeWindowAttributes(display, xwindow, 1 << 9, &mut attributes);
        XMapWindow(display, xwindow);
        XFlush(display);
        XCloseDisplay(display);
    }
}

pub fn pointer_position(window: &mut RaylibHandle) -> Option<Vector2> {
    // SAFETY: raylib exposes a live X11 window id and XQueryPointer fills stack values.
    unsafe {
        let native = window.get_window_handle();
        if native.is_null() {
            return None;
        }
        let display = XOpenDisplay(std::ptr::null());
        if display.is_null() {
            return None;
        }
        let mut root = 0;
        let mut child = 0;
        let mut root_x = 0;
        let mut root_y = 0;
        let mut x = 0;
        let mut y = 0;
        let mut mask = 0;
        let found = XQueryPointer(
            display,
            *(native.cast::<usize>()),
            &mut root,
            &mut child,
            &mut root_x,
            &mut root_y,
            &mut x,
            &mut y,
            &mut mask,
        ) != 0;
        XCloseDisplay(display);
        found.then(|| Vector2::new(x as f32, y as f32))
    }
}

pub fn shape_input(window: &mut RaylibHandle, width: i32, height: i32, visible: bool) {
    // SAFETY: raylib exposes a live X11 window id; shaping uses a separate Xlib connection.
    unsafe {
        let native = window.get_window_handle();
        if native.is_null() {
            return;
        }
        let display = XOpenDisplay(std::ptr::null());
        if display.is_null() {
            return;
        }
        let xwindow = *(native.cast::<usize>());
        if !visible {
            XShapeCombineRectangles(display, xwindow, 2, 0, 0, std::ptr::null(), 0, 0, 0);
            XCloseDisplay(display);
            return;
        }
        let island_width = (width - 40).max(1);
        let island_height = (height - 40).max(1);
        let radius = (island_height as f32 / 2.0).min(30.0);
        let origin_x = (window.get_screen_width() - island_width) / 2;
        let mut rectangles = Vec::with_capacity(island_height as usize);
        for row in 0..island_height {
            let center = row as f32 + 0.5;
            let distance = if center < radius {
                radius - center
            } else if center > island_height as f32 - radius {
                center - (island_height as f32 - radius)
            } else {
                0.0
            };
            let inset = if distance == 0.0 {
                0
            } else {
                (radius - (radius * radius - distance * distance).sqrt()).ceil() as i32
            };
            rectangles.push(XRectangle {
                x: (origin_x + inset) as i16,
                y: (20 + row) as i16,
                width: (island_width - inset * 2).max(1) as u16,
                height: 1,
            });
        }
        XShapeCombineRectangles(
            display,
            xwindow,
            2,
            0,
            0,
            rectangles.as_ptr(),
            rectangles.len() as i32,
            0,
            0,
        );
        XCloseDisplay(display);
    }
}
