use super::*;
use crate::download::tests::{FakeTransport, options};
use crate::osm_pbf::tests::{TestElement as E, write_pbf};
use crate::package::ILLINOIS;
use crate::store::tests::TempDir;

const FEATURES: &[&str] = &["OsmSchema-V0.6", "DenseNodes"];

/// Clinton and Chicago with four stations: a forecourt area plus a point
/// inside it (one station mapped twice), a private pump (excluded), a
/// charging point, an unnamed station and a multipolygon station.
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
            ],
            &[E::Relation(
                40,
                &[(31, "outer")],
                &[
                    ("type", "multipolygon"),
                    ("amenity", "fuel"),
                    ("name", "Loop Fuel"),
                ],
            )],
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
            merged: 1,
            unlocated: 0,
            towns: 2,
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
