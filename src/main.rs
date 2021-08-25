use anyhow::*;
use clang::*;
use itertools::Itertools;
use log::*;
use std::collections::hash_map::DefaultHasher;
use std::collections::BTreeSet;
use std::collections::HashMap;
use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;

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
    // let mut hasher = <RandomState as BuildHasher>::Hasher::new();
    // let mut hasher = DefaultHasher::new();
    let mut hasher = DebugHasher::new(label);
    x.hash(&mut hasher);
    hasher.finish()
}

fn get_type_id<'a>(ty: &Type<'a>) -> String {
    ty.get_canonical_type().get_display_name()
}

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
}

impl std::fmt::Display for Field {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}: {}",
            self.name.as_ref().map(|s| s.as_str()).unwrap_or("_"),
            self.type_id
        )?;
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
    // dependencies: BTreeSet<TypeId>,
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
        // write!(
        //     f,
        //     "{} {}[{}] : {} {{ {} }}",
        //     self.kind,
        //     // self.name,
        //     if self.is_anonymous() { "_" } else { &self.name },
        //     self.size,
        //     self.type_id,
        //     self.fields.iter().format(", ")
        // )?;
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
            if child.get_kind() == EntityKind::UnionDecl
                || child.get_kind() == EntityKind::StructDecl
                || child.get_kind() == EntityKind::EnumDecl
            {
                return Some((child, node.get_name()?));
            }
        }
    }

    // if let Some(type_) = node.get_typedef_underlying_type() {
    //     type_.get_class_type();
    //     if type_.get_kind() == TypeKind::Record {
    //         return Some((node.get_name()?, type_));
    //     }
    // }
    None
}

