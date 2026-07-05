//! Paint phase — iterate DrawCommands and emit Skia PDF canvas operations.

use std::collections::HashMap;
use std::rc::Rc;

use rustc_hash::FxHashMap;
use skia_safe::{
    canvas::SrcRectConstraint, path_effect::PathEffect, pdf, BlurStyle, Color4f, Data, MaskFilter,
    Paint, Path, PathBuilder, PathFillType, TextBlob,
};

use crate::render::dimension::Pt;
use crate::render::emoji::raster::EmojiRasterizer;
use crate::render::error::RenderError;
use crate::render::fonts::{self, FontRegistry};
use crate::render::layout::draw_command::{
    DrawCommand, LayoutedPage, ResolvedDashPattern, ResolvedEffect, ResolvedFill, ResolvedLineCap,
    ResolvedLineJoin, ResolvedStroke,
};
use crate::render::resolve::drawing_color::Rgba;
use crate::render::resolve::images::MediaEntry;
use crate::render::resolve::shape_geometry::{PathVerb, SubPath};
use crate::render::skia_conv::{to_color4f, to_line, to_point, to_rect, to_size};

/// Target resolution for embedded images (pixels per inch).
/// 300 DPI gives crisp on-screen zoom and print output. Skia's PDF backend
/// does not emit `/Interpolate true` on image dicts, so viewers smooth-scale
/// with nearest-neighbor — we need enough source pixels to absorb that.
const IMAGE_TARGET_DPI: f32 = 300.0;
/// Conversion factor from PDF points to target pixels.
const IMAGE_DPI_SCALE: f32 = IMAGE_TARGET_DPI / 72.0;

/// Decoded-image cache, keyed by media data-pointer identity **and** the
/// downsample target size. Keying on the target (not the pointer alone) means
/// the same media placed at two different sizes — or the same size with
/// different `srcRect` crops, since a crop inflates the target — each caches
/// its own appropriately-scaled bitmap. Keying by pointer alone would let a
/// second placement reuse the first's resolution and render blurry.
type ImageCache = HashMap<(*const [u8], i32, i32), skia_safe::Image>;

/// Downsample target dimensions (in pixels) for a display rect at
/// `IMAGE_TARGET_DPI`. Shared by the image cache key and
/// `downsample_if_oversize` so both agree on the resolution a placement needs.
fn downsample_target(rect: crate::render::geometry::PtRect) -> (i32, i32) {
    (
        (rect.size.width.raw() * IMAGE_DPI_SCALE).ceil() as i32,
        (rect.size.height.raw() * IMAGE_DPI_SCALE).ceil() as i32,
    )
}

/// The display rect scaled up by the inverse of the visible `srcRect`
/// fraction, so the downsample keeps the *cropped* region at target DPI (a
/// heavy crop magnifies few source pixels into the frame, so it needs more of
/// them). No crop, or a degenerate zero-size crop, leaves the display rect
/// unchanged.
fn crop_adjusted_downsample_rect(
    rect: &crate::render::geometry::PtRect,
    src_rect: &Option<crate::render::geometry::PtRect>,
) -> crate::render::geometry::PtRect {
    match src_rect {
        Some(c) if c.size.width.raw() > 0.0 && c.size.height.raw() > 0.0 => {
            crate::render::geometry::PtRect::from_xywh(
                rect.origin.x,
                rect.origin.y,
                Pt::new(rect.size.width.raw() / c.size.width.raw()),
                Pt::new(rect.size.height.raw() / c.size.height.raw()),
            )
        }
        _ => *rect,
    }
}

/// Render laid-out pages to PDF bytes via Skia.
///
/// `registry` owns the typeface universe for this render — paint resolves
/// every text run through it so any subsetted typefaces (swapped in by
/// `subset::apply` between layout and paint) are picked up correctly.
pub fn render_to_pdf(
    pages: &[LayoutedPage],
    registry: &FontRegistry,
) -> Result<Vec<u8>, RenderError> {
    let mut pdf_bytes: Vec<u8> = Vec::new();
    let pdf_metadata = pdf::Metadata {
        encoding_quality: Some(85),
        ..Default::default()
    };
    let mut doc = pdf::new_document(&mut pdf_bytes, Some(&pdf_metadata));
    let mut font_cache = fonts::FontCache::new();
    // Cache decoded Skia images across pages. Avoids re-copying and
    // re-decoding the same image bytes on every page (e.g. a logo repeated in
    // headers/footers). See `ImageCache` for the keying.
    let mut image_cache: ImageCache = HashMap::new();
    // Per-render emoji rasterizer — clusters that recur across pages
    // (footer 📞 etc.) are rasterized once and shared.
    let mut emoji_rasterizer = EmojiRasterizer::default();

    // Position-independent glyph runs, cached per (font slot, text). draw_str
    // rebuilds this run (text → glyph ids → advances) on every call; reusing a
    // TextBlob skips that remap for words repeated across the document.
    let mut blob_cache: FxHashMap<usize, FxHashMap<Box<str>, TextBlob>> = FxHashMap::default();
    for page in pages {
        let mut on_page = doc.begin_page(to_size(page.page_size), None);
        {
            let canvas = on_page.canvas();
            render_page(
                canvas,
                page,
                registry,
                &mut font_cache,
                &mut image_cache,
                &mut emoji_rasterizer,
                &mut blob_cache,
            );
        }
        doc = on_page.end_page();
    }

    doc.close();
    Ok(pdf_bytes)
}

