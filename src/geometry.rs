//! Regions: the `x,y,w,h` spec, and cropping a logical rectangle out of a
//! picture of an output in its own pixels.

/// A rectangle in an output's logical pixels, from its top-left corner.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// A rectangle of a picture, in its pixels, inside it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Crop {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// Reads `x,y,w,h`: four numbers, fractions allowed, the width and height
/// above zero.
pub fn parse_spec(spec: &str) -> Result<Rect, String> {
    let bad =
        |why: &str| format!("bad region {spec:?}: {why} (want \"x,y,w,h\" in logical pixels)");
    let parts: Vec<&str> = spec.split(',').map(str::trim).collect();
    if parts.len() != 4 {
        return Err(bad("not four numbers"));
    }
    let mut n = [0.0; 4];
    for (slot, part) in n.iter_mut().zip(&parts) {
        *slot = part
            .parse::<f64>()
            .ok()
            .filter(|v| v.is_finite())
            .ok_or_else(|| bad(&format!("{part:?} is not a number")))?;
    }
    let [x, y, width, height] = n;
    if width <= 0.0 || height <= 0.0 {
        return Err(bad("the width and height must be above zero"));
    }
    Ok(Rect {
        x,
        y,
        width,
        height,
    })
}

/// Whether any of `rect` is on an output of this logical size.
pub fn overlaps(rect: &Rect, logical: (i32, i32)) -> bool {
    rect.x < logical.0 as f64
        && rect.y < logical.1 as f64
        && rect.x + rect.width > 0.0
        && rect.y + rect.height > 0.0
}

/// Rounds as JavaScript's `Math.round`, which the QML crop used: halves go
/// up, -2.5 to -2.
fn js_round(v: f64) -> f64 {
    (v + 0.5).floor()
}

/// How many of the picture's pixels there are to a logical pixel, along one
/// axis: picture size over logical size, as the QML crop worked it out
/// (`sourceSize / size`). An output with no logical size falls back to its
/// scale.
fn axis_scale(pixels: u32, logical: i32, scale: f32) -> f64 {
    if logical > 0 {
        pixels as f64 / logical as f64
    } else if scale > 0.0 {
        scale as f64
    } else {
        1.0
    }
}

