//! Image extraction — navigate DrawingML hierarchy to extract image RelIds.

use std::rc::Rc;

use crate::model::{GraphicContent, Image, ImageFormat, RelId};

/// Resolved image entry — shared bytes with detected format.
///
/// All references to the same image share one `Rc<[u8]>` allocation,
/// enabling pointer-based deduplication in the painter cache.
#[derive(Clone, Debug)]
pub struct MediaEntry {
    pub data: Rc<[u8]>,
    pub format: ImageFormat,
}

/// Extract the embedded image relationship ID from a DrawingML Image.
/// Navigates: Image → graphic → Picture → blip_fill → blip → embed.
pub fn extract_image_rel_id(image: &Image) -> Option<&RelId> {
    match image.graphic.as_ref()? {
        GraphicContent::Picture(pic) => pic.blip_fill.blip.as_ref()?.embed.as_ref(),
        GraphicContent::WordProcessingShape(_) => None,
    }
}

/// §20.1.10.48 `a:srcRect` — the picture's source crop, as a rectangle in
/// `[0, 1]` relative to the image's natural extent (origin = top-left crop
/// offset, size = visible fraction). Returns `None` when there is no crop,
/// so the whole image is drawn. Word crops the source *before* stretching it
/// into the display frame; ignoring this squashes cropped logos (the visible
/// aspect ratio no longer matches the frame). See `ResolvedBlip::src_rect`
/// for the same value on the shape-fill path.
pub fn extract_src_rect(image: &Image) -> Option<crate::render::geometry::PtRect> {
    use crate::model::dimension::{Dimension, ThousandthPercent};
    use crate::render::dimension::Pt;
    use crate::render::geometry::PtRect;

    let GraphicContent::Picture(pic) = image.graphic.as_ref()? else {
        return None;
    };
    let rel = pic.blip_fill.src_rect.as_ref()?;
    // CT_RelativeRect edges are in thousandths of a percent (100% = 100000).
    let frac =
        |d: Option<Dimension<ThousandthPercent>>| d.map_or(0.0, |v| v.raw() as f32 / 100_000.0);
    let (left, top, right, bottom) = (
        frac(rel.left),
        frac(rel.top),
        frac(rel.right),
        frac(rel.bottom),
    );
    if left == 0.0 && top == 0.0 && right == 0.0 && bottom == 0.0 {
        return None;
    }
    Some(PtRect::from_xywh(
        Pt::new(left),
        Pt::new(top),
        Pt::new((1.0 - left - right).max(0.0)),
        Pt::new((1.0 - top - bottom).max(0.0)),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::dimension::Dimension;
    use crate::model::geometry::{EdgeInsets, Size};
    use crate::model::*;

    fn make_image_with_blip(rel_id: &str) -> Image {
        Image {
            extent: Size::new(Dimension::new(0), Dimension::new(0)),
            effect_extent: None,
            doc_properties: DocProperties {
                id: 1,
                name: "img".into(),
                description: None,
                hidden: None,
                title: None,
            },
            graphic_frame_locks: None,
            graphic: Some(GraphicContent::Picture(Picture {
                nv_pic_pr: NvPicProperties {
                    cnv_pr: DocProperties {
                        id: 1,
                        name: "pic".into(),
                        description: None,
                        hidden: None,
                        title: None,
                    },
                    cnv_pic_pr: None,
                },
                blip_fill: BlipFill {
                    rotate_with_shape: None,
                    dpi: None,
                    blip: Some(Blip {
                        embed: Some(RelId::new(rel_id)),
                        link: None,
                        compression: None,
                    }),
                    src_rect: None,
                    fill_kind: BlipFillKind::Unspecified,
                },
                shape_properties: None,
            })),
            placement: ImagePlacement::Inline {
                distance: EdgeInsets::new(
                    Dimension::new(0),
                    Dimension::new(0),
                    Dimension::new(0),
                    Dimension::new(0),
                ),
            },
        }
    }

    #[test]
    fn extracts_rel_id_from_picture_blip() {
        let img = make_image_with_blip("rId5");
        let rel_id = extract_image_rel_id(&img);
        assert_eq!(rel_id.map(|r| r.as_str()), Some("rId5"));
    }

    #[test]
    fn no_graphic_returns_none() {
        let img = Image {
            extent: Size::new(Dimension::new(0), Dimension::new(0)),
            effect_extent: None,
            doc_properties: DocProperties {
                id: 1,
                name: "img".into(),
                description: None,
                hidden: None,
                title: None,
            },
            graphic_frame_locks: None,
            graphic: None,
            placement: ImagePlacement::Inline {
                distance: EdgeInsets::new(
                    Dimension::new(0),
                    Dimension::new(0),
                    Dimension::new(0),
                    Dimension::new(0),
                ),
            },
        };
        assert!(extract_image_rel_id(&img).is_none());
    }

    #[test]
    fn no_blip_returns_none() {
        let img = Image {
            extent: Size::new(Dimension::new(0), Dimension::new(0)),
            effect_extent: None,
            doc_properties: DocProperties {
                id: 1,
                name: "img".into(),
                description: None,
                hidden: None,
                title: None,
            },
            graphic_frame_locks: None,
            graphic: Some(GraphicContent::Picture(Picture {
                nv_pic_pr: NvPicProperties {
                    cnv_pr: DocProperties {
                        id: 1,
                        name: "pic".into(),
                        description: None,
                        hidden: None,
                        title: None,
                    },
                    cnv_pic_pr: None,
                },
                blip_fill: BlipFill {
                    rotate_with_shape: None,
                    dpi: None,
                    blip: None,
                    src_rect: None,
                    fill_kind: BlipFillKind::Unspecified,
                },
                shape_properties: None,
            })),
            placement: ImagePlacement::Inline {
                distance: EdgeInsets::new(
                    Dimension::new(0),
                    Dimension::new(0),
                    Dimension::new(0),
                    Dimension::new(0),
                ),
            },
        };
        assert!(extract_image_rel_id(&img).is_none());
    }

    #[test]
    fn word_processing_shape_returns_none() {
        let img = Image {
            extent: Size::new(Dimension::new(0), Dimension::new(0)),
            effect_extent: None,
            doc_properties: DocProperties {
                id: 1,
                name: "img".into(),
                description: None,
                hidden: None,
                title: None,
            },
            graphic_frame_locks: None,
            graphic: Some(GraphicContent::WordProcessingShape(WordProcessingShape {
                cnv_pr: None,
                shape_properties: None,
                style_line_ref: None,
                style_effect_ref: None,
                body_pr: None,
                txbx_content: vec![],
            })),
            placement: ImagePlacement::Inline {
                distance: EdgeInsets::new(
                    Dimension::new(0),
                    Dimension::new(0),
                    Dimension::new(0),
                    Dimension::new(0),
                ),
            },
        };
        assert!(extract_image_rel_id(&img).is_none());
    }

    #[test]
    fn src_rect_none_when_no_crop() {
        // `make_image_with_blip` leaves `blip_fill.src_rect = None`.
        assert!(extract_src_rect(&make_image_with_blip("rId1")).is_none());
    }

    #[test]
    fn src_rect_converts_thousandth_percent_crop() {
        // Real values from the IP-05 header logo: top/right/bottom crop, no
        // left. Thousandths of a percent → fractions of the natural extent.
        let mut img = make_image_with_blip("rId1");
        if let Some(GraphicContent::Picture(pic)) = img.graphic.as_mut() {
            pic.blip_fill.src_rect = Some(RelativeRect {
                left: None,
                top: Some(Dimension::new(15883)),
                right: Some(Dimension::new(14520)),
                bottom: Some(Dimension::new(17647)),
            });
        }
        let r = extract_src_rect(&img).expect("crop present");
        assert!((r.origin.x.raw() - 0.0).abs() < 1e-4, "left crop = 0");
        assert!((r.origin.y.raw() - 0.15883).abs() < 1e-4, "top crop");
        assert!((r.size.width.raw() - 0.85480).abs() < 1e-4, "visible width");
        assert!(
            (r.size.height.raw() - 0.66470).abs() < 1e-4,
            "visible height"
        );
    }
}
