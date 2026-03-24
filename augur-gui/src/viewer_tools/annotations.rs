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

#[derive(Debug, Clone)]
pub struct Annotation {
    pub id: usize,
    pub shape: AnnotationShape,
    pub label: String,
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
        self.annotations
            .iter()
            .find(|annotation| annotation.id == selected)
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
            label: format!("ROI {}", id + 1),
            color: annotation_color(id),
        });
        self.selected = Some(id);
        Some(id)
    }

    pub fn select_at(&mut self, point: (u16, u16)) -> bool {
        let selected = self
            .annotations
            .iter()
            .rev()
            .find(|annotation| annotation.shape.contains(point))
            .map(|annotation| annotation.id);
        self.selected = selected;
        selected.is_some()
    }

    pub fn delete_selected(&mut self) -> bool {
        let Some(selected) = self.selected else {
            return false;
        };
        let before = self.annotations.len();
        self.annotations
            .retain(|annotation| annotation.id != selected);
        self.selected = None;
        before != self.annotations.len()
    }

    pub fn statistics_for_selected(&self, frame: &PreviewFrame) -> Option<RoiStatistics> {
        let annotation = self.selected_annotation()?;
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
            label: annotation.label.clone(),
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
        annotation_shape_from_drawing, AnnotationManager, AnnotationShapeKind, DrawingState,
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
            on_count: 0,
            off_count: 0,
            events: None,
            window_start_us: 0,
            window_end_us: 1,
        };
        let stats = annotations
            .statistics_for_selected(&frame)
            .expect("stats should exist");
        assert_eq!(stats.pixel_count, 2);
        assert_eq!(stats.combined.max, 7);
    }
}
