//! Adobe's predefined CMaps (ADR-0006, tranche 3.7b): the encodings a Type 0
//! font names (`/Encoding /UniJIS-UCS2-H`), and the UCS2 CMap of each CJK
//! collection, from the CIDs of Adobe-Japan1, -GB1, -CNS1 or -Korea1 to
//! Unicode (ISO 32000-2, 9.10.2).
//!
//! The files are Adobe's own, deflated (`cmaps/README.md`); each is inflated
//! and read by the CMap reader once per process, on first use, then shared.
//! They are trusted data of a known size, read under a budget of their own,
//! never charged to a document: a document only chooses which of these few
//! files is read. Without the `cjk-cmaps` feature none is embedded, and such
//! fonts stay unread, as before.

use std::sync::{Arc, OnceLock};

use pdf_syntax::{Budget, CancellationToken, Dictionary, Object, ResourceLimits};

use crate::text::cmap::CMap;

/// A predefined CMap: its name and its deflated file.
struct Resource {
    name: &'static [u8],
    deflated: &'static [u8],
}

#[cfg(feature = "cjk-cmaps")]
macro_rules! resource {
    ($name:literal) => {
        Resource {
            name: $name.as_bytes(),
            deflated: include_bytes!(concat!("../../cmaps/", $name, ".z")),
        }
    };
}

#[cfg(feature = "cjk-cmaps")]
const ENCODINGS: &[Resource] = &[
    resource!("UniJIS-UCS2-H"),
    resource!("UniJIS-UCS2-V"),
    resource!("UniJIS-UTF16-H"),
    resource!("UniJIS-UTF16-V"),
    resource!("90ms-RKSJ-H"),
    resource!("90ms-RKSJ-V"),
    resource!("UniGB-UCS2-H"),
    resource!("UniGB-UCS2-V"),
    resource!("UniCNS-UCS2-H"),
    resource!("UniCNS-UCS2-V"),
    resource!("UniKS-UCS2-H"),
    resource!("UniKS-UCS2-V"),
];

#[cfg(not(feature = "cjk-cmaps"))]
const ENCODINGS: &[Resource] = &[];

/// The UCS2 CMap of each collection, by `/Ordering`.
#[cfg(feature = "cjk-cmaps")]
const COLLECTIONS: &[(&[u8], Resource)] = &[
    (b"Japan1", resource!("Adobe-Japan1-UCS2")),
    (b"GB1", resource!("Adobe-GB1-UCS2")),
    (b"CNS1", resource!("Adobe-CNS1-UCS2")),
    (b"Korea1", resource!("Adobe-Korea1-UCS2")),
];

#[cfg(not(feature = "cjk-cmaps"))]
const COLLECTIONS: &[(&[u8], Resource)] = &[];

/// Most CMaps followed through `usecmap` among these files (a `-V` CMap
/// uses its `-H` one).
const MAX_DEPTH: usize = 2;

/// Each encoding once read, with its parent merged. A file that cannot be
/// read (a defect of this build, reported by the tests) is not kept.
static READ_ENCODINGS: [OnceLock<Arc<CMap>>; 12] = [const { OnceLock::new() }; 12];
static READ_COLLECTIONS: [OnceLock<Option<Arc<CMap>>>; 4] = [const { OnceLock::new() }; 4];

/// The CMap of a deflated file, inflated and read under a budget of its own.
fn read(resource: &Resource) -> Option<CMap> {
    let mut budget = Budget::new(ResourceLimits::default(), CancellationToken::new());
    let mut filter = Dictionary::new();
    filter.insert(b"Filter".to_vec(), Object::Name(b"FlateDecode".to_vec()));
    let data = pdf_syntax::decode_stream(&filter, resource.deflated, 0, &mut budget).ok()?;
    CMap::parse(&data, &mut budget).ok()
}

/// The encoding at `index`, read once. Its parent is read first, outside
/// the initialisation of this cell, so that no cell is ever entered twice;
/// a chain deeper than `MAX_DEPTH` (or a cycle) gives `None`, never kept.
fn encoding_at(index: usize, depth: usize) -> Option<Arc<CMap>> {
    let slot = READ_ENCODINGS.get(index)?;
    if let Some(cmap) = slot.get() {
        return Some(Arc::clone(cmap));
    }
    if depth > MAX_DEPTH {
        return None;
    }
    let mut cmap = read(ENCODINGS.get(index)?)?;
    if let Some(parent) = cmap.parent() {
        // Only among these files.
        let parent = encoding_at(position(parent)?, depth + 1)?;
        let mut budget = Budget::new(ResourceLimits::default(), CancellationToken::new());
        cmap.inherit(&parent, &mut budget).ok()?;
    }
    // Another thread may have read it meanwhile: either copy is the same.
    Some(Arc::clone(slot.get_or_init(|| Arc::new(cmap))))
}

/// Whether `cmap` is one of the shared predefined encodings, whose tables
/// belong to no document.
pub(crate) fn is_shared(cmap: &Arc<CMap>) -> bool {
    READ_ENCODINGS
        .iter()
        .any(|slot| slot.get().is_some_and(|shared| Arc::ptr_eq(shared, cmap)))
}

fn position(name: &[u8]) -> Option<usize> {
    ENCODINGS.iter().position(|resource| resource.name == name)
}

/// The predefined encoding CMap `name`, with its parent merged, if this
/// build has it. Whether it writes vertically is its `/WMode`.
pub(crate) fn encoding(name: &[u8]) -> Option<Arc<CMap>> {
    encoding_at(position(name)?, 0)
}

