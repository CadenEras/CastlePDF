//! Page tree (ISO 32000-2:2020, 7.7.3) and page attributes.
//!
//! Inheritable attributes (table 31) are validated once, on the node that
//! carries them, and passed down as small `Copy` values; `/Resources` is shared
//! through an `Arc`. A wide tree therefore never copies document content per
//! child or per page. A key whose value is `null`, or a reference to a free or
//! absent object, counts as absent (ISO 32000-2, 7.3.9 and 7.3.10): the
//! attribute is inherited or takes its default.

use std::collections::BTreeSet;
use std::fmt;
use std::sync::Arc;

use pdf_syntax::{Budget, Dictionary, IndirectValue, Object, ObjectId, PdfFile, Source};

use crate::error::{Error, StructureErrorKind};

/// Cancellation is checked every this many kids while a node is expanded.
const CANCEL_CHECK_KIDS: usize = 1024;

/// A rectangle in default user space, normalised so that `llx <= urx` and
/// `lly <= ury` (ISO 32000-2, 7.9.5). Coordinates are finite, but a width or
/// height may overflow to infinity for extreme values: consumers bound page
/// sizes (and apply `/UserUnit`) before allocating anything from them.
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub struct Rectangle {
    pub llx: f64,
    pub lly: f64,
    pub urx: f64,
    pub ury: f64,
}

impl Rectangle {
    /// Rectangle from two opposite corners, in any order.
    pub fn new(x1: f64, y1: f64, x2: f64, y2: f64) -> Self {
        Self {
            llx: x1.min(x2),
            lly: y1.min(y2),
            urx: x1.max(x2),
            ury: y1.max(y2),
        }
    }

    pub fn width(&self) -> f64 {
        self.urx - self.llx
    }

    pub fn height(&self) -> f64 {
        self.ury - self.lly
    }

    fn from_array(items: &[Object]) -> Option<Self> {
        let number = |o: &Object| match o {
            Object::Integer(i) => Some(*i as f64),
            Object::Real(r) if r.is_finite() => Some(*r),
            _ => None,
        };
        let [a, b, c, d] = items else {
            return None;
        };
        Some(Self::new(number(a)?, number(b)?, number(c)?, number(d)?))
    }

    /// Intersection; zero-sized at the overlap edge when disjoint.
    fn intersect(&self, other: &Self) -> Self {
        let llx = self.llx.max(other.llx);
        let lly = self.lly.max(other.lly);
        Self {
            llx,
            lly,
            urx: self.urx.min(other.urx).max(llx),
            ury: self.ury.min(other.ury).max(lly),
        }
    }
}

/// One page with its attributes resolved through inheritance.
#[derive(Clone, PartialEq)]
#[non_exhaustive]
pub struct Page {
    pub id: ObjectId,
    pub media_box: Rectangle,
    /// `/CropBox` (inherited, default `media_box`) intersected with `media_box`.
    pub crop_box: Rectangle,
    /// Size of a default user space unit in 1/72 inch (`/UserUnit`, ISO
    /// 32000-2, 14.11.2.1; not inherited): positive and finite, 1 by default.
    pub user_unit: f64,
    /// Clockwise rotation for display: 0, 90, 180 or 270.
    pub rotation: u16,
    /// `/Resources` as written (dictionary, or reference to one), inherited and
    /// shared between the pages that inherit it. `None` when absent.
    pub resources: Option<Arc<Object>>,
    /// `/Contents` as written (stream reference or array of references).
    pub contents: Option<Object>,
    /// `/Annots` as written (array, or reference to one; not inherited,
    /// 7.7.3.3). Not read further here: annotations are not drawn yet.
    pub annotations: Option<Object>,
}

/// Never prints resources or contents: they hold document data.
impl fmt::Debug for Page {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Page")
            .field("id", &self.id)
            .field("media_box", &self.media_box)
            .field("crop_box", &self.crop_box)
            .field("user_unit", &self.user_unit)
            .field("rotation", &self.rotation)
            .field("resources", &self.resources.is_some())
            .field("contents", &self.contents.is_some())
            .field("annotations", &self.annotations.is_some())
            .finish()
    }
}

