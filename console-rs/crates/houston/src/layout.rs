//! houston layouts (`Resources/layouts/<name>.json`): children in z-order plus named
//! `box_model` (fixed positions) or `flex_box` layouts. The C++ creates the components;
//! the JSON only positions them by component id (GUI_Layout::AttachLayout, Rack.dll 0x181c5ddb0).

use std::collections::HashMap;

use serde_json::Value;

use crate::geom::Rect;
use crate::json;
use crate::resources::Resources;

#[derive(Clone, Debug, PartialEq)]
pub struct LayoutChild {
    pub id: String,
    pub class: Option<String>,
    pub text: Option<String>,
    pub button_info_id: Option<String>,
    pub font_info_id: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Layout {
    pub name: String,
    /// z-order: first is drawn first.
    pub children: Vec<LayoutChild>,
    pub layouts: HashMap<String, LayoutNode>,
    pub custom_properties: HashMap<String, f64>,
}

#[derive(Clone, Debug)]
pub enum LayoutNode {
    Box(Vec<BoxItem>),
    Flex(FlexBox),
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct BoxItem {
    pub id: String,
    pub left: Option<f64>,
    pub top: Option<f64>,
    pub width: Option<f64>,
    pub height: Option<f64>,
    pub right: Option<f64>,
    pub bottom: Option<f64>,
    pub left_center: Option<f64>,
    pub right_center: Option<f64>,
    pub top_center: Option<f64>,
    pub bottom_center: Option<f64>,
}

#[derive(Clone, Debug, Default)]
pub struct FlexItem {
    pub id: Option<String>,
    pub width: Option<f64>,
    pub height: Option<f64>,
    pub grow: f64,
    pub shrink: f64,
    pub basis: Option<f64>,
    pub margin: (f64, f64, f64, f64),
    pub nested: Option<Box<FlexBox>>,
}

#[derive(Clone, Debug, Default)]
pub struct FlexBox {
    pub id: Option<String>,
    pub row: bool,
    pub justify: String,
    pub align_items: String,
    pub items: Vec<FlexItem>,
}

fn num(v: &Value, key: &str) -> Option<f64> {
    v.get(key)?.as_f64()
}

impl Layout {
    pub fn from_json(name: &str, v: &Value) -> Self {
        let children = v
            .get("children")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|c| {
                        Some(LayoutChild {
                            id: c.get("id")?.as_str()?.to_string(),
                            class: c.get("class").and_then(Value::as_str).map(String::from),
                            text: c.get("text").or_else(|| c.get("button_text")).and_then(Value::as_str).map(String::from),
                            button_info_id: c.get("button_info_id").and_then(Value::as_str).map(String::from),
                            font_info_id: c.get("font_info_id").and_then(Value::as_str).map(String::from),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        let layouts = v
            .get("layouts")
            .and_then(Value::as_object)
            .map(|m| m.iter().filter_map(|(k, n)| Some((k.clone(), LayoutNode::from_json(n)?))).collect())
            .unwrap_or_default();
        let custom_properties = v
            .get("custom_properties")
            .and_then(Value::as_object)
            .map(|m| m.iter().filter_map(|(k, n)| Some((k.clone(), n.as_f64()?))).collect())
            .unwrap_or_default();
        Self { name: name.to_string(), children, layouts, custom_properties }
    }

    pub fn load(name: &str, resources: &Resources) -> Option<Self> {
        let text = std::fs::read_to_string(resources.layouts_dir().join(format!("{name}.json"))).ok()?;
        Some(Self::from_json(name, &json::parse(&text).ok()?))
    }

    pub fn names(resources: &Resources) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(resources.layouts_dir())
            .map(|d| {
                d.filter_map(|e| {
                    let n = e.ok()?.file_name().to_string_lossy().into_owned();
                    n.strip_suffix(".json").map(String::from)
                })
                .collect()
            })
            .unwrap_or_default();
        names.sort();
        names
    }

    fn node(&self, layout: &str) -> Option<&LayoutNode> {
        self.layouts.get(layout).or_else(|| self.layouts.get("default")).or_else(|| self.layouts.values().next())
    }

    /// Frames for every positioned id inside a container of `w`×`h`.
    pub fn frames(&self, layout: &str, w: f32, h: f32) -> HashMap<String, Rect> {
        let mut out = HashMap::new();
        if let Some(node) = self.node(layout) {
            node.resolve(Rect::new(0.0, 0.0, w, h), &mut out);
        }
        out
    }

    /// Smallest container that holds every fixed item (edge anchors count their distance).
    pub fn natural_size(&self, layout: &str) -> (f32, f32) {
        match self.node(layout) {
            Some(LayoutNode::Box(items)) => {
                let mut w: f64 = 0.0;
                let mut h: f64 = 0.0;
                for it in items {
                    w = w.max(extent(it.left, it.width, it.right));
                    h = h.max(extent(it.top, it.height, it.bottom));
                }
                (w as f32, h as f32)
            }
            Some(LayoutNode::Flex(_)) => {
                let f = self.frames(layout, 0.0, 0.0);
                f.values().fold((0.0, 0.0), |(w, h), r| (w.max(r.max_x()), h.max(r.max_y())))
            }
            None => (0.0, 0.0),
        }
    }
}

fn extent(start: Option<f64>, size: Option<f64>, end: Option<f64>) -> f64 {
    match (start, size, end) {
        (Some(s), Some(z), Some(e)) => (s + z).max(z + e).max(0.0),
        (Some(s), Some(z), None) => s + z,
        (None, Some(z), Some(e)) => z + e.max(0.0),
        (Some(s), None, Some(e)) => s + e.max(0.0),
        _ => size.unwrap_or(0.0),
    }
}

impl LayoutNode {
    fn from_json(v: &Value) -> Option<Self> {
        match v.get("class")?.as_str()? {
            "box_model" => Some(Self::Box(
                v.get("children")?.as_array()?.iter().filter_map(BoxItem::from_json).collect(),
            )),
            "flex_box" => Some(Self::Flex(FlexBox::from_json(v))),
            _ => None,
        }
    }

    fn resolve(&self, container: Rect, out: &mut HashMap<String, Rect>) {
        match self {
            Self::Box(items) => {
                for it in items {
                    out.insert(it.id.clone(), it.frame(container));
                }
            }
            Self::Flex(f) => f.resolve(container, out),
        }
    }
}

impl BoxItem {
    fn from_json(v: &Value) -> Option<Self> {
        Some(Self {
            id: v.get("id")?.as_str()?.to_string(),
            left: num(v, "left"),
            top: num(v, "top"),
            width: num(v, "width"),
            height: num(v, "height"),
            right: num(v, "right"),
            bottom: num(v, "bottom"),
            left_center: num(v, "left_center"),
            right_center: num(v, "right_center"),
            top_center: num(v, "top_center"),
            bottom_center: num(v, "bottom_center"),
        })
    }

    /// GUI_Layout::PerformFixedLayout (0x181c60710), truncated to ints like JUCE setBounds.
    pub fn frame(&self, c: Rect) -> Rect {
        let (x, w) = axis(self.left, self.left_center, self.width, self.right, self.right_center, c.w as f64);
        let (y, h) = axis(self.top, self.top_center, self.height, self.bottom, self.bottom_center, c.h as f64);
        Rect::new(c.x + x.trunc() as f32, c.y + y.trunc() as f32, w.trunc().max(0.0) as f32, h.trunc().max(0.0) as f32)
    }
}

fn axis(start: Option<f64>, start_c: Option<f64>, size: Option<f64>, end: Option<f64>, end_c: Option<f64>, extent: f64) -> (f64, f64) {
    let lead = start.or(start_c.map(|v| extent / 2.0 + v));
    let trail = end.map(|v| extent - v).or(end_c.map(|v| extent / 2.0 - v));
    match (lead, size, trail) {
        (Some(l), Some(s), _) => (l, s),
        (Some(l), None, Some(t)) => (l, t - l),
        (None, Some(s), Some(t)) => (t - s, s),
        (Some(l), None, None) => (l, 0.0),
        (None, Some(s), None) => (0.0, s),
        _ => (0.0, extent),
    }
}

impl FlexBox {
    fn from_json(v: &Value) -> Self {
        Self {
            id: v.get("id").and_then(Value::as_str).map(String::from),
            row: v.get("flex_direction").and_then(Value::as_str).unwrap_or("row").starts_with("row"),
            justify: v.get("justify_content").and_then(Value::as_str).unwrap_or("flex_start").to_string(),
            align_items: v.get("align_items").and_then(Value::as_str).unwrap_or("stretch").to_string(),
            items: v.get("items").and_then(Value::as_array).map(|a| a.iter().map(FlexItem::from_json).collect()).unwrap_or_default(),
        }
    }

    fn resolve(&self, c: Rect, out: &mut HashMap<String, Rect>) {
        if let Some(id) = &self.id {
            out.insert(id.clone(), c);
        }
        let (main_extent, cross_extent) = if self.row { (c.w as f64, c.h as f64) } else { (c.h as f64, c.w as f64) };
        let mains: Vec<f64> = self.items.iter().map(|i| i.basis.or(if self.row { i.width } else { i.height }).unwrap_or(0.0)).collect();
        let margin_main: f64 = self
            .items
            .iter()
            .map(|i| if self.row { i.margin.0 + i.margin.2 } else { i.margin.1 + i.margin.3 })
            .sum();
        let free = main_extent - mains.iter().sum::<f64>() - margin_main;
        let total_grow: f64 = self.items.iter().map(|i| i.grow).sum();
        let total_shrink: f64 = self.items.iter().zip(&mains).map(|(i, m)| i.shrink * m).sum();
        let mut sizes = mains.clone();
        if free > 0.0 && total_grow > 0.0 {
            for (s, i) in sizes.iter_mut().zip(&self.items) {
                *s += free * i.grow / total_grow;
            }
        } else if free < 0.0 && total_shrink > 0.0 {
            for ((s, i), m) in sizes.iter_mut().zip(&self.items).zip(&mains) {
                *s = (*s + free * i.shrink * m / total_shrink).max(0.0);
            }
        }
        let used = sizes.iter().sum::<f64>() + margin_main;
        let mut cursor = match self.justify.as_str() {
            "center" => (main_extent - used) / 2.0,
            "flex_end" | "end" => main_extent - used,
            _ => 0.0,
        };
        for (i, item) in self.items.iter().enumerate() {
            let (ml, mt, mr, mb) = item.margin;
            cursor += if self.row { ml } else { mt };
            let cross_fixed = if self.row { item.height } else { item.width };
            let cross_margin = if self.row { mt + mb } else { ml + mr };
            let cross = cross_fixed.unwrap_or(if self.align_items == "stretch" { cross_extent - cross_margin } else { 0.0 });
            let mut cross_pos = if self.row { mt } else { ml };
            if self.align_items == "center" {
                cross_pos = (cross_extent - cross) / 2.0;
            }
            let r = if self.row {
                Rect::new(c.x + cursor as f32, c.y + cross_pos as f32, sizes[i] as f32, cross as f32)
            } else {
                Rect::new(c.x + cross_pos as f32, c.y + cursor as f32, cross as f32, sizes[i] as f32)
            };
            if let Some(n) = &item.nested {
                n.resolve(r, out);
            } else if let Some(id) = &item.id {
                out.insert(id.clone(), r);
            }
            cursor += sizes[i] + if self.row { mr } else { mb };
        }
    }
}

impl FlexItem {
    fn from_json(v: &Value) -> Self {
        let margin = match v.get("margin") {
            Some(Value::Number(n)) => {
                let m = n.as_f64().unwrap_or(0.0);
                (m, m, m, m)
            }
            Some(Value::Object(o)) => {
                let g = |k: &str| o.get(k).and_then(Value::as_f64).unwrap_or(0.0);
                (g("left"), g("top"), g("right"), g("bottom"))
            }
            _ => (0.0, 0.0, 0.0, 0.0),
        };
        let nested = (v.get("class").and_then(Value::as_str) == Some("flex_box") || v.get("items").is_some())
            .then(|| Box::new(FlexBox::from_json(v)));
        Self {
            id: v.get("id").and_then(Value::as_str).map(String::from),
            width: num(v, "width"),
            height: num(v, "height"),
            grow: num(v, "flex_grow").unwrap_or(0.0),
            shrink: num(v, "flex_shrink").unwrap_or(1.0),
            basis: num(v, "flex_basis"),
            margin,
            nested,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout(text: &str) -> Layout {
        Layout::from_json("t", &json::parse(text).unwrap())
    }

    #[test]
    fn box_model_anchors() {
        let l = layout(
            r#"{"children":[{"id":"a"},{"id":"b"},{"id":"c"},{"id":"d"}],"layouts":{"default":{"class":"box_model","children":[
            {"id":"a","position":"fixed","left":6,"top":0,"width":84,"height":129},
            {"id":"b","position":"fixed","left":0,"width":96,"bottom":77,"height":16},
            {"id":"c","position":"fixed","left":10,"right":10,"top":-5,"height":60},
            {"id":"d","position":"fixed","left_center":-20,"width":40,"top":1,"height":20},]}}}"#,
        );
        let f = l.frames("default", 96.0, 200.0);
        assert_eq!(f["a"], Rect::new(6.0, 0.0, 84.0, 129.0));
        assert_eq!(f["b"], Rect::new(0.0, 107.0, 96.0, 16.0));
        assert_eq!(f["c"], Rect::new(10.0, -5.0, 76.0, 60.0));
        assert_eq!(f["d"], Rect::new(28.0, 1.0, 40.0, 20.0));
    }

    #[test]
    fn natural_size_bottom_anchor() {
        let l = layout(
            r#"{"children":[{"id":"g"},{"id":"l"}],"layouts":{"default":{"class":"box_model","children":[
            {"id":"l","position":"fixed","left":0,"width":96,"bottom":77,"height":16},
            {"id":"g","position":"fixed","left":-10,"width":116,"bottom":-3,"height":100}]}}}"#,
        );
        assert_eq!(l.natural_size("default"), (106.0, 100.0));
    }

    #[test]
    fn flex_grow() {
        let l = layout(
            r#"{"children":[],"layouts":{"default":{"class":"flex_box","flex_direction":"row","align_items":"stretch",
            "items":[{"id":"x","width":20},{"id":"y","flex_grow":1}]}}}"#,
        );
        let f = l.frames("default", 100.0, 30.0);
        assert_eq!(f["x"], Rect::new(0.0, 0.0, 20.0, 30.0));
        assert_eq!(f["y"], Rect::new(20.0, 0.0, 80.0, 30.0));
    }
}
