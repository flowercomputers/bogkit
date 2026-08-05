use std::collections::BTreeSet;
use std::f64::consts::PI;
use std::io::BufRead;

use serde::de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Number, Value};

const MIN_ZOOM: u8 = 12;
const MAX_ZOOM: u8 = 16;
const MAX_MERCATOR_LAT: f64 = 85.051_128_779_806_6;
const EPS: f64 = 1e-12;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Tile {
    pub z: u8,
    pub x: u32,
    pub y: u32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Point {
    x: f64,
    y: f64,
}

type Ring = Vec<Point>;
type Polygon = Vec<Ring>;
type MultiPolygon = Vec<Polygon>;

#[derive(Debug)]
struct Edit {
    geometries: Vec<MultiPolygon>,
}

#[derive(Clone, Copy, Debug)]
struct Rect {
    min_x: f64,
    min_y: f64,
    max_x: f64,
    max_y: f64,
}

struct StrictValueSeed;

impl<'de> DeserializeSeed<'de> for StrictValueSeed {
    type Value = Value;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_any(StrictValueVisitor)
    }
}

struct StrictValueVisitor;

impl<'de> Visitor<'de> for StrictValueVisitor {
    type Value = Value;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a JSON value without duplicate object members")
    }

    fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E> {
        Ok(Value::Bool(value))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E> {
        Ok(Value::Number(Number::from(value)))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
        Ok(Value::Number(Number::from(value)))
    }

    fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Number::from_f64(value)
            .map(Value::Number)
            .ok_or_else(|| E::custom("non-finite JSON number"))
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E> {
        Ok(Value::String(value.to_owned()))
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
        Ok(Value::String(value))
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(Value::Null)
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(Value::Null)
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut values = Vec::new();
        while let Some(value) = sequence.next_element_seed(StrictValueSeed)? {
            values.push(value);
        }
        Ok(Value::Array(values))
    }

    fn visit_map<A>(self, mut object: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut values = Map::new();
        while let Some(key) = object.next_key::<String>()? {
            if values.contains_key(&key) {
                return Err(de::Error::custom(format!(
                    "duplicate object member `{key}`"
                )));
            }
            values.insert(key, object.next_value_seed(StrictValueSeed)?);
        }
        Ok(Value::Object(values))
    }
}

fn parse_json_strict(text: &str) -> Result<Value, serde_json::Error> {
    let mut deserializer = serde_json::Deserializer::from_str(text);
    let value = StrictValueSeed.deserialize(&mut deserializer)?;
    deserializer.end()?;
    Ok(value)
}

pub fn plan(reader: impl BufRead) -> Result<BTreeSet<Tile>, String> {
    let mut tiles = BTreeSet::new();
    for (index, line_result) in reader.lines().enumerate() {
        let line_number = index + 1;
        let line = line_result.map_err(|error| format!("input line {line_number}: {error}"))?;
        if line.trim().is_empty() {
            return Err(format!("input line {line_number}: blank line"));
        }
        let value = parse_json_strict(&line)
            .map_err(|error| format!("input line {line_number}: invalid JSON: {error}"))?;
        let edit =
            parse_edit(&value).map_err(|error| format!("input line {line_number}: {error}"))?;
        for geometry in &edit.geometries {
            collect_geometry_tiles(geometry, &mut tiles);
        }
    }
    Ok(tiles)
}

pub fn format_plan(tiles: &BTreeSet<Tile>) -> Vec<String> {
    let mut lines: Vec<_> = tiles
        .iter()
        .map(|tile| format!("{}/{}/{}", tile.z, tile.x, tile.y))
        .collect();
    lines.sort_unstable();
    lines
}

fn parse_edit(value: &Value) -> Result<Edit, String> {
    let object = value
        .as_object()
        .ok_or_else(|| "edit must be a JSON object".to_string())?;
    match object.get("id") {
        Some(Value::String(id)) if !id.is_empty() => {}
        _ => return Err("id must be a non-empty string".to_string()),
    }

    let mut geometries = Vec::with_capacity(2);
    for field in ["old", "new"] {
        match object.get(field) {
            None | Some(Value::Null) => {}
            Some(geometry) => geometries.push(parse_geometry(geometry, field)?),
        }
    }
    if geometries.is_empty() {
        return Err("at least one of old or new must contain a geometry".to_string());
    }
    Ok(Edit { geometries })
}