fn main() -> Result<()> {
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
    // let tu = TranslationUnit::from_ast(&index, &file)
    //     .map_err(|()| anyhow!("Failed to get translation unit"))?;
    let entity = tu.get_entity();
    // println!("{}", hash(&"test"));
    // let mut type_map = HashMap::new();
    let mut struct_lookup = HashMap::<TypeId, RecordInfo>::new();
    let mut targets = HashSet::new();
    entity.visit_children(|node, _parent| {
        // if node.is_definition()
        if let Some((node, name)) = find_record_def(node.get_canonical_entity()) {
            // if node.get_kind() == EntityKind::StructDecl || node.get_kind() == EntityKind::UnionDecl
            {
                debug!("FOUND: {:?}", name);
                // if let Some(type_) = node.get_type() {
                //     type_map.insert(get_type_id(&type_), node);
                // }
                let struct_ = node;
                (|| -> Option<_> {
                    // let name = struct_.get_name()?;
                    let struct_type = struct_.get_type()?;
                    let size = struct_type.get_sizeof().ok()?;
                    // println!("{:?}", name);
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
                                    // println!("{:?}", child);
                                    let name = child.get_name();
                                    // println!("{:?}", name);
                                    let type_ = child.get_type().unwrap();
                                    Field {
                                        offset: name
                                            .as_ref()
                                            .and_then(|name| struct_type.get_offsetof(&name).ok()),
                                        type_id: type_.into(),
                                        underlying: underlying_type(type_).into(),
                                        name,
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
                    // eprintln!("{:?}", new);
                    // let prev = struct_lookup.insert(new.type_id.clone(), new.clone())?;
                    // if prev != new {
                    //     error!("Difference found:\n* {:?}\n* {:?}\n", prev, new);
                    // }
                    // TODO:
                    //
                    //    - ashkan, Wed 25 Aug 2021 10:53:31 PM JST
                    // assert_eq!(prev, new);
                    None::<()>
                })();
            }
        }

        // if let Some(name) = node.get_name() {
        //     if name_filters.contains(&name) {
        //         if node.get_kind() == EntityKind::StructDecl {
        //             let struct_ = node;
        //             targets.insert(struct_);
        //         }
        //     }
        // }

        // if node.get_kind() == EntityKind::TypedefDecl {
        //     let type_ = node.get_type().unwrap();
        //     type_map.insert(get_type_id(&type_), node);
        // }
        // if let Some(name) = node.get_name() {
        //     if name_filters.contains(&name) {
        //         println!("{} {}", name, debug_hash(&node, &name));
        //         // TODO:
        //         //  node.visit_fields()?
        //         //    - ashkan, Wed 25 Aug 2021 08:44:42 PM JST
        //         if node.get_kind() == EntityKind::StructDecl {
        //             let struct_ = node;
        //             targets.push(struct_);
        //             let type_ = struct_.get_type().unwrap();
        //             let size = type_.get_sizeof().unwrap();
        //             println!(
        //                 "struct: {:?} (size: {} bytes)",
        //                 struct_.get_name().unwrap(),
        //                 size
        //             );
        //             for child in node.get_children() {
        //                 let name = child.get_name().unwrap();
        //                 // let ty_ = child.get_type().unwrap();
        //                 // ty_.is_pod();
        //                 let ty_ = child
        //                     .get_typedef_underlying_type()
        //                     .or_else(|| child.get_type())
        //                     .unwrap();
        //                 let offset = type_.get_offsetof(&name).unwrap();
        //                 println!(
        //                     "    field: {:?}: {:?} (offset: {} bits)",
        //                     name,
        //                     ty_.get_canonical_type().get_display_name(),
        //                     offset
        //                 );
        //             }
        //         }
        //     }
        // }
        EntityVisitResult::Recurse
    });
    // let mut resolved = HashMap::new();
    // eprintln!("{:#?}", type_map);

    info!("{}", struct_lookup.values().format(",\n"));
    eprintln!("{}", struct_lookup.values().format(",\n"));

    let mut type_dependencies = BTreeSet::new();
    for target in targets {
        // let struct_type = target.get_type().unwrap();
        let mut visited = HashSet::new();
        let mut discovered = HashSet::new();
        // let mut stack: Vec<TypeId> = vec![target.get_type().unwrap().into()];
        let mut stack: Vec<TypeId> = vec![target];
        discovered.insert(stack[0].clone());
        while let Some(type_id) = stack.pop() {
            // Mark
            assert_eq!(visited.insert(type_id.clone()), true, "{:?}", &type_id);
            // eprintln!("{:?}", type_id);

            // Visit
            let node = &struct_lookup[&type_id];
            type_dependencies.insert(node.type_id.clone());

            // Discover
            for field in node.fields.iter() {
                if struct_lookup.contains_key(&field.underlying) {
                    if discovered.insert(field.underlying.clone()) {
                        stack.push(field.underlying.clone());
                    }
                }
            }

            // let mut visit = |child: Entity<'_>| {
            //     let underlying = underlying_type(child.get_type().unwrap());
            //     println!(
            //         "visiting node {:?} with type {:?} with underlying {:?}",
            //         node.name,
            //         node.type_id,
            //         underlying.get_display_name()
            //     );
            //     if !underlying.is_integer() {
            //         let underlying_type_id = get_type_id(&underlying);
            //         if discovered.insert(underlying_type_id.clone()) {
            //             // if !visited.contains(&underlying_type_id) {
            //             stack.push(underlying_type_id);
            //         }
            //     }
            // };
            // node.visit_children(|child, _parent| {
            //     visit(child);
            //     EntityVisitResult::Recurse
            // });

            // for child in node.get_children() {
            //     let underlying = underlying_type(child.get_type().unwrap());
            //     println!(
            //         "{:?} {:?}",
            //         node.get_type().unwrap().get_display_name(),
            //         underlying.get_display_name()
            //     );
            //     if !underlying.is_integer() {
            //         let underlying_type_id = get_type_id(&underlying);
            //         if !visited.contains(&underlying_type_id) {
            //             stack.push(underlying_type_id);
            //         }
            //     }
            // }
        }
        // for child in target.get_children() {
        //     let name = child.get_name().unwrap();
        //     // let ty_ = child.get_type().unwrap();
        //     // ty_.is_pod();

        //     {
        //         let mut ty_ = child.get_type().unwrap();
        //         while let Some(underlying) = ty_.get_pointee_type() {
        //             ty_ = underlying;
        //         }
        //         let dty = ty_.get_canonical_type();
        //         // if !dty.is_integer() {
        //         type_dependencies.insert(dty.get_display_name());
        //         // }
        //     }
        //     let ty_ = child.get_type().unwrap();
        //     let offset = struct_type.get_offsetof(&name).unwrap();
        //     println!(
        //         "    field: {:?}: {:?} (offset: {} bits)",
        //         name,
        //         ty_.get_canonical_type().get_display_name(),
        //         offset
        //     );
        // }
        // for dep in type_dependencies.iter() {
        //     if d
        //     visited.
        // }
    }
    eprintln!("{:?}", type_dependencies);
    eprintln!();
    for dep in type_dependencies {
        println!("{}", struct_lookup[&dep]);
    }
    Ok(())
}
