//! PostGIS EWKB 到 WKT 的轻量解码器。

const EWKB_Z_FLAG: u32 = 0x8000_0000;
const EWKB_M_FLAG: u32 = 0x4000_0000;
const EWKB_SRID_FLAG: u32 = 0x2000_0000;
const EWKB_BBOX_FLAG: u32 = 0x1000_0000;
const EWKB_FLAGS: u32 = EWKB_Z_FLAG | EWKB_M_FLAG | EWKB_SRID_FLAG | EWKB_BBOX_FLAG;
const MAX_GEOMETRY_ITEMS: usize = 1_000_000;
const MAX_GEOMETRY_DEPTH: usize = 64;

pub(super) fn is_spatial_type(type_name: &str) -> bool {
    let base_name = type_name
        .split_once('(')
        .map_or(type_name, |(base_name, _)| base_name.trim());
    base_name.eq_ignore_ascii_case("geometry") || base_name.eq_ignore_ascii_case("geography")
}

/// 将 PostgreSQL 二进制格式中的 PostGIS 几何值转为可读 WKT；格式异常时退回十六进制文本。
pub(super) fn binary_to_wkt(bytes: &[u8]) -> String {
    let mut reader = GeometryReader::new(bytes);
    match reader.read_geometry(0) {
        Ok(geometry) if reader.is_empty() => geometry.to_wkt(),
        _ => bytes_to_hex(bytes),
    }
}

/// 文本传输模式可能已经是 WKT，也可能是 PostGIS 的十六进制输出；两者统一为可读文本。
pub(super) fn text_to_wkt(value: &str) -> String {
    let value = value.trim();
    decode_hex(value)
        .map(|bytes| binary_to_wkt(&bytes))
        .unwrap_or_else(|| value.to_string())
}

fn decode_hex(value: &str) -> Option<Vec<u8>> {
    let value = value
        .strip_prefix("\\x")
        .or_else(|| value.strip_prefix("0x"))
        .unwrap_or(value);
    if value.is_empty() || !value.len().is_multiple_of(2) {
        return None;
    }

    let mut bytes = Vec::with_capacity(value.len() / 2);
    let mut chars = value.as_bytes().chunks_exact(2);
    for pair in &mut chars {
        let high = hex_digit(pair[0])?;
        let low = hex_digit(pair[1])?;
        bytes.push((high << 4) | low);
    }
    chars.remainder().is_empty().then_some(bytes)
}

fn hex_digit(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

fn bytes_to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(bytes.len().saturating_mul(2));
    for &byte in bytes {
        text.push(HEX[usize::from(byte >> 4)] as char);
        text.push(HEX[usize::from(byte & 0x0f)] as char);
    }
    text
}

#[derive(Clone, Copy)]
enum ByteOrder {
    Little,
    Big,
}

#[derive(Clone, Copy)]
struct Dimensions {
    has_z: bool,
    has_m: bool,
}

impl Dimensions {
    fn coordinate_count(self) -> usize {
        2 + usize::from(self.has_z) + usize::from(self.has_m)
    }

    fn suffix(self) -> &'static str {
        match (self.has_z, self.has_m) {
            (true, true) => " ZM",
            (true, false) => " Z",
            (false, true) => " M",
            (false, false) => "",
        }
    }
}

#[derive(Clone, Copy)]
enum GeometryKind {
    Point,
    LineString,
    Polygon,
    MultiPoint,
    MultiLineString,
    MultiPolygon,
    GeometryCollection,
}

impl GeometryKind {
    fn from_type_id(type_id: u32) -> Result<Self, String> {
        match type_id {
            1 => Ok(Self::Point),
            2 => Ok(Self::LineString),
            3 => Ok(Self::Polygon),
            4 => Ok(Self::MultiPoint),
            5 => Ok(Self::MultiLineString),
            6 => Ok(Self::MultiPolygon),
            7 => Ok(Self::GeometryCollection),
            _ => Err(format!("unsupported PostGIS geometry type {type_id}")),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Point => "POINT",
            Self::LineString => "LINESTRING",
            Self::Polygon => "POLYGON",
            Self::MultiPoint => "MULTIPOINT",
            Self::MultiLineString => "MULTILINESTRING",
            Self::MultiPolygon => "MULTIPOLYGON",
            Self::GeometryCollection => "GEOMETRYCOLLECTION",
        }
    }
}

enum GeometryBody {
    Point(Option<Vec<f64>>),
    Coordinates(Vec<Vec<f64>>),
    Rings(Vec<Vec<Vec<f64>>>),
    Children(Vec<ParsedGeometry>),
}

struct ParsedGeometry {
    kind: GeometryKind,
    dimensions: Dimensions,
    body: GeometryBody,
}

