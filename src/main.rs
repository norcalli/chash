use anyhow::*;
use clang::*;
use indexmap::IndexMap;
use itertools::Itertools;
use log::*;
use std::collections::hash_map::DefaultHasher;
use std::collections::BTreeSet;
use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;

// NOTE:
//  Can use BTreeMap to order lexicorigraphically instread
//    - ashkan, Sun 29 Aug 2021 03:39:04 AM JST
use IndexMap as StructMap;

#[derive(parse_display::Display, Debug, Hash, PartialEq, Eq, Ord, PartialOrd, Clone)]
struct TypeId(String);

impl Into<TypeId> for Type<'_> {
    fn into(self) -> TypeId {
        TypeId(self.get_canonical_type().get_display_name())
    }
}

fn underlying_type<'a>(mut ty_: Type<'a>) -> Type<'a> {
    while let Some(underlying) = ty_.get_pointee_type() {
        ty_ = underlying;
    }
    ty_.get_canonical_type()
}

#[derive(Debug, Hash, PartialEq, Eq, Ord, PartialOrd, Clone)]
struct Field {
    name: Option<String>,
    type_id: TypeId,
    offset: Option<usize>,
    underlying: TypeId,
    bit_field_width: Option<usize>,
}

impl std::fmt::Display for Field {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(bit_field_width) = self.bit_field_width {
            write!(
                f,
                "{}: {}({})",
                self.name.as_ref().map(|s| s.as_str()).unwrap_or("_"),
                self.type_id,
                bit_field_width,
            )?;
        } else {
            write!(
                f,
                "{}: {}",
                self.name.as_ref().map(|s| s.as_str()).unwrap_or("_"),
                self.type_id
            )?;
        }
        if let Some(offset) = self.offset {
            write!(f, " @ {}", offset)?;
        }
        Ok(())
    }
}

#[derive(parse_display::Display, Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug)]
#[display(style = "snake_case")]
enum RecordKind {
    Struct,
    Union,
    Enum,
}

#[derive(Debug, Hash, PartialEq, Eq, Ord, PartialOrd, Clone)]
struct RecordInfo {
    kind: RecordKind,
    aliases: BTreeSet<String>,
    size: usize,
    type_id: TypeId,
    fields: Vec<Field>,
}

impl std::fmt::Display for RecordInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}[{}] {} {{ {} }}",
            self.type_id,
            self.size,
            self.kind,
            self.fields.iter().format(", ")
        )?;
        Ok(())
    }
}

fn find_record_def<'a>(node: Entity<'a>) -> Option<(Entity<'a>, String)> {
    if node.is_definition() {
        if node.get_kind() == EntityKind::StructDecl
            || node.get_kind() == EntityKind::UnionDecl
            || node.get_kind() == EntityKind::EnumDecl
        {
            let name = node.get_name().unwrap_or_else(|| {
                let type_id: TypeId = node.get_type().unwrap().into();
                type_id.0
            });
            return Some((node, name));
        }
    }
    if node.get_kind() == EntityKind::TypedefDecl {
        for child in node.get_children() {
            if child.is_definition() {
                if child.get_kind() == EntityKind::UnionDecl
                    || child.get_kind() == EntityKind::StructDecl
                    || child.get_kind() == EntityKind::EnumDecl
                {
                    return Some((child, node.get_name()?));
                }
            }
        }
    }
    None
}

