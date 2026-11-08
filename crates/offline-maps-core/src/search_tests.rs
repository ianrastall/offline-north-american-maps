use super::*;
use crate::mbtiles::tests::{TempFile, build_package};
use crate::mvt::tests::encode_layer;

const LAYERS_JSON: &str =
    r#"{"vector_layers":[{"id":"place_labels"},{"id":"street_labels"},{"id":"pois"}]}"#;

/// z14 tile containing the Chicago Loop, and one ~220 km to the south-west
/// ("Clinton" area for this test).
const CHICAGO: (u32, u32) = (4203, 6089);
const CLINTON: (u32, u32) = (4103, 6189);

fn concat(parts: &[Vec<u8>]) -> Vec<u8> {
    parts.concat()
}

fn test_package() -> (TempFile, TilePackage) {
    let clinton = concat(&[
        encode_layer(
            "place_labels",
            &[
                (
                    &[
                        ("name", "Clinton"),
                        ("kind", "town"),
                        ("population", "#7000"),
                    ],
                    1,
                    &[(2000, 2000)],
                ),
                (
                    &[
                        ("name", "Springfield"),
                        ("kind", "city"),
                        ("population", "#114000"),
                    ],
                    1,
                    &[(100, 100)],
                ),
            ],
        ),
        encode_layer(
            "street_labels",
            &[
                (
                    &[
                        ("name", "North Main Street"),
                        ("kind", "primary"),
                        ("ref", "US 51"),
                    ],
                    2,
                    &[(1900, 1000), (1900, 1500), (1900, 2100)],
                ),
                // Second segment of the same street in the same cell: merged.
                (
                    &[("name", "North Main Street"), ("kind", "primary")],
                    2,
                    &[(1900, 2100), (1900, 2600)],
                ),
                (
                    &[("name", "Oak Street"), ("kind", "residential")],
                    2,
                    &[(2500, 2500), (2600, 2500)],
                ),
            ],
        ),
        encode_layer(
            "pois",
            &[
                (
                    &[("name", "Memorial Hospital"), ("amenity", "hospital")],
                    1,
                    &[(2100, 2100)],
                ),
                // Buffer copy of a neighbouring tile's point: ignored.
                (&[("name", "Clinton"), ("kind", "town")], 1, &[(-20, 30)]),
            ],
        ),
    ]);
    let chicago = concat(&[
        encode_layer(
            "place_labels",
            &[(
                &[
                    ("name", "Chicago"),
                    ("kind", "city"),
                    ("population", "#2700000"),
                ],
                1,
                &[(1000, 1000)],
            )],
        ),
        encode_layer(
            "street_labels",
            &[
                (
                    &[("name", "Main Street"), ("kind", "residential")],
                    2,
                    &[(500, 500), (700, 500)],
                ),
                (
                    &[("name", "Oak Street"), ("kind", "residential")],
                    2,
                    &[(800, 800), (900, 800)],
                ),
            ],
        ),
    ]);
    let row = |y: u32| (1_u32 << 14) - 1 - y;
    let file = build_package(
        &[
            ("format", "pbf"),
            ("minzoom", "14"),
            ("maxzoom", "14"),
            ("bounds", "-91.5,36.9,-87.4,42.5"),
            ("json", LAYERS_JSON),
        ],
        &[
            (14, CLINTON.0, row(CLINTON.1), &clinton),
            (14, CHICAGO.0, row(CHICAGO.1), &chicago),
        ],
    );
    let package = TilePackage::open(&file.0).unwrap();
    (file, package)
}

pub(crate) fn built() -> (TempFile, SearchIndex, IndexStats) {
    let (_package_file, package) = test_package();
    let out = TempFile::new("sqlite");
    let mut calls = 0;
    let stats = build_index(
        &package,
        "abc",
        &out.0,
        &AtomicBool::new(false),
        &mut |_, _| calls += 1,
    )
    .unwrap();
    assert!(calls >= 1);
    let index = SearchIndex::open(&out.0, "abc").unwrap();
    (out, index, stats)
}

fn names(results: &[SearchResult]) -> Vec<&str> {
    results.iter().map(|r| r.name.as_str()).collect()
}

#[test]
fn indexes_and_deduplicates() {
    let (_file, index, stats) = built();
    assert_eq!(
        (stats.places, stats.streets, stats.pois),
        (3, 4, 1),
        "{stats:?}"
    );
    assert_eq!(index.stats().unwrap(), stats);
}

#[test]
fn finds_places_by_name_and_prefix() {
    let (_file, index, _) = built();
    assert_eq!(
        names(&index.search("Springfield", None, 5).unwrap())[0],
        "Springfield"
    );
    assert_eq!(
        names(&index.search("spring", None, 5).unwrap())[0],
        "Springfield"
    );
    assert!(index.search("", None, 5).unwrap().is_empty());
    assert!(index.search("\" OR *", None, 5).unwrap().is_empty());
    assert!(index.search("indianapolis", None, 5).unwrap().is_empty());
}

#[test]
fn matches_abbreviations_refs_and_town_context() {
    let (_file, index, _) = built();
    for query in ["main st clinton", "n main", "north main street", "us 51"] {
        let results = index.search(query, None, 5).unwrap();
        assert_eq!(
            results.first().map(|r| r.name.as_str()),
            Some("North Main Street"),
            "{query}"
        );
        assert_eq!(results[0].near.as_deref(), Some("Clinton"));
        assert_eq!(results[0].kind, EntryKind::Street);
    }
    let hospital = index.search("hosp", None, 5).unwrap();
    assert_eq!(names(&hospital), ["Memorial Hospital"]);
    assert_eq!(hospital[0].category, "hospital");
}

#[test]
fn orders_same_name_results_by_distance() {
    let (_file, index, _) = built();
    let chicago = Coordinates::new(41.88, -87.63).unwrap();
    let results = index.search("oak st", Some(chicago), 5).unwrap();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].near.as_deref(), Some("Chicago"));
    assert!(results[0].distance_km.unwrap() < results[1].distance_km.unwrap());

    let clinton = Coordinates::new(results[1].latitude, results[1].longitude).unwrap();
    let results = index.search("oak st", Some(clinton), 5).unwrap();
    assert_eq!(results[0].near.as_deref(), Some("Clinton"));

    let nearest = index.nearest_place(chicago).unwrap().unwrap();
    assert_eq!(nearest.name, "Chicago");
    assert!(
        index
            .nearest_place(Coordinates::new(10.0, 10.0).unwrap())
            .unwrap()
            .is_none()
    );
}

#[test]
fn rejects_index_of_another_package() {
    let (file, _, _) = built();
    assert!(SearchIndex::open(&file.0, "other").is_err());
}

#[test]
fn compass_words_do_not_block_strong_matches() {
    assert_eq!(without_compass_words("w main st"), "main st");
    assert_eq!(without_compass_words("lake shore dr n"), "lake shore dr");
    assert_eq!(without_compass_words("n"), "n");
    assert_eq!(without_compass_words("ne"), "ne");
    assert_eq!(without_compass_words("main st"), "main st");

    // Near Clinton, "North Main Street" outranks the Main Street ~220 km away.
    let (_file, index, _) = built();
    let clinton = index.search("clinton", None, 1).unwrap().remove(0);
    let near = Coordinates::new(clinton.latitude, clinton.longitude).unwrap();
    let results = index.search("main st", Some(near), 5).unwrap();
    assert_eq!(names(&results)[..2], ["North Main Street", "Main Street"]);
}