impl ParsedGeometry {
    fn to_wkt(&self) -> String {
        let prefix = format!("{}{}", self.kind.name(), self.dimensions.suffix());
        match &self.body {
            GeometryBody::Point(Some(point)) => {
                format!("{prefix} ({})", format_coordinate(point))
            }
            GeometryBody::Point(None) => format!("{prefix} EMPTY"),
            GeometryBody::Coordinates(points) if points.is_empty() => format!("{prefix} EMPTY"),
            GeometryBody::Coordinates(points) => {
                format!("{prefix} ({})", format_coordinates(points))
            }
            GeometryBody::Rings(rings) if rings.is_empty() => format!("{prefix} EMPTY"),
            GeometryBody::Rings(rings) => format!("{prefix} ({})", format_rings(rings)),
            GeometryBody::Children(children) if children.is_empty() => format!("{prefix} EMPTY"),
            GeometryBody::Children(children) => {
                let items = if matches!(self.kind, GeometryKind::GeometryCollection) {
                    children
                        .iter()
                        .map(ParsedGeometry::to_wkt)
                        .collect::<Vec<_>>()
                        .join(", ")
                } else {
                    children
                        .iter()
                        .map(ParsedGeometry::to_collection_body)
                        .collect::<Vec<_>>()
                        .join(", ")
                };
                format!("{prefix} ({items})")
            }
        }
    }

    fn to_collection_body(&self) -> String {
        match &self.body {
            GeometryBody::Point(Some(point)) => format!("({})", format_coordinate(point)),
            GeometryBody::Point(None) => "EMPTY".to_string(),
            GeometryBody::Coordinates(points) => format!("({})", format_coordinates(points)),
            GeometryBody::Rings(rings) => format!("({})", format_rings(rings)),
            GeometryBody::Children(_) => self.to_wkt(),
        }
    }
}

fn format_coordinate(coordinate: &[f64]) -> String {
    coordinate
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(" ")
}

fn format_coordinates(coordinates: &[Vec<f64>]) -> String {
    coordinates
        .iter()
        .map(|coordinate| format_coordinate(coordinate))
        .collect::<Vec<_>>()
        .join(", ")
}

fn format_rings(rings: &[Vec<Vec<f64>>]) -> String {
    rings
        .iter()
        .map(|ring| format!("({})", format_coordinates(ring)))
        .collect::<Vec<_>>()
        .join(", ")
}

