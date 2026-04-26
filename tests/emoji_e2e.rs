//! End-to-end tests for the color emoji pipeline.
//!
//! The fixture `test-files/sample-emoji.docx` exercises the cases the
//! pipeline must get right:
//!
//! - Paragraph 1: `Numbers: 1, 2, 3, 4, 5` — pure text. Asserts the
//!   default-text emoji codepoint trap (digits 0-9 have `Emoji=YES` per
//!   UTS #51) is *not* re-introduced.
//! - Paragraph 2: `Emojis: 👋 1️⃣ 👍🏿` — at least two color emoji
//!   clusters that must reach the rasterizer (single + modifier sequence).
//!   The keycap `1️⃣` is split across three runs in the source docx, so
//!   it renders as text glyphs today; this is documented in the test.

use std::path::Path;

use dxpdf::render::layout::draw_command::DrawCommand;

const TEST_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/test-files");
const FIXTURE: &str = "sample-emoji.docx";

fn parse_fixture() -> dxpdf::model::Document {
    let path = Path::new(TEST_DIR).join(FIXTURE);
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("read {FIXTURE}: {e}"));
    dxpdf::docx::parse(&bytes).unwrap_or_else(|e| panic!("parse {FIXTURE}: {e}"))
}

/// True when this host actually resolves a color emoji typeface. Tests that
/// assert the emoji-rasterization branch only fire on hosts that have one
/// (per the no-bundle policy in `docs/emoji-rendering.md`).
fn host_has_color_emoji() -> bool {
    use dxpdf::render::emoji::resolve::{resolve, EmojiTypeface, RegistryLookup};
    use dxpdf::render::fonts::FontRegistry;
    let registry = FontRegistry::new(skia_safe::FontMgr::new());
    let lookup = RegistryLookup {
        registry: &registry,
    };
    matches!(resolve(&lookup, None), EmojiTypeface::Resolved { .. })
}

fn collect_text_strings(commands: &[DrawCommand]) -> Vec<String> {
    commands
        .iter()
        .filter_map(|c| match c {
            DrawCommand::Text { text, .. } => Some(text.to_string()),
            _ => None,
        })
        .collect()
}

fn count_emoji_commands(commands: &[DrawCommand]) -> usize {
    commands
        .iter()
        .filter(|c| matches!(c, DrawCommand::EmojiCluster { .. }))
        .count()
}

// ─── E1: digit regression ────────────────────────────────────────────────────

/// E1 — the "Numbers: 1, 2, 3, 4, 5" paragraph must produce *only* text
/// commands. Direct regression for the bug where digits 0-9 (which have
/// `Emoji=YES` per UTS #51) were rasterized through the color emoji path.
#[test]
fn e1_digits_are_not_rasterized() {
    let doc = parse_fixture();
    let (_, pages) = dxpdf::render::resolve_and_layout(&doc);
    assert!(!pages.is_empty(), "fixture must produce at least one page");

    let all_commands: Vec<&DrawCommand> = pages.iter().flat_map(|p| p.commands.iter()).collect();
    let text_strings: Vec<String> = all_commands
        .iter()
        .filter_map(|c| match c {
            DrawCommand::Text { text, .. } => Some(text.to_string()),
            _ => None,
        })
        .collect();
    let joined: String = text_strings.concat();

    assert!(
        joined.contains("Numbers: 1, 2, 3, 4, 5"),
        "fixture's first paragraph must survive intact in the text stream; \
         got text spans: {text_strings:?}"
    );

    // Cross-check: no EmojiCluster command may carry a pure-digit text. If
    // any does, the regression has come back.
    for cmd in &all_commands {
        if let DrawCommand::EmojiCluster { text, .. } = cmd {
            assert!(
                text.chars().any(|c| !c.is_ascii_digit()),
                "no EmojiCluster may consist solely of ASCII digits — \
                 default-text codepoints must stay in the text path; got {text:?}"
            );
        }
    }
}

