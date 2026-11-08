use super::*;
use crate::download::tests::{FakeTransport, options};
use crate::osm_pbf::tests::{TestElement as E, write_pbf};
use crate::package::ILLINOIS;
use crate::store::tests::TempDir;

const FEATURES: &[&str] = &["OsmSchema-V0.6", "DenseNodes"];

/// Clinton and Chicago with four stations: a forecourt area plus a point
/// inside it (one station mapped twice), a private pump (excluded), a
/// charging point, an unnamed station and a multipolygon station. Addresses:
/// a point (and a repeat of it), a building without a city and a multipolygon
/// in the Lincoln Park suburb.
fn sample(with_towns: bool, with_charging: bool) -> Vec<u8> {
    let mut first = vec![
        E::Node(1, 40.1500, -88.9600, &[]),
        E::Node(2, 40.1500, -88.9590, &[]),
        E::Node(3, 40.1506, -88.9590, &[]),
        E::Node(4, 40.1506, -88.9600, &[]),
        E::Node(5, 41.8800, -87.6300, &[]),
        E::Node(6, 41.8800, -87.6290, &[]),
        E::Node(7, 41.8806, -87.6290, &[]),
        E::Node(
            20,
            40.1503,
            -88.9595,
            &[
                ("amenity", "fuel"),
                ("brand", "Shell"),
                ("opening_hours", "24/7"),
            ],
        ),
        E::Node(
            21,
            40.16,
            -88.97,
            &[
                ("amenity", "fuel"),
                ("access", "private"),
                ("name", "County garage"),
            ],
        ),
        E::Node(23, 40.14, -88.95, &[("amenity", "fuel")]),
        E::Node(12, 40.1520, -88.9640, &[]),
        E::Node(13, 40.1520, -88.9638, &[]),
        E::Node(14, 40.1522, -88.9638, &[]),
        E::Node(15, 40.1522, -88.9640, &[]),
        E::Node(
            24,
            40.1510,
            -88.9641,
            &[
                ("addr:housenumber", "101"),
                ("addr:street", "North Main Street"),
                ("addr:city", "Clinton"),
            ],
        ),
        E::Node(
            25,
            40.1511,
            -88.9641,
            &[
                ("addr:housenumber", "101"),
                ("addr:street", "North Main Street"),
                ("addr:city", "Clinton"),
            ],
        ),
    ];
    if with_towns {
        first.push(E::Node(
            10,
            40.153,
            -88.964,
            &[("place", "town"), ("name", "Clinton")],
        ));
        first.push(E::Node(
            11,
            41.878,
            -87.630,
            &[("place", "city"), ("name", "Chicago")],
        ));
        first.push(E::Node(
            26,
            41.881,
            -87.629,
            &[("place", "suburb"), ("name", "Lincoln Park")],
        ));
    }
    if with_charging {
        first.push(E::Node(
            22,
            41.879,
            -87.631,
            &[("amenity", "charging_station"), ("operator", "ChargePoint")],
        ));
    }
    write_pbf(
        &[
            &first,
            &[
                E::Way(
                    30,
                    &[1, 2, 3, 4, 1],
                    &[
                        ("amenity", "fuel"),
                        ("name", "Shell"),
                        ("brand", "Shell"),
                        ("fuel:diesel", "yes"),
                        ("fuel:octane_87", "yes"),
                        ("addr:housenumber", "101"),
                        ("addr:street", "North Main Street"),
                        ("addr:city", "Clinton"),
                    ],
                ),
                E::Way(31, &[5, 6, 7, 5], &[]),
                E::Way(
                    32,
                    &[12, 13, 14, 15, 12],
                    &[
                        ("building", "house"),
                        ("addr:housenumber", "105"),
                        ("addr:street", "North Main Street"),
                    ],
                ),
            ],
            &[
                E::Relation(
                    40,
                    &[(31, "outer")],
                    &[
                        ("type", "multipolygon"),
                        ("amenity", "fuel"),
                        ("name", "Loop Fuel"),
                    ],
                ),
                E::Relation(
                    41,
                    &[(31, "outer")],
                    &[
                        ("type", "multipolygon"),
                        ("building", "yes"),
                        ("addr:housenumber", "1600"),
                        ("addr:street", "North Clark Street"),
                    ],
                ),
            ],
        ],
        FEATURES,
    )
}