struct GeometryReader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> GeometryReader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn is_empty(&self) -> bool {
        self.offset == self.bytes.len()
    }

    fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.offset)
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], String> {
        let end = self
            .offset
            .checked_add(length)
            .ok_or_else(|| "PostGIS geometry length overflow".to_string())?;
        if end > self.bytes.len() {
            return Err("PostGIS geometry is truncated".to_string());
        }
        let value = &self.bytes[self.offset..end];
        self.offset = end;
        Ok(value)
    }

    fn read_u8(&mut self) -> Result<u8, String> {
        self.take(1).map(|value| value[0])
    }

    fn read_u32(&mut self, order: ByteOrder) -> Result<u32, String> {
        let bytes: [u8; 4] = self
            .take(4)?
            .try_into()
            .map_err(|_| "invalid PostGIS u32".to_string())?;
        Ok(match order {
            ByteOrder::Little => u32::from_le_bytes(bytes),
            ByteOrder::Big => u32::from_be_bytes(bytes),
        })
    }

    fn read_f64(&mut self, order: ByteOrder) -> Result<f64, String> {
        let bytes: [u8; 8] = self
            .take(8)?
            .try_into()
            .map_err(|_| "invalid PostGIS f64".to_string())?;
        Ok(match order {
            ByteOrder::Little => f64::from_le_bytes(bytes),
            ByteOrder::Big => f64::from_be_bytes(bytes),
        })
    }

    fn read_count(&mut self, order: ByteOrder) -> Result<usize, String> {
        let count = usize::try_from(self.read_u32(order)?)
            .map_err(|_| "PostGIS geometry item count is too large".to_string())?;
        if count > MAX_GEOMETRY_ITEMS || count > self.remaining() {
            return Err("PostGIS geometry item count is invalid".to_string());
        }
        Ok(count)
    }

    fn read_geometry(&mut self, depth: usize) -> Result<ParsedGeometry, String> {
        if depth > MAX_GEOMETRY_DEPTH {
            return Err("PostGIS geometry nesting is too deep".to_string());
        }
        let order = match self.read_u8()? {
            0 => ByteOrder::Big,
            1 => ByteOrder::Little,
            _ => return Err("invalid PostGIS byte order".to_string()),
        };
        let type_word = self.read_u32(order)?;
        let has_z = type_word & EWKB_Z_FLAG != 0;
        let has_m = type_word & EWKB_M_FLAG != 0;
        let has_srid = type_word & EWKB_SRID_FLAG != 0;
        let has_bbox = type_word & EWKB_BBOX_FLAG != 0;
        let base_type = type_word & !EWKB_FLAGS;
        let iso_dimension = base_type / 1000;
        let type_id = base_type % 1000;
        let dimensions = Dimensions {
            has_z: has_z || matches!(iso_dimension, 1 | 3),
            has_m: has_m || matches!(iso_dimension, 2 | 3),
        };
        let kind = GeometryKind::from_type_id(type_id)?;

        if has_srid {
            let _ = self.read_u32(order)?;
        }
        if has_bbox {
            for _ in 0..dimensions.coordinate_count().saturating_mul(2) {
                let _ = self.read_f64(order)?;
            }
        }

        let body = match kind {
            GeometryKind::Point => {
                let coordinate = self.read_coordinate(order, dimensions)?;
                let empty = coordinate.iter().all(|value| value.is_nan());
                GeometryBody::Point((!empty).then_some(coordinate))
            }
            GeometryKind::LineString => {
                GeometryBody::Coordinates(self.read_coordinates(order, dimensions)?)
            }
            GeometryKind::Polygon => GeometryBody::Rings(self.read_rings(order, dimensions)?),
            GeometryKind::MultiPoint
            | GeometryKind::MultiLineString
            | GeometryKind::MultiPolygon
            | GeometryKind::GeometryCollection => {
                let count = self.read_count(order)?;
                let mut children = Vec::new();
                for _ in 0..count {
                    children.push(self.read_geometry(depth + 1)?);
                }
                GeometryBody::Children(children)
            }
        };

        Ok(ParsedGeometry {
            kind,
            dimensions,
            body,
        })
    }

    fn read_coordinate(
        &mut self,
        order: ByteOrder,
        dimensions: Dimensions,
    ) -> Result<Vec<f64>, String> {
        let mut coordinate = Vec::with_capacity(dimensions.coordinate_count());
        for _ in 0..dimensions.coordinate_count() {
            coordinate.push(self.read_f64(order)?);
        }
        Ok(coordinate)
    }

    fn read_coordinates(
        &mut self,
        order: ByteOrder,
        dimensions: Dimensions,
    ) -> Result<Vec<Vec<f64>>, String> {
        let count = self.read_count(order)?;
        let mut coordinates = Vec::new();
        for _ in 0..count {
            coordinates.push(self.read_coordinate(order, dimensions)?);
        }
        Ok(coordinates)
    }

    fn read_rings(
        &mut self,
        order: ByteOrder,
        dimensions: Dimensions,
    ) -> Result<Vec<Vec<Vec<f64>>>, String> {
        let count = self.read_count(order)?;
        let mut rings = Vec::new();
        for _ in 0..count {
            rings.push(self.read_coordinates(order, dimensions)?);
        }
        Ok(rings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linestring_ewkb_is_decoded_to_wkt_coordinates() {
        let mut bytes = vec![1_u8];
        bytes.extend_from_slice(&(EWKB_SRID_FLAG | 2).to_le_bytes());
        bytes.extend_from_slice(&4326_u32.to_le_bytes());
        bytes.extend_from_slice(&2_u32.to_le_bytes());
        for value in [119.5622303_f64, 23.5619068, 119.5622159, 23.5620773] {
            bytes.extend_from_slice(&value.to_le_bytes());
        }

        assert_eq!(
            binary_to_wkt(&bytes),
            "LINESTRING (119.5622303 23.5619068, 119.5622159 23.5620773)"
        );
    }

    #[test]
    fn polygon_and_nested_geometries_are_decoded() {
        let mut polygon = vec![1_u8];
        polygon.extend_from_slice(&3_u32.to_le_bytes());
        polygon.extend_from_slice(&1_u32.to_le_bytes());
        polygon.extend_from_slice(&5_u32.to_le_bytes());
        for value in [0.0_f64, 0.0, 1.0, 0.0, 1.0, 1.0, 0.0, 1.0, 0.0, 0.0] {
            polygon.extend_from_slice(&value.to_le_bytes());
        }
        assert_eq!(
            binary_to_wkt(&polygon),
            "POLYGON ((0 0, 1 0, 1 1, 0 1, 0 0))"
        );

        let mut collection = vec![1_u8];
        collection.extend_from_slice(&7_u32.to_le_bytes());
        collection.extend_from_slice(&1_u32.to_le_bytes());
        collection.extend_from_slice(&1_u8.to_le_bytes());
        collection.extend_from_slice(&1_u32.to_le_bytes());
        collection.extend_from_slice(&1.0_f64.to_le_bytes());
        collection.extend_from_slice(&2.0_f64.to_le_bytes());
        assert_eq!(
            binary_to_wkt(&collection),
            "GEOMETRYCOLLECTION (POINT (1 2))"
        );
    }

    #[test]
    fn text_hex_and_invalid_binary_have_text_fallbacks() {
        assert_eq!(
            text_to_wkt("0101000000000000000000f03f0000000000000040"),
            "POINT (1 2)"
        );
        assert_eq!(
            text_to_wkt("LINESTRING (1 2, 3 4)"),
            "LINESTRING (1 2, 3 4)"
        );
        assert_eq!(binary_to_wkt(&[1, 2, 3]), "010203");
    }

    #[test]
    fn recognizes_postgis_spatial_types_only() {
        assert!(is_spatial_type("geometry"));
        assert!(is_spatial_type("geometry(LineString, 4326)"));
        assert!(is_spatial_type("GEOGRAPHY"));
        assert!(!is_spatial_type("bytea"));
    }
}
