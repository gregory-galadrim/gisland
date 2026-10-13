use anyhow::{Result, bail};
use pango::prelude::*;
use raylib::{core::AsRawMut, prelude::*};
use serde_json::Value;
use std::{
    collections::{BTreeSet, HashMap},
    path::{Path, PathBuf},
    time::Instant,
};
use taffy::prelude::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Visibility {
    Hidden,
    Compact,
    Expanded,
}
#[derive(Clone)]
pub struct HitRegion {
    pub rectangle: Rectangle,
    pub owner: Option<String>,
    pub action: String,
}

pub struct Renderer {
    scene: Option<Value>,
    changed: Instant,
    outgoing: Option<(Value, Visibility, Instant)>,
    meter_changed: Instant,
    meter_from: f32,
    meter_target: Option<f32>,
    visibility: Visibility,
    shape: [f32; 7],
    velocity: [f32; 7],
    motion_duration: f32,
    motion_bounce: f32,
    last_frame: Instant,
    pango: pango::Context,
    pub hits: Vec<HitRegion>,
    keyboard_action: Option<String>,
    textures: HashMap<String, Texture2D>,
    fonts: HashMap<String, Font>,
    font_characters: String,
    canvas: Option<RenderTexture2D>,
    asset_root: PathBuf,
    measurements: HashMap<String, (f32, f32)>,
}
impl Renderer {
    pub fn new(asset_root: PathBuf) -> Self {
        Self {
            scene: None,
            changed: Instant::now(),
            outgoing: None,
            meter_changed: Instant::now(),
            meter_from: 0.0,
            meter_target: None,
            visibility: Visibility::Hidden,
            shape: [230.0, 32.0, 16.0, 20.0, 8.0, 0.0, 16.0],
            velocity: [0.0; 7],
            motion_duration: 0.38,
            motion_bounce: 0.24,
            last_frame: Instant::now(),
            pango: pangocairo::FontMap::default().create_context(),
            hits: Vec::new(),
            keyboard_action: None,
            textures: HashMap::new(),
            fonts: HashMap::new(),
            font_characters: String::new(),
            canvas: None,
            asset_root,
            measurements: HashMap::new(),
        }
    }
    pub fn set_scene(&mut self, mut scene: Value) -> Result<()> {
        validate_scene(&scene)?;
        let theme = scene["theme"].clone();
        for surface in ["compact", "expanded"] {
            if let Some(node) = scene.get_mut(surface).filter(|node| !node.is_null()) {
                resolve_theme(node, &theme);
            }
        }
        if let Some((target, declared_from)) = find_progress(&scene) {
            let speed = self
                .scene
                .as_ref()
                .and_then(|value| value.pointer("/interaction/animation_speed"))
                .and_then(Value::as_f64)
                .unwrap_or(1.0) as f32;
            let current = self.meter_target.map_or(declared_from, |previous| {
                lerp(
                    self.meter_from,
                    previous,
                    ease_out(
                        (self.meter_changed.elapsed().as_secs_f32() * speed / 0.20).clamp(0.0, 1.0),
                    ),
                )
            });
            if self.meter_target.is_none_or(|previous| previous != target) {
                self.meter_from = current;
                self.meter_target = Some(target);
                self.meter_changed = Instant::now();
            }
        } else {
            self.meter_target = None;
        }
        let context_changed = self.scene.as_ref().is_none_or(|current| {
            current.get("compact_context") != scene.get("compact_context")
                || current.get("expanded_context") != scene.get("expanded_context")
        });
        let geometry_changed = compact_geometry(self.scene.as_ref())
            != compact_geometry(Some(&scene))
            || expanded_geometry(self.scene.as_ref()) != expanded_geometry(Some(&scene));
        if context_changed || geometry_changed {
            if self.outgoing.is_none()
                && let Some(previous) = self.scene.as_ref()
            {
                self.outgoing = Some((previous.clone(), self.visibility, Instant::now()));
            }
            self.motion_duration =
                number(&scene["theme"]["animation"], "context_change_ms", 320.0) / 1000.0;
            self.motion_bounce = number(&scene["theme"]["animation"], "swap_bounce", 0.20);
        }
        let declared_transition =
            ["compact_transitions", "expanded_transitions"]
                .iter()
                .any(|key| {
                    scene
                        .get(key)
                        .and_then(Value::as_object)
                        .is_some_and(|transitions| !transitions.is_empty())
                });
        if context_changed {
            self.keyboard_action = None;
        }
        if context_changed || declared_transition {
            self.changed = Instant::now();
        }
        self.scene = Some(scene);
        self.measurements.clear();
        Ok(())
    }
    pub fn set_visibility(&mut self, value: Visibility) {
        if value == self.visibility {
            return;
        }
        let animation = self
            .scene
            .as_ref()
            .map(|s| &s["theme"]["animation"])
            .unwrap_or(&Value::Null);
        let (key, duration, bounce) = if value == Visibility::Hidden {
            ("close_ms", 240.0, 0.08)
        } else if self.visibility == Visibility::Hidden {
            ("open_ms", 380.0, 0.24)
        } else if value == Visibility::Expanded {
            ("compact_to_expanded_ms", 400.0, 0.24)
        } else {
            ("collapse_ms", 260.0, 0.08)
        };
        self.motion_duration = number(animation, key, duration) / 1000.0;
        self.motion_bounce = number(
            animation,
            if bounce == 0.08 {
                "exit_bounce"
            } else {
                "bounce"
            },
            bounce,
        );
        if self.visibility != Visibility::Hidden
            && let Some(scene) = self.scene.as_ref()
        {
            self.outgoing = Some((scene.clone(), self.visibility, Instant::now()));
        }
        self.keyboard_action = None;
        self.visibility = value;
        self.changed = Instant::now();
    }
    pub fn extent(&self) -> (i32, i32) {
        (
            (self.shape[0] + 40.0).ceil() as i32,
            (self.shape[1] + 40.0).ceil() as i32,
        )
    }

    pub fn focus_next(&mut self, backwards: bool) {
        if self.hits.is_empty() {
            self.keyboard_action = None;
            return;
        }
        let current = self
            .keyboard_action
            .as_ref()
            .and_then(|action| self.hits.iter().position(|hit| &hit.action == action));
        let next = match current {
            Some(index) if backwards => (index + self.hits.len() - 1) % self.hits.len(),
            Some(index) => (index + 1) % self.hits.len(),
            None if backwards => self.hits.len() - 1,
            None => 0,
        };
        self.keyboard_action = Some(self.hits[next].action.clone());
    }
    pub fn focused_hit(&self) -> Option<HitRegion> {
        self.hits
            .iter()
            .find(|hit| self.keyboard_action.as_ref() == Some(&hit.action))
            .cloned()
    }