/// The UCS2 CMap of collection `ordering` (`Japan1`, `GB1`, `CNS1`,
/// `Korea1`) of registry Adobe, if this build has it: its codes are CIDs,
/// two bytes each.
pub(crate) fn collection(ordering: &[u8]) -> Option<Arc<CMap>> {
    let index = COLLECTIONS.iter().position(|(name, _)| *name == ordering)?;
    let (_, resource) = COLLECTIONS.get(index)?;
    READ_COLLECTIONS
        .get(index)?
        .get_or_init(|| read(resource).map(Arc::new))
        .clone()
}

/// The collection of a predefined encoding, by the prefix of its name.
pub(crate) fn encoding_collection(name: &[u8]) -> Option<&'static [u8]> {
    let prefixes: [(&[u8], &[u8]); 5] = [
        (b"UniJIS-", b"Japan1"),
        (b"90ms-", b"Japan1"),
        (b"UniGB-", b"GB1"),
        (b"UniCNS-", b"CNS1"),
        (b"UniKS-", b"Korea1"),
    ];
    prefixes
        .iter()
        .find(|(prefix, _)| name.starts_with(prefix))
        .map(|(_, ordering)| *ordering)
}

#[cfg(all(test, feature = "cjk-cmaps"))]
mod tests {
    use super::*;
    use crate::text::cmap::Code;

    fn code(value: u32, len: u8) -> Code {
        Code { value, len }
    }

    #[test]
    fn every_embedded_cmap_reads() {
        assert_eq!(READ_ENCODINGS.len(), ENCODINGS.len());
        assert_eq!(READ_COLLECTIONS.len(), COLLECTIONS.len());
        for resource in ENCODINGS {
            let cmap = encoding(resource.name).expect("embedded encoding reads");
            assert!(cmap.has_codespace());
            assert_eq!(cmap.skipped_entries(), 0);
            let vertical = resource.name.ends_with(b"-V");
            assert_eq!(cmap.is_vertical(), vertical);
        }
        for (ordering, _) in COLLECTIONS {
            let cmap = collection(ordering).expect("embedded collection reads");
            assert_eq!(cmap.skipped_entries(), 0);
        }
        assert!(encoding(b"UniJIS-UCS2-HW-H").is_none());
        assert!(collection(b"Japan2").is_none());
    }

    #[test]
    fn shared_tables_are_known_and_a_copy_is_not_shared() {
        let shared = encoding(b"UniJIS-UCS2-H").unwrap();
        assert!(is_shared(&shared));
        assert!(is_shared(&encoding(b"UniJIS-UCS2-V").unwrap()));
        // An embedded CMap over it copies its tables: they are its own.
        let mut budget = Budget::new(ResourceLimits::default(), CancellationToken::new());
        let mut own = CMap::parse(
            b"begincodespacerange <0000> <FFFF> endcodespacerange",
            &mut budget,
        )
        .unwrap();
        own.inherit(&shared, &mut budget).unwrap();
        let own = Arc::new(own);
        assert!(!is_shared(&own));
        assert!(own.tables_bytes() > 100_000, "{}", own.tables_bytes());
        assert_eq!(own.tables_bytes(), shared.tables_bytes());
    }

    #[test]
    fn encodings_give_the_cids_of_their_collection() {
        // Values from Adobe's tables: "A" is CID 34 in Japan1 and GB1, "あ"
        // (U+3042) 843 in Japan1; "中" (U+4E2D) 4559 in GB1 and 661 in
        // CNS1; "한" (U+D55C) 3296 in Korea1.
        let cid = |name: &[u8], value: u32, len: u8| {
            let cmap = encoding(name).unwrap();
            let key = cmap
                .next_code(&value.to_be_bytes()[4 - usize::from(len)..])
                .unwrap();
            assert_eq!(key, code(value, len));
            cmap.cid(key)
        };
        assert_eq!(cid(b"UniJIS-UCS2-H", 0x41, 2), Some(34));
        assert_eq!(cid(b"UniJIS-UCS2-H", 0x3042, 2), Some(843));
        assert_eq!(cid(b"UniJIS-UTF16-H", 0x3042, 2), Some(843));
        // Shift-JIS: one byte for ASCII, two for kana.
        assert_eq!(cid(b"90ms-RKSJ-H", 0x82A0, 2), Some(843));
        assert_eq!(cid(b"90ms-RKSJ-H", 0x41, 1), Some(264));
        assert_eq!(cid(b"UniGB-UCS2-H", 0x4E2D, 2), Some(4559));
        assert_eq!(cid(b"UniGB-UCS2-H", 0x41, 2), Some(34));
        assert_eq!(cid(b"UniCNS-UCS2-H", 0x4E2D, 2), Some(661));
        assert_eq!(cid(b"UniKS-UCS2-H", 0xD55C, 2), Some(3296));
        // A vertical form keeps the horizontal mapping it does not change.
        assert_eq!(cid(b"UniJIS-UCS2-V", 0x3042, 2), Some(843));
    }

    #[test]
    fn collections_give_the_unicode_of_their_cids() {
        let text = |ordering: &[u8], cid: u32| collection(ordering).unwrap().unicode(code(cid, 2));
        assert_eq!(text(b"Japan1", 843).as_deref(), Some("あ"));
        assert_eq!(text(b"Japan1", 34).as_deref(), Some("A"));
        assert_eq!(text(b"GB1", 4559).as_deref(), Some("中"));
        assert_eq!(text(b"CNS1", 661).as_deref(), Some("中"));
        assert_eq!(text(b"Korea1", 3296).as_deref(), Some("한"));
        assert_eq!(encoding_collection(b"UniJIS-UTF16-V"), Some(&b"Japan1"[..]));
        assert_eq!(encoding_collection(b"Identity-H"), None);
    }
}
