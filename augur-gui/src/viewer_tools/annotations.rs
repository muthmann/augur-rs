use augur_core::pipeline::PreviewFrame;
use egui::Color32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnnotationShapeKind {
    Rectangle,
    Ellipse,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnnotationShape {
    Rectangle {
        min: (u16, u16),
        max: (u16, u16),
    },
    Ellipse {
        center: (u16, u16),
        radius_x: u16,
        radius_y: u16,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnnotationBounds {
    pub min: (u16, u16),
    pub max: (u16, u16),
}

#[derive(Debug, Clone)]
pub struct Annotation {
    pub id: usize,
    pub shape: AnnotationShape,
    pub color: Color32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DrawingState {
    pub kind: AnnotationShapeKind,
    pub anchor: (u16, u16),
    pub current: (u16, u16),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChannelStats {
    pub mean: f64,
    pub stddev: f64,
    pub min: u16,
    pub max: u16,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RoiStatistics {
    pub label: String,
    pub pixel_count: usize,
    pub on: ChannelStats,
    pub off: ChannelStats,
    pub combined: ChannelStats,
}

#[derive(Debug, Default)]
pub struct AnnotationManager {
    annotations: Vec<Annotation>,
    next_id: usize,
    selected: Option<usize>,
    drawing: Option<DrawingState>,
}

impl AnnotationManager {
    pub fn annotations(&self) -> &[Annotation] {
        &self.annotations
    }

    pub fn selected_id(&self) -> Option<usize> {
        self.selected
    }

    pub fn selected_annotation(&self) -> Option<&Annotation> {
        let selected = self.selected?;
        self.annotation(selected)
    }

    pub fn annotation(&self, id: usize) -> Option<&Annotation> {
        self.annotations
            .iter()
            .find(|annotation| annotation.id == id)
    }

    pub fn annotation_id_at(&self, point: (u16, u16)) -> Option<usize> {
        self.annotations
            .iter()
            .rev()
            .find(|annotation| annotation.shape.contains(point))
            .map(|annotation| annotation.id)
    }

    pub fn select(&mut self, id: usize) -> bool {
        if self.annotation(id).is_none() {
            return false;
        }
        self.selected = Some(id);
        true
    }

    pub fn clear_selection(&mut self) {
        self.selected = None;
    }

    #[allow(dead_code)]
    pub fn display_label(&self, id: usize) -> Option<String> {
        self.annotations
            .iter()
            .position(|annotation| annotation.id == id)
            .map(|index| format!("ROI {}", index + 1))
    }

    pub fn pending_shape(&self) -> Option<AnnotationShape> {
        self.drawing.map(annotation_shape_from_drawing)
    }

    pub fn start_drawing(&mut self, kind: AnnotationShapeKind, anchor: (u16, u16)) {
        self.drawing = Some(DrawingState {
            kind,
            anchor,
            current: anchor,
        });
    }

    pub fn update_drawing(&mut self, current: (u16, u16)) {
        if let Some(drawing) = &mut self.drawing {
            drawing.current = current;
        }
    }

    pub fn cancel_drawing(&mut self) {
        self.drawing = None;
    }

    pub fn finish_drawing(&mut self) -> Option<usize> {
        let drawing = self.drawing.take()?;
        let shape = annotation_shape_from_drawing(drawing);
        let id = self.next_id;
        self.next_id += 1;
        self.annotations.push(Annotation {
            id,
            shape,
            color: annotation_color(id),
        });
        self.selected = Some(id);
        Some(id)
    }

    pub fn select_at(&mut self, point: (u16, u16)) -> bool {
        let selected = self.annotation_id_at(point);
        self.selected = selected;
        selected.is_some()
    }

    pub fn delete_selected(&mut self) -> Option<usize> {
        let selected = self.selected?;
        self.annotations
            .retain(|annotation| annotation.id != selected);
        self.selected = None;
        Some(selected)
    }

    pub fn translate_annotation(
        &mut self,
        id: usize,
        dx: i32,
        dy: i32,
        frame_width: u16,
        frame_height: u16,
    ) -> bool {
        let Some(annotation) = self
            .annotations
            .iter_mut()
            .find(|annotation| annotation.id == id)
        else {
            return false;
        };
        annotation
            .shape
            .translate_clamped(dx, dy, frame_width, frame_height);
        true
    }

    #[allow(dead_code)]
    pub fn statistics_for_selected(&self, frame: &PreviewFrame) -> Option<RoiStatistics> {
        let annotation = self.selected_annotation()?;
        let label = self
            .display_label(annotation.id)
            .unwrap_or_else(|| format!("ROI {}", annotation.id + 1));
        let width = usize::from(frame.width.max(1));
        let mut on_values = Vec::new();
        let mut off_values = Vec::new();
        let mut combined_values = Vec::new();

        for y in 0..frame.height {
            for x in 0..frame.width {
                if !annotation.shape.contains((x, y)) {
                    continue;
                }
                let idx = usize::from(y) * width + usize::from(x);
                on_values.push(frame.pixels_on[idx]);
                off_values.push(frame.pixels_off[idx]);
                combined_values.push(frame.pixels[idx]);
            }
        }

        if combined_values.is_empty() {
            return None;
        }

        Some(RoiStatistics {
            label,
            pixel_count: combined_values.len(),
            on: channel_stats(&on_values),
            off: channel_stats(&off_values),
            combined: channel_stats(&combined_values),
        })
    }
}

impl AnnotationShape {
    pub fn contains(&self, point: (u16, u16)) -> bool {
        match self {
            Self::Rectangle { min, max } => {
                point.0 >= min.0 && point.0 <= max.0 && point.1 >= min.1 && point.1 <= max.1
            }
            Self::Ellipse {
                center,
                radius_x,
                radius_y,
            } => {
                if *radius_x == 0 || *radius_y == 0 {
                    return false;
                }
                let dx = (f64::from(point.0) - f64::from(center.0)) / f64::from(*radius_x);
                let dy = (f64::from(point.1) - f64::from(center.1)) / f64::from(*radius_y);
                dx * dx + dy * dy <= 1.0
            }
        }
    }

    pub fn bounds_rect(&self) -> AnnotationBounds {
        match self {
            Self::Rectangle { min, max } => AnnotationBounds {
                min: *min,
                max: *max,
            },
            Self::Ellipse {
                center,
                radius_x,
                radius_y,
            } => AnnotationBounds {
                min: (
                    center.0.saturating_sub(*radius_x),
                    center.1.saturating_sub(*radius_y),
                ),
                max: (
                    center.0.saturating_add(*radius_x),
                    center.1.saturating_add(*radius_y),
                ),
            },
        }
    }

    pub fn translate_clamped(&mut self, dx: i32, dy: i32, frame_width: u16, frame_height: u16) {
        if frame_width == 0 || frame_height == 0 {
            return;
        }

        let bounds = self.bounds_rect();
        let clamped_dx = dx.clamp(
            -(i32::from(bounds.min.0)),
            i32::from(frame_width.saturating_sub(1)) - i32::from(bounds.max.0),
        );
        let clamped_dy = dy.clamp(
            -(i32::from(bounds.min.1)),
            i32::from(frame_height.saturating_sub(1)) - i32::from(bounds.max.1),
        );

        match self {
            Self::Rectangle { min, max } => {
                *min = translate_point(*min, clamped_dx, clamped_dy);
                *max = translate_point(*max, clamped_dx, clamped_dy);
            }
            Self::Ellipse { center, .. } => {
                *center = translate_point(*center, clamped_dx, clamped_dy);
            }
        }
    }
}

fn annotation_shape_from_drawing(drawing: DrawingState) -> AnnotationShape {
    match drawing.kind {
        AnnotationShapeKind::Rectangle => AnnotationShape::Rectangle {
            min: (
                drawing.anchor.0.min(drawing.current.0),
                drawing.anchor.1.min(drawing.current.1),
            ),
            max: (
                drawing.anchor.0.max(drawing.current.0),
                drawing.anchor.1.max(drawing.current.1),
            ),
        },
        AnnotationShapeKind::Ellipse => {
            let min_x = drawing.anchor.0.min(drawing.current.0);
            let min_y = drawing.anchor.1.min(drawing.current.1);
            let max_x = drawing.anchor.0.max(drawing.current.0);
            let max_y = drawing.anchor.1.max(drawing.current.1);
            AnnotationShape::Ellipse {
                center: ((min_x + max_x) / 2, (min_y + max_y) / 2),
                radius_x: (max_x - min_x).max(1) / 2 + 1,
                radius_y: (max_y - min_y).max(1) / 2 + 1,
            }
        }
    }
}

fn annotation_color(index: usize) -> Color32 {
    const PALETTE: [Color32; 6] = [
        Color32::from_rgb(255, 196, 64),
        Color32::from_rgb(64, 220, 255),
        Color32::from_rgb(140, 255, 140),
        Color32::from_rgb(255, 128, 128),
        Color32::from_rgb(220, 160, 255),
        Color32::from_rgb(255, 255, 140),
    ];
    PALETTE[index % PALETTE.len()]
}

fn translate_point(point: (u16, u16), dx: i32, dy: i32) -> (u16, u16) {
    (
        (i32::from(point.0) + dx) as u16,
        (i32::from(point.1) + dy) as u16,
    )
}

#[allow(dead_code)]
fn channel_stats(values: &[u16]) -> ChannelStats {
    let mut min = u16::MAX;
    let mut max = u16::MIN;
    let mut sum = 0.0;
    let mut sum_squares = 0.0;
    for &value in values {
        min = min.min(value);
        max = max.max(value);
        let value_f64 = f64::from(value);
        sum += value_f64;
        sum_squares += value_f64 * value_f64;
    }
    let count = values.len().max(1) as f64;
    let mean = sum / count;
    let variance = (sum_squares / count - mean * mean).max(0.0);
    ChannelStats {
        mean,
        stddev: variance.sqrt(),
        min,
        max,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        annotation_shape_from_drawing, AnnotationManager, AnnotationShape, AnnotationShapeKind,
        DrawingState,
    };
    use augur_core::pipeline::PreviewFrame;

    #[test]
    fn rectangle_shape_is_built_from_drag_bounds() {
        let shape = annotation_shape_from_drawing(DrawingState {
            kind: AnnotationShapeKind::Rectangle,
            anchor: (9, 4),
            current: (3, 7),
        });
        assert!(shape.contains((4, 5)));
        assert!(!shape.contains((10, 10)));
    }

    #[test]
    fn selected_roi_statistics_cover_combined_pixels() {
        let mut annotations = AnnotationManager::default();
        annotations.start_drawing(AnnotationShapeKind::Rectangle, (0, 0));
        annotations.update_drawing((1, 0));
        annotations.finish_drawing();
        let frame = PreviewFrame {
            width: 2,
            height: 1,
            pixels: vec![3, 7],
            pixels_on: vec![1, 3],
            pixels_off: vec![2, 4],
            cached_total_histogram: Vec::new(),
            cached_signed_histogram: Vec::new(),
            on_count: 0,
            off_count: 0,
            events: None,
            event_range: None,
            event_source: None,
            external_triggers: Vec::new(),
            window_start_us: 0,
            window_end_us: 1,
        };
        let stats = annotations
            .statistics_for_selected(&frame)
            .expect("stats should exist");
        assert_eq!(stats.pixel_count, 2);
        assert_eq!(stats.combined.max, 7);
    }

    #[test]
    fn list_selection_uses_stable_annotation_ids() {
        let mut annotations = AnnotationManager::default();
        annotations.start_drawing(AnnotationShapeKind::Rectangle, (0, 0));
        annotations.update_drawing((2, 2));
        let first_id = annotations.finish_drawing().expect("first annotation");
        annotations.start_drawing(AnnotationShapeKind::Rectangle, (3, 3));
        annotations.update_drawing((5, 5));
        let second_id = annotations.finish_drawing().expect("second annotation");

        assert!(annotations.select(first_id));
        assert_eq!(annotations.selected_id(), Some(first_id));
        assert!(annotations.select(second_id));
        assert_eq!(annotations.selected_id(), Some(second_id));
    }

    #[test]
    fn display_labels_stay_contiguous_after_delete() {
        let mut annotations = AnnotationManager::default();
        annotations.start_drawing(AnnotationShapeKind::Rectangle, (0, 0));
        annotations.update_drawing((2, 2));
        let first_id = annotations.finish_drawing().expect("first annotation");
        annotations.start_drawing(AnnotationShapeKind::Rectangle, (3, 3));
        annotations.update_drawing((5, 5));
        let second_id = annotations.finish_drawing().expect("second annotation");
        annotations.start_drawing(AnnotationShapeKind::Rectangle, (6, 6));
        annotations.update_drawing((8, 8));
        let third_id = annotations.finish_drawing().expect("third annotation");

        annotations.select(second_id);
        assert_eq!(annotations.delete_selected(), Some(second_id));

        assert_eq!(
            annotations.display_label(first_id).as_deref(),
            Some("ROI 1")
        );
        assert_eq!(
            annotations.display_label(third_id).as_deref(),
            Some("ROI 2")
        );
    }

    #[test]
    fn rectangle_translation_clamps_to_frame_bounds() {
        let mut annotations = AnnotationManager::default();
        annotations.start_drawing(AnnotationShapeKind::Rectangle, (2, 2));
        annotations.update_drawing((4, 4));
        let id = annotations.finish_drawing().expect("rectangle");

        assert!(annotations.translate_annotation(id, -10, 10, 8, 8));

        let shape = &annotations.annotation(id).expect("annotation").shape;
        assert_eq!(
            shape,
            &AnnotationShape::Rectangle {
                min: (0, 5),
                max: (2, 7),
            }
        );
    }

    #[test]
    fn ellipse_translation_clamps_to_frame_bounds() {
        let mut annotations = AnnotationManager::default();
        annotations.start_drawing(AnnotationShapeKind::Ellipse, (2, 2));
        annotations.update_drawing((6, 6));
        let id = annotations.finish_drawing().expect("ellipse");

        assert!(annotations.translate_annotation(id, 10, -10, 10, 10));

        let shape = &annotations.annotation(id).expect("annotation").shape;
        assert_eq!(
            shape,
            &AnnotationShape::Ellipse {
                center: (6, 3),
                radius_x: 3,
                radius_y: 3,
            }
        );
    }

    #[test]
    fn deleting_selected_annotation_clears_selection() {
        let mut annotations = AnnotationManager::default();
        annotations.start_drawing(AnnotationShapeKind::Rectangle, (0, 0));
        annotations.update_drawing((1, 1));
        let id = annotations.finish_drawing().expect("annotation");

        annotations.select(id);
        assert_eq!(annotations.delete_selected(), Some(id));
        assert_eq!(annotations.selected_id(), None);
        assert!(annotations.annotations().is_empty());
    }
}
