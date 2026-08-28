use anyhow::{Result, bail};
use pango::prelude::*;
use raylib::prelude::*;
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
    meter_changed: Instant,
    meter_from: f32,
    meter_target: Option<f32>,
    geometry_changed: Instant,
    compact_from: (f32, f32, f32, f32, f32),
    expanded_from: (f32, f32, f32, f32, f32),
    visibility: Visibility,
    visibility_target: f32,
    visibility_velocity: f32,
    last_frame: Instant,
    progress: f32,
    pango: pango::Context,
    pub hits: Vec<HitRegion>,
    textures: HashMap<String, Texture2D>,
    fonts: HashMap<String, Font>,
    font_characters: String,
    asset_root: PathBuf,
    measurements: HashMap<String, (f32, f32)>,
}
impl Renderer {
    pub fn new(asset_root: PathBuf) -> Self {
        Self {
            scene: None,
            changed: Instant::now(),
            meter_changed: Instant::now(),
            meter_from: 0.0,
            meter_target: None,
            geometry_changed: Instant::now(),
            compact_from: (230.0, 32.0, 16.0, 14.0, 4.0),
            expanded_from: (432.0, 344.0, 30.0, 24.0, 24.0),
            visibility: Visibility::Compact,
            visibility_target: 0.0,
            visibility_velocity: 0.0,
            last_frame: Instant::now(),
            progress: 0.0,
            pango: pangocairo::FontMap::default().create_context(),
            hits: Vec::new(),
            textures: HashMap::new(),
            fonts: HashMap::new(),
            font_characters: String::new(),
            asset_root,
            measurements: HashMap::new(),
        }
    }
    pub fn set_scene(&mut self, scene: Value) -> Result<()> {
        validate_scene(&scene)?;
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
                        (self.meter_changed.elapsed().as_secs_f32() * speed / 0.27).clamp(0.0, 1.0),
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
        if context_changed {
            let speed = self
                .scene
                .as_ref()
                .and_then(|value| value.pointer("/interaction/animation_speed"))
                .and_then(Value::as_f64)
                .unwrap_or(1.0) as f32;
            let progress = ease_in_out(
                (self.geometry_changed.elapsed().as_secs_f32() * speed / 0.325).clamp(0.0, 1.0),
            );
            self.compact_from = if self.progress >= 0.999 {
                compact_geometry(Some(&scene))
            } else {
                interpolate_compact(
                    self.compact_from,
                    compact_geometry(self.scene.as_ref()),
                    progress,
                )
            };
            self.expanded_from = if self.progress <= 0.001 {
                expanded_geometry(Some(&scene))
            } else {
                interpolate_compact(
                    self.expanded_from,
                    expanded_geometry(self.scene.as_ref()),
                    progress,
                )
            };
            self.geometry_changed = Instant::now();
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
        if context_changed || declared_transition {
            self.changed = Instant::now();
        }
        self.scene = Some(scene);
        self.measurements.clear();
        Ok(())
    }
    pub fn set_visibility(&mut self, value: Visibility) {
        self.visibility = value;
        self.visibility_target = if value == Visibility::Expanded {
            1.0
        } else {
            0.0
        };
        if value == Visibility::Hidden {
            self.progress = 0.0;
            self.visibility_velocity = 0.0;
        }
    }
    pub fn extent(&self) -> (i32, i32) {
        let reduced = self
            .scene
            .as_ref()
            .and_then(|scene| scene.pointer("/interaction/reduced_motion"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let speed = self
            .scene
            .as_ref()
            .and_then(|scene| scene.pointer("/interaction/animation_speed"))
            .and_then(Value::as_f64)
            .unwrap_or(1.0) as f32;
        let geometry_progress = if reduced {
            1.0
        } else {
            ease_in_out(
                (self.geometry_changed.elapsed().as_secs_f32() * speed / 0.325).clamp(0.0, 1.0),
            )
        };
        let compact = interpolate_compact(
            self.compact_from,
            compact_geometry(self.scene.as_ref()),
            geometry_progress,
        );
        let expanded = interpolate_compact(
            self.expanded_from,
            expanded_geometry(self.scene.as_ref()),
            geometry_progress,
        );
        (
            (lerp(compact.0, expanded.0, self.progress) + 40.0) as i32,
            (lerp(compact.1, expanded.1, self.progress) + 40.0) as i32,
        )
    }

    pub fn prepare(&mut self, window: &mut RaylibHandle, thread: &RaylibThread) {
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
                        && let Ok(font) = window.load_font_ex(thread, path, 64, Some(&characters))
                    {
                        font.texture()
                            .set_texture_filter(thread, TextureFilter::TEXTURE_FILTER_BILINEAR);
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
            let path = source.strip_prefix("file://").unwrap_or(&source);
            if Path::new(path).is_file()
                && let Some(texture) = texture_from_path(window, thread, path)
            {
                texture.set_texture_filter(thread, TextureFilter::TEXTURE_FILTER_BILINEAR);
                self.textures.insert(source, texture);
            }
        }
    }
    pub fn draw(&mut self, drawing: &mut RaylibDrawHandle<'_>, mouse: Vector2) {
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
        if self.visibility == Visibility::Hidden {
            return;
        }
        if reduced {
            self.progress = self.visibility_target;
            self.visibility_velocity = 0.0;
        } else {
            update_spring(
                &mut self.progress,
                &mut self.visibility_velocity,
                self.visibility_target,
                delta * speed,
            );
        }
        let expansion = self.progress;
        let geometry_progress = if reduced {
            1.0
        } else {
            ease_in_out(
                (self.geometry_changed.elapsed().as_secs_f32() * speed / 0.325).clamp(0.0, 1.0),
            )
        };
        let compact = interpolate_compact(
            self.compact_from,
            compact_geometry(self.scene.as_ref()),
            geometry_progress,
        );
        let expanded = interpolate_compact(
            self.expanded_from,
            expanded_geometry(self.scene.as_ref()),
            geometry_progress,
        );
        let (compact_width, compact_height, compact_radius, compact_padding_x, compact_padding_y) =
            compact;
        let width = lerp(compact_width, expanded.0, expansion);
        let height = lerp(compact_height, expanded.1, expansion);
        let radius = lerp(compact_radius, expanded.2, expansion);
        let x = (drawing.get_screen_width() as f32 - width) * 0.5;
        let y = 20.0;
        draw_shadow(drawing, Rectangle::new(x, y, width, height), radius);
        drawing.draw_rectangle_rounded(
            Rectangle::new(x, y, width, height),
            (radius * 2.0 / height.max(1.0)).clamp(0.0, 1.0),
            20,
            Color::new(0, 0, 0, 246),
        );
        let surface = if self.visibility == Visibility::Expanded {
            "expanded"
        } else {
            "compact"
        };
        let padding_x = lerp(compact_padding_x, expanded.3, expansion);
        let padding_y = lerp(compact_padding_y, expanded.4, expansion);
        let bounds = Rectangle::new(
            x + padding_x,
            y + padding_y,
            width - padding_x * 2.0,
            height - padding_y * 2.0,
        );
        if let Some(scene) = self.scene.clone() {
            self.draw_scene(drawing, &scene, surface, bounds, mouse);
        }
    }
    fn draw_scene(
        &mut self,
        drawing: &mut RaylibDrawHandle<'_>,
        scene: &Value,
        surface: &str,
        bounds: Rectangle,
        mouse: Vector2,
    ) {
        let Some(root) = scene.get(surface).filter(|value| !value.is_null()) else {
            return;
        };
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
        let Ok(root_id) = build_node(&mut tree, &mut nodes, root) else {
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
        let measurements = &mut self.measurements;
        if tree
            .compute_layout_with_measure(
                root_id,
                Size {
                    width: AvailableSpace::Definite(bounds.width),
                    height: AvailableSpace::Definite(bounds.height),
                },
                |known, available, _, context, _| {
                    measure_leaf(known, available, context, pango, measurements)
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
            1.0 - ease_in_out((self.changed.elapsed().as_secs_f32() * speed / 0.25).clamp(0.0, 1.0))
        };
        let offset = match transition {
            Some("slide-left") => 48.0 * remaining,
            Some("slide-right") => -48.0 * remaining,
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
                ease_out((elapsed / 0.27).clamp(0.0, 1.0)),
            )
        });
        draw_node(
            drawing,
            &tree,
            &nodes,
            root_id,
            Vector2::new(bounds.x + offset, bounds.y),
            mouse,
            1.0,
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
        number(height_key, defaults.1),
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

fn interpolate_compact(
    from: (f32, f32, f32, f32, f32),
    to: (f32, f32, f32, f32, f32),
    progress: f32,
) -> (f32, f32, f32, f32, f32) {
    (
        lerp(from.0, to.0, progress),
        lerp(from.1, to.1, progress),
        lerp(from.2, to.2, progress),
        lerp(from.3, to.3, progress),
        lerp(from.4, to.4, progress),
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
    let gap = gap_value(node.get("gap"));
    let mut style = Style {
        display: Display::Flex,
        gap: Size {
            width: length(gap),
            height: length(gap),
        },
        ..Default::default()
    };
    let id = match kind {
        "row" | "column" => {
            style.flex_direction = if kind == "row" {
                FlexDirection::Row
            } else {
                FlexDirection::Column
            };
            style.align_items = Some(match node.get("alignment").and_then(Value::as_str) {
                Some("start") => AlignItems::FLEX_START,
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
            if input {
                style.size.width = percent(1.0);
            }
            let (horizontal, vertical) = if input { (10.0, 8.0) } else { (6.0, 4.0) };
            style.padding = Rect {
                left: length(horizontal),
                right: length(horizontal),
                top: length(vertical),
                bottom: length(vertical),
            };
            let child = build_node(tree, nodes, node.get("content").unwrap())?;
            tree.new_with_children(style, &[child])?
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
            let size = role_size(node.get("role").and_then(Value::as_str));
            let available_width = match available.width {
                AvailableSpace::Definite(width) => width,
                _ => -1.0,
            };
            let key = format!("{size}:{available_width}:{text}");
            if let Some(&(width, height)) = measurements.get(&key) {
                return Size { width, height };
            }
            let layout = pango::Layout::new(pango);
            layout.set_text(&text);
            let mut font = pango::FontDescription::new();
            font.set_family("Inter");
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
                height: height as f32,
            }
        }
        "icon" => {
            let size = if node.get("name").and_then(Value::as_str) == Some("application") {
                32.0
            } else {
                20.0
            };
            Size {
                width: size,
                height: size,
            }
        }
        "image" => Size {
            width: 32.0,
            height: 32.0,
        },
        "progress" if node.get("shape").and_then(Value::as_str) == Some("ring") => Size {
            width: 32.0,
            height: 32.0,
        },
        "progress" => Size {
            width: 120.0,
            height: 8.0,
        },
        "indicator" => Size {
            width: 20.0,
            height: 20.0,
        },
        "spacer" => Size {
            width: 6.0,
            height: 6.0,
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
    d: &mut RaylibDrawHandle<'_>,
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
            let size = role_size(role);
            let font_name = if matches!(
                role,
                Some("title" | "caption" | "button" | "compact-primary" | "current" | "accent")
            ) {
                "semibold"
            } else {
                "regular"
            };
            if let Some(font) = fonts.get(font_name) {
                d.draw_text_ex(font, &text, origin, size, 0.0, color);
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
            if let Some(font) = fonts.get(if application { "regular" } else { "icons" }) {
                d.draw_text_ex(font, &symbol, icon_origin, 20.0, 0.0, color);
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
                .unwrap_or_else(|| lerp(from, target, ease_out((elapsed / 0.27).clamp(0.0, 1.0))))
                .clamp(0.0, 1.0);
            if node.and_then(|v| v.get("shape")).and_then(Value::as_str) == Some("ring") {
                let center = Vector2::new(origin.x + 16.0, origin.y + 16.0);
                d.draw_ring(
                    center,
                    11.0,
                    15.0,
                    -90.0,
                    270.0,
                    48,
                    Color::new(90, 90, 96, (80.0 * alpha) as u8),
                );
                d.draw_ring(
                    center,
                    11.0,
                    15.0,
                    -90.0,
                    -90.0 + progress * 360.0,
                    48,
                    color,
                );
            } else {
                let thickness = 6.0;
                let track = Rectangle::new(
                    origin.x,
                    origin.y + (rect.height - thickness) * 0.5,
                    rect.width,
                    thickness,
                );
                d.draw_rectangle_rounded(track, 1.0, 10, Color::new(90, 90, 96, 80));
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
                Color::new(124, 92, 252, (190.0 * alpha) as u8),
            );
        }
        "button" | "action_region" if hovered => d.draw_rectangle_rounded(
            rect,
            0.35,
            10,
            Color::new(255, 255, 255, (20.0 * alpha) as u8),
        ),
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
fn role_size(role: Option<&str>) -> f32 {
    match role {
        Some("title") => 22.0,
        Some("caption" | "button" | "compact-primary" | "compact-secondary") => 12.0,
        Some("muted") => 14.0,
        _ => 16.0,
    }
}
fn node_color(node: Option<&Value>, alpha: f32) -> Color {
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
fn draw_shadow(d: &mut RaylibDrawHandle<'_>, rect: Rectangle, radius: f32) {
    for step in (1..=6).rev() {
        let grow = step as f32 * 3.0;
        d.draw_rectangle_rounded(
            Rectangle::new(
                rect.x - grow,
                rect.y + 4.0 - grow,
                rect.width + grow * 2.0,
                rect.height + grow * 2.0,
            ),
            radius / rect.height.max(1.0),
            20,
            Color::new(0, 0, 0, (5 * (7 - step)) as u8),
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

fn update_spring(value: &mut f32, velocity: &mut f32, target: f32, delta: f32) {
    const STIFFNESS: f32 = 220.0;
    const DAMPING: f32 = 21.36;
    const MAX_STEP: f32 = 1.0 / 240.0;
    let mut remaining = delta.clamp(0.0, 0.05);
    while remaining > 0.0 {
        let step = remaining.min(MAX_STEP);
        let acceleration = STIFFNESS * (target - *value) - DAMPING * *velocity;
        *velocity += acceleration * step;
        *value += *velocity * step;
        remaining -= step;
    }
    if (target - *value).abs() < 0.0005 && velocity.abs() < 0.005 {
        *value = target;
        *velocity = 0.0;
    }
}
fn lerp(from: f32, to: f32, amount: f32) -> f32 {
    from + (to - from) * amount
}

#[cfg(test)]
mod tests {
    use super::{expanded_geometry, update_spring};
    use serde_json::json;

    #[test]
    fn spring_overshoots_then_settles_like_main() {
        let (mut value, mut velocity, mut peak) = (0.0, 0.0, 0.0_f32);
        for _ in 0..120 {
            update_spring(&mut value, &mut velocity, 1.0, 1.0 / 60.0);
            peak = peak.max(value);
        }
        assert!(peak > 1.0);
        assert_eq!(value, 1.0);
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