fn extract_sample(dir: &TempDir) -> Extract {
    let path = dir.0.join("sample.osm.pbf");
    fs::write(&path, sample(true, true)).unwrap();
    let mut steps = Vec::new();
    let extract = extract(&path, &AtomicBool::new(false), &mut |step, _, _| {
        if steps.last() != Some(&step) {
            steps.push(step);
        }
    })
    .unwrap();
    assert_eq!(steps, [1, 2, 3]);
    extract
}

#[test]
fn extracts_points_areas_and_multipolygons() {
    let dir = TempDir::new();
    let extract = extract_sample(&dir);
    assert_eq!(
        extract.stats,
        ExtractStats {
            fuel: 3,
            charging: 1,
            shelters: 0,
            drinking_water: 0,
            campgrounds: 0,
            toilets: 0,
            merged: 1,
            unlocated: 0,
            towns: 3,
            addresses: 3,
            address_streets: 2,
            // 101 is mapped on two points and on the Shell forecourt.
            addresses_dropped: 2,
        }
    );
    let ids: Vec<&str> = extract.stations.iter().map(|s| s.osm_id.as_str()).collect();
    assert_eq!(ids, ["node/22", "node/23", "relation/40", "way/30"]);

    let shell = &extract.stations[3];
    assert!((shell.latitude - 40.1503).abs() < 1e-6 && (shell.longitude + 88.9595).abs() < 1e-6);
    // Opening hours came from the point copy; octane grades are not listed.
    assert_eq!(
        shell.detail().as_deref(),
        Some("open 24/7 · diesel · 101 North Main Street, Clinton")
    );
    let charging = &extract.stations[0];
    assert_eq!(
        (charging.name.as_str(), charging.category.as_str()),
        ("ChargePoint", "charging_station")
    );
    assert_eq!(extract.stations[1].display_name(), "Fuel station");
    let lp = &extract.stations[2];
    assert!((lp.latitude - 41.8802).abs() < 1e-6 && (lp.longitude + 87.629_333_3).abs() < 1e-6);
}

#[test]
fn stations_are_searchable_beside_the_package() {
    let dir = TempDir::new();
    let extract = extract_sample(&dir);
    let path = dir.0.join(EXTRAS_FILE);
    let stats = write_extras(&extract, "sha", &path).unwrap();
    assert_eq!(stats.pois, 4);

    let (_file, mut index, _) = crate::search::tests::built();
    let near = crate::Coordinates::new(40.15, -88.96);
    assert!(index.search("gas", near, 5).unwrap().is_empty());
    index.attach_extras(&path).unwrap();

    let gas = index.search("gas station", near, 5).unwrap();
    assert_eq!(gas[0].name, "Shell");
    assert_eq!(gas[0].near.as_deref(), Some("Clinton"));
    assert_eq!(gas[0].category, "fuel");
    assert!(gas[0].detail.as_deref().unwrap().contains("open 24/7"));
    assert_eq!(gas[1].name, "Fuel station");

    let charger = index.search("ev charger", None, 5).unwrap();
    assert_eq!(charger[0].name, "ChargePoint");
    assert_eq!(charger[0].near.as_deref(), Some("Chicago"));
    // The package's own entries are still found.
    assert_eq!(
        index.search("springfield", None, 1).unwrap()[0].name,
        "Springfield"
    );
}