fn parse_geometry(value: &Value, field: &str) -> Result<MultiPolygon, String> {
    let object = value
        .as_object()
        .ok_or_else(|| format!("{field} geometry must be an object"))?;
    let geometry_type = required_string(object, "type", field)?;
    let coordinates = object
        .get("coordinates")
        .ok_or_else(|| format!("{field}.coordinates is required"))?;
    let polygons = match geometry_type {
        "Polygon" => vec![parse_polygon(coordinates, &format!("{field}.coordinates"))?],
        "MultiPolygon" => parse_multipolygon(coordinates, &format!("{field}.coordinates"))?,
        other => return Err(format!("{field}.type {other:?} is unsupported")),
    };

    let (min_lon, max_lon) = longitude_extent(&polygons);
    if max_lon - min_lon > 180.0 {
        return Err(format!(
            "{field} crosses the antimeridian, which this prototype does not support"
        ));
    }
    Ok(polygons)
}

fn required_string<'a>(
    object: &'a Map<String, Value>,
    key: &str,
    context: &str,
) -> Result<&'a str, String> {
    object
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{context}.{key} must be a string"))
}

fn parse_multipolygon(value: &Value, path: &str) -> Result<MultiPolygon, String> {
    let values = value
        .as_array()
        .ok_or_else(|| format!("{path} must be an array"))?;
    if values.is_empty() {
        return Err(format!("{path} must contain at least one polygon"));
    }
    let polygons: MultiPolygon = values
        .iter()
        .enumerate()
        .map(|(index, polygon)| parse_polygon(polygon, &format!("{path}[{index}]")))
        .collect::<Result<_, _>>()?;
    validate_multipolygon(&polygons, path)?;
    Ok(polygons)
}

fn parse_polygon(value: &Value, path: &str) -> Result<Polygon, String> {
    let values = value
        .as_array()
        .ok_or_else(|| format!("{path} must be an array"))?;
    if values.is_empty() {
        return Err(format!("{path} must contain an exterior ring"));
    }
    let polygon: Polygon = values
        .iter()
        .enumerate()
        .map(|(index, ring)| parse_ring(ring, &format!("{path}[{index}]")))
        .collect::<Result<_, _>>()?;
    validate_polygon(&polygon, path)?;
    Ok(polygon)
}

fn parse_ring(value: &Value, path: &str) -> Result<Ring, String> {
    let values = value
        .as_array()
        .ok_or_else(|| format!("{path} must be an array"))?;
    if values.len() < 4 {
        return Err(format!("{path} must contain at least four positions"));
    }
    let points: Ring = values
        .iter()
        .enumerate()
        .map(|(index, position)| parse_position(position, &format!("{path}[{index}]")))
        .collect::<Result<_, _>>()?;
    if points.first() != points.last() {
        return Err(format!(
            "{path} is open; first and last positions must match"
        ));
    }
    if points.windows(2).any(|segment| segment[0] == segment[1]) {
        return Err(format!("{path} contains a zero-length edge"));
    }
    let segment_count = points.len() - 1;
    for first in 0..segment_count {
        for second in (first + 1)..segment_count {
            let adjacent = second == first + 1 || (first == 0 && second + 1 == segment_count);
            if !adjacent
                && segments_intersect(
                    points[first],
                    points[first + 1],
                    points[second],
                    points[second + 1],
                )
            {
                return Err(format!("{path} self-intersects"));
            }
        }
    }
    if signed_twice_area(&points).abs() <= EPS {
        return Err(format!("{path} has zero area"));
    }
    Ok(points)
}

fn signed_twice_area(ring: &Ring) -> f64 {
    ring.windows(2)
        .map(|segment| segment[0].x * segment[1].y - segment[1].x * segment[0].y)
        .sum()
}