    pub fn prepare(&mut self, window: &mut RaylibHandle, thread: &RaylibThread) {
        let width = window.get_screen_width().max(1) as u32 * 2;
        let height = window.get_screen_height().max(1) as u32 * 2;
        if self.canvas.as_ref().is_none_or(|canvas| {
            canvas.texture().width() < width as i32 || canvas.texture().height() < height as i32
        }) {
            self.canvas = window.load_render_texture(thread, width, height).ok();
            if let Some(canvas) = &self.canvas {
                canvas
                    .texture()
                    .set_texture_filter(thread, TextureFilter::TEXTURE_FILTER_BILINEAR);
            }
        }

        let mut characters = (32_u8..=126).map(char::from).collect::<BTreeSet<_>>();
        if let Some(scene) = &self.scene {
            collect_font_characters(scene, &mut characters);
        }
        let characters = characters.into_iter().collect::<String>();
        if (self.fonts.is_empty() || self.font_characters != characters)
            && let Some(fonts) = self
                .scene
                .as_ref()
                .and_then(|scene| scene.pointer("/theme/fonts"))
                .and_then(Value::as_object)
        {
            self.fonts.clear();
            for (name, value) in fonts {
                if let Some(source) = value.as_str() {
                    let path = if Path::new(source).is_absolute() {
                        PathBuf::from(source)
                    } else {
                        self.asset_root.join(source)
                    };
                    if let Some(path) = path.to_str()
                        && let Ok(mut font) =
                            window.load_font_ex(thread, path, 128, Some(&characters))
                    {
                        // Mipmaps prevent small text aliasing; the larger atlas keeps large text sharp.
                        // SAFETY: only texture mip levels change; ownership and glyph storage remain intact.
                        unsafe {
                            let raw: &mut raylib::ffi::Font = font.as_raw_mut();
                            raylib::ffi::GenTextureMipmaps(&mut raw.texture);
                        }
                        font.texture()
                            .set_texture_filter(thread, TextureFilter::TEXTURE_FILTER_TRILINEAR);
                        self.fonts.insert(name.clone(), font);
                    }
                }
            }
            self.font_characters = characters;
        }
        let mut paths = Vec::new();
        if let Some(scene) = &self.scene {
            collect_image_paths(scene, &mut paths);
        }
        for source in paths {
            if self.textures.contains_key(&source) {
                continue;
            }
            let raw = Path::new(source.strip_prefix("file://").unwrap_or(&source));
            let full_path = if raw.is_absolute() {
                raw.to_owned()
            } else {
                self.asset_root.join(raw)
            };
            let path = full_path.to_str().unwrap_or("");
            if Path::new(path).is_file()
                && let Some(texture) = texture_from_path(window, thread, path)
            {
                texture.set_texture_filter(thread, TextureFilter::TEXTURE_FILTER_BILINEAR);
                self.textures.insert(source, texture);
            }
        }
    }
    pub fn draw(
        &mut self,
        drawing: &mut RaylibDrawHandle<'_>,
        thread: &RaylibThread,
        mouse: Vector2,
    ) {
        let width = drawing.get_screen_width();
        let height = drawing.get_screen_height();
        if let Some(mut canvas) = self.canvas.take() {
            {
                let mut target = drawing.begin_texture_mode(thread, &mut canvas);
                target.clear_background(Color::BLANK);
                let mut scaled = target.begin_mode2D(Camera2D {
                    zoom: 2.0,
                    ..Default::default()
                });
                // Preserve coverage in alpha instead of multiplying it by itself.
                unsafe {
                    raylib::ffi::rlSetBlendFactorsSeparate(
                        raylib::ffi::RL_SRC_ALPHA as i32,
                        raylib::ffi::RL_ONE_MINUS_SRC_ALPHA as i32,
                        raylib::ffi::RL_ONE as i32,
                        raylib::ffi::RL_ONE_MINUS_SRC_ALPHA as i32,
                        raylib::ffi::RL_FUNC_ADD as i32,
                        raylib::ffi::RL_FUNC_ADD as i32,
                    );
                }
                let mut blended = scaled.begin_blend_mode(BlendMode::BLEND_CUSTOM_SEPARATE);
                self.draw_content(&mut blended, mouse, width, height, 2.0);
            }
            // The transparent offscreen target already contains premultiplied RGB.
            let mut composite = drawing.begin_blend_mode(BlendMode::BLEND_ALPHA_PREMULTIPLY);
            composite.draw_texture_pro(
                canvas.texture(),
                Rectangle::new(
                    0.0,
                    canvas.texture().height() as f32 - height as f32 * 2.0,
                    width as f32 * 2.0,
                    -(height as f32 * 2.0),
                ),
                Rectangle::new(0.0, 0.0, width as f32, height as f32),
                Vector2::zero(),
                0.0,
                Color::WHITE,
            );
            self.canvas = Some(canvas);
        } else {
            self.draw_content(drawing, mouse, width, height, 1.0);
        }
    }