#[test]
fn rebuilds_entries_after_a_search_format_change() {
    let dir = TempDir::new();
    let extract = extract_sample(&dir);
    let path = dir.0.join(EXTRAS_FILE);
    write_extras(&extract, "sha", &path).unwrap();
    {
        let db = Connection::open(&path).unwrap();
        db.execute(
            "UPDATE meta SET value = '1' WHERE key = 'search_schema'",
            [],
        )
        .unwrap();
        db.execute("DELETE FROM entries", []).unwrap();
    }
    let db = open_entries(&path).unwrap();
    let count: i64 = db
        .query_row("SELECT count(*) FROM entries", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 4);
    let addresses: i64 = db
        .query_row("SELECT count(*) FROM addresses", [], |r| r.get(0))
        .unwrap();
    assert_eq!(addresses, 3);
    assert_eq!(
        meta(&db, "search_schema").unwrap(),
        SEARCH_SCHEMA.to_string()
    );
    assert_eq!(meta(&db, "source_sha256").unwrap(), "sha");
}

#[test]
fn stations_as_geojson() {
    let dir = TempDir::new();
    let path = dir.0.join(EXTRAS_FILE);
    write_extras(&extract_sample(&dir), "sha", &path).unwrap();
    let json: serde_json::Value = serde_json::from_str(&stations_geojson(&path).unwrap()).unwrap();
    let features = json["features"].as_array().unwrap();
    assert_eq!(features.len(), 4);
    let shell = features
        .iter()
        .find(|f| f["properties"]["name"] == "Shell")
        .unwrap();
    assert_eq!(shell["properties"]["category"], "fuel");
    assert_eq!(shell["geometry"]["coordinates"][1], 40.1503);
}

#[test]
fn update_installs_replaces_and_keeps_only_stations() {
    let dir = TempDir::new();
    let store = PackageStore::new(&dir.0);
    let first = sample(true, true);
    let transport = FakeTransport::new(first.clone(), "\"x1\"");
    let cancelled = AtomicBool::new(false);
    let mut stages = Vec::new();
    let mut progress = |p: Progress| {
        let stage = std::mem::discriminant(&p);
        if stages.last() != Some(&stage) {
            stages.push(stage);
        }
    };
    let run = |progress: &mut dyn FnMut(Progress)| {
        update_extras(
            &store,
            &ILLINOIS,
            &transport,
            &options(),
            progress,
            &cancelled,
        )
    };

    let ExtrasOutcome::Activated {
        extras, replaced, ..
    } = run(&mut progress).unwrap()
    else {
        panic!("expected activation");
    };
    assert_eq!(replaced, None);
    assert_eq!(extras.record.stats.fuel, 3);
    assert_eq!(extras.record.byte_length, first.len() as u64);
    assert_eq!(extras.record.source.etag.as_deref(), Some("\"x1\""));
    assert_eq!(
        stages.len(),
        5,
        "checking, downloading, verifying, indexing, activating"
    );
    // The raw extract is not kept.
    let id = extras_id(ILLINOIS.id);
    assert!(!store.staging_file(&id).exists());
    assert_eq!(active_extras(&store, ILLINOIS.id).unwrap().unwrap(), extras);

    assert!(matches!(
        run(&mut |_| {}).unwrap(),
        ExtrasOutcome::UpToDate(_)
    ));
    // An extras file in an older format is rebuilt from the same source.
    Connection::open(&extras.path)
        .unwrap()
        .execute(
            "UPDATE meta SET value = '1' WHERE key = 'extras_schema'",
            [],
        )
        .unwrap();
    let ExtrasOutcome::Activated {
        extras, replaced, ..
    } = run(&mut |_| {}).unwrap()
    else {
        panic!("expected a rebuild");
    };
    assert!(replaced.is_some());

    transport.serve(sample(true, false), "\"x2\"");
    let ExtrasOutcome::Activated {
        extras: second,
        replaced,
        prune_pending,
    } = run(&mut |_| {}).unwrap()
    else {
        panic!("expected replacement");
    };
    assert_eq!(replaced.as_deref(), Some(extras.version.as_str()));
    assert_eq!((second.record.stats.charging, prune_pending), (0, 0));
    assert!(!store.version_dir(&id, &extras.version).exists());

    // A file without towns is not this region's extract; nothing changes.
    transport.serve(sample(false, true), "\"x3\"");
    assert!(matches!(run(&mut |_| {}), Err(UpdateError::Invalid(_))));
    // Neither is a damaged file.
    transport.serve(b"not an extract".to_vec(), "\"x4\"");
    assert!(matches!(run(&mut |_| {}), Err(UpdateError::Invalid(_))));
    assert!(!store.staging_file(&id).exists());
    assert_eq!(active_extras(&store, ILLINOIS.id).unwrap().unwrap(), second);
}