fn main() -> Result<()> {
    if std::env::var_os("RUST_LOG").is_none() {
        std::env::set_var("RUST_LOG", "warn");
    }
    env_logger::init();
    let mut it = std::env::args();
    it.next().ok_or_else(|| anyhow!("Need arg"))?;
    let file: PathBuf = it.next().ok_or_else(|| anyhow!("Need arg"))?.parse()?;
    info!("{}", file.display());
    let name_filters: HashSet<_> = it.collect();
    ensure!(!name_filters.is_empty(), "Need a name filter");
    info!("{:?}", name_filters);
    let clang = Clang::new().map_err(|v| anyhow!("{}", v))?;
    let index = Index::new(&clang, false, false);
    let tu = index.parser(file).parse()?;
    let entity = tu.get_entity();
    let mut struct_lookup = StructMap::<TypeId, RecordInfo>::new();
    let mut targets = BTreeSet::new();
    entity.visit_children(|node, _parent| {
        // NOTE this doesn't work with forward declared structs. e.g. monospace_instance
        // if let Some((node, name)) = find_record_def(node.get_canonical_entity()) {
        if let Some((node, name)) = find_record_def(node) {
            debug!("FOUND: {:?}", name);
            (|| -> Option<_> {
                let struct_type = node.get_type()?;
                let size = struct_type.get_sizeof().ok()?;
                let type_id: TypeId = struct_type.into();
                let new = struct_lookup.entry(type_id.clone()).or_insert_with(|| {
                    let new = RecordInfo {
                        kind: match node.get_kind() {
                            EntityKind::StructDecl => RecordKind::Struct,
                            EntityKind::UnionDecl => RecordKind::Union,
                            EntityKind::EnumDecl => RecordKind::Enum,
                            _ => unreachable!(),
                        },
                        size,
                        type_id,
                        aliases: Default::default(),
                        fields: node
                            .get_children()
                            .into_iter()
                            // TODO:
                            //  bit fields child.is_bit_field()
                            //    - ashkan, Wed 25 Aug 2021 10:57:05 PM JST
                            // TODO:
                            //  Edge case with `v2` which doesn't have aliases
                            //    - ashkan, Wed 25 Aug 2021 11:22:56 PM JST
                            .filter(|child| {
                                child.get_kind() == EntityKind::FieldDecl
                                    || child.get_kind() == EntityKind::EnumConstantDecl
                                    || child.get_kind() == EntityKind::UnionDecl
                                    || child.get_kind() == EntityKind::StructDecl
                                // || child.get_name().is_none()
                            })
                            .map(|child| {
                                let name = child.get_name();
                                let type_ = child.get_type().unwrap();
                                Field {
                                    offset: name
                                        .as_ref()
                                        .and_then(|name| struct_type.get_offsetof(&name).ok())
                                        .or_else(|| {
                                            child
                                                .get_enum_constant_value()
                                                .map(|(_s, u)| u as usize)
                                        }),
                                    type_id: type_.into(),
                                    underlying: underlying_type(type_).into(),
                                    name,
                                    bit_field_width: child.get_bit_field_width(),
                                }
                            })
                            .collect(),
                    };
                    new
                });
                if name_filters.contains(&name) {
                    targets.insert(new.type_id.clone());
                }
                new.aliases.insert(name);
                // TODO:
                //
                //    - ashkan, Wed 25 Aug 2021 10:53:31 PM JST
                // assert_eq!(prev, new);
                None::<()>
            })();
        }
        EntityVisitResult::Recurse
    });

    eprintln!("{}", struct_lookup.values().format(",\n"));
    eprintln!();

    let mut toposorted = Vec::new();
    let mut visited = HashSet::new();
    let mut processed = HashSet::new();
    let mut discovered = HashSet::new();
    let mut stack: Vec<TypeId> = vec![];
    for target in targets {
        stack.push(target.clone());
        discovered.insert(target);
    }
    while let Some(type_id) = stack.pop() {
        // Mark
        if visited.insert(type_id.clone()) {
            // Visit
            let node = &struct_lookup[&type_id];
            // type_dependencies.insert(node.type_id.clone());

            stack.push(type_id.clone());

            // Discover
            for field in node.fields.iter() {
                if struct_lookup.contains_key(&field.underlying) {
                    if discovered.insert(field.underlying.clone()) {
                        stack.push(field.underlying.clone());
                    }
                }
            }
        } else if processed.insert(type_id.clone()) {
            toposorted.push(type_id);
        }
    }
    ensure!(!toposorted.is_empty(), "Failed to find any names");
    for dep in toposorted {
        println!("{}", struct_lookup[&dep]);
    }
    Ok(())
}

struct DebugHasher {
    label: String,
    count: u64,
}

impl DebugHasher {
    fn new(label: impl std::fmt::Display) -> Self {
        Self {
            label: label.to_string(),
            count: 0,
        }
    }
}

impl Hasher for DebugHasher {
    fn write(&mut self, bytes: &[u8]) {
        self.count += 1;
        eprintln!("Hash({}) {:?}", self.label, bytes);
    }

    fn finish(&self) -> u64 {
        eprintln!("Hash({}) Finished", self.label);
        self.count
    }
}

fn hash<T: Hash>(x: &T) -> u64 {
    // let mut hasher = <RandomState as BuildHasher>::Hasher::new();
    let mut hasher = DefaultHasher::new();
    x.hash(&mut hasher);
    hasher.finish()
}

fn debug_hash<T: Hash>(x: &T, label: impl std::fmt::Display) -> u64 {
    let mut hasher = DebugHasher::new(label);
    x.hash(&mut hasher);
    hasher.finish()
}