    fn draw_content(
        &mut self,
        drawing: &mut impl RaylibDraw,
        mouse: Vector2,
        viewport_width: i32,
        viewport_height: i32,
        scale: f32,
    ) {
        let reduced = self
            .scene
            .as_ref()
            .and_then(|s| s.pointer("/interaction/reduced_motion"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let speed = self
            .scene
            .as_ref()
            .and_then(|scene| scene.pointer("/interaction/animation_speed"))
            .and_then(Value::as_f64)
            .unwrap_or(1.0) as f32;
        let now = Instant::now();
        let delta = now.duration_since(self.last_frame).as_secs_f32();
        self.last_frame = now;
        self.hits.clear();
        let mut geometry = if self.visibility == Visibility::Expanded {
            expanded_geometry(self.scene.as_ref())
        } else {
            compact_geometry(self.scene.as_ref())
        };
        geometry.0 = geometry.0.min((viewport_width - 40).max(1) as f32);
        geometry.1 = geometry.1.min((viewport_height - 40).max(1) as f32);
        let hidden = self.visibility == Visibility::Hidden;
        let target = if hidden {
            [
                geometry.0 * 0.72,
                16.0,
                8.0,
                geometry.3,
                geometry.4,
                0.0,
                16.0,
            ]
        } else {
            [
                geometry.0, geometry.1, geometry.2, geometry.3, geometry.4, 1.0, 20.0,
            ]
        };
        for (index, target) in target.into_iter().enumerate() {
            if reduced {
                self.shape[index] = target;
                self.velocity[index] = 0.0;
            } else {
                advance_spring(
                    &mut self.shape[index],
                    &mut self.velocity[index],
                    target,
                    delta * speed,
                    self.motion_duration,
                    if index >= 5 { 0.0 } else { self.motion_bounce },
                    index == 5,
                );
            }
        }
        self.shape[0] = self.shape[0].clamp(1.0, (viewport_width - 40).max(1) as f32);
        self.shape[1] = self.shape[1].clamp(1.0, (viewport_height - 40).max(1) as f32);
        self.shape[2] = self.shape[2].clamp(0.0, self.shape[0].min(self.shape[1]) / 2.0);
        self.shape[5] = self.shape[5].clamp(0.0, 1.0);
        if self.shape[5] == 0.0 {
            return;
        }
        let [width, height, radius, padding_x, padding_y, opacity, y] = self.shape;
        let x = (viewport_width as f32 - width) * 0.5;
        draw_shadow(
            drawing,
            Rectangle::new(x, y, width, height),
            radius,
            opacity,
        );
        drawing.draw_rectangle_rounded(
            Rectangle::new(x, y, width, height),
            (radius * 2.0 / width.min(height).max(1.0)).clamp(0.0, 1.0),
            32,
            Color::BLACK.alpha(opacity),
        );
        let surface = if self.visibility == Visibility::Expanded {
            "expanded"
        } else {
            "compact"
        };
        // Lay out at the destination size so text never scales or reflows with the spring.
        let bounds = Rectangle::new(
            (viewport_width as f32 - geometry.0) * 0.5 + geometry.3,
            y + geometry.4,
            geometry.0 - geometry.3 * 2.0,
            geometry.1 - geometry.4 * 2.0,
        );
        let mut clip = drawing.begin_scissor_mode(
            ((x + padding_x) * scale) as i32,
            ((y + padding_y) * scale) as i32,
            ((width - 2.0 * padding_x).max(0.0) * scale) as i32,
            ((height - 2.0 * padding_y).max(0.0) * scale) as i32,
        );
        if let Some((previous, visibility, began)) = self.outgoing.clone() {
            let alpha = if reduced {
                0.0
            } else {
                1.0 - began.elapsed().as_secs_f32() * speed / 0.10
            };
            if alpha > 0.0 {
                let (width, height, _, px, py) = if visibility == Visibility::Expanded {
                    expanded_geometry(Some(&previous))
                } else {
                    compact_geometry(Some(&previous))
                };
                let old_bounds = Rectangle::new(
                    bounds.x + bounds.width * 0.5 - width * 0.5 + px,
                    y + py,
                    width - px * 2.0,
                    height - py * 2.0,
                );
                self.draw_scene(
                    &mut clip,
                    &previous,
                    if visibility == Visibility::Expanded {
                        "expanded"
                    } else {
                        "compact"
                    },
                    old_bounds,
                    mouse,
                    alpha,
                );
                self.hits.clear();
            } else {
                self.outgoing = None;
            }
        }
        if !hidden && let Some(scene) = self.scene.clone() {
            let alpha = if reduced {
                1.0
            } else {
                (self.changed.elapsed().as_secs_f32() * speed / 0.15).clamp(0.0, 1.0)
            };
            self.draw_scene(&mut clip, &scene, surface, bounds, mouse, alpha);
        }
        if let Some(hit) = self.focused_hit() {
            clip.draw_rectangle_rounded_lines_ex(
                hit.rectangle,
                0.7,
                20,
                2.0,
                Color::new(41, 151, 255, 255),
            );
        }
    }
    fn draw_scene(
        &mut self,
        drawing: &mut impl RaylibDraw,
        scene: &Value,
        surface: &str,
        bounds: Rectangle,
        mouse: Vector2,
        content_alpha: f32,
    ) {
        let Some(root) = scene.get(surface).filter(|value| !value.is_null()) else {
            return;
        };
        let mut root = root.clone();
        if bounds.width < 360.0 {
            fit_calendar(&mut root, bounds.width);
        }
        let owner = scene
            .get(format!("{surface}_owner"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        let resources = scene
            .get("resources")
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(|resource| {
                        let id = resource.get("id")?.as_str()?;
                        let source = resource.get("image")?.get("source")?.as_str()?;
                        Some((id.to_owned(), source.to_owned()))
                    })
                    .collect::<HashMap<_, _>>()
            })
            .unwrap_or_default();
        let mut tree: TaffyTree<Value> = TaffyTree::new();
        let mut nodes = HashMap::new();
        let Ok(root_id) = build_node(&mut tree, &mut nodes, &root) else {
            return;
        };
        if let Ok(current) = tree.style(root_id) {
            let mut style = current.clone();
            style.size.width = length(bounds.width);
            style.size.height = length(bounds.height);
            style.max_size.height = length(bounds.height);
            let _ = tree.set_style(root_id, style);
        }
        let pango = &self.pango;
        let fonts = &self.fonts;
        let measurements = &mut self.measurements;
        if tree
            .compute_layout_with_measure(
                root_id,
                Size {
                    width: AvailableSpace::Definite(bounds.width),
                    height: AvailableSpace::Definite(bounds.height),
                },
                |known, available, _, context, _| {
                    measure_leaf(known, available, context, pango, fonts, measurements)
                },
            )
            .is_err()
        {
            return;
        }
        let reduced = scene
            .pointer("/interaction/reduced_motion")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let speed = scene
            .pointer("/interaction/animation_speed")
            .and_then(Value::as_f64)
            .unwrap_or(1.0) as f32;
        let transition = scene
            .get(format!("{surface}_transitions"))
            .and_then(|value| value.get(surface))
            .and_then(Value::as_str);
        let remaining = if reduced {
            0.0
        } else {
            1.0 - ease_in_out((self.changed.elapsed().as_secs_f32() * speed / 0.18).clamp(0.0, 1.0))
        };
        let offset = match transition {
            Some("slide-left") => 12.0 * remaining,
            Some("slide-right") => -12.0 * remaining,
            _ => 0.0,
        };
        let elapsed = if reduced {
            1000.0
        } else {
            self.meter_changed.elapsed().as_secs_f32() * speed
        };
        let meter_progress = self.meter_target.map(|target| {
            lerp(
                self.meter_from,
                target,
                ease_out((elapsed / 0.20).clamp(0.0, 1.0)),
            )
        });
        draw_node(
            drawing,
            &tree,
            &nodes,
            root_id,
            Vector2::new(bounds.x + offset, bounds.y),
            mouse,
            self.shape[5] * content_alpha,
            elapsed,
            meter_progress,
            owner,
            &self.textures,
            &resources,
            &self.fonts,
            &mut self.hits,
        );
    }
}

fn find_progress(value: &Value) -> Option<(f32, f32)> {
    match value {
        Value::Object(object) => {
            if object.get("type").and_then(Value::as_str) == Some("progress") {
                let target = object.get("value")?.as_f64()? as f32;
                let from = object
                    .get("transition_from")
                    .and_then(Value::as_f64)
                    .unwrap_or(target as f64) as f32;
                return Some((target, from));
            }
            object.values().find_map(find_progress)
        }
        Value::Array(array) => array.iter().find_map(find_progress),
        _ => None,
    }
}

fn configured_geometry(
    scene: Option<&Value>,
    surface: &str,
    style_path: &str,
    width_key: &str,
    height_key: &str,
    defaults: (f32, f32, f32, f32, f32),
) -> (f32, f32, f32, f32, f32) {
    let base = scene.and_then(|scene| scene.pointer(&format!("/theme/view/{surface}")));
    let style = scene
        .and_then(|scene| scene.pointer(style_path))
        .and_then(Value::as_str)
        .and_then(|name| {
            base.and_then(|base| base.get("styles"))
                .and_then(|styles| styles.get(name))
        });
    let number = |name: &str, default: f32| {
        style
            .and_then(|value| value.get(name))
            .or_else(|| base.and_then(|value| value.get(name)))
            .and_then(Value::as_f64)
            .map(|value| value as f32)
            .unwrap_or(default)
    };
    (
        number(width_key, defaults.0),
        scene
            .and_then(|s| s.get(format!("{surface}_presentation")))
            .and_then(|p| p.get(format!("{surface}_height")))
            .and_then(Value::as_f64)
            .map_or(number(height_key, defaults.1), |h| {
                (h as f32).clamp(48.0, number(height_key, defaults.1).max(48.0))
            }),
        number("radius", defaults.2),
        number("padding_horizontal", number("padding", defaults.3)),
        number("padding_vertical", number("padding", defaults.4)),
    )
}

fn compact_geometry(scene: Option<&Value>) -> (f32, f32, f32, f32, f32) {
    configured_geometry(
        scene,
        "compact",
        "/compact_presentation/compact_style",
        "min_width",
        "min_height",
        (230.0, 32.0, 16.0, 14.0, 4.0),
    )
}

fn expanded_geometry(scene: Option<&Value>) -> (f32, f32, f32, f32, f32) {
    configured_geometry(
        scene,
        "expanded",
        "/expanded_presentation/expanded_style",
        "max_width",
        "max_height",
        (432.0, 344.0, 30.0, 24.0, 24.0),
    )
}

fn texture_from_path(
    window: &mut RaylibHandle,
    thread: &RaylibThread,
    path: &str,
) -> Option<Texture2D> {
    if Path::new(path)
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("svg"))
    {
        let pixbuf = gdk_pixbuf::Pixbuf::from_file_at_scale(path, 64, 64, true).ok()?;
        let png = pixbuf.save_to_bufferv("png", &[]).ok()?;
        let image = Image::load_image_from_mem(".png", &png).ok()?;
        window.load_texture_from_image(thread, &image).ok()
    } else {
        window.load_texture(thread, path).ok()
    }
}

fn validate_scene(scene: &Value) -> Result<()> {
    let object = scene
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("scene must be an object"))?;
    if !object.contains_key("theme") {
        bail!("scene has no theme");
    }
    for surface in ["compact", "expanded"] {
        if let Some(value) = object.get(surface).filter(|v| !v.is_null()) {
            validate_node(value, 0)?;
        }
    }
    Ok(())
}
fn validate_node(node: &Value, depth: usize) -> Result<()> {
    if depth > 64 {
        bail!("scene nesting exceeds 64");
    }
    let object = node
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("component must be an object"))?;
    let kind = object
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("component has no type"))?;
    if ![
        "text",
        "rich_text",
        "icon",
        "image",
        "row",
        "column",
        "spacer",
        "progress",
        "indicator",
        "divider",
        "button",
        "action_region",
    ]
    .contains(&kind)
    {
        bail!("unknown component type: {kind}");
    }
    if matches!(kind, "row" | "column") {
        let children = object
            .get("children")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow::anyhow!("{kind} requires children"))?;
        if children.len() > 256 {
            bail!("too many children");
        }
        for child in children {
            validate_node(child, depth + 1)?;
        }
    }
    if matches!(kind, "button" | "action_region") {
        validate_node(
            object
                .get("content")
                .ok_or_else(|| anyhow::anyhow!("{kind} requires content"))?,
            depth + 1,
        )?;
    }
    Ok(())
}