// ─── E2: emoji clusters reach the rasterizer ─────────────────────────────────

/// E2 — `Emojis: 👋 ... 👍🏿` produces at least two EmojiCluster commands
/// (the standalone hand wave and the thumbs-up + skin-tone modifier
/// sequence). Skipped on hosts without a color emoji typeface.
#[test]
fn e2_real_emojis_reach_rasterizer() {
    if !host_has_color_emoji() {
        eprintln!("skipping E2: no color emoji typeface on this host");
        return;
    }
    let doc = parse_fixture();
    let (_, pages) = dxpdf::render::resolve_and_layout(&doc);

    let n_emoji: usize = pages
        .iter()
        .map(|p| count_emoji_commands(&p.commands))
        .sum();
    assert!(
        n_emoji >= 2,
        "expected at least 2 EmojiCluster commands (👋 and 👍🏿), got {n_emoji}"
    );

    // Specifically confirm 👋 and 👍🏿 (or 👍 + skin tone) are present.
    let mut saw_wave = false;
    let mut saw_thumbs_up = false;
    for cmd in pages.iter().flat_map(|p| p.commands.iter()) {
        if let DrawCommand::EmojiCluster { text, .. } = cmd {
            if text.contains('\u{1F44B}') {
                saw_wave = true;
            }
            if text.contains('\u{1F44D}') {
                saw_thumbs_up = true;
            }
        }
    }
    assert!(
        saw_wave,
        "expected an EmojiCluster carrying U+1F44B (waving hand)"
    );
    assert!(
        saw_thumbs_up,
        "expected an EmojiCluster carrying U+1F44D (thumbs up base)"
    );
}

// ─── E3: emoji clusters carry well-formed rects ──────────────────────────────

/// E3 — every EmojiCluster command must have positive width and height
/// (i.e. measurement actually happened and the placement rect is not
/// degenerate). A zero-area rect would mean the painter places the image
/// at a single point and viewers render nothing.
#[test]
fn e3_emoji_command_rects_are_non_degenerate() {
    if !host_has_color_emoji() {
        eprintln!("skipping E3: no color emoji typeface on this host");
        return;
    }
    let doc = parse_fixture();
    let (_, pages) = dxpdf::render::resolve_and_layout(&doc);
    let mut checked = 0;
    for cmd in pages.iter().flat_map(|p| p.commands.iter()) {
        if let DrawCommand::EmojiCluster { rect, text, .. } = cmd {
            assert!(
                rect.size.width.raw() > 0.0,
                "EmojiCluster width must be > 0 for cluster {text:?}, got {}",
                rect.size.width.raw()
            );
            assert!(
                rect.size.height.raw() > 0.0,
                "EmojiCluster height must be > 0 for cluster {text:?}, got {}",
                rect.size.height.raw()
            );
            checked += 1;
        }
    }
    assert!(
        checked > 0,
        "should have inspected at least one EmojiCluster"
    );
}

// ─── E4: emoji-bearing PDF round-trips ───────────────────────────────────────

/// E4 — full pipeline (parse → layout → paint → PDF) succeeds and the PDF
/// parses cleanly via a real PDF reader. Catches any draw-command emission
/// or PDF-structure breakage from the emoji image-embedding path.
#[test]
fn e4_full_pdf_round_trip() {
    if !host_has_color_emoji() {
        eprintln!("skipping E4: no color emoji typeface on this host");
        return;
    }
    let font_mgr = skia_safe::FontMgr::new();
    let doc = parse_fixture();
    let pdf_bytes = dxpdf::render::render_with_font_mgr(&doc, &font_mgr)
        .expect("render must succeed for fixture");

    let parsed =
        lopdf::Document::load_mem(&pdf_bytes).expect("rendered PDF must parse cleanly with lopdf");
    assert_eq!(
        parsed.get_pages().len(),
        1,
        "fixture must produce exactly one page"
    );

    // Count image XObjects — must be at least one (Skia emits raster
    // emoji as inline images via the existing image draw path).
    let mut image_count = 0;
    for obj in parsed.objects.values() {
        if let Ok(stream) = obj.as_stream() {
            if stream
                .dict
                .get(b"Subtype")
                .ok()
                .and_then(|s| s.as_name().ok())
                .is_some_and(|n| n == b"Image")
            {
                image_count += 1;
            }
        }
    }
    assert!(
        image_count >= 2,
        "PDF must embed at least 2 image XObjects (👋 and 👍🏿 rasters), got {image_count}"
    );
}