fn validate_polygon(polygon: &Polygon, path: &str) -> Result<(), String> {
    let exterior = &polygon[0];
    for (hole_index, hole) in polygon[1..].iter().enumerate() {
        let hole_path = format!("{path}[{}]", hole_index + 1);
        if point_in_ring(hole[0], exterior) != Location::Inside || rings_intersect(exterior, hole) {
            return Err(format!(
                "{hole_path} must be strictly inside the exterior ring"
            ));
        }
    }

    for first in 1..polygon.len() {
        for second in (first + 1)..polygon.len() {
            if rings_intersect(&polygon[first], &polygon[second])
                || point_in_ring(polygon[first][0], &polygon[second]) != Location::Outside
                || point_in_ring(polygon[second][0], &polygon[first]) != Location::Outside
            {
                return Err(format!(
                    "{path}[{first}] and {path}[{second}] overlap or nest"
                ));
            }
        }
    }
    Ok(())
}

fn validate_multipolygon(polygons: &MultiPolygon, path: &str) -> Result<(), String> {
    for first in 0..polygons.len() {
        for second in (first + 1)..polygons.len() {
            let first_exterior = &polygons[first][0];
            let second_exterior = &polygons[second][0];
            if rings_intersect(first_exterior, second_exterior)
                || point_in_ring(first_exterior[0], second_exterior) != Location::Outside
                || point_in_ring(second_exterior[0], first_exterior) != Location::Outside
            {
                return Err(format!(
                    "{path}[{first}] and {path}[{second}] overlap or nest"
                ));
            }
        }
    }
    Ok(())
}

fn rings_intersect(first: &Ring, second: &Ring) -> bool {
    first.windows(2).any(|a| {
        second
            .windows(2)
            .any(|b| segments_intersect(a[0], a[1], b[0], b[1]))
    })
}

fn segments_intersect(a: Point, b: Point, c: Point, d: Point) -> bool {
    let orientation = |first: Point, second: Point, third: Point| {
        (second.x - first.x) * (third.y - first.y) - (second.y - first.y) * (third.x - first.x)
    };
    let ab_c = orientation(a, b, c);
    let ab_d = orientation(a, b, d);
    let cd_a = orientation(c, d, a);
    let cd_b = orientation(c, d, b);

    if ((ab_c > EPS && ab_d < -EPS) || (ab_c < -EPS && ab_d > EPS))
        && ((cd_a > EPS && cd_b < -EPS) || (cd_a < -EPS && cd_b > EPS))
    {
        return true;
    }
    (ab_c.abs() <= EPS && point_on_segment(c, a, b))
        || (ab_d.abs() <= EPS && point_on_segment(d, a, b))
        || (cd_a.abs() <= EPS && point_on_segment(a, c, d))
        || (cd_b.abs() <= EPS && point_on_segment(b, c, d))
}

fn parse_position(value: &Value, path: &str) -> Result<Point, String> {
    let values = value
        .as_array()
        .ok_or_else(|| format!("{path} must be a two-number position"))?;
    if values.len() != 2 {
        return Err(format!(
            "{path} must contain exactly longitude and latitude"
        ));
    }
    let x = finite_number(&values[0], &format!("{path}[0]"))?;
    let y = finite_number(&values[1], &format!("{path}[1]"))?;
    if !(-180.0..=180.0).contains(&x) {
        return Err(format!("{path}[0] longitude is outside [-180, 180]"));
    }
    if !(-MAX_MERCATOR_LAT..=MAX_MERCATOR_LAT).contains(&y) {
        return Err(format!("{path}[1] latitude is outside Web Mercator limits"));
    }
    Ok(Point { x, y })
}

fn finite_number(value: &Value, path: &str) -> Result<f64, String> {
    let number = value
        .as_f64()
        .ok_or_else(|| format!("{path} must be a finite number"))?;
    if !number.is_finite() {
        return Err(format!("{path} must be a finite number"));
    }
    Ok(number)
}

fn longitude_extent(polygons: &MultiPolygon) -> (f64, f64) {
    polygons
        .iter()
        .flat_map(|polygon| polygon.iter())
        .flat_map(|ring| ring.iter())
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(min, max), point| {
            (min.min(point.x), max.max(point.x))
        })
}