fn build_node(
    tree: &mut TaffyTree<Value>,
    nodes: &mut HashMap<NodeId, Value>,
    node: &Value,
) -> Result<NodeId> {
    let kind = node.get("type").and_then(Value::as_str).unwrap_or("text");
    let gap = number(node, "_gap", gap_value(node.get("gap")));
    let mut style = Style {
        display: Display::Flex,
        gap: Size {
            width: length(gap),
            height: length(gap),
        },
        flex_shrink: 0.0,
        flex_grow: number(node, "grow", 0.0),
        min_size: Size {
            width: length(0.0),
            height: length(0.0),
        },
        ..Default::default()
    };
    if node.get("grow").is_some() {
        style.flex_basis = length(0.0);
    }
    let id = match kind {
        "row" | "column" => {
            style.flex_direction = if kind == "row" {
                FlexDirection::Row
            } else {
                FlexDirection::Column
            };
            style.align_items = Some(match node.get("alignment").and_then(Value::as_str) {
                Some("start") => AlignItems::FLEX_START,
                Some("stretch") => AlignItems::STRETCH,
                Some("end") => AlignItems::FLEX_END,
                _ => AlignItems::CENTER,
            });
            let ids = node
                .get("children")
                .and_then(Value::as_array)
                .unwrap()
                .iter()
                .map(|child| build_node(tree, nodes, child))
                .collect::<Result<Vec<_>>>()?;
            tree.new_with_children(style, &ids)?
        }
        "button" | "action_region" => {
            let input = node.get("style").and_then(Value::as_str) == Some("input");
            style.align_items = Some(AlignItems::CENTER);
            style.justify_content = Some(JustifyContent::CENTER);
            let control = &node["_control"];
            if let Some(width) = control["width"].as_f64() {
                style.size.width = length(width as f32);
            }
            if let Some(height) = control["height"].as_f64() {
                style.min_size.height = length(height as f32);
            }
            if input {
                style.size.width = percent(1.0);
            }
            let (horizontal, vertical) = if input {
                (10.0, 8.0)
            } else if control.is_object() {
                (number(control, "padding", 0.0), 0.0)
            } else if kind == "action_region" {
                (0.0, 0.0)
            } else {
                (6.0, 4.0)
            };
            style.padding = Rect {
                left: length(horizontal),
                right: length(horizontal),
                top: length(vertical),
                bottom: length(vertical),
            };
            let child = build_node(tree, nodes, node.get("content").unwrap())?;
            if kind == "action_region" {
                let mut child_style = tree.style(child)?.clone();
                child_style.flex_grow = 1.0;
                child_style.flex_shrink = 1.0;
                child_style.min_size.width = length(0.0);
                tree.set_style(child, child_style)?;
            }
            tree.new_with_children(style, &[child])?
        }
        "divider" => {
            style.size = Size {
                width: percent(1.0),
                height: length(1.0),
            };
            tree.new_leaf(style)?
        }
        "spacer" => {
            if node
                .get("flexible")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                style.flex_grow = 1.0;
            }
            tree.new_leaf_with_context(style, node.clone())?
        }
        "progress" if node.get("shape").and_then(Value::as_str) != Some("ring") => {
            style.flex_grow = 1.0;
            style.min_size.width = length(80.0);
            tree.new_leaf_with_context(style, node.clone())?
        }
        _ => tree.new_leaf_with_context(style, node.clone())?,
    };
    nodes.insert(id, node.clone());
    Ok(id)
}
fn measure_leaf(
    known: Size<Option<f32>>,
    available: Size<AvailableSpace>,
    context: Option<&mut Value>,
    pango: &pango::Context,
    fonts: &HashMap<String, Font>,
    measurements: &mut HashMap<String, (f32, f32)>,
) -> Size<f32> {
    if let Size {
        width: Some(width),
        height: Some(height),
    } = known
    {
        return Size { width, height };
    }
    let Some(node) = context else {
        return Size::ZERO;
    };
    let kind = node.get("type").and_then(Value::as_str).unwrap_or("text");
    let measured = match kind {
        "text" | "rich_text" => {
            let text = component_text(node);
            let size = number(
                node,
                "_size",
                role_size(node.get("role").and_then(Value::as_str)),
            );
            let available_width = match available.width {
                AvailableSpace::Definite(width) => width,
                _ => -1.0,
            };
            let key = format!("{size}:{available_width}:{}:{text}", node["_font"]);
            if let Some(&(width, height)) = measurements.get(&key) {
                return Size { width, height };
            }
            if let Some(font) = node["_font"].as_str().and_then(|name| fonts.get(name)) {
                let draw_size = size * (2288.0 / 2048.0);
                let line_height = size * number(node, "_line_height", 1.2);
                let lines = if node["_single_line"] == true {
                    vec![text.clone()]
                } else {
                    wrap_text(
                        font,
                        &text,
                        draw_size,
                        if available_width < 0.0 {
                            f32::MAX
                        } else {
                            available_width
                        },
                    )
                };
                let natural_width = lines
                    .iter()
                    .map(|line| font.measure_text(line, draw_size, 0.0).x)
                    .fold(0.0, f32::max)
                    .ceil();
                let width = if node["_single_line"] == true && available_width >= 0.0 {
                    natural_width.min(available_width)
                } else {
                    natural_width
                };
                let height = (line_height * lines.len().max(1) as f32).ceil();
                measurements.insert(key, (width, height));
                return Size {
                    width: known.width.unwrap_or(width),
                    height: known.height.unwrap_or(height),
                };
            }
            let layout = pango::Layout::new(pango);
            layout.set_text(&text);
            let mut font = pango::FontDescription::new();
            font.set_family("Liberation Sans");
            if node["_font"] == "semibold" {
                font.set_weight(pango::Weight::Bold);
            }
            font.set_absolute_size(f64::from(size) * f64::from(pango::SCALE));
            layout.set_font_description(Some(&font));
            if let AvailableSpace::Definite(width) = available.width {
                layout.set_width((width * pango::SCALE as f32) as i32);
                layout.set_wrap(pango::WrapMode::WordChar);
            }
            let (width, height) = layout.pixel_size();
            measurements.insert(key, (width as f32, height as f32));
            Size {
                width: width as f32,
                height: (height as f32).max(size * number(node, "_line_height", 1.2)),
            }
        }
        "icon" => {
            let size = number(node, "size", number(node, "_size", 20.0));
            Size {
                width: size,
                height: size,
            }
        }
        "image" => Size {
            width: number(node, "_width", 32.0),
            height: number(node, "_height", 32.0),
        },
        "progress" if node.get("shape").and_then(Value::as_str) == Some("ring") => Size {
            width: number(node, "size", 32.0),
            height: number(node, "size", 32.0),
        },
        "progress" => Size {
            width: 120.0,
            height: 5.0,
        },
        "indicator" => Size {
            width: 20.0,
            height: 20.0,
        },
        "spacer" => Size {
            width: number(node, "size", 6.0),
            height: number(node, "size", 6.0),
        },
        _ => Size::ZERO,
    };
    Size {
        width: known.width.unwrap_or(measured.width),
        height: known.height.unwrap_or(measured.height),
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_node(
    d: &mut impl RaylibDraw,
    tree: &TaffyTree<Value>,
    nodes: &HashMap<NodeId, Value>,
    id: NodeId,
    parent: Vector2,
    mouse: Vector2,
    alpha: f32,
    elapsed: f32,
    meter_progress: Option<f32>,
    owner: Option<String>,
    textures: &HashMap<String, Texture2D>,
    resources: &HashMap<String, String>,
    fonts: &HashMap<String, Font>,
    hits: &mut Vec<HitRegion>,
) {
    let Ok(layout) = tree.layout(id) else { return };
    let origin = Vector2::new(parent.x + layout.location.x, parent.y + layout.location.y);
    let rect = Rectangle::new(origin.x, origin.y, layout.size.width, layout.size.height);
    let node = nodes.get(&id);
    let kind = node
        .and_then(|v| v.get("type"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let hovered = rect.check_collision_point_rec(mouse);
    let color = node_color(node, alpha);
    match kind {
        "text" | "rich_text" => {
            let text = node.map(component_text).unwrap_or_default();
            let role = node.and_then(|v| v.get("role")).and_then(Value::as_str);
            let size = node.map_or(role_size(role), |n| number(n, "_size", role_size(role)));
            let font_name = if matches!(
                role,
                Some("title" | "caption" | "button" | "compact-primary" | "current" | "accent")
            ) {
                "semibold"
            } else {
                "regular"
            };
            let font_name = node.and_then(|n| n["_font"].as_str()).unwrap_or(font_name);
            if let Some(font) = fonts.get(font_name) {
                let line_height = size * node.map_or(1.2, |n| number(n, "_line_height", 1.2));
                // shortcut: calibrated to bundled Liberation Sans metrics; recalibrate if replacing the text fonts.
                let draw_size = size * (2288.0 / 2048.0);
                let lines = if node.is_some_and(|n| n["_single_line"] == true) {
                    let mut label = text.clone();
                    if font.measure_text(&label, draw_size, 0.0).x > rect.width + 0.5 {
                        while !label.is_empty()
                            && font.measure_text(&(label.clone() + "…"), draw_size, 0.0).x
                                > rect.width
                        {
                            label.pop();
                        }
                        label.push('…');
                    }
                    vec![label]
                } else {
                    wrap_text(font, &text, draw_size, rect.width)
                };
                for (index, line) in lines.iter().enumerate() {
                    if index as f32 * line_height + size > rect.height + 2.0 {
                        break;
                    }
                    d.draw_text_ex(
                        font,
                        line,
                        Vector2::new(origin.x, origin.y + index as f32 * line_height),
                        draw_size,
                        0.0,
                        color,
                    );
                }
            } else {
                d.draw_text(&text, origin.x as i32, origin.y as i32, size as i32, color);
            }
            if kind == "rich_text"
                && let Some(items) = node
                    .and_then(|value| value.get("content"))
                    .and_then(Value::as_array)
            {
                for item in items {
                    if item.get("type").and_then(Value::as_str) == Some("image")
                        && let Some(texture) = item
                            .get("resource_id")
                            .and_then(Value::as_str)
                            .and_then(|id| resources.get(id))
                            .and_then(|source| textures.get(source))
                    {
                        let image = Rectangle::new(
                            rect.x + rect.width - 48.0,
                            rect.y,
                            48.0,
                            rect.height.min(48.0),
                        );
                        d.draw_texture_pro(
                            texture,
                            Rectangle::new(0.0, 0.0, texture.width as f32, texture.height as f32),
                            image,
                            Vector2::zero(),
                            0.0,
                            Color::WHITE.alpha(alpha),
                        );
                    }
                }
            }
        }
        "icon" => {
            let name = node
                .and_then(|v| v.get("name"))
                .and_then(Value::as_str)
                .unwrap_or("");
            let application = name == "application";
            let codepoint = match name {
                "chevron-left" => 0xF053,
                "chevron-right" => 0xF054,
                "close" => 0xF00D,
                "lock" => 0xF023,
                "sleep" => 0xF186,
                "reboot" => 0xF2F1,
                "power-off" => 0xF011,
                "volume-muted" => 0xF6A9,
                "volume-low" => 0xF027,
                "volume-high" => 0xF028,
                "notification" => 0xF0F3,
                "warning" => 0xF071,
                _ => 0xF111,
            };
            let symbol = if application {
                "?".to_owned()
            } else {
                char::from_u32(codepoint).unwrap_or('•').to_string()
            };
            let icon_origin = if application {
                Vector2::new(origin.x + 6.0, origin.y + 6.0)
            } else {
                origin
            };
            if let Some(texture) = node
                .and_then(|n| n["_icon"]["source"].as_str())
                .and_then(|source| textures.get(source))
            {
                let icon_rect = if node.is_some_and(|n| n["style"] == "app-symbol") {
                    d.draw_rectangle_rounded(
                        rect,
                        0.6,
                        20,
                        Color::new(39, 39, 41, 255).alpha(alpha),
                    );
                    Rectangle::new(rect.x + 10.0, rect.y + 10.0, 20.0, 20.0)
                } else {
                    rect
                };
                d.draw_texture_pro(
                    texture,
                    Rectangle::new(0.0, 0.0, texture.width as f32, texture.height as f32),
                    icon_rect,
                    Vector2::zero(),
                    0.0,
                    color,
                );
            } else if let Some(font) = fonts.get(if application { "regular" } else { "icons" }) {
                d.draw_text_ex(
                    font,
                    &symbol,
                    icon_origin,
                    rect.width.min(rect.height),
                    0.0,
                    color,
                );
            } else {
                d.draw_text(
                    if application { "?" } else { "•" },
                    icon_origin.x as i32,
                    icon_origin.y as i32,
                    20,
                    color,
                );
            }
        }
        "image" => {
            let source = node
                .and_then(|value| value.get("source"))
                .and_then(|value| value.get("source"))
                .and_then(Value::as_str)
                .or_else(|| {
                    node.and_then(|value| value.get("resource_id"))
                        .and_then(Value::as_str)
                        .and_then(|id| resources.get(id).map(String::as_str))
                });
            if let Some(texture) = source.and_then(|source| textures.get(source)) {
                d.draw_texture_pro(
                    texture,
                    Rectangle::new(0.0, 0.0, texture.width as f32, texture.height as f32),
                    rect,
                    Vector2::zero(),
                    0.0,
                    Color::WHITE.alpha(alpha),
                );
            } else {
                d.draw_circle(
                    (origin.x + 16.0) as i32,
                    (origin.y + 16.0) as i32,
                    16.0,
                    color.alpha(0.55),
                );
            }
        }
        "progress" => {
            let target = node
                .and_then(|v| v.get("value"))
                .and_then(Value::as_f64)
                .unwrap_or(0.0) as f32;
            let from = node
                .and_then(|v| v.get("transition_from"))
                .and_then(Value::as_f64)
                .unwrap_or(target as f64) as f32;
            let progress = meter_progress
                .unwrap_or_else(|| lerp(from, target, ease_out((elapsed / 0.20).clamp(0.0, 1.0))))
                .clamp(0.0, 1.0);
            if node.and_then(|v| v.get("shape")).and_then(Value::as_str) == Some("ring") {
                let radius =
                    rect.width.min(rect.height) * 0.5 - if rect.width > 32.0 { 2.5 } else { 0.0 };
                let center =
                    Vector2::new(origin.x + rect.width * 0.5, origin.y + rect.height * 0.5);
                d.draw_ring(
                    center,
                    radius - 5.0,
                    radius,
                    -90.0,
                    270.0,
                    48,
                    color.alpha(0.25 * alpha),
                );
                d.draw_ring(
                    center,
                    radius - 5.0,
                    radius,
                    -90.0,
                    -90.0 + progress * 360.0,
                    48,
                    color,
                );
                if progress > 0.0 {
                    let middle = radius - 2.5;
                    for angle in [
                        -std::f32::consts::FRAC_PI_2,
                        -std::f32::consts::FRAC_PI_2 + progress * std::f32::consts::TAU,
                    ] {
                        d.draw_circle_v(
                            Vector2::new(
                                center.x + middle * angle.cos(),
                                center.y + middle * angle.sin(),
                            ),
                            2.5,
                            color,
                        );
                    }
                }
                if rect.width > 32.0 {
                    let points = [
                        (-1.0, -12.0),
                        (-10.0, 2.0),
                        (-2.0, 2.0),
                        (-5.0, 14.0),
                        (6.0, -6.0),
                        (-3.0, -6.0),
                        (-1.0, -12.0),
                    ];
                    for edge in points.windows(2) {
                        d.draw_line_ex(
                            Vector2::new(center.x + edge[0].0, center.y + edge[0].1),
                            Vector2::new(center.x + edge[1].0, center.y + edge[1].1),
                            1.8,
                            Color::new(245, 245, 247, 255).alpha(alpha),
                        );
                    }
                }
            } else {
                let thickness = 5.0;
                let track = Rectangle::new(
                    origin.x,
                    origin.y + (rect.height - thickness) * 0.5,
                    rect.width,
                    thickness,
                );
                d.draw_rectangle_rounded(
                    track,
                    1.0,
                    10,
                    Color::new(39, 39, 41, (255.0 * alpha) as u8),
                );
                d.draw_rectangle_rounded(
                    Rectangle::new(track.x, track.y, track.width * progress, track.height),
                    1.0,
                    10,
                    if node
                        .and_then(|value| value.get("state"))
                        .and_then(Value::as_str)
                        == Some("foreground")
                    {
                        Color::WHITE.alpha(alpha)
                    } else {
                        color
                    },
                );
            }
        }
        "indicator" => {
            let effect = node
                .and_then(|v| v.get("effect"))
                .and_then(Value::as_str)
                .unwrap_or("");
            let pulse = if effect == "breathe" {
                0.65 + (elapsed * std::f32::consts::TAU / 1.6).sin() * 0.35
            } else {
                1.0
            };
            let center = Vector2::new(origin.x + rect.width / 2.0, origin.y + rect.height / 2.0);
            if effect == "shadow" {
                d.draw_circle_v(
                    Vector2::new(center.x + 1.0, center.y + 2.0),
                    5.0,
                    Color::BLACK.alpha(0.32 * alpha),
                );
            }
            if matches!(effect, "glow" | "breathe") {
                d.draw_circle_v(center, 8.0 * pulse, color.alpha(0.28));
            }
            d.draw_circle_v(center, 3.5, color);
        }
        "button" | "action_region"
            if node
                .and_then(|value| value.get("style"))
                .and_then(Value::as_str)
                == Some("input") =>
        {
            d.draw_rectangle_rounded(
                rect,
                0.35,
                10,
                Color::new(28, 28, 30, (255.0 * alpha) as u8),
            );
            d.draw_rectangle_rounded_lines_ex(
                rect,
                0.35,
                10,
                1.5,
                Color::new(41, 151, 255, (190.0 * alpha) as u8),
            );
        }
        "button" | "action_region" => {
            let control = node.map(|n| &n["_control"]);
            let radius = control.map_or(12.0, |c| number(c, "radius", 12.0));
            let roundness = (radius * 2.0 / rect.width.min(rect.height).max(1.0)).clamp(0.0, 1.0);
            if let Some(background) = control
                .and_then(|c| c["background"].as_str())
                .and_then(parse_color)
            {
                d.draw_rectangle_rounded(rect, roundness, 20, background.alpha(alpha));
            }
            if hovered {
                d.draw_rectangle_rounded(rect, roundness, 20, Color::WHITE.alpha(0.12 * alpha));
            }
            let border = control
                .and_then(|c| c["border"].as_str())
                .and_then(parse_color);
            if let Some(border) = border {
                d.draw_rectangle_rounded_lines_ex(rect, roundness, 20, 1.0, border.alpha(alpha));
            }
            if node.is_some_and(|n| n["focused"] == true) {
                d.draw_rectangle_rounded_lines_ex(
                    rect,
                    roundness,
                    20,
                    2.0,
                    Color::new(41, 151, 255, 255).alpha(alpha),
                );
            }
        }
        "divider" => d.draw_rectangle_rec(rect, color),
        _ => {}
    }
    if matches!(kind, "button" | "action_region")
        && let Some(action) = node
            .and_then(|v| v.get("action_id"))
            .and_then(Value::as_str)
    {
        hits.push(HitRegion {
            rectangle: rect,
            owner: owner.clone(),
            action: action.to_owned(),
        });
    }
    if kind == "rich_text"
        && let Some(items) = node
            .and_then(|value| value.get("content"))
            .and_then(Value::as_array)
    {
        for item in items {
            if let Some(action) = item.get("action_id").and_then(Value::as_str) {
                hits.push(HitRegion {
                    rectangle: rect,
                    owner: owner.clone(),
                    action: action.to_owned(),
                });
            }
        }
    }
    if let Ok(children) = tree.children(id) {
        for child in children {
            draw_node(
                d,
                tree,
                nodes,
                child,
                origin,
                mouse,
                alpha,
                elapsed,
                meter_progress,
                owner.clone(),
                textures,
                resources,
                fonts,
                hits,
            );
        }
    }
}

fn component_text(node: &Value) -> String {
    if let Some(value) = node.get("value").and_then(Value::as_str) {
        return value.to_owned();
    }
    if let Some(value) = node.get("content").and_then(Value::as_str) {
        return value.to_owned();
    }
    node.get("content")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    item.get("value")
                        .or_else(|| item.get("text"))
                        .and_then(Value::as_str)
                })
                .collect::<String>()
        })
        .unwrap_or_default()
}