fn render_page(
    canvas: &skia_safe::Canvas,
    page: &LayoutedPage,
    registry: &FontRegistry,
    font_cache: &mut fonts::FontCache,
    image_cache: &mut ImageCache,
    emoji_rasterizer: &mut EmojiRasterizer,
    blob_cache: &mut FxHashMap<usize, FxHashMap<Box<str>, TextBlob>>,
) {
    // Reusable paints — built once per page instead of per draw command (a
    // large doc emits 100k+). Each carries the fixed config for its purpose;
    // only the fields that vary (color, stroke width) are set per command, so
    // no state leaks between commands. `default_paint` is never mutated — it
    // backs the image/emoji `draw_image_rect` calls.
    let mut text_paint = Paint::default();
    text_paint.set_anti_alias(true);
    let mut stroke_paint = Paint::default();
    stroke_paint.set_anti_alias(true);
    stroke_paint.set_stroke(true);
    let mut rect_paint = Paint::default();
    rect_paint.set_anti_alias(false);
    let default_paint = Paint::default();

    for cmd in &page.commands {
        match cmd {
            DrawCommand::Text {
                position,
                text,
                font_family,
                char_spacing,
                font_size,
                bold,
                italic,
                color,
                text_scale,
            } => {
                let (slot, base_font) =
                    font_cache.get_indexed(registry, font_family, *font_size, *bold, *italic);
                // §17.3.2.45: a non-1.0 scale is applied via Skia's scale_x —
                // that scales glyph advances and horizontal glyph extent
                // without touching the cached, shared Font. Cloning and
                // mutating a fresh Font keeps the cache invariant intact. A
                // scaled font is a one-off clone whose glyphs differ, so it
                // bypasses the (slot-keyed) blob cache.
                let scaled_font;
                let (font, blob_slot): (&skia_safe::Font, Option<usize>) =
                    if (*text_scale - 1.0).abs() > f32::EPSILON {
                        let mut f = base_font.clone();
                        f.set_scale_x(*text_scale);
                        scaled_font = f;
                        (&scaled_font, None)
                    } else {
                        (base_font, Some(slot))
                    };
                log::trace!(
                    "[paint] '{}' → font='{}' size={:.1}pt bold={} italic={} scale={:.2}",
                    &text[..text.len().min(30)],
                    font.typeface().family_name(),
                    font_size.raw(),
                    bold,
                    italic,
                    text_scale,
                );
                text_paint.set_color4f(to_color4f(*color), None);

                if char_spacing.abs() > Pt::ZERO {
                    // §17.3.2.35 w:spacing — draw each character with
                    // explicit spacing to match the measured fragment width.
                    let char_count = text.chars().count();
                    let glyphs = font.text_to_glyphs_vec(&**text);
                    // Batch path: use text_to_glyphs + get_widths when glyph
                    // count matches char count (common Latin/CJK text).
                    // Fallback to per-char measure_str for ligatures or
                    // complex scripts where counts diverge.
                    let batch_widths = if glyphs.len() == char_count {
                        let mut widths = vec![0f32; glyphs.len()];
                        font.get_widths(&glyphs, &mut widths);
                        Some(widths)
                    } else {
                        None
                    };

                    let mut cursor = *position;
                    let mut buf = [0u8; 4];
                    for (i, ch) in text.chars().enumerate() {
                        let s = ch.encode_utf8(&mut buf);
                        // Per-glyph widths from `get_widths` already include
                        // scale_x — they're advances of the scaled font.
                        // measure_str on the scaled font likewise returns the
                        // scaled advance, so no further multiplication is
                        // needed. char_spacing stays unscaled (§17.3.2.45).
                        let w = if let Some(ref widths) = batch_widths {
                            widths[i]
                        } else {
                            font.measure_str(&*s, None).0
                        };
                        canvas.draw_str(&*s, to_point(cursor), font, &text_paint);
                        cursor.x += Pt::new(w) + *char_spacing;
                    }
                } else if let Some(slot) = blob_slot {
                    // Common path: reuse a cached, position-independent glyph
                    // run instead of remapping text→glyphs on every draw_str.
                    // Visually identical — a TextBlob built from the font uses
                    // the same default cmap+advance positioning draw_str does.
                    let inner = blob_cache.entry(slot).or_default();
                    if let Some(blob) = inner.get(&**text) {
                        canvas.draw_text_blob(blob, to_point(*position), &text_paint);
                    } else if let Some(blob) = TextBlob::from_str(&**text, font) {
                        canvas.draw_text_blob(&blob, to_point(*position), &text_paint);
                        inner.insert(Box::from(&**text), blob);
                    }
                } else {
                    canvas.draw_str(text, to_point(*position), font, &text_paint);
                }
            }
            DrawCommand::Underline { line, color, width }
            | DrawCommand::Line { line, color, width } => {
                stroke_paint.set_stroke_width(f32::from(*width));
                stroke_paint.set_color4f(to_color4f(*color), None);

                let (start, end) = to_line(*line);
                canvas.draw_line(start, end, &stroke_paint);
            }
            DrawCommand::Image {
                rect,
                image_data,
                src_rect,
            } => {
                let dst = to_rect(*rect);
                // §20.1.10.48: draw only the cropped source sub-rectangle
                // (in the image's own pixel space), stretched to `dst`.
                let draw = |image: &skia_safe::Image| match src_rect {
                    Some(crop) => {
                        let (iw, ih) = (image.width() as f32, image.height() as f32);
                        let src = skia_safe::Rect::from_xywh(
                            crop.origin.x.raw() * iw,
                            crop.origin.y.raw() * ih,
                            crop.size.width.raw() * iw,
                            crop.size.height.raw() * ih,
                        );
                        canvas.draw_image_rect(
                            image,
                            Some((&src, SrcRectConstraint::Strict)),
                            dst,
                            &default_paint,
                        );
                    }
                    None => {
                        canvas.draw_image_rect(image, None, dst, &default_paint);
                    }
                };
                // Size the downsample to the pre-crop extent, then key the
                // cache on (media identity, that target) so a different-size or
                // different-crop placement of the same media does not reuse
                // this placement's resolution.
                let downsample_rect = crop_adjusted_downsample_rect(rect, src_rect);
                let (target_w, target_h) = downsample_target(downsample_rect);
                let key = (Rc::as_ptr(&image_data.data), target_w, target_h);
                if let Some(image) = image_cache.get(&key) {
                    draw(image);
                } else if let Some(image) = decode_image(image_data) {
                    let image = downsample_if_oversize(image, downsample_rect);
                    draw(&image);
                    image_cache.insert(key, image);
                } else {
                    let magic = &image_data.data[..image_data.data.len().min(4)];
                    log::warn!(
                        "[paint] unsupported image format {:?} — could not decode {} bytes \
                         (magic: {:02x?}); image will be blank",
                        image_data.format,
                        image_data.data.len(),
                        magic,
                    );
                }
            }
            DrawCommand::EmojiCluster {
                rect,
                text,
                typeface,
                size,
                presentation,
                structure,
            } => {
                use crate::render::emoji::cluster::EmojiCluster;
                use skia_safe::{CubicResampler, SamplingOptions};
                let cluster = EmojiCluster {
                    text: text.as_str(),
                    presentation: *presentation,
                    structure: *structure,
                };
                // Pass `rect.size` so the rasterizer allocates an image
                // whose aspect matches the rect → uniform scaling at
                // `draw_image_rect`, no anisotropic distortion.
                let img = emoji_rasterizer.rasterize(&cluster, typeface, *size, rect.size);
                // Mitchell cubic resampling — same filter we use for
                // photographic image downsampling. Without explicit
                // sampling, Skia defaults to nearest/bilinear which makes
                // the emoji look blurry/pixelated at typical PDF zoom.
                let sampling = SamplingOptions::from(CubicResampler::mitchell());
                canvas.draw_image_rect_with_sampling_options(
                    &img.image,
                    None,
                    to_rect(*rect),
                    sampling,
                    &default_paint,
                );
            }
            DrawCommand::Rect { rect, color } => {
                rect_paint.set_color4f(to_color4f(*color), None);
                canvas.draw_rect(to_rect(*rect), &rect_paint);
            }
            DrawCommand::LinkAnnotation { rect, url } => {
                let mut url_bytes = url.as_bytes().to_vec();
                url_bytes.push(0);
                let url_data = Data::new_copy(&url_bytes);
                canvas.annotate_rect_with_url(to_rect(*rect), &url_data);
            }
            DrawCommand::InternalLink { rect, destination } => {
                let mut name_bytes = destination.as_bytes().to_vec();
                name_bytes.push(0);
                let name_data = Data::new_copy(&name_bytes);
                canvas.annotate_link_to_destination(to_rect(*rect), &name_data);
            }
            DrawCommand::NamedDestination { position, name } => {
                let mut name_bytes = name.as_bytes().to_vec();
                name_bytes.push(0);
                let name_data = Data::new_copy(&name_bytes);
                canvas.annotate_named_destination(to_point(*position), &name_data);
            }
            DrawCommand::Path {
                origin,
                rotation,
                flip_h,
                flip_v,
                extent,
                paths,
                fill,
                stroke,
                effects,
            } => {
                canvas.save();
                // Translate to the shape's origin.
                canvas.translate((origin.x.raw(), origin.y.raw()));
                // Apply flip / rotation around the shape's center.
                let cx = extent.width.raw() / 2.0;
                let cy = extent.height.raw() / 2.0;
                let rot_deg = rotation.raw() as f32 / 60_000.0;
                if *flip_h || *flip_v || rot_deg != 0.0 {
                    canvas.translate((cx, cy));
                    if rot_deg != 0.0 {
                        canvas.rotate(rot_deg, None);
                    }
                    let sx = if *flip_h { -1.0 } else { 1.0 };
                    let sy = if *flip_v { -1.0 } else { 1.0 };
                    if sx != 1.0 || sy != 1.0 {
                        canvas.scale((sx, sy));
                    }
                    canvas.translate((-cx, -cy));
                }
                let skia_path = build_skia_path(paths);
                let strokable = build_skia_path_stroked_only(paths);
                // §20.1.8 effects render beneath the shape itself, in the
                // order they appear in the effect list.
                for effect in effects {
                    paint_effect(
                        canvas,
                        effect,
                        fill,
                        stroke.as_ref(),
                        &skia_path,
                        &strokable,
                    );
                }
                if let Some(paint) = fill_to_paint(fill) {
                    canvas.draw_path(&skia_path, &paint);
                }
                if let Some(stroke) = stroke.as_ref() {
                    let paint = stroke_to_paint(stroke);
                    // Only stroke subpaths whose .stroked flag is set.
                    canvas.draw_path(&strokable, &paint);
                }
                canvas.restore();
            }
        }
    }
}

