//! Saved places: user-created data, stored apart from map packages.
//!
//! The file lives at the store root (`saved-places.json`), outside every
//! region and version directory, so package updates, pruning and region
//! removal never touch it. Each change is written atomically. A file that
//! cannot be read is reported and left untouched; it is never replaced
//! without the caller deciding to move it aside.
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::Coordinates;
use crate::mbtiles::Bounds;
use crate::store::write_atomic;

pub const PLACES_FILE: &str = "saved-places.json";
pub const PLACES_SCHEMA: u32 = 1;
pub const MAX_PLACES: usize = 10_000;
pub const MAX_NAME_CHARS: usize = 120;
pub const MAX_NOTE_CHARS: usize = 2_000;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SavedPlace {
    pub id: u64,
    pub name: String,
    pub note: String,
    pub latitude: f64,
    pub longitude: f64,
    pub created_at: String,
    pub updated_at: String,
}

impl SavedPlace {
    pub fn coordinates(&self) -> Option<Coordinates> {
        Coordinates::new(self.latitude, self.longitude)
    }

    /// Whether the map package covering `bounds` shows this place.
    pub fn is_covered_by(&self, bounds: &Bounds) -> bool {
        (bounds.west..=bounds.east).contains(&self.longitude)
            && (bounds.south..=bounds.north).contains(&self.latitude)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct PlacesFile {
    schema: u32,
    next_id: u64,
    places: Vec<SavedPlace>,
}

impl Default for PlacesFile {
    fn default() -> Self {
        Self {
            schema: PLACES_SCHEMA,
            next_id: 1,
            places: Vec::new(),
        }
    }
}

#[derive(Debug)]
pub enum PlacesError {
    Io(io::Error),
    /// The file exists but cannot be read as saved places. It was not changed.
    Unreadable(String),
    Invalid(String),
    NotFound(u64),
    Full,
}

impl std::fmt::Display for PlacesError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "Saved places could not be written: {e}"),
            Self::Unreadable(e) => write!(
                f,
                "Saved places file cannot be read ({e}). It has not been changed."
            ),
            Self::Invalid(e) => write!(f, "{e}"),
            Self::NotFound(id) => write!(f, "Saved place {id} no longer exists."),
            Self::Full => write!(f, "No more than {MAX_PLACES} places can be saved."),
        }
    }
}

impl std::error::Error for PlacesError {}

impl From<io::Error> for PlacesError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

pub struct SavedPlaces {
    path: PathBuf,
    data: PlacesFile,
}

fn clean_name(name: &str) -> Result<String, PlacesError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(PlacesError::Invalid("A saved place needs a name.".into()));
    }
    if name.chars().count() > MAX_NAME_CHARS {
        return Err(PlacesError::Invalid(format!(
            "Names are limited to {MAX_NAME_CHARS} characters."
        )));
    }
    if name.chars().any(char::is_control) {
        return Err(PlacesError::Invalid(
            "Names cannot contain control characters.".into(),
        ));
    }
    Ok(name.to_owned())
}

fn clean_note(note: &str) -> Result<String, PlacesError> {
    let note = note.trim();
    if note.chars().count() > MAX_NOTE_CHARS {
        return Err(PlacesError::Invalid(format!(
            "Notes are limited to {MAX_NOTE_CHARS} characters."
        )));
    }
    Ok(note.to_owned())
}