fn collect_font_characters(value: &Value, characters: &mut BTreeSet<char>) {
    if let Some(text) = value.as_str() {
        characters.extend(text.chars().filter(|c| !c.is_control()));
    } else if let Some(object) = value.as_object() {
        if let Some(codepoint) = object.get("codepoint").and_then(Value::as_u64)
            && let Ok(codepoint) = u32::try_from(codepoint)
            && let Some(character) = char::from_u32(codepoint)
        {
            characters.insert(character);
        }
        for child in object.values() {
            collect_font_characters(child, characters);
        }
    } else if let Some(array) = value.as_array() {
        for child in array {
            collect_font_characters(child, characters);
        }
    }
}

fn collect_image_paths(value: &Value, paths: &mut Vec<String>) {
    if let Some(source) = value.get("source").and_then(Value::as_str)
        && matches!(
            value.get("kind").and_then(Value::as_str),
            Some("path" | "icon")
        )
    {
        paths.push(source.to_owned());
    }
    if let Some(object) = value.as_object() {
        for child in object.values() {
            collect_image_paths(child, paths);
        }
    } else if let Some(array) = value.as_array() {
        for child in array {
            collect_image_paths(child, paths);
        }
    }
}
fn fit_calendar(node: &mut Value, width: f32) {
    if node["style"]
        .as_str()
        .is_some_and(|style| style.starts_with("day"))
    {
        node["_control"]["width"] = Value::from(((width - 24.0) / 7.0).min(44.0));
    }
    if let Some(children) = node.get_mut("children").and_then(Value::as_array_mut) {
        for child in children {
            fit_calendar(child, width);
        }
    }
    if matches!(node["type"].as_str(), Some("button" | "action_region")) {
        fit_calendar(&mut node["content"], width);
    }
}