// ── Shape path helpers ──────────────────────────────────────────────────────

/// Build a Skia path from all subpaths, regardless of stroke flag. Used for
/// fill painting — OOXML fills every subpath's interior per its fill mode.
fn build_skia_path(paths: &[SubPath]) -> Path {
    let mut builder = PathBuilder::new();
    builder.set_fill_type(PathFillType::Winding);
    for sub in paths {
        emit_subpath(&mut builder, sub);
    }
    builder.snapshot()
}

/// Build a Skia path limited to subpaths with `.stroked == true`.
fn build_skia_path_stroked_only(paths: &[SubPath]) -> Path {
    let mut builder = PathBuilder::new();
    for sub in paths {
        if sub.stroked {
            emit_subpath(&mut builder, sub);
        }
    }
    builder.snapshot()
}

fn emit_subpath(builder: &mut PathBuilder, sub: &SubPath) {
    // Track the last point manually: `PathBuilder` has no `last_pt()` query,
    // and `arc_to` needs the current pen position to derive the bounding oval.
    let mut last_pt: (f32, f32) = (0.0, 0.0);
    for verb in &sub.verbs {
        match verb {
            PathVerb::MoveTo(p) => {
                let pt = (p.x.raw(), p.y.raw());
                builder.move_to(pt);
                last_pt = pt;
            }
            PathVerb::LineTo(p) => {
                let pt = (p.x.raw(), p.y.raw());
                builder.line_to(pt);
                last_pt = pt;
            }
            PathVerb::QuadTo(c, p) => {
                let pt = (p.x.raw(), p.y.raw());
                builder.quad_to((c.x.raw(), c.y.raw()), pt);
                last_pt = pt;
            }
            PathVerb::CubicTo(c1, c2, p) => {
                let pt = (p.x.raw(), p.y.raw());
                builder.cubic_to((c1.x.raw(), c1.y.raw()), (c2.x.raw(), c2.y.raw()), pt);
                last_pt = pt;
            }
            PathVerb::ArcTo {
                radii,
                start_angle,
                swing_angle,
            } => {
                // OOXML arcTo positions the arc on the oval centered at the
                // current pen point offset by (-wr, -hr) — §20.1.9.3. Skia's
                // PathBuilder::arc_to expects the bounding oval; we compute
                // it from the current point + radii. Angles are kept in
                // OOXML's convention (0° = 3 o'clock, clockwise +) which
                // matches Skia.
                let (cx, cy) = last_pt;
                let (wr, hr) = (radii.width.raw(), radii.height.raw());
                let oval = skia_safe::Rect::from_xywh(cx - wr, cy - hr, wr * 2.0, hr * 2.0);
                let start_deg = start_angle.raw() as f32 / 60_000.0;
                let sweep_deg = swing_angle.raw() as f32 / 60_000.0;
                builder.arc_to(oval, start_deg, sweep_deg, false);
                // Update last point to the arc's end position.
                let end_rad = (start_deg + sweep_deg).to_radians();
                last_pt = (cx + wr * end_rad.cos(), cy + hr * end_rad.sin());
            }
            PathVerb::Close => {
                builder.close();
            }
        }
    }
}