#[test]
fn extracts_addresses_with_towns_and_areas() {
    let dir = TempDir::new();
    let extract = extract_sample(&dir);
    let streets: Vec<(&str, &str, Vec<String>)> = extract
        .address_streets
        .iter()
        .map(|s| (s.name.as_str(), s.town.as_str(), s.areas.clone()))
        .collect();
    assert_eq!(
        streets,
        [
            (
                "North Clark Street",
                "Chicago",
                vec!["Lincoln Park".to_owned()]
            ),
            ("North Main Street", "Clinton", vec![]),
        ]
    );
    let find = |number: &str| {
        extract
            .addresses
            .iter()
            .find(|a| a.number == number)
            .map(|a| (a.street, a.latitude, a.longitude))
            .unwrap()
    };
    // Of the repeated 101s, a point is preferred over the forecourt area,
    // then the southernmost.
    let (street, lat, _) = find("101");
    assert_eq!((street, lat), (1, 40.151));
    // A building is placed between its first node and the one opposite.
    let (street, lat, lon) = find("105");
    assert_eq!(street, 1);
    assert!((lat - 40.1521).abs() < 1e-6 && (lon + 88.9639).abs() < 1e-6);
    let (street, lat, lon) = find("1600");
    assert_eq!(street, 0);
    assert!((lat - 41.8803).abs() < 1e-6 && (lon + 87.6295).abs() < 1e-6);
}

#[test]
fn finds_house_numbers_and_nearest_listed_numbers() {
    let dir = TempDir::new();
    let path = dir.0.join(EXTRAS_FILE);
    write_extras(&extract_sample(&dir), "sha", &path).unwrap();
    let (_file, mut index, _) = crate::search::tests::built();
    index.attach_extras(&path).unwrap();
    let search = |q: &str| index.search(q, None, 5).unwrap();

    let exact = search("101 n main st clinton");
    assert_eq!(exact[0].name, "101 North Main Street");
    assert_eq!(exact[0].kind, crate::search::EntryKind::Address);
    assert_eq!(exact[0].near.as_deref(), Some("Clinton"));
    assert_eq!(exact[0].detail, None);
    assert_eq!(search("105 North Main")[0].name, "105 North Main Street");

    // 103 is not mapped: 101 and 105 are equally near; the lower is offered.
    let near = search("103 main street");
    assert_eq!(near[0].name, "101 North Main Street");
    assert!(
        near[0]
            .detail
            .as_deref()
            .unwrap()
            .starts_with("103 is not listed")
    );

    // Suburbs are searchable as well as the town.
    assert_eq!(
        search("1600 clark lincoln park")[0].name,
        "1600 North Clark Street"
    );
    assert_eq!(
        search("1600 clark chicago")[0].near.as_deref(),
        Some("Chicago")
    );

    // Not address-shaped: ordinals and lone numbers use ordinary search.
    for q in ["1st street", "101", "main 101"] {
        assert!(
            search(q)
                .iter()
                .all(|r| r.kind != crate::search::EntryKind::Address),
            "{q}"
        );
    }
}