fn number(node: &Value, name: &str, default: f32) -> f32 {
    node.get(name)
        .and_then(Value::as_f64)
        .filter(|n| n.is_finite())
        .map_or(default, |n| n as f32)
}

fn parse_color(value: &str) -> Option<Color> {
    let hex = value.strip_prefix('#')?;
    if hex.len() != 6 && hex.len() != 8 {
        return None;
    }
    let rgb = u32::from_str_radix(hex, 16).ok()?;
    Some(if hex.len() == 6 {
        Color::new((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8, 255)
    } else {
        Color::new(
            (rgb >> 24) as u8,
            (rgb >> 16) as u8,
            (rgb >> 8) as u8,
            rgb as u8,
        )
    })
}

fn resolve_theme(node: &mut Value, theme: &Value) {
    let role = node["role"].as_str().unwrap_or("body");
    let single_line = role.starts_with("compact-");
    let typography = &theme["typography"][role];
    let palette = &theme["palette"];
    let token = node["state"]
        .as_str()
        .or_else(|| typography["color"].as_str())
        .unwrap_or("foreground");
    let color = if node["type"] == "divider" {
        &palette["divider"]
    } else {
        &palette[token]
    };
    let size = if node["type"] == "icon" && node["role"].is_null() {
        20.0
    } else {
        typography["size"]
            .as_f64()
            .unwrap_or(role_size(Some(role)) as f64)
    };
    let font = typography["font"].as_str().unwrap_or("regular").to_owned();
    let image = theme["images"][role].clone();
    let gap = theme["gaps"][node["gap"].as_str().unwrap_or("normal")].clone();
    let mut control = theme["controls"][node["style"].as_str().unwrap_or("")].clone();
    for key in ["background", "border"] {
        if let Some(token) = control[key].as_str() {
            let resolved = palette[token].clone();
            if !resolved.is_null() {
                control[key] = resolved;
            }
        }
    }
    if let Some(source) = theme["icons"][node["name"].as_str().unwrap_or("")]["source"].as_str() {
        node["_icon"] = serde_json::json!({ "kind": "path", "source": source });
    }
    node["_single_line"] = Value::from(single_line);
    node["_color"] = color.clone();
    node["_size"] = Value::from(size.clamp(1.0, 128.0));
    node["_font"] = Value::from(font);
    node["_line_height"] = typography["line_height"].clone();
    node["_control"] = control;
    node["_gap"] = gap;
    node["_width"] = image["width"].clone();
    node["_height"] = image["height"].clone();
    if let Some(children) = node.get_mut("children").and_then(Value::as_array_mut) {
        for child in children {
            resolve_theme(child, theme);
        }
    }
    if matches!(node["type"].as_str(), Some("button" | "action_region")) {
        resolve_theme(&mut node["content"], theme);
    }
}

fn wrap_text(font: &Font, text: &str, size: f32, width: f32) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split_inclusive(char::is_whitespace) {
        if !line.is_empty()
            && font
                .measure_text(&(line.clone() + word.trim_end()), size, 0.0)
                .x
                > width + 0.5
        {
            lines.push(line.trim_end().to_owned());
            line.clear();
        }
        for character in word.chars() {
            if character == '\n' {
                lines.push(std::mem::take(&mut line));
                continue;
            }
            if !line.is_empty()
                && font
                    .measure_text(&(line.clone() + &character.to_string()), size, 0.0)
                    .x
                    > width + 0.5
            {
                lines.push(std::mem::take(&mut line));
            }
            line.push(character);
        }
    }
    if !line.is_empty() {
        lines.push(line.trim_end().to_owned());
    }
    lines
}

