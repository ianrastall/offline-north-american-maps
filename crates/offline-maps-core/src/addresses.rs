//! House-number search over addresses from the raw OpenStreetMap extract.
//!
//! Addresses (`addr:housenumber` with `addr:street` or `addr:place`, on points,
//! buildings and multipolygons) are stored in the extras database as one row
//! per street and town plus one compact row per house number. A query that
//! starts with a house number ("101 n main st clinton") is matched against
//! the streets; the number itself is then looked up on each matching street.
//! When the number is not mapped, the nearest listed number on that street is
//! offered instead and says so.
use std::collections::HashMap;

use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};

use crate::Coordinates;
use crate::mbtiles::PackageError;
use crate::osm_pbf::{Tags, tag};
use crate::search::{
    EntryKind, SearchResult, canonical, distance_km, expand, match_expression, tokens,
    without_compass_words,
};

/// Longest house number kept; longer values are notes, not numbers.
const MAX_NUMBER_CHARS: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddressStreet {
    pub name: String,
    /// `addr:city`, else the settlement the address most likely belongs to
    /// (see `Settlements`); may be empty.
    pub town: String,
    /// Suburbs and quarters the street's addresses lie in, searchable too.
    pub areas: Vec<String>,
}

/// A city, town, village or hamlet with the radius it is taken to cover,
/// or a suburb or quarter (an area within a city).
#[derive(Debug, Clone, PartialEq)]
pub struct Settlement {
    pub name: String,
    pub lon: f64,
    pub lat: f64,
    pub radius_km: f64,
    pub area: bool,
}

impl Settlement {
    /// Radius from population where tagged (about 20 km for Chicago, 2 km for
    /// a town of 25,000), otherwise from the kind of place.
    pub(crate) fn from_tags(tags: &Tags<'_>, lon: f64, lat: f64) -> Option<Self> {
        let kind = tag(tags, "place")?;
        let name = tag(tags, "name").map(str::trim).filter(|n| !n.is_empty())?;
        let (default, area) = match kind {
            "city" => (6.0, false),
            "town" => (2.5, false),
            "village" => (1.2, false),
            "hamlet" => (0.5, false),
            "suburb" | "quarter" => (1.0, true),
            _ => return None,
        };
        let population = tag(tags, "population")
            .and_then(|p| p.replace(',', "").trim().parse::<f64>().ok())
            .filter(|p| *p > 0.0);
        let radius_km = match (area, population) {
            (false, Some(p)) => (0.012 * p.sqrt()).clamp(0.5, 25.0),
            _ => default,
        };
        Some(Self {
            name: name.to_owned(),
            lon,
            lat,
            radius_km,
            area,
        })
    }
}

/// Settlement lookup over a 0.25-degree grid.
pub(crate) struct Settlements {
    cells: HashMap<(i32, i32), Vec<usize>>,
    items: Vec<Settlement>,
}

const CELLS_PER_DEGREE: f64 = 4.0;

fn grid_cell(lon: f64, lat: f64) -> (i32, i32) {
    (
        (lon * CELLS_PER_DEGREE).floor() as i32,
        (lat * CELLS_PER_DEGREE).floor() as i32,
    )
}

impl Settlements {
    pub(crate) fn new(items: Vec<Settlement>) -> Self {
        let mut cells: HashMap<(i32, i32), Vec<usize>> = HashMap::new();
        for (i, s) in items.iter().enumerate() {
            cells.entry(grid_cell(s.lon, s.lat)).or_default().push(i);
        }
        Self { cells, items }
    }

    fn around(&self, lon: f64, lat: f64, reach: i32) -> impl Iterator<Item = &Settlement> {
        let (cx, cy) = grid_cell(lon, lat);
        (-reach..=reach)
            .flat_map(move |dx| (-reach..=reach).map(move |dy| (cx + dx, cy + dy)))
            .filter_map(|c| self.cells.get(&c))
            .flatten()
            .map(|i| &self.items[*i])
    }

    /// The settlement whose covered radius best contains the point: the
    /// smallest distance divided by radius, within 40 km. A large city wins
    /// over a village a little nearer to its edge, but not over a village
    /// whose centre is close by.
    pub(crate) fn town(&self, lon: f64, lat: f64) -> Option<&str> {
        self.around(lon, lat, 2)
            .filter(|s| !s.area)
            .map(|s| (distance_km(lat, lon, s.lat, s.lon), s))
            .filter(|(d, _)| *d <= 40.0)
            .min_by(|(da, a), (db, b)| (da / a.radius_km).total_cmp(&(db / b.radius_km)))
            .map(|(_, s)| s.name.as_str())
    }

