//! The Newznab categories, and what a `cat` parameter asks for.

pub const UNCATEGORIZED: i64 = 8000; // read-time only, never stored; what no_match decides
pub const MISC: i64 = 8010;
pub const HASHED: i64 = 8020;

pub struct Sub {
    pub id: i64,
    pub name: &'static str,
}

pub struct Root {
    pub id: i64,
    pub name: &'static str,
    pub subs: &'static [Sub],
}

const fn s(id: i64, name: &'static str) -> Sub {
    Sub { id, name }
}

pub const TABLE: &[Root] = &[
    Root {
        id: 1000,
        name: "Console",
        subs: &[
            s(1010, "NDS"),
            s(1020, "PSP"),
            s(1030, "Wii"),
            s(1040, "Xbox"),
            s(1050, "Xbox 360"),
            s(1060, "WiiWare VC"),
            s(1070, "Xbox 360 DLC"),
            s(1080, "PS3"),
            s(1110, "3DS"),
            s(1120, "PS Vita"),
            s(1130, "WiiU"),
            s(1140, "Xbox One"),
            s(1180, "PS4"),
            s(1999, "Other"),
        ],
    },
    Root {
        id: 2000,
        name: "Movies",
        subs: &[
            s(2010, "Foreign"),
            s(2030, "SD"),
            s(2040, "HD"),
            s(2045, "UHD"),
            s(2050, "3D"),
            s(2060, "BluRay"),
            s(2070, "DVD"),
            s(2080, "WEBDL"),
            s(2090, "X265"),
            s(2999, "Other"),
        ],
    },
    Root {
        id: 3000,
        name: "Audio",
        subs: &[
            s(3010, "MP3"),
            s(3020, "Video"),
            s(3030, "Audiobook"),
            s(3040, "Lossless"),
            s(3050, "Podcast"),
            s(3060, "Foreign"),
            s(3999, "Other"),
        ],
    },
    Root {
        id: 4000,
        name: "PC",
        subs: &[
            s(4010, "0day"),
            s(4020, "ISO"),
            s(4030, "Mac"),
            s(4040, "Phone-Other"),
            s(4050, "Games"),
            s(4060, "Phone-IOS"),
            s(4070, "Phone-Android"),
        ],
    },
    Root {
        id: 5000,
        name: "TV",
        subs: &[
            s(5010, "WEB-DL"),
            s(5020, "Foreign"),
            s(5030, "SD"),
            s(5040, "HD"),
            s(5045, "UHD"),
            s(5060, "Sport"),
            s(5070, "Anime"),
            s(5080, "Documentary"),
            s(5090, "X265"),
            s(5999, "Other"),
        ],
    },
    Root {
        id: 6000,
        name: "XXX",
        subs: &[
            s(6010, "DVD"),
            s(6020, "WMV"),
            s(6030, "XviD"),
            s(6040, "x264"),
            s(6041, "HD Clips"),
            s(6042, "SD Clips"),
            s(6045, "UHD"),
            s(6046, "VR"),
            s(6047, "OnlyFans"),
            s(6050, "Packs"),
            s(6060, "Imageset"),
            s(6080, "SD"),
            s(6090, "WEBDL"),
            s(6999, "Other"),
        ],
    },
    Root {
        id: 7000,
        name: "Books",
        subs: &[
            s(7010, "Magazines"),
            s(7020, "Ebook"),
            s(7030, "Comics"),
            s(7040, "Technical"),
            s(7060, "Foreign"),
            s(7999, "Other"),
        ],
    },
    Root { id: 8000, name: "Other", subs: &[s(8010, "Misc"), s(8020, "Hashed")] },
];

/// The same sub-categories under Prowlarr's standard ids.
const ALIASES: [(i64, i64); 5] = [(1090, 1999), (2020, 2999), (5050, 5999), (6070, 6999), (7050, 7999)];

pub const fn root_of(id: i64) -> i64 {
    id - id.rem_euclid(1000)
}

pub const fn is_tv(id: i64) -> bool {
    root_of(id) == 5000
}

pub const fn is_movie(id: i64) -> bool {
    root_of(id) == 2000
}

/// "TV/HD", "Other/Hashed". None for an id outside the table.
pub fn label(id: i64) -> Option<String> {
    TABLE.iter().find_map(|root| {
        if root.id == id {
            return Some(root.name.to_string());
        }
        root.subs.iter().find(|s| s.id == id).map(|s| format!("{}/{}", root.name, s.name))
    })
}