fn role_size(role: Option<&str>) -> f32 {
    match role {
        Some("title") => 22.0,
        Some("caption" | "button" | "compact-primary" | "compact-secondary") => 12.0,
        Some("muted") => 14.0,
        _ => 16.0,
    }
}
fn node_color(node: Option<&Value>, alpha: f32) -> Color {
    if let Some(color) = node
        .and_then(|n| n["_color"].as_str())
        .and_then(parse_color)
    {
        return color.alpha(alpha);
    }
    let role = node
        .and_then(|v| v.get("state").or_else(|| v.get("role")))
        .and_then(Value::as_str);
    let color = match role {
        Some("muted" | "input-muted" | "caption" | "compact-secondary") => {
            Color::new(139, 139, 147, 255)
        }
        Some("accent") => Color::new(124, 92, 252, 255),
        Some("success") => Color::new(48, 209, 88, 255),
        Some("warning") => Color::new(255, 214, 10, 255),
        Some("error") => Color::new(255, 69, 58, 255),
        _ => Color::new(247, 247, 248, 255),
    };
    color.alpha(alpha)
}
fn gap_value(value: Option<&Value>) -> f32 {
    match value.and_then(Value::as_str) {
        Some("xsmall") => 4.0,
        Some("small") => 8.0,
        Some("large") => 20.0,
        _ => 12.0,
    }
}
fn draw_shadow(d: &mut impl RaylibDraw, rect: Rectangle, radius: f32, opacity: f32) {
    for step in (1..=16).rev() {
        let grow = step as f32;
        let shadow = Rectangle::new(
            rect.x - grow,
            rect.y + 8.0 - grow,
            rect.width + grow * 2.0,
            rect.height + grow * 2.0,
        );
        d.draw_rectangle_rounded(
            shadow,
            ((radius + grow) * 2.0 / shadow.width.min(shadow.height)).clamp(0.0, 1.0),
            32,
            Color::new(0, 0, 0, (1.5 * opacity) as u8),
        );
    }
}
fn cubic_bezier(progress: f32, x1: f32, y1: f32, x2: f32, y2: f32) -> f32 {
    let sample = |point: f32, control1: f32, control2: f32| {
        let inverse = 1.0 - point;
        3.0 * inverse * inverse * point * control1
            + 3.0 * inverse * point * point * control2
            + point * point * point
    };
    let derivative = |point: f32, control1: f32, control2: f32| {
        let inverse = 1.0 - point;
        3.0 * inverse * inverse * control1
            + 6.0 * inverse * point * (control2 - control1)
            + 3.0 * point * point * (1.0 - control2)
    };
    let progress = progress.clamp(0.0, 1.0);
    let mut point = progress;
    for _ in 0..6 {
        let slope = derivative(point, x1, x2);
        if slope.abs() < 0.0001 {
            break;
        }
        point = (point - (sample(point, x1, x2) - progress) / slope).clamp(0.0, 1.0);
    }
    sample(point, y1, y2)
}

fn ease_in_out(value: f32) -> f32 {
    cubic_bezier(value, 0.42, 0.0, 0.58, 1.0)
}

fn ease_out(value: f32) -> f32 {
    cubic_bezier(value, 0.0, 0.0, 0.58, 1.0)
}