    /// The nearest suburb or quarter within 2 km.
    pub(crate) fn area(&self, lon: f64, lat: f64) -> Option<&str> {
        self.around(lon, lat, 1)
            .filter(|s| s.area)
            .map(|s| (distance_km(lat, lon, s.lat, s.lon), s))
            .filter(|(d, _)| *d <= 2.0)
            .min_by(|(a, _), (b, _)| a.total_cmp(b))
            .map(|(_, s)| s.name.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Address {
    /// Index into the extract's address streets.
    pub street: u32,
    pub number: String,
    pub latitude: f64,
    pub longitude: f64,
}

/// (street or place, city, house number) from an element's tags.
pub(crate) fn from_tags(tags: &Tags<'_>) -> Option<(String, Option<String>, String)> {
    let text = |key: &str| {
        tag(tags, key)
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_owned)
    };
    let number = text("addr:housenumber").filter(|n| n.chars().count() <= MAX_NUMBER_CHARS)?;
    let street = text("addr:street").or_else(|| text("addr:place"))?;
    Some((street, text("addr:city"), number))
}

/// Comparison form of a house number: lower-case letters and digits only
/// ("101-A" and "101 a" both become "101a").
pub(crate) fn number_key(number: &str) -> String {
    number
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// The leading digits of a key, for finding the nearest listed number.
fn leading_number(key: &str) -> Option<i64> {
    let digits: String = key.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// A query word that is a house number: digits, optionally one letter
/// ("101", "101a"), but not an ordinal ("1st", "22nd").
fn is_house_number(key: &str) -> bool {
    let digits = key.chars().take_while(char::is_ascii_digit).count();
    digits > 0 && digits <= 7 && key.len() - digits <= 1
}

const TABLES: &str = "
    CREATE TABLE address_streets (
        id INTEGER PRIMARY KEY, name TEXT NOT NULL, town TEXT NOT NULL, areas TEXT NOT NULL,
        match_name TEXT NOT NULL, lat REAL NOT NULL, lon REAL NOT NULL);
    CREATE VIRTUAL TABLE address_streets_fts USING fts5(
        terms, content = '', prefix = '2 3',
        tokenize = 'unicode61 remove_diacritics 2');
    CREATE TABLE addresses (
        street_id INTEGER NOT NULL, key TEXT NOT NULL, number TEXT NOT NULL,
        num INTEGER NOT NULL, lat INTEGER NOT NULL, lon INTEGER NOT NULL,
        PRIMARY KEY (street_id, key)) WITHOUT ROWID;";

/// Positions are stored in millionths of a degree (about 0.1 m).
fn micro(degrees: f64) -> i64 {
    (degrees * 1e6).round() as i64
}

pub(crate) fn write_tables(
    tx: &Transaction<'_>,
    streets: &[AddressStreet],
    addresses: &[Address],
) -> rusqlite::Result<()> {
    tx.execute_batch(TABLES)?;
    let mut centres = vec![(0.0, 0.0, 0.0); streets.len()];
    {
        let mut insert = tx.prepare(
            "INSERT OR IGNORE INTO addresses (street_id, key, number, num, lat, lon)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )?;
        for a in addresses {
            let key = number_key(&a.number);
            insert.execute(params![
                a.street,
                key,
                a.number,
                leading_number(&key).unwrap_or(-1),
                micro(a.latitude),
                micro(a.longitude),
            ])?;
            let c = &mut centres[a.street as usize];
            *c = (c.0 + a.latitude, c.1 + a.longitude, c.2 + 1.0);
        }
    }
    let mut insert = tx.prepare(
        "INSERT INTO address_streets (id, name, town, areas, match_name, lat, lon)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
    )?;
    let mut terms = tx.prepare("INSERT INTO address_streets_fts (rowid, terms) VALUES (?1, ?2)")?;
    for (id, (street, (lat, lon, n))) in streets.iter().zip(centres).enumerate() {
        let n = f64::max(n, 1.0);
        let areas = street.areas.join("; ");
        insert.execute(params![
            id as i64,
            street.name,
            street.town,
            areas,
            canonical(&street.name),
            lat / n,
            lon / n
        ])?;
        terms.execute(params![
            id as i64,
            format!(
                "{} {} {}",
                expand(&street.name),
                expand(&street.town),
                expand(&areas)
            )
        ])?;
    }
    Ok(())
}

/// Reads the address tables back (for rebuilding an extras file).
pub(crate) fn read_tables(db: &Connection) -> rusqlite::Result<(Vec<AddressStreet>, Vec<Address>)> {
    let streets = db
        .prepare("SELECT name, town, areas FROM address_streets ORDER BY id")?
        .query_map([], |r| {
            let areas: String = r.get(2)?;
            Ok(AddressStreet {
                name: r.get(0)?,
                town: r.get(1)?,
                areas: areas
                    .split("; ")
                    .filter(|a| !a.is_empty())
                    .map(str::to_owned)
                    .collect(),
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let addresses = db
        .prepare("SELECT street_id, number, lat, lon FROM addresses")?
        .query_map([], |r| {
            Ok(Address {
                street: r.get(0)?,
                number: r.get(1)?,
                latitude: r.get::<_, i64>(2)? as f64 / 1e6,
                longitude: r.get::<_, i64>(3)? as f64 / 1e6,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok((streets, addresses))
}

/// Address matches from one extras database, best first: (match strength,
/// distance or number gap, result). `exact` is false when only the nearest
/// listed numbers were found.
pub(crate) struct Found {
    pub(crate) exact: bool,
    pub(crate) matches: Vec<(u8, f64, SearchResult)>,
}

impl Found {
    /// Joins matches from several regions: exact numbers win over nearest
    /// listed ones, then by strength and distance.
    pub(crate) fn join(all: Vec<Found>, limit: usize) -> Vec<SearchResult> {
        let any_exact = all.iter().any(|f| f.exact);
        let mut matches: Vec<_> = all
            .into_iter()
            .filter(|f| f.exact == any_exact)
            .flat_map(|f| f.matches)
            .collect();
        matches.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)));
        matches.into_iter().map(|(_, _, r)| r).take(limit).collect()
    }
}

/// Address results for a query that starts with a house number, or `None`
/// when the query is not shaped like an address. Exact numbers are returned
/// when any street has one; otherwise the nearest listed number on each
/// matching street, marked in `detail`.
pub(crate) fn search_ranked(
    db: &Connection,
    query: &str,
    near: Option<Coordinates>,
) -> Result<Option<Found>, PackageError> {
    let query = query.trim();
    let (first, rest) = query
        .split_once(char::is_whitespace)
        .map_or((query, ""), |(a, b)| (a, b.trim()));
    let key = number_key(first);
    let words: Vec<String> = tokens(rest).into_iter().take(8).collect();
    if !first.starts_with(|c: char| c.is_ascii_digit())
        || !is_house_number(&key)
        || words.is_empty()
    {
        return Ok(None);
    }
    let wanted = leading_number(&key).unwrap_or(0);
    let rest_canonical = canonical(rest);
    let (lat0, lon0) = near.map_or((0.0, 0.0), |c| (c.latitude(), c.longitude()));
    let scale = lat0.to_radians().cos().powi(2);
    let mut streets = db.prepare_cached(
        "SELECT s.id, s.name, s.town, s.match_name
         FROM address_streets_fts f JOIN address_streets s ON s.id = f.rowid
         WHERE address_streets_fts MATCH ?1
         ORDER BY CASE WHEN ?2 THEN (s.lat - ?3) * (s.lat - ?3) + (s.lon - ?4) * (s.lon - ?4) * ?5
                       ELSE s.id END
         LIMIT 2000",
    )?;
    let mut exact_lookup = db.prepare_cached(
        "SELECT number, lat, lon FROM addresses WHERE street_id = ?1 AND key = ?2",
    )?;
    let mut nearest_lookup = db.prepare_cached(
        "SELECT number, lat, lon FROM addresses WHERE street_id = ?1 AND num >= 0
         ORDER BY abs(num - ?2), num LIMIT 1",
    )?;

    type Match = (u8, f64, SearchResult);
    let (mut exact, mut approximate): (Vec<Match>, Vec<Match>) = (Vec::new(), Vec::new());
    let mut rows = streets.query(params![
        match_expression(&words),
        near.is_some(),
        lat0,
        lon0,
        scale
    ])?;
    while let Some(row) = rows.next()? {
        let (id, name, town, match_name): (i64, String, String, String) =
            (row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?);
        // Extras built before 0.8.0 kept accents in `match_name`.
        let match_name = crate::search::fold(&match_name);
        let core = without_compass_words(&match_name);
        let with_town = canonical(&format!("{name} {town}"));
        let strength = u8::from(
            !(match_name.starts_with(&rest_canonical)
                || core.starts_with(&rest_canonical)
                || with_town.starts_with(&rest_canonical)
                || format!("{core} {}", canonical(&town)).starts_with(&rest_canonical)),
        );
        let result = |number: String, lat: i64, lon: i64, detail: Option<String>| {
            let (lat, lon) = (lat as f64 / 1e6, lon as f64 / 1e6);
            SearchResult {
                name: format!("{number} {name}"),
                kind: EntryKind::Address,
                category: "address".into(),
                near: (!town.is_empty()).then(|| town.clone()),
                latitude: lat,
                longitude: lon,
                reference: None,
                distance_km: near.map(|c| distance_km(c.latitude(), c.longitude(), lat, lon)),
                detail,
            }
        };
        if let Some((number, lat, lon)) = exact_lookup
            .query_row(params![id, key], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .optional()?
        {
            let r = result(number, lat, lon, None);
            exact.push((strength, r.distance_km.unwrap_or(0.0), r));
        } else if exact.is_empty()
            && let Some((number, lat, lon)) = nearest_lookup
                .query_row(params![id, wanted], |r| {
                    Ok((r.get::<_, String>(0)?, r.get(1)?, r.get(2)?))
                })
                .optional()?
        {
            let gap = leading_number(&number_key(&number))
                .map_or(f64::MAX, |n| (n - wanted).abs() as f64);
            let detail = Some(format!(
                "{first} is not listed; nearest listed number on this street"
            ));
            let r = result(number, lat, lon, detail);
            let order = r.distance_km.unwrap_or(gap);
            approximate.push((strength, order, r));
        }
    }
    let is_exact = !exact.is_empty();
    let mut matches = if is_exact { exact } else { approximate };
    matches.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)));
    Ok(Some(Found {
        exact: is_exact,
        matches,
    }))
}