fn fill_to_paint(fill: &ResolvedFill) -> Option<Paint> {
    match fill {
        ResolvedFill::None => None,
        ResolvedFill::Solid(color) => {
            let mut paint = Paint::default();
            paint.set_anti_alias(true);
            paint.set_style(skia_safe::PaintStyle::Fill);
            paint.set_color4f(rgba_to_color4f(*color), None);
            Some(paint)
        }
        ResolvedFill::Gradient(_) => {
            log::warn!("paint: gradient fill not yet rendered (Tier 2)");
            None
        }
        ResolvedFill::Blip(_) => {
            log::warn!("paint: blip fill not yet rendered (Tier 2)");
            None
        }
        ResolvedFill::Pattern(_) => {
            log::warn!("paint: pattern fill not yet rendered (Tier 3)");
            None
        }
    }
}

fn stroke_to_paint(stroke: &ResolvedStroke) -> Paint {
    let mut paint = Paint::default();
    paint.set_anti_alias(true);
    paint.set_style(skia_safe::PaintStyle::Stroke);
    paint.set_stroke_width(stroke.width.raw());
    paint.set_color4f(rgba_to_color4f(stroke.color), None);
    paint.set_stroke_cap(match stroke.cap {
        ResolvedLineCap::Butt => skia_safe::PaintCap::Butt,
        ResolvedLineCap::Round => skia_safe::PaintCap::Round,
        ResolvedLineCap::Square => skia_safe::PaintCap::Square,
    });
    paint.set_stroke_join(match stroke.join {
        ResolvedLineJoin::Round => skia_safe::PaintJoin::Round,
        ResolvedLineJoin::Bevel => skia_safe::PaintJoin::Bevel,
        ResolvedLineJoin::Miter => skia_safe::PaintJoin::Miter,
    });
    if let ResolvedDashPattern::Dashes(dashes) = &stroke.dash {
        if !dashes.is_empty() {
            let floats: Vec<f32> = dashes.iter().map(|p| p.raw()).collect();
            if let Some(effect) = PathEffect::dash(&floats, 0.0) {
                paint.set_path_effect(effect);
            }
        }
    }
    paint
}