pub(crate) fn is_leaf(id: i64) -> bool {
    TABLE.iter().any(|root| root.subs.iter().any(|s| s.id == id))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatFilter {
    pub leaves: Vec<i64>,
    pub uncategorized: bool,
}

/// None: no filter. Some with nothing in it: matches nothing.
pub fn parse_filter(param: &str) -> Option<CatFilter> {
    let tokens: Vec<i64> = param.split(',').filter_map(|t| t.trim().parse().ok()).collect();
    if tokens.is_empty() || tokens.contains(&-1) {
        return None;
    }
    let mut filter = CatFilter { leaves: Vec::new(), uncategorized: false };
    for id in tokens {
        if let Some(root) = TABLE.iter().find(|r| r.id == id) {
            filter.leaves.extend(root.subs.iter().map(|s| s.id));
            filter.uncategorized |= id == UNCATEGORIZED;
        } else if is_leaf(id) {
            filter.leaves.push(id);
        } else if let Some((_, target)) = ALIASES.iter().find(|(alias, _)| *alias == id) {
            filter.leaves.push(*target);
        }
    }
    filter.leaves.sort_unstable();
    filter.leaves.dedup();
    let every_leaf = TABLE.iter().map(|r| r.subs.len()).sum::<usize>();
    if filter.leaves.len() == every_leaf && filter.uncategorized {
        return None;
    }
    Some(filter)
}

/// `<category id=".." name=".."><subcat id=".." name=".."/>...</category>` for each root.
pub fn caps_xml() -> String {
    let mut out = String::new();
    for root in TABLE {
        out.push_str(&format!("<category id=\"{}\" name=\"{}\">", root.id, root.name));
        for sub in root.subs {
            out.push_str(&format!("<subcat id=\"{}\" name=\"{}\"/>", sub.id, sub.name));
        }
        out.push_str("</category>");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_id_is_unique_and_under_its_root() {
        let mut seen = std::collections::HashSet::new();
        for root in TABLE {
            assert!(seen.insert(root.id));
            assert_eq!(root_of(root.id), root.id);
            for sub in root.subs {
                assert!(seen.insert(sub.id), "{}", sub.id);
                assert_eq!(root_of(sub.id), root.id, "{}", sub.id);
            }
        }
    }

    #[test]
    fn no_sub_is_a_root() {
        for root in TABLE {
            assert!(root.subs.iter().all(|s| s.id != root.id));
            assert!(root.subs.iter().all(|s| !TABLE.iter().any(|r| r.id == s.id)));
        }
    }

    #[test]
    fn the_table_has_70_leaves() {
        assert_eq!(TABLE.iter().map(|r| r.subs.len()).sum::<usize>(), 70);
    }

    #[test]
    fn the_ids_clients_rely_on_are_there() {
        for id in [2040, 2045, 2999, 5030, 5040, 5045, 5999, 7020, 8010, 8020] {
            assert!(is_leaf(id), "{id}");
        }
        assert!(is_tv(5040) && !is_tv(2040));
        assert!(is_movie(2040) && !is_movie(5040));
    }

    #[test]
    fn a_parent_expands_to_its_own_subs() {
        let f = parse_filter("2000").unwrap();
        assert_eq!(f.leaves, [2010, 2030, 2040, 2045, 2050, 2060, 2070, 2080, 2090, 2999]);
        assert!(!f.leaves.contains(&2000));
        let f = parse_filter("5000,5040").unwrap();
        let mut unique = f.leaves.clone();
        unique.dedup();
        assert_eq!(unique, f.leaves);
    }

    #[test]
    fn eight_thousand_includes_rows_with_no_category() {
        let f = parse_filter("8000").unwrap();
        assert!(f.uncategorized);
        assert_eq!(f.leaves, [8010, 8020]);
        assert!(!parse_filter("8010").unwrap().uncategorized);
        let books = parse_filter("7000").unwrap();
        assert!(!books.uncategorized);
        assert_eq!(books.leaves.len(), 6);
        let caps = caps_xml();
        assert!(caps.contains(r#"<category id="7000" name="Books">"#));
        assert!(caps.contains(r#"<category id="8000" name="Other">"#));
    }

    #[test]
    fn parse_filter_table() {
        for none in ["", "abc", "-1", "5000,-1", " , "] {
            assert_eq!(parse_filter(none), None, "{none}");
        }
        assert_eq!(parse_filter("9999"), Some(CatFilter { leaves: vec![], uncategorized: false }));
        assert_eq!(parse_filter("abc,5040").unwrap().leaves, [5040]);
        assert_eq!(parse_filter(" 5030 , 5040 ").unwrap().leaves, [5030, 5040]);
        assert_eq!(parse_filter("2020").unwrap().leaves, [2999]);
        assert_eq!(parse_filter("5050").unwrap().leaves, [5999]);
        let every_root: Vec<String> = TABLE.iter().map(|r| r.id.to_string()).collect();
        assert_eq!(parse_filter(&every_root.join(",")), None);
    }

    #[test]
    fn labels() {
        assert_eq!(label(5040).as_deref(), Some("TV/HD"));
        assert_eq!(label(8020).as_deref(), Some("Other/Hashed"));
        assert_eq!(label(5000).as_deref(), Some("TV"));
        assert_eq!(label(9999), None);
    }

    #[test]
    fn caps_xml_lists_every_root_and_sub() {
        let caps = caps_xml();
        assert!(caps.contains(r#"<category id="5000" name="TV">"#));
        assert!(caps.contains(r#"<subcat id="5040" name="HD"/>"#));
        for root in TABLE {
            assert!(caps.contains(&format!("id=\"{}\"", root.id)));
            for sub in root.subs {
                assert!(caps.contains(&format!("id=\"{}\"", sub.id)));
            }
        }
    }
}