// Closed-form damped spring: retargeting preserves both position and velocity.
fn advance_spring(
    value: &mut f32,
    velocity: &mut f32,
    target: f32,
    delta: f32,
    duration: f32,
    bounce: f32,
    opacity: bool,
) {
    if duration <= 0.0 {
        *value = target;
        *velocity = 0.0;
        return;
    }
    let omega = std::f32::consts::TAU / (duration * 0.6);
    let damping = (1.0 - bounce).clamp(0.01, 1.0);
    let displacement = *value - target;
    let dt = delta.clamp(0.0, 0.1);
    if damping >= 1.0 {
        let decay = (-omega * dt).exp();
        let term = *velocity + omega * displacement;
        *value = target + (displacement + term * dt) * decay;
        *velocity = (*velocity - omega * term * dt) * decay;
    } else {
        let alpha = damping * omega;
        let frequency = omega * (1.0 - damping * damping).sqrt();
        let decay = (-alpha * dt).exp();
        let (sine, cosine) = (frequency * dt).sin_cos();
        let term = (*velocity + alpha * displacement) / frequency;
        *value = target + decay * (displacement * cosine + term * sine);
        *velocity = decay * (*velocity * cosine - (alpha * term + frequency * displacement) * sine);
    }
    let (position_epsilon, speed_epsilon) = if opacity { (0.002, 0.04) } else { (0.25, 4.0) };
    if (target - *value).abs() < position_epsilon && velocity.abs() < speed_epsilon {
        *value = target;
        *velocity = 0.0;
    }
}
fn lerp(from: f32, to: f32, amount: f32) -> f32 {
    from + (to - from) * amount
}

#[cfg(test)]
mod tests {
    use super::{advance_spring, expanded_geometry};
    use serde_json::json;

    #[test]
    fn keyboard_focus_cycles_and_wraps() {
        let mut renderer = super::Renderer::new(std::path::PathBuf::from("assets"));
        for action in ["cancel", "confirm"] {
            renderer.hits.push(super::HitRegion {
                rectangle: raylib::prelude::Rectangle::new(0.0, 0.0, 44.0, 44.0),
                owner: None,
                action: action.into(),
            });
        }
        renderer.focus_next(false);
        assert_eq!(renderer.focused_hit().unwrap().action, "cancel");
        renderer.focus_next(true);
        assert_eq!(renderer.focused_hit().unwrap().action, "confirm");
        renderer.focus_next(false);
        assert_eq!(renderer.focused_hit().unwrap().action, "cancel");
    }

    #[test]
    fn spring_retarget_preserves_momentum_and_critical_motion_never_overshoots() {
        let (mut value, mut velocity) = (0.0, 0.0);
        advance_spring(&mut value, &mut velocity, 100.0, 0.04, 0.4, 0.24, false);
        let before = value;
        advance_spring(&mut value, &mut velocity, 0.0, 0.001, 0.26, 0.08, false);
        assert!(
            value > before,
            "an interruption must preserve forward momentum"
        );
        let (mut opacity, mut speed) = (0.0, 0.0);
        for _ in 0..240 {
            advance_spring(&mut opacity, &mut speed, 1.0, 1.0 / 240.0, 0.38, 0.0, true);
            assert!((0.0..=1.0).contains(&opacity));
        }
        assert_eq!(opacity, 1.0);
    }

    #[test]
    #[ignore = "requires Xvfb; writes native screenshots to GISLAND_SCREENSHOTS"]
    fn render_design_fixtures() {
        use mlua::LuaSerdeExt;
        use raylib::prelude::*;
        let lua = mlua::Lua::new();
        let fixtures: mlua::Value = lua
            .load(include_str!("../scripts/design-fixtures.lua"))
            .eval()
            .unwrap();
        let scenes: Vec<serde_json::Value> = lua.from_value(fixtures).unwrap();
        let output = std::env::var("GISLAND_SCREENSHOTS")
            .unwrap_or_else(|_| "/tmp/gisland-screenshots".into());
        std::fs::create_dir_all(&output).unwrap();
        let (mut window, thread) = raylib::init()
            .size(472, 580)
            .title("gisland visual tests")
            .msaa_4x()
            .build();
        window.set_target_fps(60);
        let mut renderer = super::Renderer::new(std::path::PathBuf::from("assets"));
        for viewport_width in [472, 360] {
            window.set_window_size(viewport_width, 580);
            for scene in scenes.clone() {
                let name = scene["name"].as_str().unwrap().to_owned();
                let name = if viewport_width == 472 {
                    name
                } else {
                    format!("{name}-narrow")
                };
                let expanded = scene["surface"] == "expanded";
                renderer.set_scene(scene).unwrap();
                renderer.set_visibility(if expanded {
                    super::Visibility::Expanded
                } else {
                    super::Visibility::Compact
                });
                renderer.prepare(&mut window, &thread);
                for _ in 0..3 {
                    let mut d = window.begin_drawing(&thread);
                    d.clear_background(Color::new(245, 245, 247, 255));
                    renderer.draw(&mut d, &thread, Vector2::new(-1.0, -1.0));
                }
                for hit in &renderer.hits {
                    assert!(
                        hit.rectangle.x >= 0.0
                            && hit.rectangle.x + hit.rectangle.width <= viewport_width as f32 + 0.5,
                        "{name}: horizontal overflow for {}",
                        hit.action
                    );
                    assert!(
                        hit.rectangle.y >= 0.0 && hit.rectangle.y + hit.rectangle.height <= 580.5,
                        "{name}: vertical overflow for {}",
                        hit.action
                    );
                }
                let screenshot = window.load_image_from_screen(&thread);
                assert_eq!(
                    screenshot.get_color(viewport_width / 2, 22),
                    Color::BLACK,
                    "{name}: surface must be opaque black"
                );
                assert!(
                    (200..245).contains(&screenshot.get_color(viewport_width / 2, 18).r),
                    "{name}: shadow must remain translucent"
                );
                if name == "calendar" {
                    let has_coverage = (235..247).any(|y| {
                        (110..132).any(|x| {
                            let pixel = screenshot.get_color(x, y);
                            pixel.r == 0 && pixel.b > 0 && pixel.b < 227
                        })
                    });
                    assert!(
                        has_coverage,
                        "calendar: selected circle must have antialiased edges"
                    );
                }
                screenshot.export_image(&format!("{output}/{name}.png"));
                assert!(std::path::Path::new(&format!("{output}/{name}.png")).is_file());
            }
        }
    }

    #[test]
    fn scene_uses_theme_typography_and_semantic_colors() {
        let mut renderer = super::Renderer::new(std::path::PathBuf::from("assets"));
        renderer
            .set_scene(json!({
                "theme": { "palette": { "foreground": "#123456" }, "typography": {
                    "body": { "size": 23, "color": "foreground", "font": "regular" }
                } },
                "compact": { "type": "text", "role": "body", "value": "Hello" }
            }))
            .unwrap();
        let node = &renderer.scene.as_ref().unwrap()["compact"];
        assert_eq!(
            super::node_color(Some(node), 1.0),
            raylib::prelude::Color::new(18, 52, 86, 255)
        );
    }

    #[test]
    fn scene_accepts_dividers_between_facts() {
        assert!(
            super::validate_scene(&json!({ "theme": {}, "expanded": {
            "type": "column", "children": [{ "type": "divider" }]
        } }))
            .is_ok()
        );
    }

    #[test]
    fn spring_matches_handoff_overshoot_and_settles() {
        let (mut value, mut velocity, mut peak) = (0.0, 0.0, 0.0_f32);
        for _ in 0..480 {
            advance_spring(
                &mut value,
                &mut velocity,
                100.0,
                1.0 / 240.0,
                0.4,
                0.24,
                false,
            );
            peak = peak.max(value);
        }
        assert!((102.4..102.7).contains(&peak), "peak {peak}");
        assert_eq!(value, 100.0);
        assert_eq!(velocity, 0.0);
    }

    #[test]
    fn expanded_style_overrides_default_geometry() {
        let scene = json!({
            "theme": { "view": { "expanded": {
                "padding": 24, "radius": 30, "max_width": 432, "max_height": 344,
                "styles": { "notification-small": {
                    "padding_horizontal": 14, "padding_vertical": 10, "radius": 22,
                    "max_width": 340, "max_height": 64
                } }
            } } },
            "expanded_presentation": { "expanded_style": "notification-small" }
        });
        assert_eq!(
            expanded_geometry(Some(&scene)),
            (340.0, 64.0, 22.0, 14.0, 10.0)
        );
    }
}