fn rgba_to_color4f(c: Rgba) -> Color4f {
    Color4f::new(c.r, c.g, c.b, c.a)
}

/// Paint a shape effect beneath the shape itself. The effect color is the
/// one already resolved from the effect's `<a:srgbClr>` / `<a:schemeClr>`
/// plus color transforms; the fill/stroke silhouette drives the shadow's
/// shape.
fn paint_effect(
    canvas: &skia_safe::Canvas,
    effect: &ResolvedEffect,
    fill: &ResolvedFill,
    stroke: Option<&ResolvedStroke>,
    shape_path: &Path,
    strokable_path: &Path,
) {
    match effect {
        ResolvedEffect::OuterShadow {
            blur_radius,
            offset,
            color,
        } => {
            // §20.1.8.45: a Gaussian blur. Skia's mask-filter sigma ≈
            // radius / 2 — the conventional approximation used by other
            // renderers (LibreOffice, Chromium's CSS filter).
            let sigma = (blur_radius.raw() * 0.5).max(0.0);
            let mask = if sigma > 0.0 {
                MaskFilter::blur(BlurStyle::Normal, sigma, None)
            } else {
                None
            };
            canvas.save();
            canvas.translate((offset.x.raw(), offset.y.raw()));
            // Fill silhouette (when the shape has a fill).
            if !matches!(fill, ResolvedFill::None) {
                let mut paint = Paint::default();
                paint.set_anti_alias(true);
                paint.set_style(skia_safe::PaintStyle::Fill);
                paint.set_color4f(rgba_to_color4f(*color), None);
                if let Some(m) = mask.clone() {
                    paint.set_mask_filter(m);
                }
                canvas.draw_path(shape_path, &paint);
            }
            // Stroke silhouette — cast the shadow from the stroke's own
            // outline so line-preset shapes cast a visible shadow.
            if let Some(s) = stroke {
                let mut paint = Paint::default();
                paint.set_anti_alias(true);
                paint.set_style(skia_safe::PaintStyle::Stroke);
                paint.set_stroke_width(s.width.raw());
                paint.set_stroke_cap(match s.cap {
                    ResolvedLineCap::Butt => skia_safe::PaintCap::Butt,
                    ResolvedLineCap::Round => skia_safe::PaintCap::Round,
                    ResolvedLineCap::Square => skia_safe::PaintCap::Square,
                });
                paint.set_stroke_join(match s.join {
                    ResolvedLineJoin::Round => skia_safe::PaintJoin::Round,
                    ResolvedLineJoin::Bevel => skia_safe::PaintJoin::Bevel,
                    ResolvedLineJoin::Miter => skia_safe::PaintJoin::Miter,
                });
                paint.set_color4f(rgba_to_color4f(*color), None);
                if let Some(m) = mask.clone() {
                    paint.set_mask_filter(m);
                }
                canvas.draw_path(strokable_path, &paint);
            }
            canvas.restore();
        }
    }
}