fn collect_geometry_tiles(geometry: &MultiPolygon, tiles: &mut BTreeSet<Tile>) {
    for polygon in geometry {
        let bounds = polygon_bounds(polygon);
        for z in MIN_ZOOM..=MAX_ZOOM {
            let n = 1_u32 << z;
            let x_min_world = longitude_to_world_x(bounds.min_x, n);
            let x_max_world = longitude_to_world_x(bounds.max_x, n);
            let y_a = latitude_to_world_y(bounds.min_y, n);
            let y_b = latitude_to_world_y(bounds.max_y, n);
            let (x_start, x_end) = touching_index_range(x_min_world, x_max_world, n);
            let (y_start, y_end) = touching_index_range(y_a.min(y_b), y_a.max(y_b), n);
            for x in x_start..=x_end {
                for y in y_start..=y_end {
                    if polygon_intersects_rect(polygon, tile_rect(z, x, y)) {
                        tiles.insert(Tile { z, x, y });
                    }
                }
            }
        }
    }
}

fn polygon_bounds(polygon: &Polygon) -> Rect {
    polygon.iter().flat_map(|ring| ring.iter()).fold(
        Rect {
            min_x: f64::INFINITY,
            min_y: f64::INFINITY,
            max_x: f64::NEG_INFINITY,
            max_y: f64::NEG_INFINITY,
        },
        |bounds, point| Rect {
            min_x: bounds.min_x.min(point.x),
            min_y: bounds.min_y.min(point.y),
            max_x: bounds.max_x.max(point.x),
            max_y: bounds.max_y.max(point.y),
        },
    )
}

fn longitude_to_world_x(longitude: f64, n: u32) -> f64 {
    (longitude + 180.0) / 360.0 * f64::from(n)
}

fn latitude_to_world_y(latitude: f64, n: u32) -> f64 {
    let radians = latitude.to_radians();
    (1.0 - radians.tan().asinh() / PI) / 2.0 * f64::from(n)
}

fn touching_index_range(min: f64, max: f64, n: u32) -> (u32, u32) {
    let lower = if near_integer(min) {
        min.round() as i64 - 1
    } else {
        min.floor() as i64
    };
    let upper = if near_integer(max) {
        max.round() as i64
    } else {
        max.floor() as i64
    };
    let last = i64::from(n) - 1;
    (lower.clamp(0, last) as u32, upper.clamp(0, last) as u32)
}

fn near_integer(value: f64) -> bool {
    (value - value.round()).abs() <= 1e-10
}

fn tile_rect(z: u8, x: u32, y: u32) -> Rect {
    let n = f64::from(1_u32 << z);
    Rect {
        min_x: f64::from(x) / n * 360.0 - 180.0,
        max_x: f64::from(x + 1) / n * 360.0 - 180.0,
        min_y: world_y_to_latitude(f64::from(y + 1), n),
        max_y: world_y_to_latitude(f64::from(y), n),
    }
}

fn world_y_to_latitude(y: f64, n: f64) -> f64 {
    (PI * (1.0 - 2.0 * y / n)).sinh().atan().to_degrees()
}

fn polygon_intersects_rect(polygon: &Polygon, rect: Rect) -> bool {
    if polygon.iter().any(|ring| {
        ring.windows(2)
            .any(|segment| segment_intersects_rect(segment[0], segment[1], rect))
    }) {
        return true;
    }

    let corners = [
        Point {
            x: rect.min_x,
            y: rect.min_y,
        },
        Point {
            x: rect.min_x,
            y: rect.max_y,
        },
        Point {
            x: rect.max_x,
            y: rect.min_y,
        },
        Point {
            x: rect.max_x,
            y: rect.max_y,
        },
    ];
    corners
        .into_iter()
        .any(|corner| point_in_filled_polygon(corner, polygon))
}