/// Inheritable attributes, validated on the node that carries them.
#[derive(Debug, Clone, Default)]
struct Inherited {
    media_box: Option<Rectangle>,
    crop_box: Option<Rectangle>,
    rotation: Option<u16>,
    resources: Option<Arc<Object>>,
}

enum Step {
    /// Visit node `id`, listed by `parent` (None for the root).
    Visit {
        id: ObjectId,
        parent: Option<ObjectId>,
        inherited: Inherited,
        depth: u32,
    },
    /// After the kids of `id`: check its `/Count` against the leaves collected.
    Close {
        id: ObjectId,
        count: usize,
        first_page: usize,
    },
}

fn invalid(object: ObjectId, kind: StructureErrorKind) -> Error {
    Error::InvalidStructure { object, kind }
}

/// Resolves `object` once if it is a reference; a stream or a second
/// reference is refused. A free or absent target resolves to `Null`.
pub(crate) fn direct<S: Source>(
    file: &PdfFile<S>,
    object: &Object,
    owner: ObjectId,
    budget: &mut Budget,
) -> Result<Object, Error> {
    let Object::Reference(id) = object else {
        return Ok(object.clone());
    };
    match file.resolve(*id, budget)? {
        IndirectValue::Object(Object::Reference(_)) | IndirectValue::Stream(_) => {
            Err(invalid(owner, StructureErrorKind::UnexpectedIndirection))
        }
        IndirectValue::Object(object) => Ok(object),
    }
}

/// The value of `key` resolved once, `None` when absent or null.
fn attribute<S: Source>(
    file: &PdfFile<S>,
    node: &Dictionary,
    key: &[u8],
    owner: ObjectId,
    budget: &mut Budget,
) -> Result<Option<Object>, Error> {
    match node.get(key) {
        None | Some(Object::Null) => Ok(None),
        Some(object) => match direct(file, object, owner, budget)? {
            Object::Null => Ok(None),
            object => Ok(Some(object)),
        },
    }
}

/// Own attributes of `node` over those inherited from its ancestors.
fn inherit<S: Source>(
    file: &PdfFile<S>,
    node: &mut Dictionary,
    id: ObjectId,
    parent: &Inherited,
    budget: &mut Budget,
) -> Result<Inherited, Error> {
    let rectangle = |object: Object| {
        match object {
            Object::Array(items) => Rectangle::from_array(&items),
            _ => None,
        }
        .ok_or(invalid(id, StructureErrorKind::InvalidBox))
    };
    let media_box = match attribute(file, node, b"MediaBox", id, budget)? {
        Some(object) => Some(rectangle(object)?),
        None => parent.media_box,
    };
    let crop_box = match attribute(file, node, b"CropBox", id, budget)? {
        Some(object) => Some(rectangle(object)?),
        None => parent.crop_box,
    };
    let rotation = match attribute(file, node, b"Rotate", id, budget)? {
        Some(Object::Integer(degrees)) if degrees % 90 == 0 => Some(
            u16::try_from(degrees.rem_euclid(360))
                .map_err(|_| invalid(id, StructureErrorKind::InvalidRotate))?,
        ),
        Some(_) => return Err(invalid(id, StructureErrorKind::InvalidRotate)),
        None => parent.rotation,
    };
    // Resources stay as written (no copy of a resolved dictionary); a reference
    // is resolved once only to tell a dictionary from null.
    let resources = match node.remove(b"Resources") {
        None | Some(Object::Null) => parent.resources.clone(),
        Some(written) => match direct(file, &written, id, budget)? {
            Object::Null => parent.resources.clone(),
            Object::Dictionary(_) => Some(Arc::new(written)),
            _ => return Err(invalid(id, StructureErrorKind::InvalidResources)),
        },
    };
    Ok(Inherited {
        media_box,
        crop_box,
        rotation,
        resources,
    })
}