impl SavedPlaces {
    /// Opens `<store root>/saved-places.json`. A missing file is an empty list.
    pub fn open(store_root: &Path) -> Result<Self, PlacesError> {
        let path = store_root.join(PLACES_FILE);
        let data = match fs::read_to_string(&path) {
            Ok(text) => {
                let data: PlacesFile = serde_json::from_str(&text)
                    .map_err(|e| PlacesError::Unreadable(e.to_string()))?;
                if data.schema != PLACES_SCHEMA {
                    return Err(PlacesError::Unreadable(format!(
                        "schema {} is not supported",
                        data.schema
                    )));
                }
                if data
                    .places
                    .iter()
                    .any(|p| p.coordinates().is_none() || p.id >= data.next_id)
                {
                    return Err(PlacesError::Unreadable("contains invalid entries".into()));
                }
                data
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => PlacesFile::default(),
            Err(error) => return Err(error.into()),
        };
        Ok(Self { path, data })
    }

    /// Renames an unreadable file aside (`saved-places.unreadable-<time>.json`)
    /// so a new list can be started without losing it. Returns the new path.
    pub fn move_aside_unreadable(store_root: &Path) -> io::Result<PathBuf> {
        let stamp: String = crate::time::utc_now()
            .chars()
            .filter(char::is_ascii_alphanumeric)
            .collect();
        let target = store_root.join(format!("saved-places.unreadable-{stamp}.json"));
        fs::rename(store_root.join(PLACES_FILE), &target)?;
        Ok(target)
    }

    pub fn list(&self) -> &[SavedPlace] {
        &self.data.places
    }

    pub fn get(&self, id: u64) -> Option<&SavedPlace> {
        self.data.places.iter().find(|p| p.id == id)
    }

    /// Case-insensitive match on name or note.
    pub fn find(&self, query: &str) -> Vec<&SavedPlace> {
        let query = query.trim().to_lowercase();
        self.data
            .places
            .iter()
            .filter(|p| {
                !query.is_empty()
                    && (p.name.to_lowercase().contains(&query)
                        || p.note.to_lowercase().contains(&query))
            })
            .collect()
    }

    pub fn add(
        &mut self,
        name: &str,
        note: &str,
        at: Coordinates,
    ) -> Result<SavedPlace, PlacesError> {
        if self.data.places.len() >= MAX_PLACES {
            return Err(PlacesError::Full);
        }
        let now = crate::time::utc_now();
        let place = SavedPlace {
            id: self.data.next_id,
            name: clean_name(name)?,
            note: clean_note(note)?,
            latitude: at.latitude(),
            longitude: at.longitude(),
            created_at: now.clone(),
            updated_at: now,
        };
        let mut next = self.data.clone();
        next.next_id += 1;
        next.places.push(place.clone());
        self.commit(next)?;
        Ok(place)
    }

    pub fn update(&mut self, id: u64, name: &str, note: &str) -> Result<(), PlacesError> {
        let (name, note) = (clean_name(name)?, clean_note(note)?);
        let mut next = self.data.clone();
        let place = next
            .places
            .iter_mut()
            .find(|p| p.id == id)
            .ok_or(PlacesError::NotFound(id))?;
        place.name = name;
        place.note = note;
        place.updated_at = crate::time::utc_now();
        self.commit(next)
    }

    pub fn remove(&mut self, id: u64) -> Result<(), PlacesError> {
        let mut next = self.data.clone();
        let before = next.places.len();
        next.places.retain(|p| p.id != id);
        if next.places.len() == before {
            return Err(PlacesError::NotFound(id));
        }
        self.commit(next)
    }

    /// Writes `next` to disk, then adopts it; a failed write leaves both the
    /// file and this list as they were.
    fn commit(&mut self, next: PlacesFile) -> Result<(), PlacesError> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_vec_pretty(&next).map_err(io::Error::other)?;
        write_atomic(&self.path, &json)?;
        self.data = next;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::tests::TempDir;

    fn at(lat: f64, lon: f64) -> Coordinates {
        Coordinates::new(lat, lon).unwrap()
    }

    #[test]
    fn add_update_remove_and_persist() {
        let dir = TempDir::new();
        let mut places = SavedPlaces::open(&dir.0).unwrap();
        assert!(places.list().is_empty());
        let home = places
            .add("  Home ", "Back door key with neighbour", at(40.15, -88.96))
            .unwrap();
        let shelter = places.add("Shelter", "", at(37.73, -89.22)).unwrap();
        assert_eq!((home.id, shelter.id, home.name.as_str()), (1, 2, "Home"));

        places
            .update(shelter.id, "County shelter", "Bring ID")
            .unwrap();
        places.remove(home.id).unwrap();
        assert!(matches!(
            places.remove(home.id),
            Err(PlacesError::NotFound(1))
        ));

        let reopened = SavedPlaces::open(&dir.0).unwrap();
        assert_eq!(reopened.list().len(), 1);
        assert_eq!(reopened.get(2).unwrap().name, "County shelter");
        assert_eq!(reopened.find("id").len(), 1);
        assert!(reopened.find("").is_empty());
        // Ids are not reused after removal.
        let mut reopened = reopened;
        assert_eq!(reopened.add("Work", "", at(41.0, -88.0)).unwrap().id, 3);
        assert!(!dir.0.join("saved-places.json.tmp").exists());
    }

    #[test]
    fn validates_input() {
        let dir = TempDir::new();
        let mut places = SavedPlaces::open(&dir.0).unwrap();
        for name in ["", "   ", "tab\there", &"x".repeat(121)] {
            assert!(
                matches!(
                    places.add(name, "", at(40.0, -89.0)),
                    Err(PlacesError::Invalid(_))
                ),
                "{name:?}"
            );
        }
        assert!(
            places
                .add("Ok", &"n".repeat(2001), at(40.0, -89.0))
                .is_err()
        );
        assert!(places.list().is_empty());
        assert!(!dir.0.join(PLACES_FILE).exists());
    }

    #[test]
    fn unreadable_file_is_reported_and_preserved() {
        let dir = TempDir::new();
        fs::write(dir.0.join(PLACES_FILE), b"{ not json").unwrap();
        assert!(matches!(
            SavedPlaces::open(&dir.0),
            Err(PlacesError::Unreadable(_))
        ));
        assert_eq!(fs::read(dir.0.join(PLACES_FILE)).unwrap(), b"{ not json");

        let aside = SavedPlaces::move_aside_unreadable(&dir.0).unwrap();
        assert_eq!(fs::read(&aside).unwrap(), b"{ not json");
        assert!(SavedPlaces::open(&dir.0).unwrap().list().is_empty());

        fs::write(
            dir.0.join(PLACES_FILE),
            br#"{"schema":1,"next_id":1,"places":[{"id":5,"name":"x","note":"","latitude":91,"longitude":0,"created_at":"","updated_at":""}]}"#,
        )
        .unwrap();
        assert!(matches!(
            SavedPlaces::open(&dir.0),
            Err(PlacesError::Unreadable(_))
        ));
    }

    #[test]
    fn coverage_against_package_bounds() {
        let dir = TempDir::new();
        let mut places = SavedPlaces::open(&dir.0).unwrap();
        let illinois = Bounds::new(-91.51608, 36.96731, -87.49264, 42.50941).unwrap();
        let inside = places.add("Clinton", "", at(40.15, -88.96)).unwrap();
        let outside = places.add("Indianapolis", "", at(39.77, -86.16)).unwrap();
        assert!(inside.is_covered_by(&illinois));
        assert!(!outside.is_covered_by(&illinois));
    }
}