/// Decode a `MediaEntry` to a Skia image, dispatching on format.
///
/// Returns `None` if the format is unsupported or the data is malformed.
fn decode_image(entry: &MediaEntry) -> Option<skia_safe::Image> {
    use crate::model::ImageFormat;
    match entry.format {
        ImageFormat::Emf => crate::render::emf::decode_emf_bitmap(&entry.data),
        // All other formats are handled by Skia's built-in decoder.
        _ => skia_safe::Image::from_encoded(Data::new_copy(&entry.data)),
    }
}

/// Downsample an image if its native pixel dimensions significantly exceed
/// the display dimensions at `IMAGE_TARGET_DPI`. Uses Mitchell-Netravali
/// cubic filtering for high-quality results.
fn downsample_if_oversize(
    image: skia_safe::Image,
    rect: crate::render::geometry::PtRect,
) -> skia_safe::Image {
    use skia_safe::CubicResampler;
    use skia_safe::{AlphaType, ColorType, ImageInfo, SamplingOptions};

    let (target_w, target_h) = downsample_target(rect);
    if image.width() > target_w && image.height() > target_h && target_w > 0 && target_h > 0 {
        log::debug!(
            "[paint] downsampling image {}×{} → {}×{} (display {:.0}×{:.0}pt @ {:.0} DPI)",
            image.width(),
            image.height(),
            target_w,
            target_h,
            rect.size.width.raw(),
            rect.size.height.raw(),
            IMAGE_TARGET_DPI,
        );
        // Draw scaled image onto an opaque surface so Skia applies JPEG
        // encoding (encoding_quality) instead of lossless FlateDecode.
        let info = ImageInfo::new(
            (target_w, target_h),
            ColorType::RGBA8888,
            AlphaType::Opaque,
            None,
        );
        let sampling = SamplingOptions::from(CubicResampler::mitchell());
        if let Some(mut surface) = skia_safe::surfaces::raster(&info, None, None) {
            let dst = skia_safe::Rect::from_iwh(target_w, target_h);
            surface.canvas().draw_image_rect_with_sampling_options(
                &image,
                None,
                dst,
                sampling,
                &Paint::default(),
            );
            surface.image_snapshot()
        } else {
            image
        }
    } else {
        image
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::geometry::{PtOffset, PtSize};
    use crate::render::resolve::color::RgbColor;
    use skia_safe::FontMgr;
    use std::rc::Rc;

    fn test_font_mgr() -> FontMgr {
        FontMgr::new()
    }

    fn test_registry() -> FontRegistry {
        FontRegistry::new(test_font_mgr())
    }

    // ── render_to_pdf integration ───────────────────────────────────

    #[test]
    fn render_text_command_produces_pdf() {
        let registry = test_registry();
        let page = LayoutedPage {
            commands: vec![DrawCommand::Text {
                position: PtOffset::new(Pt::new(72.0), Pt::new(100.0)),
                text: "Hello world".into(),
                font_family: Rc::from("Helvetica"),
                char_spacing: Pt::ZERO,
                font_size: Pt::new(12.0),
                bold: false,
                italic: false,
                color: RgbColor::BLACK,
                text_scale: 1.0,
            }],
            page_size: PtSize::new(Pt::new(612.0), Pt::new(792.0)),
        };

        let pdf_bytes = render_to_pdf(&[page], &registry).expect("render_to_pdf must succeed");
        assert!(pdf_bytes.len() > 100, "PDF output must be non-trivial");
        assert_eq!(&pdf_bytes[..5], b"%PDF-", "output must be valid PDF");
    }

    #[test]
    fn render_text_with_char_spacing_produces_pdf() {
        let registry = test_registry();
        let page = LayoutedPage {
            commands: vec![DrawCommand::Text {
                position: PtOffset::new(Pt::new(72.0), Pt::new(100.0)),
                text: "Spaced".into(),
                font_family: Rc::from("Helvetica"),
                char_spacing: Pt::new(2.0),
                font_size: Pt::new(14.0),
                bold: true,
                italic: false,
                color: RgbColor::BLACK,
                text_scale: 1.0,
            }],
            page_size: PtSize::new(Pt::new(612.0), Pt::new(792.0)),
        };

        let pdf_bytes = render_to_pdf(&[page], &registry).expect("render_to_pdf must succeed");
        assert!(pdf_bytes.len() > 100);
        assert_eq!(&pdf_bytes[..5], b"%PDF-");
    }

    #[test]
    fn render_empty_text_produces_pdf() {
        let registry = test_registry();
        let page = LayoutedPage {
            commands: vec![DrawCommand::Text {
                position: PtOffset::new(Pt::new(72.0), Pt::new(100.0)),
                text: Rc::from(""),
                font_family: Rc::from("Helvetica"),
                char_spacing: Pt::ZERO,
                font_size: Pt::new(12.0),
                bold: false,
                italic: false,
                color: RgbColor::BLACK,
                text_scale: 1.0,
            }],
            page_size: PtSize::new(Pt::new(612.0), Pt::new(792.0)),
        };

        let pdf_bytes = render_to_pdf(&[page], &registry).expect("empty text must not panic");
        assert_eq!(&pdf_bytes[..5], b"%PDF-");
    }

    // ── DrawCommand::Path ─────────────────────────────────────────────

    #[test]
    fn render_path_solid_filled_rect() {
        use crate::model::dimension::Dimension;
        use crate::model::PathFillMode;
        use crate::render::layout::draw_command::{
            ResolvedDashPattern, ResolvedFill, ResolvedLineCap, ResolvedLineJoin, ResolvedStroke,
        };
        use crate::render::resolve::drawing_color::Rgba;
        use crate::render::resolve::shape_geometry::{PathVerb, SubPath};

        let verbs = vec![
            PathVerb::MoveTo(PtOffset::new(Pt::ZERO, Pt::ZERO)),
            PathVerb::LineTo(PtOffset::new(Pt::new(100.0), Pt::ZERO)),
            PathVerb::LineTo(PtOffset::new(Pt::new(100.0), Pt::new(50.0))),
            PathVerb::LineTo(PtOffset::new(Pt::ZERO, Pt::new(50.0))),
            PathVerb::Close,
        ];
        let page = LayoutedPage {
            commands: vec![DrawCommand::Path {
                origin: PtOffset::new(Pt::new(72.0), Pt::new(100.0)),
                rotation: Dimension::new(0),
                flip_h: false,
                flip_v: false,
                extent: PtSize::new(Pt::new(100.0), Pt::new(50.0)),
                paths: vec![SubPath {
                    verbs,
                    fill_mode: PathFillMode::Norm,
                    stroked: true,
                }],
                fill: ResolvedFill::Solid(Rgba {
                    r: 1.0,
                    g: 0.0,
                    b: 0.0,
                    a: 1.0,
                }),
                stroke: Some(ResolvedStroke {
                    width: Pt::new(1.0),
                    color: Rgba::BLACK,
                    dash: ResolvedDashPattern::Solid,
                    cap: ResolvedLineCap::Butt,
                    join: ResolvedLineJoin::Miter,
                }),
                effects: vec![],
            }],
            page_size: PtSize::new(Pt::new(612.0), Pt::new(792.0)),
        };
        let pdf = render_to_pdf(&[page], &test_registry()).expect("render path");
        assert_eq!(&pdf[..5], b"%PDF-");
    }

    #[test]
    fn render_path_dashed_line() {
        use crate::model::dimension::Dimension;
        use crate::model::PathFillMode;
        use crate::render::layout::draw_command::{
            ResolvedDashPattern, ResolvedFill, ResolvedLineCap, ResolvedLineJoin, ResolvedStroke,
        };
        use crate::render::resolve::drawing_color::Rgba;
        use crate::render::resolve::shape_geometry::{PathVerb, SubPath};

        let page = LayoutedPage {
            commands: vec![DrawCommand::Path {
                origin: PtOffset::new(Pt::new(50.0), Pt::new(50.0)),
                rotation: Dimension::new(0),
                flip_h: false,
                flip_v: false,
                extent: PtSize::new(Pt::new(100.0), Pt::new(0.0)),
                paths: vec![SubPath {
                    verbs: vec![
                        PathVerb::MoveTo(PtOffset::new(Pt::ZERO, Pt::ZERO)),
                        PathVerb::LineTo(PtOffset::new(Pt::new(100.0), Pt::ZERO)),
                    ],
                    fill_mode: PathFillMode::None,
                    stroked: true,
                }],
                fill: ResolvedFill::None,
                stroke: Some(ResolvedStroke {
                    width: Pt::new(2.0),
                    color: Rgba {
                        r: 0.85,
                        g: 0.6,
                        b: 0.2,
                        a: 1.0,
                    },
                    dash: ResolvedDashPattern::Dashes(vec![Pt::new(6.0), Pt::new(3.0)]),
                    cap: ResolvedLineCap::Round,
                    join: ResolvedLineJoin::Round,
                }),
                effects: vec![],
            }],
            page_size: PtSize::new(Pt::new(612.0), Pt::new(792.0)),
        };
        let pdf = render_to_pdf(&[page], &test_registry()).expect("render dashed line");
        assert_eq!(&pdf[..5], b"%PDF-");
    }

    #[test]
    fn render_unicode_text_produces_pdf() {
        let registry = test_registry();
        let page = LayoutedPage {
            commands: vec![DrawCommand::Text {
                position: PtOffset::new(Pt::new(72.0), Pt::new(100.0)),
                text: "Ärzte für Ökologie — 日本語".into(),
                font_family: Rc::from("Helvetica"),
                char_spacing: Pt::ZERO,
                font_size: Pt::new(11.0),
                bold: false,
                italic: false,
                color: RgbColor::BLACK,
                text_scale: 1.0,
            }],
            page_size: PtSize::new(Pt::new(612.0), Pt::new(792.0)),
        };

        let pdf_bytes = render_to_pdf(&[page], &registry).expect("unicode text must not panic");
        assert_eq!(&pdf_bytes[..5], b"%PDF-");
    }

    // ── image cache keying ──────────────────────────────────────────

    fn rect(w: f32, h: f32) -> crate::render::geometry::PtRect {
        crate::render::geometry::PtRect::from_xywh(Pt::ZERO, Pt::ZERO, Pt::new(w), Pt::new(h))
    }

    #[test]
    fn downsample_target_scales_points_to_target_dpi() {
        // 72pt @ 300 DPI = 300px; 36pt = 150px.
        assert_eq!(downsample_target(rect(72.0, 36.0)), (300, 150));
    }

    #[test]
    fn crop_inflates_downsample_target_so_cache_keys_differ() {
        // Same on-page size; one placement crops to the middle 50%×50%.
        let display = rect(72.0, 36.0);
        let uncropped = crop_adjusted_downsample_rect(&display, &None);
        let cropped = crop_adjusted_downsample_rect(
            &display,
            &Some(crate::render::geometry::PtRect::from_xywh(
                Pt::new(0.25),
                Pt::new(0.25),
                Pt::new(0.5),
                Pt::new(0.5),
            )),
        );
        // The crop needs 2× the source resolution for the same display size,
        // so the downsample targets — and thus the cache keys — differ, and
        // the second placement won't reuse the first's lower-res bitmap.
        assert_eq!(downsample_target(uncropped), (300, 150));
        assert_eq!(downsample_target(cropped), (600, 300));
        assert_ne!(downsample_target(uncropped), downsample_target(cropped));
    }

    #[test]
    fn degenerate_zero_crop_leaves_downsample_rect_unchanged() {
        let display = rect(72.0, 36.0);
        // A zero-area crop must not divide by zero; falls back to the display rect.
        let zero = Some(crate::render::geometry::PtRect::from_xywh(
            Pt::ZERO,
            Pt::ZERO,
            Pt::ZERO,
            Pt::ZERO,
        ));
        assert_eq!(
            downsample_target(crop_adjusted_downsample_rect(&display, &zero)),
            downsample_target(display),
        );
    }
}