#[test]
fn house_number_forms() {
    use crate::addresses::number_key;
    assert_eq!(number_key("101-A"), "101a");
    assert_eq!(number_key(" 101 a"), "101a");
    let dir = TempDir::new();
    let path = dir.0.join(EXTRAS_FILE);
    write_extras(&extract_sample(&dir), "sha", &path).unwrap();
    let (_file, mut index, _) = crate::search::tests::built();
    index.attach_extras(&path).unwrap();
    // "101a" is not mapped; the nearest listed number is 101.
    let r = index.search("101a main", None, 1).unwrap();
    assert_eq!(r[0].name, "101 North Main Street");
    assert!(r[0].detail.is_some());
}

#[test]
fn outdoor_places_are_classified() {
    let category = |pairs: &[(&str, &str)]| {
        let tags: Vec<(&str, &str)> = pairs.to_vec();
        point_category(&tags)
    };
    assert_eq!(category(&[("amenity", "shelter")]), Some("shelter"));
    assert_eq!(
        category(&[("amenity", "shelter"), ("shelter_type", "public_transport")]),
        None
    );
    assert_eq!(
        category(&[("amenity", "shelter"), ("highway", "bus_stop")]),
        None
    );
    assert_eq!(category(&[("tourism", "wilderness_hut")]), Some("shelter"));
    assert_eq!(
        category(&[("amenity", "drinking_water")]),
        Some("drinking_water")
    );
    assert_eq!(
        category(&[("amenity", "water_point"), ("drinking_water", "no")]),
        None
    );
    assert_eq!(
        category(&[("natural", "spring"), ("drinking_water", "yes")]),
        Some("drinking_water")
    );
    assert_eq!(category(&[("natural", "spring")]), None);
    assert_eq!(category(&[("tourism", "camp_site")]), Some("camp_site"));
    assert_eq!(category(&[("tourism", "caravan_site")]), Some("camp_site"));
    assert_eq!(category(&[("amenity", "toilets")]), Some("toilets"));
    assert_eq!(category(&[("amenity", "fuel")]), Some("fuel"));
    assert_eq!(category(&[("amenity", "bench")]), None);
}

#[test]
fn outdoor_places_are_extracted_and_named() {
    let dir = TempDir::new();
    let path = dir.0.join("outdoor.osm.pbf");
    fs::write(
        &path,
        write_pbf(
            &[&[
                E::Node(
                    1,
                    37.60,
                    -88.38,
                    &[("amenity", "shelter"), ("shelter_type", "lean_to")],
                ),
                E::Node(2, 37.61, -88.38, &[("amenity", "drinking_water")]),
                E::Node(
                    3,
                    37.62,
                    -88.38,
                    &[("tourism", "camp_site"), ("name", "Pharaoh Campground")],
                ),
                E::Node(
                    4,
                    37.63,
                    -88.38,
                    &[("amenity", "toilets"), ("access", "customers")],
                ),
                E::Node(5, 37.64, -88.38, &[("amenity", "toilets")]),
                E::Node(
                    6,
                    37.65,
                    -88.38,
                    &[("amenity", "shelter"), ("shelter_type", "public_transport")],
                ),
            ]],
            FEATURES,
        ),
    )
    .unwrap();
    let extract = extract(&path, &AtomicBool::new(false), &mut |_, _, _| {}).unwrap();
    assert_eq!(
        (
            extract.stats.shelters,
            extract.stats.drinking_water,
            extract.stats.campgrounds,
            extract.stats.toilets
        ),
        (1, 1, 1, 1)
    );
    let names: Vec<String> = extract.stations.iter().map(Station::display_name).collect();
    assert_eq!(
        names,
        ["Shelter", "Drinking water", "Pharaoh Campground", "Toilets"]
    );
    assert!(extract.stations.iter().all(|s| s.fuels.is_empty()));
}