/// Walks the page tree from `root` in document order. Iterative, so the depth
/// limit (`max_nesting_depth`) bounds memory, not the call stack; every node is
/// charged one object and may be visited only once.
pub(crate) fn collect_pages<S: Source>(
    file: &PdfFile<S>,
    root: ObjectId,
    budget: &mut Budget,
) -> Result<Vec<Page>, Error> {
    let mut pages = Vec::new();
    let mut visited = BTreeSet::new();
    let mut steps = vec![Step::Visit {
        id: root,
        parent: None,
        inherited: Inherited::default(),
        depth: 1,
    }];
    while let Some(step) = steps.pop() {
        budget.check_cancelled()?;
        let (id, parent, inherited, depth) = match step {
            Step::Close {
                id,
                count,
                first_page,
            } => {
                if pages.len() - first_page != count {
                    return Err(invalid(id, StructureErrorKind::CountMismatch));
                }
                continue;
            }
            Step::Visit {
                id,
                parent,
                inherited,
                depth,
            } => (id, parent, inherited, depth),
        };
        if !visited.insert(id) {
            return Err(invalid(id, StructureErrorKind::RepeatedNode));
        }
        budget.check_depth(depth)?;
        budget.charge_object()?;
        let mut node = match direct(file, &Object::Reference(id), id, budget)? {
            Object::Dictionary(node) => node,
            // A kid that is free or absent: reported on the node listing it.
            Object::Null => {
                return Err(match parent {
                    Some(parent) => invalid(parent, StructureErrorKind::MissingKid),
                    None => invalid(id, StructureErrorKind::InvalidNode),
                });
            }
            _ => return Err(invalid(id, StructureErrorKind::InvalidNode)),
        };
        let listed_parent = match node.get(b"Parent") {
            None | Some(Object::Null) => None,
            Some(object) => Some(
                object
                    .as_reference()
                    .ok_or(invalid(id, StructureErrorKind::InvalidParent))?,
            ),
        };
        if listed_parent != parent {
            return Err(invalid(id, StructureErrorKind::InvalidParent));
        }
        let is_pages = match node.get(b"Type").and_then(Object::as_name) {
            Some(b"Pages") => true,
            Some(b"Page") if parent.is_some() => false,
            _ => return Err(invalid(id, StructureErrorKind::InvalidNode)),
        };
        let inherited = inherit(file, &mut node, id, &inherited, budget)?;
        if is_pages {
            let count = node
                .get(b"Count")
                .and_then(Object::as_integer)
                .and_then(|c| usize::try_from(c).ok())
                .ok_or(invalid(id, StructureErrorKind::CountMismatch))?;
            let Some(Object::Array(kids)) = attribute(file, &node, b"Kids", id, budget)? else {
                return Err(invalid(id, StructureErrorKind::InvalidKids));
            };
            steps.push(Step::Close {
                id,
                count,
                first_page: pages.len(),
            });
            // Reversed, so that the stack pops kids in document order. Each
            // step holds `Copy` values and one `Arc`, never document content.
            for (i, kid) in kids.iter().rev().enumerate() {
                if i % CANCEL_CHECK_KIDS == 0 {
                    budget.check_cancelled()?;
                }
                let kid = kid
                    .as_reference()
                    .ok_or(invalid(id, StructureErrorKind::InvalidKids))?;
                steps.push(Step::Visit {
                    id: kid,
                    parent: Some(id),
                    inherited: inherited.clone(),
                    depth: depth.saturating_add(1),
                });
            }
        } else {
            let media_box = inherited
                .media_box
                .ok_or(invalid(id, StructureErrorKind::InvalidBox))?;
            let crop_box = inherited
                .crop_box
                .map_or(media_box, |crop| crop.intersect(&media_box));
            let user_unit = match attribute(file, &node, b"UserUnit", id, budget)? {
                None => 1.0,
                Some(Object::Integer(units)) if units > 0 => units as f64,
                Some(Object::Real(units)) if units.is_finite() && units > 0.0 => units,
                Some(_) => return Err(invalid(id, StructureErrorKind::InvalidUserUnit)),
            };
            let contents = match node.remove(b"Contents") {
                None | Some(Object::Null) => None,
                Some(contents) => Some(contents),
            };
            let annotations = match node.remove(b"Annots") {
                None | Some(Object::Null) => None,
                Some(annotations) => Some(annotations),
            };
            pages.push(Page {
                id,
                media_box,
                crop_box,
                user_unit,
                rotation: inherited.rotation.unwrap_or(0),
                resources: inherited.resources,
                contents,
                annotations,
            });
        }
    }
    Ok(pages)
}