/// The pixels of a `picture`-sized picture of an output of `logical` size
/// that `rect` covers (all of it for `None`). As the QML did: the scale is
/// the picture's size over the logical size, edges round as `Math.round`,
/// and the size is at least a pixel. What falls off the picture is cut off;
/// `None` when nothing is left.
pub fn crop(
    rect: Option<&Rect>,
    logical: (i32, i32),
    scale: f32,
    picture: (u32, u32),
) -> Option<Crop> {
    let (pw, ph) = picture;
    if pw == 0 || ph == 0 {
        return None;
    }
    let Some(r) = rect else {
        return Some(Crop {
            x: 0,
            y: 0,
            width: pw,
            height: ph,
        });
    };
    let sx = axis_scale(pw, logical.0, scale);
    let sy = axis_scale(ph, logical.1, scale);
    let span = |at: f64, len: f64, s: f64, limit: u32| -> Option<(u32, u32)> {
        let start = js_round(at * s);
        let end = start + js_round(len * s).max(1.0);
        let (start, end) = (start.max(0.0), end.min(limit as f64));
        (end > start).then_some((start as u32, (end - start) as u32))
    };
    let (x, width) = span(r.x, r.width, sx, pw)?;
    let (y, height) = span(r.y, r.height, sy, ph)?;
    Some(Crop {
        x,
        y,
        width,
        height,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f64, y: f64, width: f64, height: f64) -> Rect {
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    fn crop_of(x: u32, y: u32, width: u32, height: u32) -> Crop {
        Crop {
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn specs_are_four_numbers() {
        assert_eq!(
            parse_spec("10,20,300,40"),
            Ok(rect(10.0, 20.0, 300.0, 40.0))
        );
        assert_eq!(parse_spec(" 1.5, -2 ,3,4 "), Ok(rect(1.5, -2.0, 3.0, 4.0)));
        for bad in [
            "",
            "1,2,3",
            "1,2,3,4,5",
            "1,2,,4",
            "a,2,3,4",
            "1,2,NaN,4",
            "1,2,inf,4",
            "1,2,0,4",
            "1,2,3,-4",
            "1;2;3;4",
        ] {
            assert!(parse_spec(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn rounding_is_javascripts() {
        assert_eq!(js_round(2.5), 3.0);
        assert_eq!(js_round(-2.5), -2.0);
        assert_eq!(js_round(2.4999), 2.0);
        assert_eq!(js_round(-0.6), -1.0);
    }

    #[test]
    fn a_whole_output_is_the_whole_picture() {
        assert_eq!(
            crop(None, (1536, 864), 1.25, (1920, 1080)),
            Some(crop_of(0, 0, 1920, 1080))
        );
        assert_eq!(crop(None, (1536, 864), 1.25, (0, 1080)), None);
    }

    #[test]
    fn at_scale_1_logical_is_physical() {
        // The ScreenPad: 1920x515 at 1.
        let at = |r: Rect| crop(Some(&r), (1920, 515), 1.0, (1920, 515));
        assert_eq!(
            at(rect(10.0, 20.0, 300.0, 40.0)),
            Some(crop_of(10, 20, 300, 40))
        );
        assert_eq!(
            at(rect(0.0, 0.0, 1920.0, 515.0)),
            Some(crop_of(0, 0, 1920, 515))
        );
        // Fractions round as Math.round; the size is at least a pixel.
        assert_eq!(at(rect(10.5, 20.4, 0.2, 0.7)), Some(crop_of(11, 20, 1, 1)));
    }

    #[test]
    fn at_scale_1_25_edges_round() {
        // eDP-1: 1920x1080 pixels, 1536x864 logical.
        let at = |r: Rect| crop(Some(&r), (1536, 864), 1.25, (1920, 1080));
        assert_eq!(
            at(rect(100.0, 200.0, 300.0, 400.0)),
            Some(crop_of(125, 250, 375, 500))
        );
        // 1.25 * 1 = 1.25 -> 1; 1.25 * 2 = 2.5 -> 3 (halves go up); 1.25 * 3 = 3.75 -> 4.
        assert_eq!(at(rect(1.0, 2.0, 3.0, 2.0)), Some(crop_of(1, 3, 4, 3)));
        assert_eq!(
            at(rect(0.0, 0.0, 1536.0, 864.0)),
            Some(crop_of(0, 0, 1920, 1080))
        );
        assert_eq!(at(rect(10.0, 10.0, 0.3, 0.3)), Some(crop_of(13, 13, 1, 1)));
    }

    #[test]
    fn at_scale_2_everything_doubles() {
        // The TV: 3840x2160 pixels, 1920x1080 logical.
        let at = |r: Rect| crop(Some(&r), (1920, 1080), 2.0, (3840, 2160));
        assert_eq!(
            at(rect(10.0, 20.0, 30.0, 40.0)),
            Some(crop_of(20, 40, 60, 80))
        );
        assert_eq!(at(rect(0.25, 0.75, 0.5, 0.5)), Some(crop_of(1, 2, 1, 1)));
        assert_eq!(
            at(rect(1919.0, 1079.0, 1.0, 1.0)),
            Some(crop_of(3838, 2158, 2, 2))
        );
    }

    #[test]
    fn what_falls_off_the_output_is_cut_off() {
        let at = |r: Rect| crop(Some(&r), (1920, 515), 1.0, (1920, 515));
        assert_eq!(
            at(rect(1900.0, 500.0, 100.0, 100.0)),
            Some(crop_of(1900, 500, 20, 15))
        );
        assert_eq!(
            at(rect(-10.0, -5.0, 20.0, 10.0)),
            Some(crop_of(0, 0, 10, 5))
        );
        assert_eq!(at(rect(1920.0, 0.0, 10.0, 10.0)), None);
        assert_eq!(at(rect(-20.0, 0.0, 10.0, 10.0)), None);
    }

    #[test]
    fn overlap_is_checked_in_logical_pixels() {
        assert!(overlaps(&rect(0.0, 0.0, 1.0, 1.0), (1536, 864)));
        assert!(overlaps(&rect(-5.0, -5.0, 6.0, 6.0), (1536, 864)));
        assert!(!overlaps(&rect(1536.0, 0.0, 10.0, 10.0), (1536, 864)));
        assert!(!overlaps(&rect(-10.0, 0.0, 10.0, 10.0), (1536, 864)));
    }

    #[test]
    fn no_logical_size_falls_back_to_the_scale() {
        assert_eq!(
            crop(
                Some(&rect(10.0, 10.0, 10.0, 10.0)),
                (0, 0),
                2.0,
                (3840, 2160)
            ),
            Some(crop_of(20, 20, 20, 20))
        );
    }
}