// ─── E5: text path preserves the prefix verbatim ─────────────────────────────

/// E5 — the literal prefix `Emojis: ` must survive in the text stream.
/// If text+emoji adjacency or grapheme-cluster boundary handling regressed,
/// this prefix would fragment or lose characters.
#[test]
fn e5_emoji_paragraph_prefix_intact() {
    let doc = parse_fixture();
    let (_, pages) = dxpdf::render::resolve_and_layout(&doc);
    let all_commands: Vec<&DrawCommand> = pages.iter().flat_map(|p| p.commands.iter()).collect();
    let text_spans: Vec<String> = all_commands
        .iter()
        .filter_map(|c| match c {
            DrawCommand::Text { text, .. } => Some(text.to_string()),
            _ => None,
        })
        .collect();
    let joined: String = text_spans.concat();
    assert!(
        joined.contains("Emojis:"),
        "expected the literal 'Emojis:' prefix in the text stream; got: {text_spans:?}"
    );
}

// ─── Underline regression (§17.3.2.40) ────────────────────────────────────────

/// The fixture's styles.xml carries `<w:u w:val="none"/>` in its rPrDefault.
/// Per OOXML §17.3.2.40 that's the explicit "no underline" override; no
/// `DrawCommand::Underline` may be emitted for any text fragment in the
/// document. Direct regression against the bug where every Word-saved doc
/// got stray underlines under every character because the model's
/// `Some(UnderlineStyle::None)` was conflated with `Some(_actual_style_)`.
#[test]
fn underline_explicit_none_emits_no_underline_commands() {
    let doc = parse_fixture();
    let (_, pages) = dxpdf::render::resolve_and_layout(&doc);
    let underline_count: usize = pages
        .iter()
        .flat_map(|p| p.commands.iter())
        .filter(|c| matches!(c, DrawCommand::Underline { .. }))
        .count();
    assert_eq!(
        underline_count, 0,
        "fixture's <w:u w:val=\"none\"/> override must produce zero \
         Underline draw commands"
    );
}

/// Same shape, applied to borders: the fixture's pPrDefault has
/// `<w:pBdr><w:top w:val="nil"/>...</w:pBdr>` and the rPrDefault has
/// `<w:bdr w:val="nil"/>`. Per OOXML §17.18.2 ST_Border, `nil` (and `none`)
/// mean "no border" — the painter must emit zero `DrawCommand::Line`
/// commands for the document. Direct regression against the bug where
/// every Word-saved doc got hairline boxes around every word and every
/// paragraph because `Some(Border { style: BorderStyle::None })` was
/// treated as "draw the border".
#[test]
fn explicit_nil_borders_emit_no_line_commands() {
    let doc = parse_fixture();
    let (_, pages) = dxpdf::render::resolve_and_layout(&doc);
    let line_count: usize = pages
        .iter()
        .flat_map(|p| p.commands.iter())
        .filter(|c| matches!(c, DrawCommand::Line { .. }))
        .count();
    assert_eq!(
        line_count, 0,
        "fixture's <w:bdr/<w:pBdr> nil cascade must produce zero \
         Line draw commands"
    );
}

// Sanity helper kept as a separate test so test-only code paths don't lint
// as unused when the gated tests skip.
#[test]
fn _helpers_are_used() {
    let _ = collect_text_strings(&[]);
    let _ = count_emoji_commands(&[]);
}