fn segment_intersects_rect(a: Point, b: Point, rect: Rect) -> bool {
    let dx = b.x - a.x;
    let dy = b.y - a.y;
    let mut t_min: f64 = 0.0;
    let mut t_max: f64 = 1.0;
    for (p, q) in [
        (-dx, a.x - rect.min_x),
        (dx, rect.max_x - a.x),
        (-dy, a.y - rect.min_y),
        (dy, rect.max_y - a.y),
    ] {
        if p.abs() <= EPS {
            if q < -EPS {
                return false;
            }
        } else {
            let ratio = q / p;
            if p < 0.0 {
                t_min = t_min.max(ratio);
            } else {
                t_max = t_max.min(ratio);
            }
            if t_min - t_max > EPS {
                return false;
            }
        }
    }
    true
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Location {
    Outside,
    Inside,
    Boundary,
}

fn point_in_filled_polygon(point: Point, polygon: &Polygon) -> bool {
    match point_in_ring(point, &polygon[0]) {
        Location::Outside => false,
        Location::Boundary => true,
        Location::Inside => !polygon[1..]
            .iter()
            .any(|hole| point_in_ring(point, hole) == Location::Inside),
    }
}

fn point_in_ring(point: Point, ring: &Ring) -> Location {
    let mut inside = false;
    for segment in ring.windows(2) {
        let a = segment[0];
        let b = segment[1];
        if point_on_segment(point, a, b) {
            return Location::Boundary;
        }
        if (a.y > point.y) != (b.y > point.y) {
            let crossing_x = (b.x - a.x) * (point.y - a.y) / (b.y - a.y) + a.x;
            if crossing_x > point.x {
                inside = !inside;
            }
        }
    }
    if inside {
        Location::Inside
    } else {
        Location::Outside
    }
}

fn point_on_segment(point: Point, a: Point, b: Point) -> bool {
    let cross = (b.x - a.x) * (point.y - a.y) - (b.y - a.y) * (point.x - a.x);
    let scale = (b.x - a.x).abs() + (b.y - a.y).abs() + 1.0;
    if cross.abs() > EPS * scale {
        return false;
    }
    point.x >= a.x.min(b.x) - EPS
        && point.x <= a.x.max(b.x) + EPS
        && point.y >= a.y.min(b.y) - EPS
        && point.y <= a.y.max(b.y) + EPS
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn plan_text(input: &str) -> Result<BTreeSet<Tile>, String> {
        plan(Cursor::new(input))
    }

    #[test]
    fn insertion_and_deletion_have_the_same_plan() {
        let geometry = r#"{"type":"Polygon","coordinates":[[[-73.99,40.72],[-73.98,40.72],[-73.98,40.73],[-73.99,40.73],[-73.99,40.72]]]}"#;
        let inserted = plan_text(&format!(r#"{{"id":"p","new":{geometry}}}"#)).unwrap();
        let deleted = plan_text(&format!(r#"{{"id":"p","old":{geometry}}}"#)).unwrap();
        assert_eq!(inserted, deleted);
        assert!(!inserted.is_empty());
    }

    #[test]
    fn exact_tile_boundary_touches_both_sides() {
        let boundary = f64::from(19_301_u32) / f64::from(1_u32 << 16) * 360.0 - 180.0;
        let input = format!(
            r#"{{"id":"edge","new":{{"type":"Polygon","coordinates":[[[{boundary},40.7],[{},40.7],[{},40.72],[{boundary},40.72],[{boundary},40.7]]]}}}}"#,
            boundary + 0.0002,
            boundary + 0.0002,
        );
        let tiles = plan_text(&input).unwrap();
        let xs: BTreeSet<_> = tiles
            .iter()
            .filter(|tile| tile.z == 16)
            .map(|tile| tile.x)
            .collect();
        assert!(xs.contains(&19_300));
        assert!(xs.contains(&19_301));
    }

    #[test]
    fn exact_tile_corner_touches_all_four_neighbors() {
        let z = 16;
        let x = 19_301;
        let y = 24_641;
        let corner_x = longitude_to_world_x(tile_rect(z, x, y).min_x, 1_u32 << z);
        let corner_y = latitude_to_world_y(tile_rect(z, x, y).max_y, 1_u32 << z);
        assert!(near_integer(corner_x));
        assert!(near_integer(corner_y));
        let corner = tile_rect(z, x, y);
        let input = format!(
            r#"{{"id":"corner","new":{{"type":"Polygon","coordinates":[[[{},{}],[{},{}],[{},{}],[{},{}]]]}}}}"#,
            corner.min_x,
            corner.max_y,
            corner.min_x + 0.0002,
            corner.max_y,
            corner.min_x,
            corner.max_y - 0.0002,
            corner.min_x,
            corner.max_y,
        );
        let tiles = plan_text(&input).unwrap();
        for expected in [
            Tile {
                z,
                x: x - 1,
                y: y - 1,
            },
            Tile { z, x, y: y - 1 },
            Tile { z, x: x - 1, y },
            Tile { z, x, y },
        ] {
            assert!(tiles.contains(&expected), "missing {expected:?}");
        }
    }

    #[test]
    fn tile_wholly_inside_hole_is_excluded() {
        let z = 16;
        let x = 19_301;
        let y = 24_641;
        let tile = tile_rect(z, x, y);
        let pad_x = tile.max_x - tile.min_x;
        let pad_y = tile.max_y - tile.min_y;
        let outer = vec![
            Point {
                x: tile.min_x - pad_x,
                y: tile.min_y - pad_y,
            },
            Point {
                x: tile.max_x + pad_x,
                y: tile.min_y - pad_y,
            },
            Point {
                x: tile.max_x + pad_x,
                y: tile.max_y + pad_y,
            },
            Point {
                x: tile.min_x - pad_x,
                y: tile.max_y + pad_y,
            },
            Point {
                x: tile.min_x - pad_x,
                y: tile.min_y - pad_y,
            },
        ];
        let hole = vec![
            Point {
                x: tile.min_x - pad_x / 4.0,
                y: tile.min_y - pad_y / 4.0,
            },
            Point {
                x: tile.max_x + pad_x / 4.0,
                y: tile.min_y - pad_y / 4.0,
            },
            Point {
                x: tile.max_x + pad_x / 4.0,
                y: tile.max_y + pad_y / 4.0,
            },
            Point {
                x: tile.min_x - pad_x / 4.0,
                y: tile.max_y + pad_y / 4.0,
            },
            Point {
                x: tile.min_x - pad_x / 4.0,
                y: tile.min_y - pad_y / 4.0,
            },
        ];
        assert!(!polygon_intersects_rect(&vec![outer, hole], tile));
    }

    #[test]
    fn malformed_line_reports_line_and_returns_no_plan() {
        let valid =
            r#"{"id":"ok","new":{"type":"Polygon","coordinates":[[[0,0],[1,0],[1,1],[0,0]]]}}"#;
        let open =
            r#"{"id":"bad","new":{"type":"Polygon","coordinates":[[[0,0],[1,0],[1,1],[0,1]]]}}"#;
        let error = plan_text(&format!("{valid}\n{open}\n")).unwrap_err();
        assert!(error.starts_with("input line 2:"));
        assert!(error.contains("is open"));
    }

    #[test]
    fn duplicate_members_are_rejected_recursively() {
        let cases = [
            r#"{"id":"dup","new":null,"new":{"type":"Polygon","coordinates":[[[0,0],[1,0],[1,1],[0,0]]]}}"#,
            r#"{"id":"dup","new":{"type":"LineString","type":"Polygon","coordinates":[[[0,0],[1,0],[1,1],[0,0]]]}}"#,
            r#"{"id":"dup","meta":{"value":1,"value":2},"new":{"type":"Polygon","coordinates":[[[0,0],[1,0],[1,1],[0,0]]]}}"#,
        ];
        for input in cases {
            let error = plan_text(input).unwrap_err();
            assert!(error.contains("duplicate object member"), "{error}");
        }
    }

    #[test]
    fn invalid_topology_is_rejected() {
        let cases = [
            r#"{"id":"bow","new":{"type":"Polygon","coordinates":[[[0,0],[2,2],[0,2],[2,0],[0,0]]]}}"#,
            r#"{"id":"zero","new":{"type":"Polygon","coordinates":[[[0,0],[0,0],[0,0],[0,0]]]}}"#,
            r#"{"id":"outside-hole","new":{"type":"Polygon","coordinates":[[[0,0],[2,0],[2,2],[0,2],[0,0]],[[3,3],[4,3],[4,4],[3,4],[3,3]]]}}"#,
            r#"{"id":"overlap-hole","new":{"type":"Polygon","coordinates":[[[0,0],[5,0],[5,5],[0,5],[0,0]],[[1,1],[3,1],[3,3],[1,3],[1,1]],[[2,2],[4,2],[4,4],[2,4],[2,2]]]}}"#,
        ];
        for input in cases {
            assert!(plan_text(input).is_err(), "accepted {input}");
        }
    }

    #[test]
    fn output_is_lexicographic_by_rendered_tile_id() {
        let tiles = BTreeSet::from([
            Tile { z: 12, x: 20, y: 3 },
            Tile { z: 12, x: 3, y: 10 },
            Tile { z: 12, x: 3, y: 2 },
        ]);
        assert_eq!(format_plan(&tiles), ["12/20/3", "12/3/10", "12/3/2"]);
    }
}
