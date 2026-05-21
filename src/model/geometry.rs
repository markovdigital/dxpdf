use crate::model::dimension::{Dimension, Unit};

/// A 2D offset (x, y) in a given unit.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Offset<U: Unit> {
    pub x: Dimension<U>,
    pub y: Dimension<U>,
}

impl<U: Unit> Offset<U> {
    pub const ZERO: Self = Self {
        x: Dimension::ZERO,
        y: Dimension::ZERO,
    };

    pub const fn new(x: Dimension<U>, y: Dimension<U>) -> Self {
        Self { x, y }
    }
}

/// A 2D size (width, height) in a given unit.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Size<U: Unit> {
    pub width: Dimension<U>,
    pub height: Dimension<U>,
}

impl<U: Unit> Size<U> {
    pub const ZERO: Self = Self {
        width: Dimension::ZERO,
        height: Dimension::ZERO,
    };

    pub const fn new(width: Dimension<U>, height: Dimension<U>) -> Self {
        Self { width, height }
    }
}

/// A rectangle defined by origin + size.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect<U: Unit> {
    pub origin: Offset<U>,
    pub size: Size<U>,
}

impl<U: Unit> Rect<U> {
    pub const fn new(origin: Offset<U>, size: Size<U>) -> Self {
        Self { origin, size }
    }
}

/// Insets from each edge (top, right, bottom, left) — used for margins and padding.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EdgeInsets<U: Unit> {
    pub top: Dimension<U>,
    pub right: Dimension<U>,
    pub bottom: Dimension<U>,
    pub left: Dimension<U>,
}

impl<U: Unit> EdgeInsets<U> {
    pub const ZERO: Self = Self {
        top: Dimension::ZERO,
        right: Dimension::ZERO,
        bottom: Dimension::ZERO,
        left: Dimension::ZERO,
    };

    pub const fn new(
        top: Dimension<U>,
        right: Dimension<U>,
        bottom: Dimension<U>,
        left: Dimension<U>,
    ) -> Self {
        Self {
            top,
            right,
            bottom,
            left,
        }
    }

    pub const fn uniform(value: Dimension<U>) -> Self {
        Self {
            top: value,
            right: value,
            bottom: value,
            left: value,
        }
    }
}

/// Edge insets where each side may be unspecified — used for cell-level
/// overrides (`<w:tcMar>`) that cascade against a parent default. Per
/// OOXML §17.4.42, an explicit `<w:tcMar>` with only some sides present
/// inherits the remaining sides from the table-level `<w:tblCellMar>`;
/// `unwrap_or_default()` per side would incorrectly zero out the inherited
/// values (visible as missing left/right padding inside a cell whose
/// `tcMar` set only `top` and `bottom`). `resolve_against` performs the
/// per-side fallback.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PartialEdgeInsets<U: Unit> {
    pub top: Option<Dimension<U>>,
    pub right: Option<Dimension<U>>,
    pub bottom: Option<Dimension<U>>,
    pub left: Option<Dimension<U>>,
}

impl<U: Unit> PartialEdgeInsets<U> {
    pub const fn new(
        top: Option<Dimension<U>>,
        right: Option<Dimension<U>>,
        bottom: Option<Dimension<U>>,
        left: Option<Dimension<U>>,
    ) -> Self {
        Self {
            top,
            right,
            bottom,
            left,
        }
    }

    /// Resolve unspecified sides against a fully-specified default.
    pub fn resolve_against(self, default: EdgeInsets<U>) -> EdgeInsets<U> {
        EdgeInsets {
            top: self.top.unwrap_or(default.top),
            right: self.right.unwrap_or(default.right),
            bottom: self.bottom.unwrap_or(default.bottom),
            left: self.left.unwrap_or(default.left),
        }
    }
}
